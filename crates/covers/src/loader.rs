//! Cover requests by URL and size, served from memory, disk or network on a few worker threads.
//!
//! - Concurrent requests for the same cover and size share one fetch and decode (a "flight").
//! - A flight whose tickets are all dropped is skipped before it starts, or before decoding (the bytes
//!   are still kept on disk).
//! - Newest request first (LIFO), so the covers on screen after a fling come before the ones scrolled
//!   past.
//! - Workers start on demand up to the limit and sleep on a condvar; the disk cache is opened by the
//!   first worker, so the requesting (UI) thread never touches the disk.
//! - Workers exit and free their buffers when the loader rests: after [`Config::idle`] with no work,
//!   when hidden ([`Loader::show`]) or on low memory ([`Loader::trim`]). While hidden no worker waits.
//!
//! [`Paint`] decides the output: RGBA rows ([`Rgba`]) or a platform picture (Android Bitmaps).

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::{mpsc, Arc, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::Duration;

use nori_core::covers::is_provider_cover;
use nori_core::covers::CoverNet;
use nori_core::transport::{FailureKind, Transport, TransportError};
use parking_lot::{Condvar, Mutex};

use crate::decode::{self, header, Decoder};
use crate::disk::{DiskCache, Key};
use crate::memory::{Image, MemoryCache, Sized};
use crate::scale::Alpha;

pub struct Config {
    /// Disk cache directory; None disables the disk cache.
    pub dir: Option<PathBuf>,
    pub disk_bytes: u64,
    /// Memory cache limit in bytes; 0 for a client with its own cache (Android keeps its Bitmaps).
    pub memory_bytes: usize,
    /// Maximum worker threads.
    pub workers: usize,
    /// Alpha encoding for [`Rgba`].
    pub alpha: Alpha,
    /// Fetch timeout; 0 uses the transport's default.
    pub timeout_ms: u32,
    /// Idle time after which a visible loader rests ([`Loader::rest`]).
    pub idle: Duration,
}

impl Config {
    /// Defaults: the core's disk limit (`cover_rules`), 64 MB in memory, 2 to 4 workers (decoding is
    /// CPU-bound; more threads do not help).
    pub fn new(dir: impl Into<PathBuf>) -> Config {
        Config {
            dir: Some(dir.into()),
            disk_bytes: nori_core::covers::cover_rules().disk_bytes,
            memory_bytes: 64 << 20,
            workers: thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 4)),
            alpha: Alpha::Straight,
            timeout_ms: 0,
            idle: IDLE,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The request failed.
    Transport { kind: FailureKind, detail: Option<String> },
    /// Non-2xx status or empty body.
    Status(u16),
    Decode(decode::Error),
    /// The loader was dropped.
    Closed,
    /// Processing this cover panicked; the loader continues with the next.
    Panicked(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Transport { detail, .. } => f.write_str(detail.as_deref().unwrap_or("network error")),
            Error::Status(s) => write!(f, "HTTP {s}"),
            Error::Decode(e) => e.fmt(f),
            Error::Closed => f.write_str("closed"),
            Error::Panicked(why) => write!(f, "panicked: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<TransportError> for Error {
    fn from(e: TransportError) -> Error {
        let TransportError::Failed { kind, detail } = e;
        Error::Transport { kind, detail }
    }
}

/// Turns a cover file into the client's picture type, on a worker thread.
pub trait Paint: Send + Sync + 'static {
    /// Decoded cover, cloned to every waiter (should be cheap: an `Arc` or a handle).
    type Picture: Clone + Send + 'static;

    /// Decodes `bytes` for a `width` x `height` view (0 x 0: own size, capped at `decode::WHOLE_SIDE`).
    fn paint(&self, decoder: &mut Decoder, bytes: &[u8], width: u32, height: u32) -> Result<Self::Picture, decode::Error>;

    /// Size in bytes, for the memory cache.
    fn bytes(picture: &Self::Picture) -> usize;

    /// Frees per-cover scratch when the loader rests. Called outside the loader lock, possibly while
    /// other covers are painted.
    fn rest(&self) {}
}

/// Paints tight RGBA rows at exactly the requested size.
pub struct Rgba(pub Alpha);

impl Paint for Rgba {
    type Picture = Arc<Image>;

    fn paint(&self, decoder: &mut Decoder, bytes: &[u8], width: u32, height: u32) -> Result<Arc<Image>, decode::Error> {
        let (w, h) = if width == 0 || height == 0 {
            header(bytes)?.fill(0, 0)
        } else {
            (width as usize, height as usize)
        };
        let pixels = decoder.decode(bytes, w, h, self.0)?;
        Ok(Arc::new(Image { width: w as u32, height: h as u32, pixels: pixels.into_boxed_slice() }))
    }

    fn bytes(picture: &Arc<Image>) -> usize {
        picture.pixels.len()
    }
}

type Done<P> = Box<dyn FnOnce(Result<P, Error>) + Send>;

/// What a flight makes: a view's picture at a size, or only the file on disk (`Loader::warm`), which
/// never shares a flight with a view's request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Job {
    Picture(Sized),
    Disk(Key),
}

impl Job {
    fn key(&self) -> Key {
        match self {
            Job::Picture(s) => s.key,
            Job::Disk(k) => *k,
        }
    }
}

/// Default [`Config::idle`]: well above the gaps between covers while scrolling.
const IDLE: Duration = Duration::from_secs(20);

/// One in-progress cover at one size and its waiters.
struct Flight<P> {
    url: String,
    started: bool,
    waiters: Vec<(u64, Done<P>)>,
}

struct Jobs<P> {
    flights: HashMap<Job, Flight<P>>,
    /// Unstarted flights, newest last. Cancelled ones stay until a worker skips them.
    queue: Vec<Job>,
    /// Running worker threads, and how many of them are waiting.
    workers: usize,
    idle: usize,
    /// Bumped on each rest; workers from an older generation exit when out of work.
    generation: u64,
    /// Workers of the current generation (counted against the limit).
    current: usize,
    /// Set by [`Loader::show`].
    hidden: bool,
    next_id: u64,
    closed: bool,
}

impl<P> Default for Jobs<P> {
    fn default() -> Jobs<P> {
        Jobs { flights: HashMap::new(), queue: Vec::new(), workers: 0, idle: 0, generation: 0, current: 0, hidden: false, next_id: 0, closed: false }
    }
}

impl<P> Jobs<P> {
    /// Starts a new generation so running workers exit when idle; returns whether any are running.
    fn retire(&mut self) -> bool {
        self.generation += 1;
        self.current = 0;
        self.workers > 0
    }
}

struct Inner<P: Paint> {
    net: Arc<CoverNet>,
    dir: Option<PathBuf>,
    disk_bytes: u64,
    /// Opened lazily, off the requesting thread.
    disk: OnceLock<Option<DiskCache>>,
    memory: MemoryCache<P::Picture>,
    jobs: Mutex<Jobs<P::Picture>>,
    work: Condvar,
    workers: usize,
    paint: P,
    timeout_ms: u32,
    idle: Duration,
}

pub struct Loader<P: Paint = Rgba> {
    inner: Arc<Inner<P>>,
}

/// Type-erased loader handle so [`Ticket`] is not generic over [`Paint`].
trait Leave: Send + Sync {
    fn leave(&self, key: &Sized, id: u64);
}

impl<P: Paint> Leave for Inner<P> {
    fn leave(&self, key: &Sized, id: u64) {
        let mut jobs = self.jobs.lock();
        let job = Job::Picture(*key);
        let Some(f) = jobs.flights.get_mut(&job) else { return };
        f.waiters.retain(|(i, _)| *i != id);
        if f.waiters.is_empty() && !f.started {
            jobs.flights.remove(&job);
        }
    }
}

/// A request's handle. Dropping it (or [`Ticket::cancel`]) cancels the callback; [`Ticket::detach`]
/// lets the request finish unobserved. A cover completing concurrently with the drop may still be
/// delivered once: callbacks run outside the loader lock so a drop never waits on client code (on
/// Android a `@FastNative` call waiting on Java could stall the GC). Clients ignore such late covers.
#[must_use = "dropping a ticket cancels its request"]
pub struct Ticket {
    inner: Option<Arc<dyn Leave>>,
    key: Sized,
    id: u64,
}

impl Ticket {
    pub fn cancel(self) {}

    pub fn detach(mut self) {
        self.inner = None;
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.leave(&self.key, self.id);
        }
    }
}

impl Loader {
    /// A loader producing RGBA rows.
    pub fn new(config: Config, net: Arc<CoverNet>) -> Loader {
        let alpha = config.alpha;
        Loader::with_paint(config, net, Rgba(alpha))
    }
}

impl<P: Paint> Loader<P> {
    pub fn with_paint(config: Config, net: Arc<CoverNet>, paint: P) -> Loader<P> {
        let inner = Inner {
            net,
            dir: config.dir,
            disk_bytes: config.disk_bytes,
            disk: OnceLock::new(),
            memory: MemoryCache::new(config.memory_bytes),
            jobs: Mutex::new(Jobs::default()),
            work: Condvar::new(),
            workers: config.workers.max(1),
            paint,
            timeout_ms: config.timeout_ms,
            idle: config.idle,
        };
        Loader { inner: Arc::new(inner) }
    }

    /// The decoded cover from memory, if cached (lets a view skip the placeholder).
    pub fn cached(&self, url: &str, width: u32, height: u32) -> Option<P::Picture> {
        self.inner.memory.get(&Sized { key: Key::of(&self.inner.net, url), width, height })
    }

    /// Requests `url` decoded for `width` x `height` (0 x 0: own size, capped at `decode::WHOLE_SIDE`).
    /// `done` runs on a worker thread, or synchronously on a memory hit; not after the ticket is dropped
    /// (see [`Ticket`]).
    pub fn request(&self, url: &str, width: u32, height: u32, done: impl FnOnce(Result<P::Picture, Error>) + Send + 'static) -> Ticket {
        let key = Sized { key: Key::of(&self.inner.net, url), width, height };
        let inner = &self.inner;
        if let Some(picture) = inner.memory.get(&key) {
            done(Ok(picture));
            return Ticket { inner: None, key, id: 0 };
        }
        let mut jobs = inner.jobs.lock();
        jobs.next_id += 1;
        let id = jobs.next_id;
        match jobs.flights.entry(Job::Picture(key)) {
            Entry::Occupied(mut f) => f.get_mut().waiters.push((id, Box::new(done))),
            Entry::Vacant(v) => {
                // A flight may have completed between the first lookup and taking the lock.
                if let Some(picture) = inner.memory.get(&key) {
                    drop(jobs);
                    done(Ok(picture));
                    return Ticket { inner: None, key, id: 0 };
                }
                v.insert(Flight { url: url.to_owned(), started: false, waiters: vec![(id, Box::new(done))] });
                inner.enqueue(&mut jobs, Job::Picture(key), true);
            }
        }
        Ticket { inner: Some(inner.clone()), key, id }
    }

    /// Prefetches `url` to disk without decoding (e.g. for a downloaded song). Queued behind every view
    /// request. No-op for provider covers, which are never kept.
    pub fn warm(&self, url: &str) {
        if is_provider_cover(url) || self.inner.dir.is_none() {
            return;
        }
        let job = Job::Disk(Key::of(&self.inner.net, url));
        let inner = &self.inner;
        let mut jobs = inner.jobs.lock();
        if let Entry::Vacant(v) = jobs.flights.entry(job) {
            v.insert(Flight { url: url.to_owned(), started: false, waiters: Vec::new() });
            inner.enqueue(&mut jobs, job, false);
        }
    }

    /// Blocking [`Loader::request`]. Must not be called from a request callback (deadlock).
    pub fn load(&self, url: &str, width: u32, height: u32) -> Result<P::Picture, Error> {
        let (tx, rx) = mpsc::sync_channel(1);
        let _ticket = self.request(url, width, height, move |r| {
            let _ = tx.send(r);
        });
        rx.recv().unwrap_or(Err(Error::Closed))
    }

    /// Reads the raw cover file into `out` on this thread (from disk, or fetched and kept).
    pub fn read(&self, url: &str, out: &mut Vec<u8>) -> Result<(), Error> {
        let waker = Waker::from(Arc::new(Unpark(thread::current())));
        self.inner.bytes(Key::of(&self.inner.net, url), url, out, &waker)
    }

    pub fn paint(&self) -> &P {
        &self.inner.paint
    }

    /// Drops decoded covers and rests the workers. A low-memory signal.
    pub fn trim(&self) {
        self.inner.memory.clear();
        self.rest();
    }

    /// Frees worker threads and their buffers once idle, and calls [`Paint::rest`]. The memory cache
    /// is kept; the next request starts a worker again.
    pub fn rest(&self) {
        if self.inner.jobs.lock().retire() {
            self.inner.work.notify_all();
        }
        self.inner.paint.rest();
    }

    /// Sets whether covers are on screen (initially true). Hidden, the loader rests and workers exit as
    /// soon as they run out of work instead of waiting.
    pub fn show(&self, shown: bool) {
        self.inner.jobs.lock().hidden = !shown;
        if !shown {
            self.rest();
        }
    }

    /// The disk cache, opening it (reads the directory) if no worker has.
    pub fn disk(&self) -> Option<&DiskCache> {
        self.inner.disk()
    }
}

impl<P: Paint> Drop for Loader<P> {
    /// Stops workers after their current cover; pending waiters get `Closed`.
    fn drop(&mut self) {
        let flights = {
            let mut jobs = self.inner.jobs.lock();
            jobs.closed = true;
            jobs.queue.clear();
            std::mem::take(&mut jobs.flights)
        };
        self.inner.work.notify_all();
        for (_, f) in flights {
            for (_, done) in f.waiters {
                done(Err(Error::Closed));
            }
        }
    }
}

/// Waker that unparks the thread blocked in [`block_on`].
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Polls `f` to completion, parking between polls (Android's transport completes on OkHttp threads).
fn block_on<F: Future>(f: F, waker: &Waker) -> F::Output {
    let mut cx = Context::from_waker(waker);
    let mut f = pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        thread::park();
    }
}

/// Per-worker state reused across covers.
struct Worker {
    decoder: Decoder,
    bytes: Vec<u8>,
    waker: Waker,
}

impl Worker {
    fn new() -> Worker {
        Worker { decoder: Decoder::new(), bytes: Vec::new(), waker: Waker::from(Arc::new(Unpark(thread::current()))) }
    }
}

/// Decrements the worker counts (for generation `.1`) however the thread exits.
struct Leaving<'a, P: Paint>(&'a Inner<P>, u64);

impl<P: Paint> Drop for Leaving<'_, P> {
    fn drop(&mut self) {
        let mut jobs = self.0.jobs.lock();
        jobs.workers -= 1;
        if jobs.generation == self.1 {
            jobs.current -= 1;
        }
    }
}

/// The panic payload's message, if it is a string.
fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default()
}

impl<P: Paint> Inner<P> {
    fn disk(&self) -> Option<&DiskCache> {
        self.disk.get_or_init(|| self.dir.as_ref().and_then(|d| DiskCache::open(d, self.disk_bytes).ok())).as_ref()
    }

    /// Queues flight `job` (`first`: served next, else last), waking an idle worker or spawning one
    /// below the limit.
    fn enqueue(self: &Arc<Self>, jobs: &mut Jobs<P::Picture>, job: Job, first: bool) {
        if first {
            jobs.queue.push(job);
        } else {
            jobs.queue.insert(0, job);
        }
        if jobs.idle == 0 && jobs.current < self.workers {
            jobs.workers += 1;
            jobs.current += 1;
            let (inner, generation) = (self.clone(), jobs.generation);
            thread::Builder::new().name("nori-covers".into()).spawn(move || inner.work(generation)).expect("a thread for covers");
        } else {
            self.work.notify_one();
        }
    }

    fn work(self: Arc<Self>, generation: u64) {
        let _leaving = Leaving(&self, generation);
        let mut w = Worker::new();
        loop {
            let (job, url) = {
                let mut jobs = self.jobs.lock();
                loop {
                    if jobs.closed {
                        return;
                    }
                    // A worker from an old generation leaves the queue to newer ones, taking it only if
                    // none exist (rest was called with covers queued).
                    if jobs.generation != generation && jobs.current > 0 {
                        if !jobs.queue.is_empty() {
                            self.work.notify_one();
                        }
                        return;
                    }
                    if let Some(job) = jobs.queue.pop() {
                        match jobs.flights.get_mut(&job) {
                            Some(f) if !f.started => {
                                f.started = true;
                                break (job, std::mem::take(&mut f.url));
                            }
                            _ => continue,
                        }
                    }
                    if jobs.generation != generation || jobs.hidden {
                        return;
                    }
                    jobs.idle += 1;
                    // The last idle worker waits `idle` at most, then rests the loader.
                    let timed_out = if jobs.idle == jobs.workers {
                        self.work.wait_for(&mut jobs, self.idle).timed_out()
                    } else {
                        self.work.wait(&mut jobs);
                        false
                    };
                    jobs.idle -= 1;
                    if timed_out && jobs.idle + 1 == jobs.workers && jobs.queue.is_empty() && jobs.generation == generation {
                        jobs.retire();
                        drop(jobs);
                        self.work.notify_all();
                        self.paint.rest();
                        return;
                    }
                }
            };
            // A panic fails only this cover.
            let made = panic::catch_unwind(AssertUnwindSafe(|| match job {
                Job::Disk(key) => {
                    self.warm(key, &url, &mut w);
                    None
                }
                Job::Picture(sized) => self.fetch(&sized, &url, &mut w),
            }));
            let result = match made {
                Ok(Some(result)) => result,
                // The flight has already ended.
                Ok(None) => continue,
                Err(p) => {
                    // Reset possibly inconsistent buffers and refetch the file next time.
                    w = Worker::new();
                    if let Some(d) = self.disk() {
                        d.remove(job.key());
                    }
                    Err(Error::Panicked(panic_message(&*p)))
                }
            };
            let waiters = self.jobs.lock().flights.remove(&job).map(|f| f.waiters).unwrap_or_default();
            for (_, done) in waiters {
                let r = result.clone();
                // A panicking callback loses its own cover, not the worker.
                let _ = panic::catch_unwind(AssertUnwindSafe(move || done(r)));
            }
        }
    }

    /// Fetches the file to disk if missing, then ends the (waiter-less) flight.
    fn warm(&self, key: Key, url: &str, w: &mut Worker) {
        if self.disk().is_some_and(|d| !d.contains(key)) {
            let _ = self.bytes(key, url, &mut w.bytes, &w.waker);
        }
        self.jobs.lock().flights.remove(&Job::Disk(key));
    }

    /// Fetches and decodes; None when every waiter left, in which case the flight is already ended.
    fn fetch(&self, key: &Sized, url: &str, w: &mut Worker) -> Option<Result<P::Picture, Error>> {
        if let Err(e) = self.bytes(key.key, url, &mut w.bytes, &w.waker) {
            return Some(Err(e));
        }
        {
            let mut jobs = self.jobs.lock();
            let job = Job::Picture(*key);
            if jobs.flights.get(&job).is_none_or(|f| f.waiters.is_empty()) {
                // End the flight under the same lock as the check, so a request arriving later starts
                // its own flight instead of joining one that will never answer.
                jobs.flights.remove(&job);
                return None;
            }
        }
        Some(match self.paint.paint(&mut w.decoder, &w.bytes, key.width, key.height) {
            Ok(picture) => {
                self.memory.put(*key, picture.clone(), P::bytes(&picture));
                Ok(picture)
            }
            Err(e) => {
                // Drop corrupt files (maybe truncated) so they are refetched; keep unsupported formats
                // so they are not refetched every time.
                if let (decode::Error::Corrupt(_), Some(d)) = (&e, self.disk()) {
                    d.remove(key.key);
                }
                Err(Error::Decode(e))
            }
        })
    }

    /// Reads the cover file into `out` from disk, or fetches it and stores it (except provider covers).
    fn bytes(&self, key: Key, url: &str, out: &mut Vec<u8>, waker: &Waker) -> Result<(), Error> {
        if let Some(d) = self.disk() {
            if d.read(key, out) {
                return Ok(());
            }
            // Migrate a file stored under the legacy full-URL key.
            let whole = Key::of_address(url);
            if whole != key && d.read(whole, out) {
                let _ = d.put(key, out);
                d.remove(whole);
                return Ok(());
            }
        }
        let r = block_on(self.net.get(url.to_owned(), self.timeout_ms), waker)?;
        if !(200..300).contains(&r.status) || r.body.is_empty() {
            return Err(Error::Status(r.status));
        }
        *out = r.body;
        // Provider artwork changes under the same URL once the item is in the library; an answer that is
        // no picture (a Subsonic error sent with 200) is not kept either.
        if !is_provider_cover(url) && decode::format(out).is_some() {
            if let Some(d) = self.disk() {
                let _ = d.put(key, out);
            }
        }
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nori_core::covers::CoverNet;

    use super::{Config, Loader};
    use crate::disk::Key;
    use crate::memory::{Image, Sized};
    use crate::scale::Alpha;

    #[test]
    fn trim_drops_decoded_covers() {
        let loader = Loader::new(
            Config {
                dir: None,
                disk_bytes: 0,
                memory_bytes: 1 << 20,
                workers: 1,
                alpha: Alpha::Straight,
                timeout_ms: 0,
                idle: std::time::Duration::from_secs(60),
            },
            CoverNet::new(),
        );
        loader.inner.memory.put(
            Sized {
                key: Key([1; 16]),
                width: 1,
                height: 1,
            },
            Arc::new(Image {
                width: 1,
                height: 1,
                pixels: Box::new([9, 8, 7, 6]),
            }),
            4,
        );
        assert!(loader.inner.memory.bytes() >= 4);
        loader.trim();
        assert_eq!(loader.inner.memory.bytes(), 0);
    }
}
