//! Covers for a screen: asked for by address and size, answered from memory, the disk or the network, on
//! a few worker threads. Views asking for one cover at one size while it is on its way share the one
//! fetch and decode. A request whose views have all gone (a row scrolled away) is dropped before it
//! starts, or before its decode when the bytes are already coming: those are kept on disk all the same.
//!
//! The newest request is served first. In a list flung past a hundred covers, the ones on screen now
//! were asked for last, and the rows that flew by may be cancelled before a worker reaches them.
//!
//! Workers are started on the first requests, as many as are waited for up to the limit, and sleep on a
//! condition variable when there is nothing to do: an idle loader never wakes. Nothing touches the disk
//! on the thread that asks, not even opening the cache (its directory is read by the first worker), so a
//! GUI can ask from its own thread.
//!
//! A worker keeps its buffers from cover to cover while covers come, and lets them go with its thread
//! once the loader rests: when no cover has been asked for in a while ([`Config::idle`]), when no screen
//! shows covers ([`Loader::show`]), or when the client is short of memory ([`Loader::rest`]). Only the
//! first waits: the last worker to run out of work waits that long for the next cover, and only while a
//! screen shows covers (the phone is awake then). Out of sight, a worker ends as soon as it runs out of
//! work, so nothing waits and nothing wakes.
//!
//! What a cover is decoded into is the client's ([`Paint`]): RGBA rows ([`Rgba`]), or a platform's own
//! picture made at the right size and decoded straight into (Android's Bitmaps, crates/android).

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
use nori_core::transport::{FailureKind, Transport, TransportError};
use parking_lot::{Condvar, Mutex};

use crate::decode::{self, header, Decoder};
use crate::disk::{DiskCache, Key};
use crate::memory::{Image, MemoryCache, Sized};
use crate::scale::Alpha;

pub struct Config {
    /// Where covers are kept on disk; None keeps none.
    pub dir: Option<PathBuf>,
    pub disk_bytes: u64,
    /// Decoded covers kept in memory, in bytes. 0 for a client that keeps its own (Android keeps the
    /// Bitmaps it draws, which only it can hold).
    pub memory_bytes: usize,
    /// At most this many covers fetched and decoded at once.
    pub workers: usize,
    /// How [`Rgba`] writes a picture with transparency.
    pub alpha: Alpha,
    /// A fetch gives up after this long; 0 is the transport's own timeouts.
    pub timeout_ms: u32,
    /// A loader on screen whose workers have all waited this long for a cover rests ([`Loader::rest`]).
    pub idle: Duration,
}

impl Config {
    /// The core's limits (`cover_rules`) for the disk, 64 MB of decoded covers (about 180 at 300x300 and
    /// the player's at full size), and two to four workers: decoding is CPU work, and a fifth thread
    /// makes no cover on screen come sooner.
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
    /// The request did not come back.
    Transport { kind: FailureKind, detail: Option<String> },
    /// The server answered with an error, or with nothing.
    Status(u16),
    Decode(decode::Error),
    /// The loader was dropped first.
    Closed,
    /// Fetching, keeping or decoding this cover panicked; the loader goes on with the next.
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

/// What a loader makes of a cover's file, on the worker that fetched it.
pub trait Paint: Send + Sync + 'static {
    /// A decoded cover, handed to every view that waits for it (so cheap to clone: an `Arc`, a handle).
    type Picture: Clone + Send + 'static;

    /// The cover in `bytes`, decoded with `decoder` for a view `width` x `height` (0 x 0: at the file's
    /// own size, at most `decode::WHOLE_SIDE` a side).
    fn paint(&self, decoder: &mut Decoder, bytes: &[u8], width: u32, height: u32) -> Result<Self::Picture, decode::Error>;

    /// How many bytes the picture holds, for the memory cache's limit.
    fn bytes(picture: &Self::Picture) -> usize;

    /// Lets go of whatever is kept between covers, for a loader that rests (see [`Loader::rest`]). Called
    /// outside the loader's lock, possibly while other covers are painted.
    fn rest(&self) {}
}

/// RGBA rows at exactly the size asked for, for a client that draws them itself.
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

/// The size a warm-up is filed under: no view is that big, so it never shares a flight with one.
const WARM: u32 = u32::MAX;

/// How long a loader's workers wait for the next cover before the loader rests ([`Config::idle`]): long
/// past the gaps between a scrolling list's covers, so a list scrolled on keeps its threads and buffers.
const IDLE: Duration = Duration::from_secs(20);

/// One cover at one size on its way, and who waits for it.
struct Flight<P> {
    url: String,
    started: bool,
    /// Fetched onto the disk whether or not anyone waits, and not decoded (`Loader::warm`).
    warm: bool,
    waiters: Vec<(u64, Done<P>)>,
}

struct Jobs<P> {
    flights: HashMap<Sized, Flight<P>>,
    /// Flights not started yet, newest last. A cancelled one stays here until a worker skips it.
    queue: Vec<Sized>,
    /// Worker threads running, and how many of them wait for work.
    workers: usize,
    idle: usize,
    /// Which rest this is: a worker started before the last one ends when it finds nothing to do.
    generation: u64,
    /// Workers started since the last rest, which count against the limit.
    current: usize,
    /// No screen shows covers ([`Loader::show`]).
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
    /// Asks every worker running to end once it has nothing to do, and answers whether there was any.
    fn retire(&mut self) -> bool {
        self.generation += 1;
        self.current = 0;
        self.workers > 0
    }
}

struct Inner<P: Paint> {
    transport: Arc<dyn Transport>,
    dir: Option<PathBuf>,
    disk_bytes: u64,
    /// Opened by the first worker that needs it.
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

/// What a ticket reaches back into when it is dropped: the loader, whatever it paints.
trait Leave: Send + Sync {
    fn leave(&self, key: &Sized, id: u64);
}

impl<P: Paint> Leave for Inner<P> {
    fn leave(&self, key: &Sized, id: u64) {
        let mut jobs = self.jobs.lock();
        let Some(f) = jobs.flights.get_mut(key) else { return };
        f.waiters.retain(|(i, _)| *i != id);
        if f.waiters.is_empty() && !f.started && !f.warm {
            jobs.flights.remove(key);
        }
    }
}

/// A request's claim on its cover. Dropping it (or [`Ticket::cancel`]) says the view no longer wants the
/// cover, and its callback is not called for a cover finished after that; [`Ticket::detach`] lets the
/// request run on unwatched. A cover finished as the ticket is dropped may still be handed over, once:
/// the callback runs outside the loader's lock, and holding a lock over it would make whoever drops a
/// ticket wait on the client's own code (on Android, a `@FastNative` door waiting on Java, which can hold
/// up the garbage collector). A client drops such a late cover on its own side.
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
    /// A loader of RGBA rows over the client's `transport` (on a desktop, `nori-http`'s).
    pub fn new(config: Config, transport: Arc<dyn Transport>) -> Loader {
        let alpha = config.alpha;
        Loader::with_paint(config, transport, Rgba(alpha))
    }
}

impl<P: Paint> Loader<P> {
    /// A loader whose covers `paint` decodes.
    pub fn with_paint(config: Config, transport: Arc<dyn Transport>, paint: P) -> Loader<P> {
        let inner = Inner {
            transport,
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

    /// The cover at `width` x `height` if it is decoded and kept: what a view draws right away, with no
    /// placeholder, before it asks.
    pub fn cached(&self, url: &str, width: u32, height: u32) -> Option<P::Picture> {
        self.inner.memory.get(&Sized { key: Key::of(url), width, height })
    }

    /// Asks for the cover at `url`, decoded to fill `width` x `height` (0 x 0: at its own size, at most
    /// `decode::WHOLE_SIDE` a side). `done` gets it on a worker thread (a GUI posts it to its own), or at
    /// once on this one when it is in memory; it is not called if the ticket is dropped before the cover
    /// is finished (see [`Ticket`]).
    pub fn request(&self, url: &str, width: u32, height: u32, done: impl FnOnce(Result<P::Picture, Error>) + Send + 'static) -> Ticket {
        let key = Sized { key: Key::of(url), width, height };
        let inner = &self.inner;
        if let Some(picture) = inner.memory.get(&key) {
            done(Ok(picture));
            return Ticket { inner: None, key, id: 0 };
        }
        let mut jobs = inner.jobs.lock();
        jobs.next_id += 1;
        let id = jobs.next_id;
        match jobs.flights.entry(key) {
            Entry::Occupied(mut f) => f.get_mut().waiters.push((id, Box::new(done))),
            Entry::Vacant(v) => {
                // A flight that landed between the look above and the lock left its cover in memory.
                if let Some(picture) = inner.memory.get(&key) {
                    drop(jobs);
                    done(Ok(picture));
                    return Ticket { inner: None, key, id: 0 };
                }
                v.insert(Flight { url: url.to_owned(), started: false, warm: false, waiters: vec![(id, Box::new(done))] });
                inner.enqueue(&mut jobs, key, true);
            }
        }
        Ticket { inner: Some(inner.clone()), key, id }
    }

    /// Fetches the cover at `url` onto the disk, if it is not there, without decoding it: a cover that
    /// will be wanted (a song downloaded from a menu, which should arrive with its picture) without
    /// holding memory for it now. It waits behind every view's request, so it never keeps a cover on
    /// screen waiting. Provider covers are never kept, so they are not fetched either.
    pub fn warm(&self, url: &str) {
        if is_provider_cover(url) || self.inner.dir.is_none() {
            return;
        }
        let key = Sized { key: Key::of(url), width: WARM, height: WARM };
        let inner = &self.inner;
        let mut jobs = inner.jobs.lock();
        if let Entry::Vacant(v) = jobs.flights.entry(key) {
            v.insert(Flight { url: url.to_owned(), started: false, warm: true, waiters: Vec::new() });
            inner.enqueue(&mut jobs, key, false);
        }
    }

    /// The cover, waiting for it on this thread. Not from a `request` callback: that would wait on the
    /// worker it runs on.
    pub fn load(&self, url: &str, width: u32, height: u32) -> Result<P::Picture, Error> {
        let (tx, rx) = mpsc::sync_channel(1);
        let _ticket = self.request(url, width, height, move |r| {
            let _ = tx.send(r);
        });
        rx.recv().unwrap_or(Err(Error::Closed))
    }

    /// The file of the cover at `url` into `out`, from the disk or fetched (and kept), on this thread:
    /// for a caller that wants something other than a picture out of it (a page's colours).
    pub fn read(&self, url: &str, out: &mut Vec<u8>) -> Result<(), Error> {
        let waker = Waker::from(Arc::new(Unpark(thread::current())));
        self.inner.bytes(Key::of(url), url, out, &waker)
    }

    /// What paints this loader's covers: the client's own, for what it keeps beside them.
    pub fn paint(&self) -> &P {
        &self.inner.paint
    }

    /// Lets go of what the loader keeps for covers to come, for a client short of memory: every worker
    /// ends as soon as it has nothing to do, with its decoder's buffers and whatever the platform keeps
    /// per thread, and the painter lets go of its own ([`Paint::rest`]). The next request starts a worker
    /// again. Decoded covers in memory stay.
    pub fn rest(&self) {
        if self.inner.jobs.lock().retire() {
            self.inner.work.notify_all();
        }
        self.inner.paint.rest();
    }

    /// Whether a screen shows this loader's covers (at first, one does). Out of sight the loader rests,
    /// and a worker started for a cover asked for then (a notification's) ends as soon as it runs out of
    /// work: nobody scrolls, and a worker that waited would hold its buffers, or wake to let them go.
    pub fn show(&self, shown: bool) {
        let mut jobs = self.inner.jobs.lock();
        jobs.hidden = !shown;
        if !shown {
            let any = jobs.retire();
            drop(jobs);
            if any {
                self.inner.work.notify_all();
            }
            self.inner.paint.rest();
        }
    }

    /// The disk cache, opened now if no worker has yet: this reads its directory.
    pub fn disk(&self) -> Option<&DiskCache> {
        self.inner.disk()
    }
}

impl<P: Paint> Drop for Loader<P> {
    /// Stops the workers once they finish what they are on; whoever still waits gets `Closed`.
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

/// Wakes the thread parked on a future the transport has not finished.
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs `f` to its end on this thread. A desktop transport finishes on its first poll; one that does not
/// (Android's, answered by OkHttp's own threads) wakes this thread when it can go on.
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

/// What a worker keeps from cover to cover.
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

/// Counts a worker (started in the rest `.1`) out however its thread ends, so the next request starts
/// another in its place.
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

/// What a panic said, when it said it in words.
fn said(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default()
}

impl<P: Paint> Inner<P> {
    fn disk(&self) -> Option<&DiskCache> {
        self.disk.get_or_init(|| self.dir.as_ref().and_then(|d| DiskCache::open(d, self.disk_bytes).ok())).as_ref()
    }

    /// Queues the flight `key`, just filed, for a worker, `first` ahead of everything waiting or else
    /// behind it: an idle worker is woken, or one more started while there are fewer than the limit.
    fn enqueue(self: &Arc<Self>, jobs: &mut Jobs<P::Picture>, key: Sized, first: bool) {
        if first {
            jobs.queue.push(key);
        } else {
            jobs.queue.insert(0, key);
        }
        // Only workers started since the last rest count against the limit: the others end.
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
            let (key, url, warm) = {
                let mut jobs = self.jobs.lock();
                loop {
                    if jobs.closed {
                        return;
                    }
                    // A worker from before the last rest leaves what is queued to those started since, and
                    // takes it only when there are none (the client said rest while covers were queued).
                    if jobs.generation != generation && jobs.current > 0 {
                        if !jobs.queue.is_empty() {
                            self.work.notify_one();
                        }
                        return;
                    }
                    if let Some(key) = jobs.queue.pop() {
                        match jobs.flights.get_mut(&key) {
                            Some(f) if !f.started => {
                                f.started = true;
                                break (key, std::mem::take(&mut f.url), f.warm);
                            }
                            _ => continue,
                        }
                    }
                    if jobs.generation != generation || jobs.hidden {
                        return;
                    }
                    jobs.idle += 1;
                    // The last worker to run out of work waits only so long: if no cover has come for any
                    // worker by then, the loader rests.
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
            // One broken file must not stop every cover after it: a panic is this cover's error, and its
            // waiters are answered like any other's.
            let job = panic::catch_unwind(AssertUnwindSafe(|| if warm { self.warm(&key, &url, &mut w) } else { self.fetch(&key, &url, &mut w) }));
            let result = job.unwrap_or_else(|p| {
                // The decoder's buffers may be anywhere mid-picture, and the file is fetched again next
                // time rather than kept.
                w = Worker::new();
                if let Some(d) = self.disk() {
                    d.remove(key.key);
                }
                Err(Error::Panicked(said(&*p)))
            });
            // A fetch nobody waited for has ended its flight already; the one filed under the same key
            // since is a new request's, not this one's to answer.
            let ended = !warm && matches!(result, Err(Error::Closed));
            let waiters = if ended { Vec::new() } else { self.jobs.lock().flights.remove(&key).map(|f| f.waiters).unwrap_or_default() };
            for (_, done) in waiters {
                let r = result.clone();
                // Nor does a client's call back that panics: it loses its own cover, not the thread.
                let _ = panic::catch_unwind(AssertUnwindSafe(move || done(r)));
            }
        }
    }

    /// The file onto the disk, unless it is there; no one waits for a warm-up, so it answers nobody.
    fn warm(&self, key: &Sized, url: &str, w: &mut Worker) -> Result<P::Picture, Error> {
        if self.disk().is_some_and(|d| !d.contains(key.key)) {
            self.bytes(key.key, url, &mut w.bytes, &w.waker)?;
        }
        Err(Error::Closed)
    }

    fn fetch(&self, key: &Sized, url: &str, w: &mut Worker) -> Result<P::Picture, Error> {
        self.bytes(key.key, url, &mut w.bytes, &w.waker)?;
        {
            let mut jobs = self.jobs.lock();
            if jobs.flights.get(key).is_none_or(|f| f.waiters.is_empty()) {
                // Nobody wants it decoded any more, and the flight ends here, under the same lock as the
                // look: a view that asks for this cover from now on starts a flight of its own, answered
                // from the disk. Ended by the worker after the lock was let go, a view that joined in
                // between (the player skipping away from a song and straight back) was answered with
                // nothing, and drew the placeholder for good.
                jobs.flights.remove(key);
                return Err(Error::Closed);
            }
        }
        match self.paint.paint(&mut w.decoder, &w.bytes, key.width, key.height) {
            Ok(picture) => {
                self.memory.put(*key, picture.clone(), P::bytes(&picture));
                Ok(picture)
            }
            Err(e) => {
                // A file that does not decode is fetched again next time rather than kept: it may have
                // been cut short. One in a format this cannot decode stays, or it would be fetched
                // again every time it is shown.
                if let (decode::Error::Corrupt(_), Some(d)) = (&e, self.disk()) {
                    d.remove(key.key);
                }
                Err(Error::Decode(e))
            }
        }
    }

    /// The cover's file into `out`: from the disk, or fetched and kept there (a provider's is not).
    fn bytes(&self, key: Key, url: &str, out: &mut Vec<u8>, waker: &Waker) -> Result<(), Error> {
        if let Some(d) = self.disk() {
            if d.read(key, out) {
                return Ok(());
            }
            // Kept before keys left the signature out: moved under its key the first time it is read.
            let whole = Key::of_address(url);
            if whole != key && d.read(whole, out) {
                let _ = d.put(key, out);
                d.remove(whole);
                return Ok(());
            }
        }
        let r = block_on(self.transport.get(url.to_owned(), self.timeout_ms), waker)?;
        if !(200..300).contains(&r.status) || r.body.is_empty() {
            return Err(Error::Status(r.status));
        }
        *out = r.body;
        // Provider artwork is redrawn under the same address once the item is in the library.
        if !is_provider_cover(url) {
            if let Some(d) = self.disk() {
                let _ = d.put(key, out);
            }
        }
        Ok(())
    }
}
