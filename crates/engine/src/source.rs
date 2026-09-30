//! A song's bytes over the network, fetched in bursts through the client's [`ByteSource`]. One loader
//! thread per song fills a window ahead of the demuxer up to the `load_control` high mark, closes the
//! connection, and sleeps until the reader comes within the low mark. Most songs fit the memory cap and
//! arrive in one burst. The cap is shared: a song fetched ahead gets what the playing one leaves
//! (`Loader::limit`). With a stream cache entry, each burst is written to it; once the entry is whole
//! the loader drops its memory copy and reads the file.
//!
//! A live stream keeps one connection open and a window that moves with the reader ([`Loader::live`]);
//! ICY `StreamTitle`s are taken out of the bytes and said once the reader passes them
//! ([`Loader::announced`]).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::Thread;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use symphonia::core::io::MediaSource;

use crate::arriving::Listening;
use crate::store::Writer;

/// Makes the stream cache entry a loader writes into, on the loader's thread (it may wait for the
/// fetching ahead to hand the song over, `Store::writer_for_player`). A partly written entry is continued.
pub type Keep = Box<dyn FnOnce() -> Option<Writer> + Send>;

/// A response body: its offset in the resource (0 when the server ignores ranges), the resource's
/// length when known, and the bytes.
pub struct Body {
    pub start: u64,
    pub len: Option<u64>,
    pub reader: Box<dyn Read + Send>,
}

/// Why [`ByteSource::open`] gave no body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// Nothing at or after `from` (HTTP 416, or a rangeless answer shorter than `from`); `len` from
    /// `Content-Range: bytes */N` if given. Not a failure: how a transcode's real end is learned when
    /// its promised length was an estimate (Navidrome's `estimateContentLength`).
    PastEnd { len: Option<u64> },
    /// The server refused with this status (retried; not the network's failure).
    Status(u16),
    /// Unreachable now (retried).
    Failed(String),
    /// No answer or next byte within [`Waits::stall_ms`]. Fails a song whose first bytes never come;
    /// a body that stalls half way is asked again from where it stopped.
    TimedOut,
}

impl From<String> for OpenError {
    fn from(why: String) -> OpenError {
        OpenError::Failed(why)
    }
}

impl From<&str> for OpenError {
    fn from(why: &str) -> OpenError {
        OpenError::Failed(why.to_string())
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::PastEnd { len: Some(l) } => write!(f, "past the end, which is at byte {l}"),
            OpenError::PastEnd { len: None } => f.write_str("past the end"),
            OpenError::Status(status) => write!(f, "HTTP {status}"),
            OpenError::Failed(why) => f.write_str(why),
            OpenError::TimedOut => f.write_str("no answer in time"),
        }
    }
}

// ---- requests called off ----

/// Real-time limits of a song's fetch ([`crate::Clock::waits`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waits {
    /// A request with no answer, or no next byte, for this long is called off ([`OpenError::TimedOut`]).
    /// Well under [`READ_TIMEOUT`], so a server that never answers fails as the network's failure.
    pub stall_ms: u64,
    /// A dropped connection is tried again after twice this, doubled for each retry after it.
    pub retry_ms: u64,
}

impl Default for Waits {
    fn default() -> Self {
        Waits { stall_ms: 20_000, retry_ms: 500 }
    }
}

/// Cancels a running request from another thread: its song was let go, it stalled, or newer requests
/// crowded it out ([`MAX_ASKING`]). The client's HTTP stack registers [`Cancel::on_cancel`] or polls
/// [`Cancel::cancelled`]. One per loader, reset for each request ([`Cancel::begin`]).
#[derive(Clone)]
pub struct Cancel(Arc<Mutex<CallState>>);

struct CallState {
    /// The song was let go: every request of it is off.
    closed: bool,
    /// This request is off; `timed_out` if it stalled.
    off: bool,
    timed_out: bool,
    /// How the client cancels the running request.
    hook: Option<Box<dyn FnOnce() + Send>>,
    /// Called off at this moment unless it moves first.
    deadline: Option<std::time::Instant>,
    watched: bool,
    stall_ms: u64,
}

impl Default for Cancel {
    fn default() -> Self {
        Cancel::stalling_after(Waits::default().stall_ms)
    }
}

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// A request called off after `ms` without an answer or a byte.
    pub(crate) fn stalling_after(ms: u64) -> Cancel {
        let s = CallState { closed: false, off: false, timed_out: false, hook: None, deadline: None, watched: false, stall_ms: ms };
        Cancel(Arc::new(Mutex::new(s)))
    }

    /// Whether the request is off (for an HTTP stack that polls).
    pub fn cancelled(&self) -> bool {
        let s = self.0.lock();
        s.closed || s.off
    }

    /// Whether it was called off for stalling.
    pub fn timed_out(&self) -> bool {
        self.0.lock().timed_out
    }

    /// Registers how to cancel the running request; runs it at once if already off. Replaces the last.
    pub fn on_cancel(&self, off: impl FnOnce() + Send + 'static) {
        let mut s = self.0.lock();
        if s.closed || s.off {
            drop(s);
            off();
            return;
        }
        s.hook = Some(Box::new(off));
    }

    /// A new request begins: clears the last one's cancellation (not a closed song) and arms the stall.
    pub(crate) fn begin(&self) {
        self.stalls(true);
    }

    /// The request moved (an answer, a byte): re-arms the stall.
    pub(crate) fn moved(&self) {
        self.stalls(false);
    }

    /// Arms the stall deadline. `anew`: a new request, cleared under the same lock so the last one's
    /// cancellation cannot land on it.
    fn stalls(&self, anew: bool) {
        let mut s = self.0.lock();
        if anew {
            s.off = false;
            s.timed_out = false;
            s.hook = None;
        }
        s.deadline = Some(std::time::Instant::now() + Duration::from_millis(s.stall_ms));
        if !s.watched {
            s.watched = true;
            drop(s);
            watch(Arc::downgrade(&self.0));
        }
    }

    /// The request is over (its body dropped).
    pub(crate) fn end(&self) {
        let mut s = self.0.lock();
        s.hook = None;
        s.deadline = None;
    }

    /// Cancels the running request.
    pub(crate) fn call_off(&self, stalled: bool) {
        let hook = {
            let mut s = self.0.lock();
            s.off = true;
            s.timed_out |= stalled;
            s.deadline = None;
            s.hook.take()
        };
        if let Some(h) = hook {
            h();
        }
    }

    /// The song was let go: cancels this and every later request.
    pub(crate) fn close(&self) {
        self.0.lock().closed = true;
        self.call_off(false);
    }

    pub(crate) fn closed(&self) -> bool {
        self.0.lock().closed
    }
}

/// Requests with a stall deadline, and whether the thread that cancels stalled ones runs. Process-wide:
/// one sleeping timer thread for all loaders, alive only while there are requests.
#[derive(Default)]
struct Watch {
    calls: Vec<Weak<Mutex<CallState>>>,
    running: bool,
}

static WATCH: Mutex<Watch> = Mutex::new(Watch { calls: Vec::new(), running: false });
static WATCH_CV: Condvar = Condvar::new();

fn watch(call: Weak<Mutex<CallState>>) {
    let mut w = WATCH.lock();
    w.calls.push(call);
    if w.running {
        WATCH_CV.notify_all();
        return;
    }
    w.running = true;
    drop(w);
    if std::thread::Builder::new().name("nori-stall".into()).spawn(watching).is_err() {
        WATCH.lock().running = false;
    }
}

fn watching() {
    let mut w = WATCH.lock();
    loop {
        let now = std::time::Instant::now();
        let mut due = Vec::new();
        let mut next: Option<std::time::Instant> = None;
        w.calls.retain(|c| {
            let Some(c) = c.upgrade() else { return false };
            let mut s = c.lock();
            match s.deadline {
                Some(at) if at <= now => {
                    due.push(c.clone());
                    s.watched = false;
                    false
                }
                Some(at) => {
                    next = Some(next.map_or(at, |n| n.min(at)));
                    true
                }
                None => {
                    s.watched = false;
                    false
                }
            }
        });
        if !due.is_empty() {
            drop(w);
            for c in due {
                Cancel(c).call_off(true);
            }
            w = WATCH.lock();
            continue;
        }
        match next {
            Some(at) => {
                WATCH_CV.wait_until(&mut w, at);
            }
            None => {
                w.running = false;
                return;
            }
        }
    }
}

/// Requests to one [`ByteSource`] waiting for an answer at once. A server that never answers must not
/// take every request the client's HTTP stack lets run to it (OkHttp's per-host cap): past this many the
/// oldest still waiting is called off, the newest being the one wanted.
const MAX_ASKING: usize = 4;

/// Process-wide, as the sources share one HTTP client; each source keeps under its own cap (the one
/// fetching ahead asks once at a time, so the player's and its stay within OkHttp's five per host).
static ASKING: Asking = Asking(Mutex::new(Vec::new()));

/// The requests waiting for an answer, oldest first, by the source they go to.
struct Asking(Mutex<Vec<(usize, Cancel)>>);

impl Asking {
    /// `cancel`'s request to `source` is about to be made: it counts until [`Asking::answered`], and the
    /// oldest waiting on the same source is called off when there are too many.
    fn asking(&self, source: usize, cancel: &Cancel) {
        let crowded = {
            let mut a = self.0.lock();
            a.retain(|(_, c)| !c.cancelled());
            a.push((source, cancel.clone()));
            let same = a.iter().filter(|(s, _)| *s == source).count();
            (same > MAX_ASKING).then(|| a.iter().position(|(s, _)| *s == source).map(|k| a.remove(k).1)).flatten()
        };
        if let Some(c) = crowded {
            c.call_off(false);
        }
    }

    fn answered(&self, cancel: &Cancel) {
        self.0.lock().retain(|(_, c)| !Arc::ptr_eq(&c.0, &cancel.0));
    }
}

/// Opens `url` from `from` through `source` (cache `key` if it keeps what it reads) as `cancel`'s
/// request, watched for stalls and crowding while it opens and while its body is read.
pub(crate) fn open_watched(source: &dyn ByteSource, url: &str, key: Option<&str>, from: u64, cancel: &Cancel) -> Result<Body, OpenError> {
    if cancel.closed() {
        return Err("the song is no longer wanted".into());
    }
    cancel.begin();
    ASKING.asking(source as *const dyn ByteSource as *const () as usize, cancel);
    let opened = source.open_cancellable(url, key, from, cancel);
    ASKING.answered(cancel);
    let stalled = cancel.timed_out();
    match opened {
        Ok(mut b) if !cancel.cancelled() => {
            cancel.moved();
            b.reader = Box::new(Watched { inner: b.reader, cancel: cancel.clone() });
            Ok(b)
        }
        Ok(_) if stalled => Err(OpenError::TimedOut),
        Ok(_) => Err("called off".into()),
        // Cancelled for stalling, whatever error the client made of it.
        Err(OpenError::Failed(_)) if stalled => Err(OpenError::TimedOut),
        Err(e) => {
            cancel.end();
            Err(e)
        }
    }
}

/// A body under watch: each read re-arms the stall; the end disarms it.
struct Watched {
    inner: Box<dyn Read + Send>,
    cancel: Cancel,
}

impl Read for Watched {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let got = self.inner.read(buf);
        match &got {
            Ok(0) => self.cancel.end(),
            Ok(_) => self.cancel.moved(),
            Err(_) if self.cancel.timed_out() => return Err(io::Error::new(io::ErrorKind::TimedOut, OpenError::TimedOut.to_string())),
            Err(_) => {}
        }
        got
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        self.cancel.end();
    }
}

/// The client's HTTP for audio: GET `url` from byte `from` (a `Range` request), blocking on the loader's
/// thread. A range at or past the end is [`OpenError::PastEnd`].
pub trait ByteSource: Send + Sync {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError>;

    /// [`ByteSource::open`] cached under `key` by a client whose HTTP stack caches (media3 on Android).
    fn open_keyed(&self, url: &str, _key: &str, from: u64) -> Result<Body, OpenError> {
        self.open(url, from)
    }

    /// [`ByteSource::open`] (or `open_keyed`) that `cancel` can abort: the client should hook
    /// [`Cancel::on_cancel`] so a hung request frees its connection and dispatcher slot. By default the
    /// request is not cancellable.
    fn open_cancellable(&self, url: &str, key: Option<&str>, from: u64, _cancel: &Cancel) -> Result<Body, OpenError> {
        match key {
            Some(k) => self.open_keyed(url, k, from),
            None => self.open(url, from),
        }
    }

    /// A live stream with `Icy-MetaData: 1`: the body and the answer's `icy-metaint`, if any.
    fn open_live(&self, url: &str) -> Result<(Body, Option<usize>), String> {
        self.open(url, 0).map(|b| (b, None)).map_err(|e| e.to_string())
    }
}

/// `url` from `from`: a rangeless answer is skipped forward to `from`; one ending before it is
/// [`OpenError::PastEnd`].
fn open_at(source: &dyn ByteSource, url: &str, from: u64, cancel: &Cancel) -> Result<Body, OpenError> {
    let mut b = open_watched(source, url, None, from, cancel)?;
    if b.start < from {
        let want = from - b.start;
        match io::copy(&mut (&mut b.reader).take(want), &mut io::sink()) {
            Ok(n) if n == want => {}
            Ok(n) => return Err(OpenError::PastEnd { len: Some(b.start + n) }),
            Err(_) if cancel.timed_out() => return Err(OpenError::TimedOut),
            Err(e) => return Err(OpenError::Failed(e.to_string())),
        }
        b.len = b.len.filter(|&l| l >= from);
        b.start = from;
    }
    Ok(b)
}

/// Fill up to `high` bytes ahead of the reader, start again below `low`, hold at most `cap`
/// (from `nori_player::transport::load_control`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub low: u64,
    pub high: u64,
    pub cap: u64,
}

/// Bytes per ms assumed when unknown (320 kbps, erring towards fetching more).
const BYTES_PER_MS_GUESS: u64 = 40;
/// Read size: as much as makes a song ready ([`READY`]), so a burst is a few large reads.
const CHUNK: usize = 256 * 1024;
/// A reader this far past what is loaded has seeked: fetch from there.
const FAR: u64 = 1024 * 1024;
/// Bytes ahead that make a song ready to read without waiting.
pub(crate) const READY: u64 = 256 * 1024;
/// A dropped connection is tried again this many times before the song counts as failed.
const RETRIES: u32 = 3;
/// How long a read waits for bytes before failing.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// A live stream is ready with this much: two seconds at 128 kbps.
const LIVE_READY: u64 = 32 * 1024;
/// A live stream keeps this much behind the reader (for the demuxer) and at most this much ahead
/// (reached only while paused; the connection then waits in the socket).
const LIVE_BEHIND: u64 = 256 * 1024;
const LIVE_AHEAD: u64 = 2 * 1024 * 1024;

impl Window {
    /// The window for a song of `duration_ms` and `len` bytes (either may be unknown), from
    /// `load_control` (min ms, max ms, .., .., byte cap).
    pub fn for_song(load: [i64; 5], duration_ms: Option<i64>, len: Option<u64>) -> Window {
        let per_ms = match (duration_ms, len) {
            (Some(d), Some(l)) if d > 0 => (l / d as u64).max(1),
            _ => BYTES_PER_MS_GUESS,
        };
        let cap = load[4].max(1) as u64;
        Window { low: (load[0] as u64 * per_ms).min(cap / 2), high: (load[1] as u64 * per_ms).min(cap), cap }
    }
}

#[derive(Default)]
struct State {
    /// Bytes `base..base + data.len()` of the resource.
    base: u64,
    data: Vec<u8>,
    /// The whole song's cache entry, read instead of `data` (then empty).
    disk: Option<Arc<File>>,
    len: Option<u64>,
    /// Loaded to the end.
    done: bool,
    error: Option<String>,
    /// The server's error status, if it answered.
    answered: Option<u16>,
    /// Where the demuxer reads.
    reader_at: u64,
    /// The reader wants bytes from here, outside what is kept.
    restart: Option<u64>,
    closed: bool,
    /// The engine thread to wake when ready.
    waiter: Option<Thread>,
    /// Readers blocked in a read. A count: one giving up must not leave another unwoken.
    blocked: u32,
    /// Connections opened: one per burst.
    bursts: u32,
    window: Option<Window>,
    /// Bytes held at most, below the window's cap (`Loader::limit`).
    budget: Option<u64>,
    /// A live stream.
    live: bool,
    /// ICY titles not yet passed by the reader (with their byte), and the last one passed.
    titles: std::collections::VecDeque<(u64, String)>,
    announced: Option<String>,
    /// Times the length turned out shorter than promised ([`Loader::shortened`]).
    shortened: u32,
    /// The song ends before this byte ([`State::not_at`]): a promised length at or past it is ignored.
    before: Option<u64>,
}

impl State {
    /// The resource ends at `at`, shortening a promised length.
    fn ends_at(&mut self, at: u64) {
        match self.len {
            Some(l) if l <= at => {}
            Some(_) => {
                self.len = Some(at);
                self.shortened += 1;
            }
            None => self.len = Some(at),
        }
    }

    /// The resource ends somewhere before `at`: drops a promised length at or past it.
    fn not_at(&mut self, at: u64) {
        self.before = Some(self.before.map_or(at, |b| b.min(at)));
        if self.len.is_some_and(|l| l >= at) {
            self.len = None;
            self.shortened += 1;
        }
    }

    /// The song's window, within the budget.
    fn window(&self, load: [i64; 5], duration_ms: Option<i64>) -> Window {
        let w = self.window.unwrap_or_else(|| Window::for_song(load, duration_ms, self.len));
        match self.budget {
            Some(b) if b < w.cap => Window { low: w.low.min(b / 2), high: w.high.min(b), cap: b },
            _ => w,
        }
    }

    fn end(&self) -> u64 {
        match (&self.disk, self.len) {
            (Some(_), Some(len)) => len,
            _ => self.base + self.data.len() as u64,
        }
    }

    fn ahead(&self) -> u64 {
        self.end().saturating_sub(self.reader_at)
    }

    fn at_end(&self) -> bool {
        self.done || self.len.is_some_and(|l| self.end() >= l)
    }

    fn ready(&self) -> bool {
        self.at_end() || self.error.is_some() || self.ahead() >= if self.live { LIVE_READY } else { READY }
    }

    /// Takes the titles the reader passed; keeps the last.
    fn passed(&mut self) {
        while self.titles.front().is_some_and(|(at, _)| *at <= self.reader_at) {
            self.announced = self.titles.pop_front().map(|(_, t)| t);
        }
    }
}

/// One song's bytes, shared by its loader thread and readers.
struct Loaded {
    state: Mutex<State>,
    cv: Condvar,
    /// Its running request, cancelled when the song is let go.
    cancel: Cancel,
    retry_ms: u64,
}

/// A song being loaded; dropping the last handle stops the loader and frees its memory.
pub struct Loader(Arc<Loaded>);

impl Loader {
    /// Starts loading `url` on a thread of its own, writing into `keep` if given.
    pub fn start(source: Arc<dyn ByteSource>, url: String, load: [i64; 5], duration_ms: Option<i64>, keep: Option<Writer>) -> Arc<Loader> {
        Loader::start_within(source, url, load, duration_ms, keep.map(|w| Box::new(move || Some(w)) as Keep), None, None, Waits::default())
    }

    /// [`Loader::start`] with a `budget` (`Loader::limit`), a lazily made cache entry, and a `taker`
    /// fed every byte in order from the first (AutoMix's measuring, `crate::arriving`); a seek drops it.
    #[allow(clippy::too_many_arguments)]
    pub fn start_within(source: Arc<dyn ByteSource>, url: String, load: [i64; 5], duration_ms: Option<i64>, keep: Option<Keep>, budget: Option<u64>, taker: Option<Listening>, waits: Waits) -> Arc<Loader> {
        let loaded = Arc::new(Loaded::new(State { budget, ..State::default() }, waits));
        alive(&loaded);
        let l = loaded.clone();
        std::thread::Builder::new()
            .name("nori-load".into())
            .spawn(move || {
                // A panicking loader fails its song rather than leaving readers waiting.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| l.run(&*source, &url, load, duration_ms, keep, taker))).is_err() {
                    l.gave_up("the song's loader failed");
                }
            })
            .expect("a thread for loading");
        Arc::new(Loader(loaded))
    }

    /// Bytes are still on their way: wanted, not failed, not complete.
    pub fn fetching(&self) -> bool {
        let s = self.0.state.lock();
        !s.closed && s.error.is_none() && !s.at_end()
    }

    /// A live stream at `url`, loaded while held.
    pub fn live(source: Arc<dyn ByteSource>, url: String, waits: Waits) -> Arc<Loader> {
        let loaded = Arc::new(Loaded::new(State { live: true, ..State::default() }, waits));
        alive(&loaded);
        let l = loaded.clone();
        std::thread::Builder::new().name("nori-live".into()).spawn(move || l.run_live(&*source, &url)).expect("a thread for loading");
        Arc::new(Loader(loaded))
    }

    /// The latest ICY title the reader reached, once.
    pub fn announced(&self) -> Option<String> {
        let mut s = self.0.state.lock();
        s.passed();
        s.announced.take()
    }

    /// Its state in words, for a stall report.
    pub fn words(&self) -> String {
        let s = self.0.state.lock();
        let len = s.len.map_or("?".to_string(), |l| l.to_string());
        let mut w = format!("{}..{} of {len} bytes, reader at {}", s.base, s.end(), s.reader_at);
        if s.disk.is_some() {
            w.push_str(", read from the disk");
        }
        if s.done {
            w.push_str(", loaded to its end");
        }
        if let Some(at) = s.restart {
            w.push_str(&format!(", wanted from {at}"));
        }
        if s.blocked > 0 {
            w.push_str(&format!(", {} readers blocked", s.blocked));
        }
        if s.waiter.is_some() {
            w.push_str(", the engine waits on it");
        }
        if !s.ready() {
            w.push_str(", not ready");
        }
        if let Some(e) = &s.error {
            w.push_str(&format!(", gave up: {e}"));
        }
        if s.closed {
            w.push_str(", closed");
        }
        w.push_str(&format!(", {} bursts", s.bursts));
        w
    }

    /// Connections opened: one per burst.
    pub fn bursts(&self) -> u32 {
        self.0.state.lock().bursts
    }

    /// Bytes held in memory.
    pub fn held(&self) -> usize {
        self.0.state.lock().data.len()
    }

    /// Reads from its cache entry, its memory copy dropped.
    pub fn on_disk(&self) -> bool {
        self.0.state.lock().disk.is_some()
    }

    /// Bytes it will hold once its burst is in (capped); 0 once read from disk.
    pub fn holding(&self) -> u64 {
        let s = self.0.state.lock();
        if s.disk.is_some() {
            return 0;
        }
        let cap = s.window.map_or(u64::MAX, |w| w.cap).min(s.budget.unwrap_or(u64::MAX));
        s.len.map_or(s.data.len() as u64, |l| l.saturating_sub(s.base)).min(cap)
    }

    /// Holds at most `bytes` (None: its window's cap). A song fetched ahead gets what the playing one
    /// leaves; opened to play, it gets the whole cap.
    pub fn limit(&self, bytes: Option<u64>) {
        let mut s = self.0.state.lock();
        if s.budget != bytes {
            s.budget = bytes;
            self.0.cv.notify_all();
        }
    }

    /// The whole song is in memory.
    pub fn complete(&self) -> bool {
        let s = self.0.state.lock();
        s.base == 0 && s.at_end() && s.error.is_none()
    }

    /// Blocks until the whole song is in memory. False when it never will be (failed, or larger than
    /// one burst).
    pub(crate) fn wait_whole(&self) -> bool {
        let l = &*self.0;
        let mut s = l.state.lock();
        loop {
            if s.error.is_some() || s.closed {
                return false;
            }
            if s.base == 0 && s.at_end() {
                return true;
            }
            if s.window.is_some_and(|w| s.len.is_some_and(|len| len > w.high)) {
                return false;
            }
            s.blocked += 1;
            l.cv.wait(&mut s);
            s.blocked -= 1;
        }
    }

    /// Times the length turned out shorter than promised: a demuxer re-reads the end when this moves.
    pub fn shortened(&self) -> u32 {
        self.0.state.lock().shortened
    }

    /// The length in bytes, if known.
    pub fn length(&self) -> Option<u64> {
        self.0.state.lock().len
    }

    /// Why the loader gave up, if it did.
    pub fn error(&self) -> Option<String> {
        self.0.state.lock().error.clone()
    }

    /// The server's error status when the loader gave up (the song's failure, not the network's).
    pub fn answered(&self) -> Option<u16> {
        let s = self.0.state.lock();
        s.error.as_ref().and(s.answered)
    }

    /// Whether a read would not block; if not, `engine` is woken when it would not.
    pub(crate) fn ready_or_wake(&self, engine: &Thread) -> bool {
        let mut s = self.0.state.lock();
        if s.ready() {
            return true;
        }
        s.waiter = Some(engine.clone());
        false
    }

    pub fn reader(self: &Arc<Self>) -> LoadedReader {
        LoadedReader { loader: self.clone(), pos: 0, stop: None }
    }

    /// A reader that fails once `stop` is set, for an opening nobody wants any more (it would otherwise
    /// keep moving the fetch against the wanted reader).
    pub(crate) fn reader_until(self: &Arc<Self>, stop: Arc<AtomicBool>) -> LoadedReader {
        LoadedReader { loader: self.clone(), pos: 0, stop: Some(stop) }
    }

    /// Wakes blocked readers to check `stop`.
    pub(crate) fn nudge(&self) {
        self.0.cv.notify_all();
    }
}

impl Drop for Loader {
    /// Stops the loader and cancels its request at once.
    fn drop(&mut self) {
        self.0.state.lock().closed = true;
        self.0.cv.notify_all();
        self.0.cancel.close();
    }
}

impl Loaded {
    fn new(state: State, waits: Waits) -> Loaded {
        Loaded { state: Mutex::new(state), cv: Condvar::new(), cancel: Cancel::stalling_after(waits.stall_ms), retry_ms: waits.retry_ms }
    }

    /// Waits before the retry after `failures` failures in a row (2, 4, 8 times `retry_ms`); false once
    /// the song is let go.
    fn rest(&self, failures: u32) -> bool {
        let until = std::time::Instant::now() + Duration::from_millis(self.retry_ms << failures);
        let mut s = self.state.lock();
        while !s.closed && !self.cv.wait_until(&mut s, until).timed_out() {}
        !s.closed
    }

    fn run(&self, source: &dyn ByteSource, url: &str, load: [i64; 5], duration_ms: Option<i64>, keep: Option<Keep>, mut taker: Option<Listening>) {
        let mut body: Option<Box<dyn Read + Send>> = None;
        let mut chunk = vec![0u8; CHUNK];
        let mut failures = 0;
        let mut keep = keep.and_then(|k| k());
        // An entry the fetching ahead began: read its start from disk and fetch the rest at once.
        let mut go_on = false;
        if let Some(k) = keep.as_mut().filter(|k| k.written() > 0) {
            match k.read_back() {
                Ok(bytes) => {
                    if let Some(t) = taker.as_mut() {
                        t.take(&bytes);
                    }
                    let mut s = self.state.lock();
                    s.data = bytes;
                    go_on = true;
                    self.wake(&mut s);
                }
                Err(_) => keep = None,
            }
        }
        loop {
            let from = {
                let mut s = self.state.lock();
                loop {
                    if s.closed {
                        return;
                    }
                    if let Some(at) = s.restart.take() {
                        body = None;
                        s.base = at;
                        s.data.clear();
                        s.done = false;
                        s.error = None;
                        // A gap: neither the cache entry nor the taker can continue.
                        keep = None;
                        taker = None;
                    }
                    let w = s.window(load, duration_ms);
                    if s.at_end() {
                        body = None;
                        s.done = true;
                        if let (Some(k), Some(len), None) = (keep.take(), s.len, &s.error) {
                            // Whole in the entry: read from there and drop the memory copy.
                            let whole = s.base == 0 && s.data.len() as u64 == len;
                            if let Some(file) = k.finish_open(len).filter(|_| whole) {
                                s.data = Vec::new();
                                s.disk = Some(Arc::new(file));
                            }
                        }
                        if let Some(t) = taker.take() {
                            t.end(s.error.is_none());
                        }
                        self.wake(&mut s);
                    } else if (body.is_some() && s.ahead() < w.high) || (body.is_none() && (go_on || s.ahead() < w.low.max(1))) {
                        break;
                    } else if body.is_some() {
                        // At the high mark: close until the reader nears the low mark.
                        body = None;
                    }
                    self.cv.wait(&mut s);
                }
                // Over the cap: drop what the reader left behind.
                let w = s.window(load, duration_ms);
                let keep_from = s.reader_at.saturating_sub(FAR);
                if s.data.len() as u64 > w.cap && keep_from > s.base {
                    let drop = (keep_from - s.base) as usize;
                    s.data.drain(..drop);
                    s.base = keep_from;
                }
                s.end()
            };
            if body.is_none() {
                match open_at(source, url, from, &self.cancel) {
                    Ok(b) => {
                        let reader = b.reader;
                        go_on = false;
                        let mut s = self.state.lock();
                        let promised = b.len.filter(|&l| s.before.is_none_or(|b| l < b));
                        match (s.len, promised) {
                            (None, l) => s.len = l,
                            // An earlier end than first promised: that was an estimate.
                            (Some(had), Some(l)) if l < had && l >= s.end() => s.ends_at(l),
                            _ => {}
                        }
                        s.window = Some(Window::for_song(load, duration_ms, s.len));
                        s.bursts += 1;
                        // Reserve the burst once (doubling copied it and held up to twice the song).
                        if let Some(len) = s.len {
                            let want = len.saturating_sub(s.base).min(s.window(load, duration_ms).high) as usize;
                            let more = want.saturating_sub(s.data.len());
                            s.data.reserve_exact(more);
                        }
                        body = Some(reader);
                    }
                    // Past the end (a demuxer probing an estimated length): the real end, not a failure.
                    Err(OpenError::PastEnd { len }) => {
                        let mut s = self.state.lock();
                        if s.end() == from && s.restart.is_none() {
                            match len.filter(|&l| l <= from) {
                                // Bytes held up to `from`: it ends right there.
                                _ if !s.data.is_empty() => s.ends_at(from),
                                Some(l) => s.ends_at(l),
                                // Somewhere before `from`: unknown until the bytes run out.
                                None => s.not_at(from),
                            }
                            s.done = true;
                            failures = 0;
                            go_on = false;
                        }
                        self.wake(&mut s);
                        continue;
                    }
                    Err(_) if self.cancel.closed() => return,
                    Err(e) => {
                        failures += 1;
                        // A timeout fails at once: retrying a server that never answers (octo-fiesta
                        // fetching an unobtainable song) only keeps the player waiting.
                        if failures > RETRIES || e == OpenError::TimedOut {
                            let mut s = self.state.lock();
                            s.answered = if let OpenError::Status(status) = e { Some(status) } else { None };
                            s.error = Some(e.to_string());
                            s.done = true;
                            self.wake(&mut s);
                            continue;
                        }
                        if !self.rest(failures) {
                            return;
                        }
                        continue;
                    }
                }
            }
            let got = body.as_mut().expect("a body is open").read(&mut chunk);
            let mut s = self.state.lock();
            let mut took = 0;
            match got {
                // A clean end is the real end, whatever was promised.
                Ok(0) => {
                    body = None;
                    if s.end() == from && s.restart.is_none() {
                        s.done = true;
                        s.ends_at(from);
                    }
                }
                Ok(n) => {
                    failures = 0;
                    if s.end() == from && s.restart.is_none() {
                        s.data.extend_from_slice(&chunk[..n]);
                        took = n;
                    }
                }
                // Broken: fetch again from there. Stalls count as failures, so a server that answers and
                // never sends gives up after a few.
                Err(_) => {
                    body = None;
                    if self.cancel.timed_out() {
                        failures += 1;
                        if failures > RETRIES {
                            s.error = Some(OpenError::TimedOut.to_string());
                            s.done = true;
                        }
                    }
                }
            }
            self.wake(&mut s);
            drop(s);
            // Written without the lock, so the demuxer reads on.
            if took > 0 && keep.as_mut().is_some_and(|k| !k.write(from, &chunk[..took])) {
                keep = None;
            }
            if took > 0 {
                if let Some(t) = taker.as_mut() {
                    t.take(&chunk[..took]);
                }
            }
        }
    }

    /// Reads a live stream's connection, strips ICY titles and drops what the reader passed. A dropped
    /// connection is reopened (the stream resumes where the station is); one that cannot be is the end.
    fn run_live(&self, source: &dyn ByteSource, url: &str) {
        let mut body: Option<Icy> = None;
        let mut chunk = vec![0u8; CHUNK];
        let mut failures = 0;
        loop {
            {
                let mut s = self.state.lock();
                loop {
                    if s.closed {
                        return;
                    }
                    if body.is_none() || s.ahead() < LIVE_AHEAD {
                        break;
                    }
                    // The reader stopped: the connection waits in the socket.
                    self.cv.wait(&mut s);
                }
                let keep_from = s.reader_at.saturating_sub(LIVE_BEHIND);
                if keep_from > s.base {
                    let drop = ((keep_from - s.base) as usize).min(s.data.len());
                    s.data.drain(..drop);
                    s.base += drop as u64;
                }
            }
            if body.is_none() {
                match source.open_live(url) {
                    Ok((b, every)) => {
                        self.state.lock().bursts += 1;
                        body = Some(Icy::new(b.reader, every));
                    }
                    Err(e) => {
                        failures += 1;
                        if failures > RETRIES {
                            let mut s = self.state.lock();
                            s.error = Some(e);
                            s.done = true;
                            self.wake(&mut s);
                            return;
                        }
                        if !self.rest(failures) {
                            return;
                        }
                        continue;
                    }
                }
            }
            let icy = body.as_mut().expect("a body is open");
            let got = icy.read(&mut chunk);
            let title = icy.title.take();
            let mut s = self.state.lock();
            match got {
                Ok(n) if n > 0 => {
                    failures = 0;
                    s.data.extend_from_slice(&chunk[..n]);
                }
                // Closed or broken: reconnect.
                _ => {
                    body = None;
                    failures += 1;
                }
            }
            if let Some(t) = title {
                let at = s.end();
                s.titles.push_back((at, t));
            }
            self.wake(&mut s);
        }
    }

    /// The loader thread died (panicked): readers are told the song failed and the engine is woken.
    fn gave_up(&self, why: &str) {
        let mut s = self.state.lock();
        if s.error.is_none() && !s.at_end() {
            s.error = Some(why.to_string());
        }
        s.done = true;
        s.restart = None;
        self.cv.notify_all();
        self.wake(&mut s);
    }

    /// Wakes blocked readers, and the waiting engine once ready.
    fn wake(&self, s: &mut State) {
        if s.blocked > 0 {
            self.cv.notify_all();
        }
        if s.ready() {
            if let Some(t) = s.waiter.take() {
                t.unpark();
            }
        }
    }
}

/// A song's bytes as a seekable stream for the demuxer; reads block until the bytes are there.
pub struct LoadedReader {
    loader: Arc<Loader>,
    pos: u64,
    /// Set once nobody wants this read ([`Loader::reader_until`]).
    stop: Option<Arc<AtomicBool>>,
}

impl Read for LoadedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let l = &*self.loader.0;
        let mut s = l.state.lock();
        loop {
            if self.stop.as_ref().is_some_and(|s| s.load(Ordering::Acquire)) {
                return Err(io::Error::other("the song is no longer wanted"));
            }
            let end = s.end();
            if let Some(file) = s.disk.clone() {
                if self.pos >= end {
                    return Ok(0);
                }
                s.reader_at = self.pos;
                drop(s);
                let want = buf.len().min((end - self.pos) as usize);
                let n = read_at(&file, &mut buf[..want], self.pos)?;
                self.pos += n as u64;
                return Ok(n);
            }
            if self.pos >= s.base && self.pos < end {
                let from = (self.pos - s.base) as usize;
                let n = buf.len().min(s.data.len() - from);
                buf[..n].copy_from_slice(&s.data[from..from + n]);
                self.pos += n as u64;
                s.reader_at = self.pos;
                // Within the low mark: the next burst is due.
                if !s.at_end() && (s.window.is_some_and(|w| s.ahead() < w.low) || (s.live && s.ahead() < LIVE_AHEAD)) {
                    l.cv.notify_all();
                }
                return Ok(n);
            }
            if s.at_end() && self.pos >= end && s.restart.is_none() {
                if let Some(e) = &s.error {
                    return Err(io::Error::other(e.clone()));
                }
                return Ok(0);
            }
            // At or past the known end: nothing to fetch.
            if !s.live && s.len.is_some_and(|l| self.pos >= l) {
                return Ok(0);
            }
            if s.live && self.pos < s.base {
                return Err(io::Error::other("a live stream cannot be read again from further back"));
            }
            if !s.live && (self.pos < s.base || self.pos > end + FAR) {
                s.restart = Some(self.pos);
            }
            s.reader_at = self.pos;
            s.blocked += 1;
            l.cv.notify_all();
            let timed_out = l.cv.wait_for(&mut s, READ_TIMEOUT).timed_out();
            s.blocked -= 1;
            if timed_out {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the song's bytes did not come"));
            }
        }
    }
}

/// Reads at `at` without moving a file position: several readers share one entry.
fn read_at(file: &File, buf: &mut [u8], at: u64) -> io::Result<usize> {
    #[cfg(unix)]
    return std::os::unix::fs::FileExt::read_at(file, buf, at);
    #[cfg(windows)]
    return std::os::windows::fs::FileExt::seek_read(file, buf, at);
    #[cfg(not(any(unix, windows)))]
    {
        let mut f = file;
        f.seek(SeekFrom::Start(at))?;
        f.read(buf)
    }
}

/// Every loader alive, for the perf report's memory line ([`held`]). Process-wide diagnostics.
static ALIVE: Mutex<Vec<Weak<Loaded>>> = Mutex::new(Vec::new());

fn alive(loaded: &Arc<Loaded>) {
    let mut all = ALIVE.lock();
    all.retain(|w| w.strong_count() > 0);
    all.push(Arc::downgrade(loaded));
}

/// What the loaders hold, for the perf report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    /// Loaders alive.
    pub songs: u32,
    /// Bytes allocated for them.
    pub bytes: u64,
    /// Of them, those reading from disk.
    pub on_disk: u32,
}

/// What every loader holds now.
pub fn held() -> Held {
    let all: Vec<Arc<Loaded>> = ALIVE.lock().iter().filter_map(Weak::upgrade).collect();
    let mut h = Held::default();
    for l in all {
        let s = l.state.lock();
        h.songs += 1;
        h.bytes += s.data.capacity() as u64;
        h.on_disk += s.disk.is_some() as u32;
    }
    h
}

/// A live stream body with ICY metadata stripped: every `every` bytes comes a length byte (in units
/// of 16) and that much of `StreamTitle='...';`.
struct Icy {
    inner: Box<dyn Read + Send>,
    every: Option<usize>,
    /// Music bytes before the next metadata block.
    left: usize,
    /// The last title, until the loader takes it.
    title: Option<String>,
}

impl Icy {
    fn new(inner: Box<dyn Read + Send>, every: Option<usize>) -> Icy {
        let every = every.filter(|&n| n > 0);
        Icy { inner, every, left: every.unwrap_or(0), title: None }
    }

    /// Reads one metadata block.
    fn announcement(&mut self) -> io::Result<()> {
        let mut len = [0u8; 1];
        self.inner.read_exact(&mut len)?;
        let n = len[0] as usize * 16;
        if n == 0 {
            return Ok(());
        }
        let mut text = vec![0u8; n];
        self.inner.read_exact(&mut text)?;
        if let Some(t) = stream_title(&text) {
            self.title = Some(t);
        }
        Ok(())
    }
}

impl Read for Icy {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(every) = self.every else { return self.inner.read(buf) };
        if self.left == 0 {
            self.announcement()?;
            self.left = every;
        }
        let want = buf.len().min(self.left);
        let n = self.inner.read(&mut buf[..want])?;
        self.left -= n;
        Ok(n)
    }
}

/// The title in an ICY block (`StreamTitle='Artist - Song';StreamUrl='';`, NUL padded). Non-UTF-8 is
/// read as Latin-1, which stations use as often.
pub fn stream_title(text: &[u8]) -> Option<String> {
    let text = match std::str::from_utf8(text) {
        Ok(t) => t.to_string(),
        Err(_) => text.iter().map(|&b| b as char).collect(),
    };
    let from = text.find("StreamTitle='")? + "StreamTitle='".len();
    let rest = &text[from..];
    // Ends at the field's closing quote; quotes inside are kept.
    let end = rest.find("';").or_else(|| rest.rfind('\'')).unwrap_or(rest.len());
    let title = rest[..end].trim_matches(char::from(0)).trim();
    (!title.is_empty()).then(|| title.to_string())
}

impl Seek for LoadedReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = self.loader.0.state.lock().len;
        self.pos = match to {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(d) => self.pos.checked_add_signed(d).ok_or_else(|| io::Error::other("seek before the start"))?,
            SeekFrom::End(d) => len.ok_or_else(|| io::Error::other("length unknown"))?.checked_add_signed(d).ok_or_else(|| io::Error::other("seek before the start"))?,
        };
        Ok(self.pos)
    }
}

impl MediaSource for LoadedReader {
    fn is_seekable(&self) -> bool {
        self.loader.0.state.lock().len.is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        self.loader.0.state.lock().len
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;

    /// A count a test waits on.
    #[derive(Default)]
    struct Signal(Mutex<u64>, Condvar);

    impl Signal {
        fn bump(&self) {
            *self.0.lock() += 1;
            self.1.notify_all();
        }

        fn reach(&self, n: u64) {
            let until = Instant::now() + Duration::from_secs(10);
            let mut c = self.0.lock();
            while *c < n {
                assert!(!self.1.wait_until(&mut c, until).timed_out() || *c >= n, "{} of {n}", *c);
            }
        }
    }

    /// Waits as a blocked reader (counted in `blocked`) until `f` holds of the loader.
    fn until(l: &Loader, what: &str, f: impl Fn(&State) -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        let mut s = l.0.state.lock();
        s.blocked += 1;
        while !f(&s) {
            assert!(!l.0.cv.wait_until(&mut s, until).timed_out() || f(&s), "{what}");
        }
        s.blocked -= 1;
    }

    /// Bytes served and connections closed.
    #[derive(Default)]
    struct Served {
        bytes: AtomicU64,
        closed: Signal,
    }

    /// Serves `len` made-up bytes (byte `i` is `i as u8`) and counts requests.
    struct Counting {
        len: u64,
        opens: Mutex<Vec<u64>>,
        served: Arc<Served>,
    }

    struct Made {
        at: u64,
        len: u64,
        served: Arc<Served>,
    }

    impl Drop for Made {
        fn drop(&mut self) {
            self.served.closed.bump();
        }
    }

    impl Read for Made {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min((self.len - self.at) as usize);
            for (k, b) in buf[..n].iter_mut().enumerate() {
                *b = (self.at + k as u64) as u8;
            }
            self.at += n as u64;
            self.served.bytes.fetch_add(n as u64, Ordering::Relaxed);
            Ok(n)
        }
    }

    impl ByteSource for Counting {
        fn open(&self, _: &str, from: u64) -> Result<Body, OpenError> {
            self.opens.lock().push(from);
            Ok(Body { start: from, len: Some(self.len), reader: Box::new(Made { at: from, len: self.len, served: self.served.clone() }) })
        }
    }

    fn server(len: u64) -> Arc<Counting> {
        Arc::new(Counting { len, opens: Mutex::new(Vec::new()), served: Arc::default() })
    }

    /// Bytes served once `bursts` connections are closed.
    fn settled(s: &Counting, bursts: u64) -> u64 {
        s.served.closed.reach(bursts);
        s.served.bytes.load(Ordering::Relaxed)
    }

    fn read(r: &mut LoadedReader, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        r.read_exact(&mut out).unwrap();
        out
    }

    /// 100 bytes/ms for a 10 s, 1 MB song: a 1-4 s window is 100-400 kB.
    const LOAD: [i64; 5] = [1_000, 4_000, 0, 0, 1 << 30];

    #[test]
    fn fetches_to_high_mark_then_waits_for_low_mark() {
        let s = server(1_000_000);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(10_000), None);
        let mut r = l.reader();
        let first = settled(&s, 1);
        assert!((400_000..400_000 + CHUNK as u64).contains(&first), "one burst to the high mark: {first}");
        assert_eq!(*s.opens.lock(), vec![0]);
        // More than the low mark ahead: nothing fetched.
        let got = read(&mut r, 250_000);
        assert!(got.iter().enumerate().all(|(i, &b)| b == i as u8), "the bytes are the song's");
        // Within the low mark: the next burst, from where the last stopped.
        let at = first as usize - 90_000;
        read(&mut r, at - 250_000);
        let second = settled(&s, 2);
        assert_eq!(s.opens.lock().clone(), vec![0, first], "one more request, picking up where the first stopped");
        assert!(second - first >= 300_000, "a whole burst, not a top-up: {}", second - first);
        let rest = read(&mut r, 1_000_000 - at);
        assert!(rest.iter().enumerate().all(|(i, &b)| b == (at + i) as u8));
        assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0, "and then the end");
        assert_eq!(l.bursts() as usize, s.opens.lock().len());
    }

    #[test]
    fn small_song_fetched_in_one_request() {
        let s = server(300_000);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(3_000), None);
        assert_eq!(settled(&s, 1), 300_000);
        let mut r = l.reader();
        let all = read(&mut r, 300_000);
        assert!(all.iter().enumerate().all(|(i, &b)| b == i as u8));
        assert_eq!(*s.opens.lock(), vec![0]);
    }

    #[test]
    fn fetched_ahead_holds_budget_then_rest() {
        let s = server(600_000);
        let l = Loader::start_within(s.clone(), "song".into(), LOAD, Some(6_000), None, Some(200_000), None, Waits::default());
        let ahead = settled(&s, 1);
        assert!(ahead <= 200_000 + CHUNK as u64, "no more than its budget while it waits: {ahead}");
        assert!(l.held() >= 100_000, "but its start is at hand: {}", l.held());
        assert_eq!(l.holding(), 200_000);
        // Now playing: the whole cap; the rest comes as the reader nears the end.
        l.limit(None);
        let mut r = l.reader();
        let all = read(&mut r, 600_000);
        assert!(all.iter().enumerate().all(|(i, &b)| b == i as u8), "every byte, in order");
        assert_eq!(*s.opens.lock(), vec![0, ahead], "one more request, from where the budget stopped it");
    }

    #[test]
    fn whole_cached_song_read_from_disk() {
        let d = nori_testdir::TempDir::new("source-disk");
        let store = Arc::new(crate::store::Store::open(d.path(), 1 << 22, Box::new(crate::store::Recent::default())).unwrap());
        let s = server(300_000);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(3_000), store.writer("song:0"));
        let mut r = l.reader();
        let all = read(&mut r, 300_000);
        assert!(all.iter().enumerate().all(|(i, &b)| b == i as u8));
        assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0, "the end");
        // Dropped on the loader thread, a moment after the last bytes.
        until(&l, "read from the disk", |s| s.disk.is_some());
        assert_eq!((l.held(), l.holding()), (0, 0), "nothing kept in memory");
        assert!(l.complete());
        // Readable from anywhere.
        let mut again = l.reader();
        again.seek(SeekFrom::Start(123_456)).unwrap();
        assert_eq!(read(&mut again, 10), (123_456..123_466).map(|i| i as u8).collect::<Vec<_>>());
        assert_eq!(*s.opens.lock(), vec![0], "and the network was asked once");
        let h = held();
        assert!(h.on_disk >= 1 && h.songs >= 1, "{h:?}");
    }

    #[test]
    fn bytes_reserved_once() {
        let s = server(300_000);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(3_000), None);
        settled(&s, 1);
        assert_eq!(l.0.state.lock().data.capacity(), 300_000, "made once, not grown by doubling");
    }

    #[test]
    fn seek_past_loaded_fetches_from_there() {
        let s = server(5_000_000);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(50_000), None);
        settled(&s, 1);
        let mut r = l.reader();
        r.seek(SeekFrom::Start(4_000_000)).unwrap();
        let got = read(&mut r, 1000);
        assert!(got.iter().enumerate().all(|(i, &b)| b == (4_000_000 + i) as u8));
        assert_eq!(*s.opens.lock(), vec![0, 4_000_000], "not the megabytes in between");
    }

    /// A transcoding server: promises `promised` bytes, sends `real`, answers 416 past `real` (with the
    /// length when `says`). The first body breaks after `cut` bytes.
    struct Estimating {
        real: u64,
        promised: u64,
        says: bool,
        cut: Mutex<Option<u64>>,
        /// The body ends with an error, as OkHttp reads one shorter than its Content-Length.
        breaks_at_end: bool,
        opens: Mutex<Vec<u64>>,
    }

    struct Cut {
        made: Made,
        cut: Option<u64>,
        breaks_at_end: bool,
    }

    impl Read for Cut {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let stop = self.cut.unwrap_or(self.made.len);
            if self.made.at >= stop && (self.cut.is_some() || self.breaks_at_end) {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset"));
            }
            let n = buf.len().min((stop - self.made.at) as usize);
            self.made.read(&mut buf[..n])
        }
    }

    impl ByteSource for Estimating {
        fn open(&self, _: &str, from: u64) -> Result<Body, OpenError> {
            self.opens.lock().push(from);
            if from >= self.real {
                return Err(OpenError::PastEnd { len: self.says.then_some(self.real) });
            }
            let made = Made { at: from, len: self.real, served: Arc::default() };
            let reader = Cut { made, cut: self.cut.lock().take(), breaks_at_end: self.breaks_at_end };
            Ok(Body { start: from, len: Some(self.promised), reader: Box::new(reader) })
        }
    }

    fn estimating(real: u64, promised: u64, says: bool) -> Estimating {
        Estimating { real, promised, says, cut: Mutex::new(None), breaks_at_end: false, opens: Mutex::new(Vec::new()) }
    }

    #[test]
    fn clean_early_end_is_real_end() {
        let s = Arc::new(estimating(300_000, 320_000, true));
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(3_000), None);
        let mut r = l.reader();
        let all = read(&mut r, 300_000);
        assert!(all.iter().enumerate().all(|(i, &b)| b == i as u8));
        assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0, "the end");
        assert_eq!(l.length(), Some(300_000), "the real length, not the estimate");
        assert_eq!(l.shortened(), 1);
        assert_eq!(r.seek(SeekFrom::End(0)).unwrap(), 300_000);
        assert!(l.complete() && l.error().is_none());
        assert_eq!(*s.opens.lock(), vec![0], "nothing asked for past the end");
    }

    #[test]
    fn read_past_real_end_is_end() {
        for says in [true, false] {
            let s = Arc::new(estimating(3_000_000, 3_200_000, says));
            let l = Loader::start(s.clone(), "song".into(), LOAD, Some(30_000), None);
            let mut r = l.reader();
            read(&mut r, 1000);
            // Where an Ogg reader looks for the last page.
            let probe = 3_200_000 - 65_307;
            r.seek(SeekFrom::Start(probe)).unwrap();
            assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0, "nothing there: the end (says {says})");
            assert!(l.error().is_none(), "not a failure: {:?}", l.error());
            assert_eq!(l.length(), says.then_some(3_000_000), "the real length, or none known (says {says})");
            assert_eq!(l.shortened(), 1);
            assert_eq!(s.opens.lock().iter().filter(|&&o| o == probe).count(), 1, "asked once: {:?}", s.opens.lock());

            r.seek(SeekFrom::Start(2_999_000)).unwrap();
            let tail = read(&mut r, 1000);
            assert!(tail.iter().enumerate().all(|(i, &b)| b == (2_999_000 + i) as u8));
            assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0);
            assert_eq!(l.length(), Some(3_000_000), "the end found where the bytes stop (says {says})");
        }
    }

    #[test]
    fn broken_body_at_end_learns_end() {
        let mut e = estimating(300_000, 320_000, false);
        e.breaks_at_end = true;
        let s = Arc::new(e);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(3_000), None);
        let mut r = l.reader();
        read(&mut r, 300_000);
        assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0);
        assert_eq!(l.length(), Some(300_000));
        assert!(l.error().is_none());
        assert_eq!(*s.opens.lock(), vec![0, 300_000], "asked again where it broke, and told that is the end");
    }

    #[test]
    fn dropped_connection_resumes() {
        let e = estimating(600_000, 640_000, true);
        *e.cut.lock() = Some(150_000);
        let s = Arc::new(e);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(6_000), None);
        let mut r = l.reader();
        let all = read(&mut r, 600_000);
        assert!(all.iter().enumerate().all(|(i, &b)| b == i as u8), "every byte, in order");
        assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0);
        assert_eq!(s.opens.lock()[..2], [0, 150_000], "asked again from where it broke");
        assert_eq!(l.length(), Some(600_000), "ended where the bytes did, not where the network dropped");
        assert!(l.error().is_none());
    }

    /// Regression: readers giving up left the wanted one asleep until its timeout (music stopped at a
    /// song's end).
    #[test]
    fn abandoned_readers_leave_waiter_woken() {
        /// Answers only when let, so all readers are waiting.
        struct Late(Arc<Counting>, Arc<Signal>);
        impl ByteSource for Late {
            fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
                self.1.reach(1);
                self.0.open(url, from)
            }
        }
        let blocked = |l: &Loader| l.0.state.lock().blocked;
        let answer = Arc::new(Signal::default());
        let l = Loader::start(Arc::new(Late(server(2_000_000), answer.clone())), "song".into(), LOAD, Some(20_000), None);
        let mut wanted = l.reader();
        let player = std::thread::spawn(move || {
            let mut b = [0u8; 16];
            let n = wanted.read(&mut b);
            (n, Instant::now())
        });
        let stop = Arc::new(AtomicBool::new(false));
        let others: Vec<_> = (0..8)
            .map(|_| {
                let mut r = l.reader_until(stop.clone());
                std::thread::spawn(move || r.read(&mut [0u8; 16]).is_err())
            })
            .collect();
        // Ten with this wait.
        until(&l, "the player's reader and eight opened for nothing all wait on the bytes", |s| s.blocked == 10);
        stop.store(true, Ordering::Release);
        l.nudge();
        let gave_up = others.into_iter().map(|o| o.join().unwrap()).filter(|&e| e).count();
        assert_eq!(gave_up, 8, "the readers nobody wants give up");
        assert_eq!(blocked(&l), 1, "the player's reader still waits, and is counted as waiting");
        let answered = Instant::now();
        answer.bump();
        let (n, at) = player.join().unwrap();
        assert_eq!(n.expect("the bytes"), 16);
        let took = at - answered;
        assert!(took < Duration::from_secs(3), "read as the bytes came, not after the {READ_TIMEOUT:?} timeout: {took:?}");
    }

    #[test]
    fn unwanted_reader_stops_waiting() {
        /// Answers from past the start only when let.
        struct Slow(Arc<Counting>, Arc<Signal>);
        impl ByteSource for Slow {
            fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
                if from > 0 {
                    self.1.reach(1);
                }
                self.0.open(url, from)
            }
        }
        let answer = Arc::new(Signal::default());
        let l = Loader::start(Arc::new(Slow(server(5_000_000), answer.clone())), "song".into(), LOAD, Some(50_000), None);
        let stop = Arc::new(AtomicBool::new(false));
        let mut r = l.reader_until(stop.clone());
        read(&mut r, 1000);
        r.seek(SeekFrom::Start(4_000_000)).unwrap();
        let (l2, stop2) = (l.clone(), stop.clone());
        std::thread::spawn(move || {
            until(&l2, "the reader waits", |s| s.blocked == 2);
            stop2.store(true, Ordering::Release);
            l2.nudge();
        });
        assert!(r.read(&mut [0u8; 16]).is_err(), "given up");
        assert!(r.read(&mut [0u8; 16]).is_err(), "and for good");
        assert_eq!(l.0.state.lock().bursts, 1, "given up before the bytes came");
        answer.bump();
    }

    #[test]
    fn rangeless_short_answer_is_end() {
        struct Whole(u64, Mutex<Vec<u64>>);
        impl ByteSource for Whole {
            fn open(&self, _: &str, from: u64) -> Result<Body, OpenError> {
                self.1.lock().push(from);
                Ok(Body { start: 0, len: Some(self.0 + 50_000), reader: Box::new(Made { at: 0, len: self.0, served: Arc::default() }) })
            }
        }
        let s = Arc::new(Whole(2_000_000, Mutex::new(Vec::new())));
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(20_000), None);
        let mut r = l.reader();
        read(&mut r, 1000);
        r.seek(SeekFrom::Start(2_020_000)).unwrap();
        assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0);
        assert_eq!(l.length(), Some(2_000_000), "where the whole answer ended");
        assert!(l.error().is_none());
    }

    // ---- requests called off ----

    /// Never answers (or never sends a byte) until cancelled; counts requests and cancellations.
    #[derive(Default)]
    struct Silent {
        headers: bool,
        asked: Signal,
        called_off: Arc<Signal>,
    }

    struct Nothing(Arc<Signal>, Arc<Signal>);

    impl Nothing {
        fn wait(&self) {
            self.0.reach(1);
            self.1.bump();
        }
    }

    impl Read for Nothing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            self.wait();
            Err(io::Error::other("Canceled"))
        }
    }

    impl ByteSource for Silent {
        fn open(&self, _: &str, _: u64) -> Result<Body, OpenError> {
            unreachable!("asked as a request that may be called off")
        }

        fn open_cancellable(&self, _: &str, _: Option<&str>, from: u64, cancel: &Cancel) -> Result<Body, OpenError> {
            self.asked.bump();
            let off = Arc::new(Signal::default());
            let o = off.clone();
            cancel.on_cancel(move || o.bump());
            let nothing = Nothing(off, self.called_off.clone());
            if self.headers {
                nothing.wait();
                return Err("Canceled".into());
            }
            Ok(Body { start: from, len: Some(1_000_000), reader: Box::new(nothing) })
        }
    }

    #[test]
    fn dropped_song_cancels_hung_request() {
        for headers in [true, false] {
            let s = Arc::new(Silent { headers, ..Silent::default() });
            let l = Loader::start(s.clone(), "ext-1".into(), LOAD, Some(3_000), None);
            s.asked.reach(1);
            drop(l);
            s.called_off.reach(1);
            assert_eq!(*s.asked.0.lock(), 1, "and not asked again (headers {headers})");
        }
    }

    #[test]
    fn stalled_request_times_out() {
        let s = Silent { headers: true, ..Silent::default() };
        let cancel = Cancel::stalling_after(100);
        let t = Instant::now();
        let got = open_watched(&s, "ext-1", None, 0, &cancel);
        assert_eq!(got.err(), Some(OpenError::TimedOut));
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        // A body whose bytes stop fails its read as timed out.
        let s = Silent { headers: false, ..Silent::default() };
        let cancel = Cancel::stalling_after(100);
        let Ok(mut b) = open_watched(&s, "ext-1", None, 0, &cancel) else { panic!("the headers came") };
        let e = b.reader.read(&mut [0u8; 16]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn timeout_fails_song_at_once() {
        struct Late(Mutex<u32>);
        impl ByteSource for Late {
            fn open(&self, _: &str, _: u64) -> Result<Body, OpenError> {
                *self.0.lock() += 1;
                Err(OpenError::TimedOut)
            }
        }
        let s = Arc::new(Late(Mutex::new(0)));
        let l = Loader::start(s.clone(), "ext-1".into(), LOAD, Some(3_000), None);
        let mut r = l.reader();
        assert!(r.read(&mut [0u8; 16]).is_err());
        assert_eq!(l.error(), Some(OpenError::TimedOut.to_string()));
        assert_eq!(l.answered(), None, "the network's failure, not the server's");
        assert_eq!(*s.0.lock(), 1, "asked once");
    }

    #[test]
    fn let_go_song_stops_retrying_at_once() {
        struct Down(Signal, Arc<Signal>);
        impl ByteSource for Down {
            fn open(&self, _: &str, _: u64) -> Result<Body, OpenError> {
                self.0.bump();
                Err("down".into())
            }
        }
        impl Drop for Down {
            fn drop(&mut self) {
                self.1.bump();
            }
        }
        let gone = Arc::new(Signal::default());
        let s = Arc::new(Down(Signal::default(), gone.clone()));
        let l = Loader::start_within(s.clone(), "song".into(), LOAD, Some(3_000), None, None, None, Waits { stall_ms: 20_000, retry_ms: 60_000 });
        s.0.reach(1);
        drop((s, l));
        gone.reach(1);
    }

    #[test]
    fn crowding_cancels_oldest_per_source() {
        let asking = Asking(Mutex::new(Vec::new()));
        let off = Arc::new(Mutex::new(Vec::new()));
        let ask = |source: usize, k: usize| {
            let c = Cancel::new();
            let o = off.clone();
            c.on_cancel(move || o.lock().push(k));
            asking.asking(source, &c);
            c
        };
        let calls: Vec<Cancel> = (0..MAX_ASKING + 2).map(|k| ask(1, k)).collect();
        assert_eq!(*off.lock(), [0, 1], "the two oldest called off");
        assert!(calls[2..].iter().all(|c| !c.cancelled()), "the newest go on");
        let other = ask(2, 99);
        assert!(!other.cancelled() && off.lock().len() == 2, "another source has its own cap");
        // An answer makes room.
        asking.answered(&calls[5]);
        ask(1, 100);
        assert_eq!(off.lock().len(), 2);
    }
}
