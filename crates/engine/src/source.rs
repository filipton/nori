//! Bytes of a song over the network, fetched in bursts. A client hands in one door, [`ByteSource`]:
//! open a URL from a byte offset and read. A loader thread per song fills a window ahead of where the
//! demuxer reads - up to the core's `load_control` high mark in one go - then closes the connection and
//! sleeps until the demuxer has come within the low mark of the end of what is there. A song that fits
//! the memory cap (most do) is fetched whole in its first burst, so the radio wakes once per song. The
//! cap is one budget for the songs kept: a song fetched ahead holds what the one playing leaves of it
//! (`Loader::limit`), and the rest in a burst of its own once it plays.
//! Given a stream cache entry to fill, the loader writes each burst into it as it comes; a song heard
//! again then plays from the disk and the network is not asked at all. Once the whole song is in the
//! entry, the loader lets its copy in memory go and reads the entry instead: a song is a few megabytes of
//! music and, from many a library, as many again of pictures in its tags, and the page cache has the file
//! it has just written.
//!
//! A live stream (internet radio) has no end and cannot be asked for again from where it stopped, so it
//! is not fetched in bursts: its one connection stays open for as long as it plays, and what it holds is
//! a window that moves with the reader ([`Loader::live`]). The station's announcements (ICY
//! `StreamTitle`), sent between its bytes, are taken out as they come and said once the reader passes
//! them ([`Loader::announced`]).

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

/// The stream cache entry a loader writes into, made on the loader's own thread: making it may wait for
/// the fetching ahead to hand over the song (`Store::writer_for_player`). An entry that holds bytes
/// already is gone on with from there.
pub type Keep = Box<dyn FnOnce() -> Option<Writer> + Send>;

/// A response body being read: where in the resource it starts (the offset asked for, or nought when
/// the server would not do ranges), the whole resource's length when known, and the bytes.
pub struct Body {
    pub start: u64,
    pub len: Option<u64>,
    pub reader: Box<dyn Read + Send>,
}

/// Why [`ByteSource::open`] gave no body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// There is nothing at `from` or after it: the resource ends there or before (an HTTP 416, or a
    /// server that would not do ranges sending fewer than `from` bytes). `len` is its whole length when
    /// the answer said (`Content-Range: bytes */N`). A transcoding server promises an estimated length
    /// at first (Navidrome's `estimateContentLength`) and the real one ends short of it: this is how the
    /// real end is learned, and it is not a failure.
    PastEnd { len: Option<u64> },
    /// The server answered with this error status: it was reached, and would not give the song. Asked
    /// for again (a 503 passes), and a song that fails so is not the network's failure.
    Status(u16),
    /// The bytes could not be had now (no network, the server out of reach): asked for again.
    Failed(String),
    /// No answer, or no next byte, within [`Waits::stall_ms`]: called off. A song whose first bytes
    /// never come fails at once, as the network's failure; a body that stalls half way is asked for
    /// again from where it stopped.
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

/// Real-time limits of a song's fetch. A test on a clock it moves by hand changes them
/// ([`crate::Clock::waits`]): that clock cannot see real time.
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

/// A request that may be called off from another thread: its song let go, its answer or next byte
/// later than its stall, or crowded out by newer ones ([`MAX_ASKING`]).
/// Handed to [`ByteSource::open_cancellable`]; the client's HTTP stack calls its own request off when
/// told ([`Cancel::on_cancel`]), or looks at [`Cancel::cancelled`] while it waits. One per loader (and
/// per song fetched ahead), made anew for each request ([`Cancel::begin`]).
#[derive(Clone)]
pub struct Cancel(Arc<Called>);

struct Called {
    s: Mutex<CallState>,
}

struct CallState {
    /// The song is let go: every request of it, now and to come, is off.
    closed: bool,
    /// This request is off; `timed_out` says it stalled.
    off: bool,
    timed_out: bool,
    /// How the client calls this request off, once it is running.
    hook: Option<Box<dyn FnOnce() + Send>>,
    /// Called off at this moment unless it moves on first.
    deadline: Option<std::time::Instant>,
    watched: bool,
    /// Called off after this long without moving, ms.
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
        Cancel(Arc::new(Called { s: Mutex::new(s) }))
    }

    /// Whether the request is called off: an HTTP stack that cannot be told looks at this as it waits.
    pub fn cancelled(&self) -> bool {
        let s = self.0.s.lock();
        s.closed || s.off
    }

    /// Whether it was called off for stalling.
    pub fn timed_out(&self) -> bool {
        self.0.s.lock().timed_out
    }

    /// The client's way of calling its running request off (cancelling the platform's call), run at once
    /// if it is off already. One at a time: a request's own replaces the one before.
    pub fn on_cancel(&self, off: impl FnOnce() + Send + 'static) {
        let mut s = self.0.s.lock();
        if s.closed || s.off {
            drop(s);
            off();
            return;
        }
        s.hook = Some(Box::new(off));
    }

    /// A new request begins: whatever called the last one off is forgotten (not a song let go), and it
    /// is called off unless it answers within its stall.
    pub(crate) fn begin(&self) {
        self.stalls(true);
    }

    /// The request moved on (its answer, a byte): called off unless the next comes within its stall.
    pub(crate) fn moved(&self) {
        self.stalls(false);
    }

    /// Watched from now on: called off unless it moves within its stall. `anew`: a new request, which
    /// forgets whatever called the one before off, in the same step as its deadline is set, so the one
    /// before's cannot land on it.
    fn stalls(&self, anew: bool) {
        let mut s = self.0.s.lock();
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

    /// The request is over (its body let go): nothing to call off, nothing to watch.
    pub(crate) fn end(&self) {
        let mut s = self.0.s.lock();
        s.hook = None;
        s.deadline = None;
    }

    /// Calls the running request off, `stalled` or not.
    pub(crate) fn call_off(&self, stalled: bool) {
        let hook = {
            let mut s = self.0.s.lock();
            s.off = true;
            s.timed_out |= stalled;
            s.deadline = None;
            s.hook.take()
        };
        if let Some(h) = hook {
            h();
        }
    }

    /// The song is let go: its request is called off, and any it would make.
    pub(crate) fn close(&self) {
        self.0.s.lock().closed = true;
        self.call_off(false);
    }

    /// Whether the song is let go.
    pub(crate) fn closed(&self) -> bool {
        self.0.s.lock().closed
    }
}

/// The requests waiting for an answer or a byte, and the one thread that calls off those that stall: it
/// lives only while there are any, and sleeps until the first of them is due.
#[derive(Default)]
struct Watch {
    calls: Vec<std::sync::Weak<Called>>,
    running: bool,
}

static WATCH: Mutex<Watch> = Mutex::new(Watch { calls: Vec::new(), running: false });
static WATCH_CV: Condvar = Condvar::new();

fn watch(call: std::sync::Weak<Called>) {
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
            let mut s = c.s.lock();
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
pub const MAX_ASKING: usize = 4;

/// Process-wide: the player's loaders and the fetching ahead share one HTTP client, and so one cap.
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

/// `source`'s answer for `url` (under the cache key `key`, when it keeps what it reads) from `from`, made
/// as `cancel`'s request: called off when it stalls, is crowded out or the song is let go. Its body is
/// watched the same way as it is read.
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
        // Called off for stalling: whatever the client made of it, it stalled.
        Err(OpenError::Failed(_)) if stalled => Err(OpenError::TimedOut),
        Err(e) => {
            cancel.end();
            Err(e)
        }
    }
}

/// A body read under watch: every read that brings bytes puts the stall off again, and one that ends it
/// lets the watch go.
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

/// The client's HTTP client, for audio: a GET of `url` from byte `from` on (a `Range` request).
/// Blocking is fine: it runs on the loader's own thread. A range that starts at or past the end of the
/// resource is [`OpenError::PastEnd`], with the whole length when the server says it.
pub trait ByteSource: Send + Sync {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError>;

    /// [`ByteSource::open`] for the song kept under the cache key `key`: for a client whose HTTP stack
    /// keeps what it reads under the key it is told (media3's cache on Android), as the fetching ahead
    /// opens its songs.
    fn open_keyed(&self, url: &str, _key: &str, from: u64) -> Result<Body, OpenError> {
        self.open(url, from)
    }

    /// [`ByteSource::open`] (or [`ByteSource::open_keyed`] with a cache `key`) as a request that may be
    /// called off from another thread while it waits for its answer or its body's bytes: the song let go,
    /// its answer or a byte later than its stall, newer requests crowding it out. The client tells its
    /// HTTP stack through [`Cancel::on_cancel`] (cancelling the call, which makes the open or the body's
    /// read fail at once), or looks at [`Cancel::cancelled`] while it waits. A request left running holds
    /// what the platform has only so much of (a connection, a slot of its dispatcher, a cache entry's
    /// lock) until the server gives up on it, and a server that never answers never does. By default the
    /// request is not told, and runs on until it ends.
    fn open_cancellable(&self, url: &str, key: Option<&str>, from: u64, _cancel: &Cancel) -> Result<Body, OpenError> {
        match key {
            Some(k) => self.open_keyed(url, k, from),
            None => self.open(url, from),
        }
    }

    /// A live stream from where it is now, asking for the station's announcements between its bytes
    /// (the `Icy-MetaData: 1` header): the body, and how many bytes of music come between two
    /// announcements (the answer's `icy-metaint`; none when the server sends none).
    fn open_live(&self, url: &str) -> Result<(Body, Option<usize>), String> {
        self.open(url, 0).map(|b| (b, None)).map_err(|e| e.to_string())
    }
}

/// `source`'s body of `url` read from `from` on: a whole answer from a server that would not do ranges
/// is read past what comes before. One that ends before `from` is [`OpenError::PastEnd`] with where it
/// ended; one that breaks on the way is a failure.
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

/// How much to keep loaded, as `nori_player::transport::load_control` gives it: fill up to `high`
/// bytes ahead of the reader, start again below `low`, and never hold more than `cap` in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub low: u64,
    pub high: u64,
    pub cap: u64,
}

/// A bitrate to size the window by when the song's is not known: 320 kbps, so the window errs on
/// the side of fetching more.
const BYTES_PER_MS_GUESS: u64 = 40;
/// Read in pieces this big: as much as makes a song ready to play ([`READY`]), so a burst crosses from
/// the client's HTTP stack a few times a second rather than once per network packet.
const CHUNK: usize = 256 * 1024;
/// A reader this far past what is loaded has seeked: the fetch starts again there.
const FAR: u64 = 1024 * 1024;
/// A song's bytes are ready to be read without waiting when this much is there (or all of it).
pub(crate) const READY: u64 = 256 * 1024;
/// A dropped connection is tried again this many times before the song counts as failed.
const RETRIES: u32 = 3;
/// How long the demuxer waits for bytes before it gives up on the song.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// A live stream is ready to be read when this much is there: two seconds at 128 kbps, and the
/// server's own burst on connecting is usually more.
const LIVE_READY: u64 = 32 * 1024;
/// What a live stream keeps behind the reader (for the container reader's look back), and the most it
/// holds ahead of it: the server sends a live stream at its own pace after a first burst, so this is
/// only ever reached by a reader that stopped (paused), and the connection then waits in the socket.
const LIVE_BEHIND: u64 = 256 * 1024;
const LIVE_AHEAD: u64 = 2 * 1024 * 1024;

impl Window {
    /// The window for a song of `duration_ms` and `len` bytes (either may be unknown), from
    /// `load_control` (min buffer ms, max buffer ms, .., .., byte cap).
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
    /// The whole song, read from its stream cache entry rather than kept in `data` (empty then), once
    /// the entry has all of it.
    disk: Option<Arc<File>>,
    len: Option<u64>,
    /// Loaded to the end of the resource.
    done: bool,
    error: Option<String>,
    /// The error status the server gave up with, when it answered at all.
    answered: Option<u16>,
    /// Where the demuxer reads.
    reader_at: u64,
    /// The demuxer wants bytes from here, outside what is kept.
    restart: Option<u64>,
    closed: bool,
    /// The engine is waiting for bytes, and is woken when they are there.
    waiter: Option<Thread>,
    /// Readers blocked in a read, woken when bytes come. A count, not a flag: several readers may wait on
    /// one song (the one the player needs, and one being opened for nothing that gives up as the engine
    /// moves on), and the one that gives up must not leave the other unwoken.
    blocked: u32,
    /// Times the network was opened: one per burst.
    bursts: u32,
    window: Option<Window>,
    /// At most this many bytes held, below the window's own cap: a song fetched ahead gets what the
    /// one playing leaves of the cap (`Loader::limit`).
    budget: Option<u64>,
    /// A live stream: endless, one connection, a window that moves with the reader.
    live: bool,
    /// The station's announcements not yet passed by the reader, each with the byte it came at, and the
    /// last one passed, until it is asked for.
    titles: std::collections::VecDeque<(u64, String)>,
    announced: Option<String>,
    /// Times the length was found shorter than the server first said (an estimate), for a container
    /// reader that looked for the end where the estimate put it to look again ([`Loader::shortened`]).
    shortened: u32,
    /// The song ends before this byte, not said where ([`State::not_at`]): a length promised at or past
    /// it is the same estimate again, and not taken.
    before: Option<u64>,
}

impl State {
    /// The resource ends at `at`: the real end, when a promised length was an estimate beyond it.
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

    /// The resource ends somewhere before `at`, not said where: a length at or past it was an estimate,
    /// and is no longer taken for one.
    fn not_at(&mut self, at: u64) {
        self.before = Some(self.before.map_or(at, |b| b.min(at)));
        if self.len.is_some_and(|l| l >= at) {
            self.len = None;
            self.shortened += 1;
        }
    }

    /// The window for the song, as its length and bitrate size it, within the budget.
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

    /// The announcements the reader has passed: the last of them is the one heard next.
    fn passed(&mut self) {
        while self.titles.front().is_some_and(|(at, _)| *at <= self.reader_at) {
            self.announced = self.titles.pop_front().map(|(_, t)| t);
        }
    }
}

/// One song's bytes, shared by the loader thread and the song's readers.
struct Loaded {
    state: Mutex<State>,
    cv: Condvar,
    /// The request running for it: called off when the song is let go.
    cancel: Cancel,
    retry_ms: u64,
}

/// A song being loaded. The library and the song's readers share it; when the last of them lets go
/// the loader stops and its memory goes.
pub struct Loader(Arc<Loaded>);

impl Loader {
    /// Starts loading `url` through `source` on a thread of its own, sized by `load` and the song's
    /// tagged length, writing what it fetches into `keep` when given one.
    pub fn start(source: Arc<dyn ByteSource>, url: String, load: [i64; 5], duration_ms: Option<i64>, keep: Option<Writer>) -> Arc<Loader> {
        Loader::start_within(source, url, load, duration_ms, keep.map(|w| Box::new(move || Some(w)) as Keep), None, None, Waits::default())
    }

    /// [`Loader::start`], holding no more than `budget` bytes from its first burst on (`Loader::limit`),
    /// its entry made as [`Keep`] says, and `taker` hearing the song's bytes as they come, from its first
    /// (AutoMix's measuring, `crate::arriving`): told the song was whole only when every byte of it came
    /// in order, and given up at a jump.
    #[allow(clippy::too_many_arguments)]
    pub fn start_within(source: Arc<dyn ByteSource>, url: String, load: [i64; 5], duration_ms: Option<i64>, keep: Option<Keep>, budget: Option<u64>, taker: Option<Listening>, waits: Waits) -> Arc<Loader> {
        let loaded = Arc::new(Loaded::new(State { budget, ..State::default() }, waits));
        alive(&loaded);
        let l = loaded.clone();
        std::thread::Builder::new()
            .name("nori-load".into())
            .spawn(move || {
                // A loader that panicked would leave its song waiting for bytes that never come, with
                // nobody told: the song fails instead, as one whose bytes stopped coming.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| l.run(&*source, &url, load, duration_ms, keep, taker))).is_err() {
                    l.gave_up("the song's loader failed");
                }
            })
            .expect("a thread for loading");
        Arc::new(Loader(loaded))
    }

    /// Whether the song's bytes are still on their way: it is wanted, has not given up, and is not all
    /// here. A song waiting for its bytes with none of this is waiting for nothing.
    pub fn fetching(&self) -> bool {
        let s = self.0.state.lock();
        !s.closed && s.error.is_none() && !s.at_end()
    }

    /// A live stream (internet radio) at `url`, played for as long as it is held: see the module's words.
    pub fn live(source: Arc<dyn ByteSource>, url: String, waits: Waits) -> Arc<Loader> {
        let loaded = Arc::new(Loaded::new(State { live: true, ..State::default() }, waits));
        alive(&loaded);
        let l = loaded.clone();
        std::thread::Builder::new().name("nori-live".into()).spawn(move || l.run_live(&*source, &url)).expect("a thread for loading");
        Arc::new(Loader(loaded))
    }

    /// The station's latest announcement the reader has reached, once: None when there is none new.
    pub fn announced(&self) -> Option<String> {
        let mut s = self.0.state.lock();
        s.passed();
        s.announced.take()
    }

    /// Where it stands, in words, for a perf report's invariant break: the bytes it holds of the song,
    /// where its reader is and wants to be, readers blocked, an engine waiting on it, and how it ended.
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

    /// Times the network was opened for this song: one per burst.
    pub fn bursts(&self) -> u32 {
        self.0.state.lock().bursts
    }

    /// Bytes held in memory.
    pub fn held(&self) -> usize {
        self.0.state.lock().data.len()
    }

    /// Whether the song is read from its stream cache entry, its copy in memory let go.
    pub fn on_disk(&self) -> bool {
        self.0.state.lock().disk.is_some()
    }

    /// Bytes it will hold once its burst is in: the rest of the song from where it keeps it, or what is
    /// here while the length is not known yet; never more than its window's cap. None once it reads the
    /// song from the disk.
    pub fn holding(&self) -> u64 {
        let s = self.0.state.lock();
        if s.disk.is_some() {
            return 0;
        }
        let cap = s.window.map_or(u64::MAX, |w| w.cap).min(s.budget.unwrap_or(u64::MAX));
        s.len.map_or(s.data.len() as u64, |l| l.saturating_sub(s.base)).min(cap)
    }

    /// Holds at most `bytes` from now on, or as much as its window lets it with none. A song fetched
    /// ahead is limited to what the one playing leaves of the cap, as one player's buffer would be; it
    /// is let have all of it once it is opened to be played, and fetches the rest in its next burst.
    pub fn limit(&self, bytes: Option<u64>) {
        let mut s = self.0.state.lock();
        if s.budget != bytes {
            s.budget = bytes;
            self.0.cv.notify_all();
        }
    }

    /// The whole song is in memory: nothing read from it can wait.
    pub fn complete(&self) -> bool {
        let s = self.0.state.lock();
        s.base == 0 && s.at_end() && s.error.is_none()
    }

    /// Waits, on the caller's own thread, until the whole song is in memory. False when it never will
    /// be: its connection failed, or it is larger than one burst fetches.
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
            l.cv.wait_for(&mut s, Duration::from_millis(200));
            s.blocked -= 1;
        }
    }

    /// Times the song's length was found shorter than the server first promised: a container reader
    /// opened on the promised one reads the end again once this moves.
    pub fn shortened(&self) -> u32 {
        self.0.state.lock().shortened
    }

    /// The song's whole length in bytes, as far as it is known.
    pub fn length(&self) -> Option<u64> {
        self.0.state.lock().len
    }

    /// Why the connection gave up for good, once it has.
    pub fn error(&self) -> Option<String> {
        self.0.state.lock().error.clone()
    }

    /// The error status the server answered with when the connection gave up: it was reached, so the song
    /// failed for its own reasons rather than the network's. None when it did not answer (or all is well).
    pub fn answered(&self) -> Option<u16> {
        let s = self.0.state.lock();
        s.error.as_ref().and(s.answered)
    }

    /// Whether the next read can be answered at once; if not, `engine` is woken when it can.
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

    /// A reader that gives up, rather than waiting on, once `stop` is set: for a song being opened on a
    /// thread of its own that nobody wants any more. Left waiting, it and the reader of the song
    /// wanted instead would each move the fetch to where they read, over and over.
    pub(crate) fn reader_until(self: &Arc<Self>, stop: Arc<AtomicBool>) -> LoadedReader {
        LoadedReader { loader: self.clone(), pos: 0, stop: Some(stop) }
    }

    /// Wakes the readers waiting for bytes, to look whether they are still wanted.
    pub(crate) fn nudge(&self) {
        self.0.cv.notify_all();
    }
}

impl Drop for Loader {
    /// The song is let go: its loader stops, and a request still waiting for its answer or its bytes is
    /// called off at once rather than left to hold the platform's connection until the server gives up.
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

    /// The wait before the retry after `failures` failures in a row: 2, 4, 8 times `retry_ms`.
    fn retry_wait(&self, failures: u32) -> Duration {
        Duration::from_millis(self.retry_ms << failures)
    }

    fn run(&self, source: &dyn ByteSource, url: &str, load: [i64; 5], duration_ms: Option<i64>, keep: Option<Keep>, mut taker: Option<Listening>) {
        let mut body: Option<Box<dyn Read + Send>> = None;
        let mut chunk = vec![0u8; CHUNK];
        let mut failures = 0;
        let mut keep = keep.and_then(|k| k());
        // An entry the fetching ahead began: what it holds is the song's start, read from the disk, and
        // the network is asked only for the rest, at once, in the burst the fetch ahead was making.
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
                        // A jump past what was loaded leaves a gap the cache entry cannot have, nor
                        // what hears the song from its start.
                        keep = None;
                        taker = None;
                    }
                    let w = s.window(load, duration_ms);
                    if s.at_end() {
                        // The whole song is here: the connection goes, the network sleeps, and the cache
                        // has it for next time.
                        body = None;
                        s.done = true;
                        if let (Some(k), Some(len), None) = (keep.take(), s.len, &s.error) {
                            // All of it is in the entry: read from there, and the memory goes.
                            let whole = s.base == 0 && s.data.len() as u64 == len;
                            if let Some(file) = k.finish_open(len).filter(|_| whole) {
                                s.data = Vec::new();
                                s.disk = Some(Arc::new(file));
                            }
                        }
                        if let Some(t) = taker.take() {
                            // A restart gave it up: what it heard came in order from the first byte.
                            t.end(s.error.is_none());
                        }
                        self.wake(&mut s);
                    } else if body.is_some() && s.ahead() < w.high {
                        break;
                    } else if body.is_none() && (go_on || s.ahead() < w.low.max(1)) {
                        break;
                    } else if body.is_some() {
                        // Up to the high mark: close the connection and leave the network alone until
                        // the reader comes within the low mark.
                        body = None;
                    }
                    self.cv.wait(&mut s);
                }
                // What the reader has left behind goes, once more than the cap is held.
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
                            // A later answer that puts the end sooner: the first was an estimate.
                            (Some(had), Some(l)) if l < had && l >= s.end() => s.ends_at(l),
                            _ => {}
                        }
                        s.window = Some(Window::for_song(load, duration_ms, s.len));
                        s.bursts += 1;
                        // The burst's bytes in one piece, made once: grown by doubling, a song's memory
                        // was copied at every step and held up to twice its size.
                        if let Some(len) = s.len {
                            let want = len.saturating_sub(s.base).min(s.window(load, duration_ms).high) as usize;
                            let more = want.saturating_sub(s.data.len());
                            s.data.reserve_exact(more);
                        }
                        body = Some(reader);
                    }
                    // Asked for from a place the song does not reach: it ends there, or where the server
                    // says it does. Not a failure: a transcoding server's first answer promises an
                    // estimate, and a container reader that looks for the end where that put it lands
                    // past the real one.
                    Err(OpenError::PastEnd { len }) => {
                        let mut s = self.state.lock();
                        if s.end() == from && s.restart.is_none() {
                            match len.filter(|&l| l <= from) {
                                // Bytes held up to `from` are the song's, so with any here it ends right there.
                                _ if !s.data.is_empty() => s.ends_at(from),
                                Some(l) => s.ends_at(l),
                                // Somewhere before `from`, and where is not said: the promised length is
                                // wrong, and the real one is not known until the bytes run out.
                                None => s.not_at(from),
                            }
                            // Nothing to fetch here: a reader at `from` reads the end.
                            s.done = true;
                            failures = 0;
                            go_on = false;
                        }
                        self.wake(&mut s);
                        continue;
                    }
                    // Let go while it asked: nothing more to do.
                    Err(_) if self.cancel.closed() => return,
                    Err(e) => {
                        failures += 1;
                        // No answer in time: a server that does not answer a song is not asked again
                        // and again while the player waits on it (octo-fiesta fetching a provider's song
                        // it cannot have). The network's failure, as the queue's rules take it.
                        if failures > RETRIES || e == OpenError::TimedOut {
                            let mut s = self.state.lock();
                            s.answered = if let OpenError::Status(status) = e { Some(status) } else { None };
                            s.error = Some(e.to_string());
                            s.done = true;
                            self.wake(&mut s);
                            continue;
                        }
                        std::thread::sleep(self.retry_wait(failures));
                        continue;
                    }
                }
            }
            let got = body.as_mut().expect("a body is open").read(&mut chunk);
            let mut s = self.state.lock();
            let mut took = 0;
            match got {
                // The end, cleanly: where the song really ends, whatever length was promised. A body that
                // breaks is an error instead, and is asked for again from where it broke.
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
                // Broken off: asked for again from where it broke. One that stalled counts as a failure,
                // so a server that answers and then never sends gives up for good after a few.
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
            // Written with the lock let go: the demuxer reads on meanwhile.
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

    /// A live stream: its one connection read as the bytes come, the announcements taken out of them,
    /// what the reader has left behind let go. A connection that drops is made again, the stream going on
    /// from wherever the station is by then; one that cannot be made again is the stream's end.
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
                    // A reader that stopped: the connection waits in the socket until it reads on.
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
                        std::thread::sleep(self.retry_wait(failures));
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
                // The station closed the stream, or the connection broke: made again.
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

    /// Bytes arrived: a blocked reader reads on, and a waiting engine is told once there is enough.
    /// The loader stopped for good without saying why (its thread panicked): readers waiting are told the
    /// song failed, and the engine is woken to hear it.
    fn gave_up(&self, why: &str) {
        let mut s = self.state.lock();
        if s.error.is_none() && !s.at_end() {
            s.error = Some(why.to_string());
        }
        // Nothing more comes, nor from anywhere a reader asks: it ends here.
        s.done = true;
        s.restart = None;
        self.cv.notify_all();
        self.wake(&mut s);
    }

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

/// A song's bytes as a seekable stream for the demuxer: reads block until the bytes are there.
pub struct LoadedReader {
    loader: Arc<Loader>,
    pos: u64,
    /// Set once nobody wants what this reads ([`Loader::reader_until`]).
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
                // Within the low mark of the end of what is here: the loader's next burst is due.
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
            // At or past the song's end: nothing to fetch there (a container reader looking for the end
            // where an estimated length put it, once the real one is known).
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

/// `buf.len()` bytes or fewer of `file` from byte `at`, leaving no place in it behind: several readers may
/// read one song's entry at once.
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

/// Every song's loader still running or held, for the perf report's memory line ([`held`]).
static ALIVE: Mutex<Vec<Weak<Loaded>>> = Mutex::new(Vec::new());

fn alive(loaded: &Arc<Loaded>) {
    let mut all = ALIVE.lock();
    all.retain(|w| w.strong_count() > 0);
    all.push(Arc::downgrade(loaded));
}

/// What the songs' loaders hold now: for the perf report, read at a stretch's ends only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    /// Loaders alive: kept by the engine for a song it plays or will, or still running.
    pub songs: u32,
    /// Bytes they keep in memory, as allocated.
    pub bytes: u64,
    /// Of them, songs read from their stream cache entry, their memory let go.
    pub on_disk: u32,
}

/// What every song's loader holds in memory now.
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

/// A live stream's body with the station's announcements taken out: every `every` bytes of music the
/// server puts one byte saying how many sixteens of bytes of announcement follow (none, mostly), and
/// the announcement itself, `StreamTitle='...';`.
struct Icy {
    inner: Box<dyn Read + Send>,
    every: Option<usize>,
    /// Bytes of music left before the next announcement.
    left: usize,
    /// The last title announced, until the loader takes it.
    title: Option<String>,
}

impl Icy {
    fn new(inner: Box<dyn Read + Send>, every: Option<usize>) -> Icy {
        let every = every.filter(|&n| n > 0);
        Icy { inner, every, left: every.unwrap_or(0), title: None }
    }

    /// The announcement at this point of the stream, read whole.
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

/// The title in an ICY announcement (`StreamTitle='Artist - Song';StreamUrl='';`), padded with NULs to
/// its sixteens; None when it names none. Stations send Latin-1 as often as UTF-8, so bytes that are not
/// UTF-8 are read as Latin-1.
pub fn stream_title(text: &[u8]) -> Option<String> {
    let text = match std::str::from_utf8(text) {
        Ok(t) => t.to_string(),
        Err(_) => text.iter().map(|&b| b as char).collect(),
    };
    let from = text.find("StreamTitle='")? + "StreamTitle='".len();
    let rest = &text[from..];
    // The title ends at the quote that closes the field; a quote inside it is left in.
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

    /// A server that makes up `len` bytes (byte `i` is `i as u8`) and counts what it is asked for.
    struct Counting {
        len: u64,
        opens: Mutex<Vec<u64>>,
        served: Arc<AtomicU64>,
    }

    struct Made {
        at: u64,
        len: u64,
        served: Arc<AtomicU64>,
    }

    impl Read for Made {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min((self.len - self.at) as usize);
            for (k, b) in buf[..n].iter_mut().enumerate() {
                *b = (self.at + k as u64) as u8;
            }
            self.at += n as u64;
            self.served.fetch_add(n as u64, Ordering::Relaxed);
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
        Arc::new(Counting { len, opens: Mutex::new(Vec::new()), served: Arc::new(AtomicU64::new(0)) })
    }

    /// Waits until the loader has stopped fetching: nothing served for a while.
    fn settled(s: &Counting) -> u64 {
        let until = Instant::now() + Duration::from_secs(10);
        let mut last = u64::MAX;
        while Instant::now() < until {
            let now = s.served.load(Ordering::Relaxed);
            if now == last {
                return now;
            }
            last = now;
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the loader never rested");
    }

    fn read(r: &mut LoadedReader, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        r.read_exact(&mut out).unwrap();
        out
    }

    /// A million bytes for ten seconds of music: a hundred bytes a millisecond, so a window of one
    /// to four seconds is 100 kB to 400 kB.
    const LOAD: [i64; 5] = [1_000, 4_000, 0, 0, 1 << 30];

    #[test]
    fn it_fetches_up_to_the_high_mark_then_leaves_the_network_alone_until_the_low_mark() {
        let s = server(1_000_000);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(10_000), None);
        let mut r = l.reader();
        let first = settled(&s);
        assert!((400_000..400_000 + CHUNK as u64).contains(&first), "one burst to the high mark: {first}");
        assert_eq!(*s.opens.lock(), vec![0]);
        // Playing on with more than the low mark still ahead: not a byte fetched.
        let got = read(&mut r, 250_000);
        assert!(got.iter().enumerate().all(|(i, &b)| b == i as u8), "the bytes are the song's");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(s.served.load(Ordering::Relaxed), first, "the network sleeps between bursts");
        assert_eq!(s.opens.lock().len(), 1);
        // Within the low mark of the end of what is there: the next burst, from where the last stopped.
        let at = first as usize - 90_000;
        read(&mut r, at - 250_000);
        let second = settled(&s);
        assert_eq!(s.opens.lock().clone(), vec![0, first], "a second request, picking up where the first stopped");
        assert!(second - first >= 300_000, "a whole burst, not a top-up: {}", second - first);
        let rest = read(&mut r, 1_000_000 - at);
        assert!(rest.iter().enumerate().all(|(i, &b)| b == (at + i) as u8));
        assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0, "and then the end");
        assert_eq!(l.bursts() as usize, s.opens.lock().len());
    }

    #[test]
    fn a_song_that_fits_the_window_is_fetched_whole_in_one_request() {
        let s = server(300_000);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(3_000), None);
        assert_eq!(settled(&s), 300_000);
        let mut r = l.reader();
        let all = read(&mut r, 300_000);
        assert!(all.iter().enumerate().all(|(i, &b)| b == i as u8));
        assert_eq!(*s.opens.lock(), vec![0]);
    }

    #[test]
    fn a_song_fetched_ahead_holds_its_budget_and_the_rest_once_it_plays() {
        let s = server(600_000);
        let l = Loader::start_within(s.clone(), "song".into(), LOAD, Some(6_000), None, Some(200_000), None, Waits::default());
        let ahead = settled(&s);
        assert!(ahead <= 200_000 + CHUNK as u64, "no more than its budget while it waits: {ahead}");
        assert!(l.held() >= 100_000, "but its start is at hand: {}", l.held());
        assert_eq!(l.holding(), 200_000);
        // Played now: the whole cap. The rest comes when the reader nears the end of what is there.
        l.limit(None);
        let mut r = l.reader();
        let all = read(&mut r, 600_000);
        assert!(all.iter().enumerate().all(|(i, &b)| b == i as u8), "every byte, in order");
        assert_eq!(*s.opens.lock(), vec![0, ahead], "one more request, from where the budget stopped it");
    }

    #[test]
    fn a_song_whole_in_the_stream_cache_is_read_from_there_and_its_memory_goes() {
        let d = nori_testdir::TempDir::new("source-disk");
        let store = Arc::new(crate::store::Store::open(d.path(), 1 << 22, Box::new(crate::store::Recent::default())).unwrap());
        let s = server(300_000);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(3_000), store.writer("song:0"));
        let mut r = l.reader();
        let all = read(&mut r, 300_000);
        assert!(all.iter().enumerate().all(|(i, &b)| b == i as u8));
        assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0, "the end");
        // The loader lets its copy go on its own thread once the last bytes are in: a moment after the
        // reader may have read them.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !l.on_disk() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(l.on_disk(), "{}", l.words());
        assert_eq!((l.held(), l.holding()), (0, 0), "nothing kept in memory");
        assert!(l.complete());
        // Read again from anywhere, as a seek back would.
        let mut again = l.reader();
        again.seek(SeekFrom::Start(123_456)).unwrap();
        assert_eq!(read(&mut again, 10), (123_456..123_466).map(|i| i as u8).collect::<Vec<_>>());
        assert_eq!(*s.opens.lock(), vec![0], "and the network was asked once");
        let h = held();
        assert!(h.on_disk >= 1 && h.songs >= 1, "{h:?}");
    }

    #[test]
    fn a_song_s_bytes_are_held_in_one_piece_the_size_of_the_song() {
        let s = server(300_000);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(3_000), None);
        settled(&s);
        assert_eq!(l.0.state.lock().data.capacity(), 300_000, "made once, not grown by doubling");
    }

    #[test]
    fn a_seek_past_what_is_loaded_fetches_from_there() {
        let s = server(5_000_000);
        let l = Loader::start(s.clone(), "song".into(), LOAD, Some(50_000), None);
        settled(&s);
        let mut r = l.reader();
        r.seek(SeekFrom::Start(4_000_000)).unwrap();
        let got = read(&mut r, 1000);
        assert!(got.iter().enumerate().all(|(i, &b)| b == (4_000_000 + i) as u8));
        assert_eq!(*s.opens.lock(), vec![0, 4_000_000], "not the megabytes in between");
    }

    /// A transcoding server: every answer promises `promised` bytes (an estimate), `real` come, and a
    /// range from `real` on is answered 416, with the real length when `says` (`Content-Range: */N`).
    /// The first body breaks with an error after `cut` bytes, once, as a dropped network would.
    struct Estimating {
        real: u64,
        promised: u64,
        says: bool,
        cut: Mutex<Option<u64>>,
        /// The body ends with an error rather than cleanly, as OkHttp reads a body shorter than its
        /// Content-Length.
        breaks_at_end: bool,
        opens: Mutex<Vec<u64>>,
        served: Arc<AtomicU64>,
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
            let made = Made { at: from, len: self.real, served: self.served.clone() };
            let reader = Cut { made, cut: self.cut.lock().take(), breaks_at_end: self.breaks_at_end };
            Ok(Body { start: from, len: Some(self.promised), reader: Box::new(reader) })
        }
    }

    fn estimating(real: u64, promised: u64, says: bool) -> Estimating {
        Estimating { real, promised, says, cut: Mutex::new(None), breaks_at_end: false, opens: Mutex::new(Vec::new()), served: Arc::new(AtomicU64::new(0)) }
    }

    #[test]
    fn a_clean_end_short_of_the_promised_length_is_the_song_s_end() {
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
    fn a_read_past_the_real_end_learns_it_and_is_the_end_not_a_failure() {
        for says in [true, false] {
            let s = Arc::new(estimating(3_000_000, 3_200_000, says));
            let l = Loader::start(s.clone(), "song".into(), LOAD, Some(30_000), None);
            let mut r = l.reader();
            read(&mut r, 1000);
            // Where an Ogg reader looks for the last page: one page's most before the promised end.
            let probe = 3_200_000 - 65_307;
            r.seek(SeekFrom::Start(probe)).unwrap();
            assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0, "nothing there: the end (says {says})");
            assert!(l.error().is_none(), "not a failure: {:?}", l.error());
            assert_eq!(l.length(), says.then_some(3_000_000), "the real length, or none known (says {says})");
            assert_eq!(l.shortened(), 1);
            assert_eq!(s.opens.lock().iter().filter(|&&o| o == probe).count(), 1, "asked once: {:?}", s.opens.lock());
            // And what is there is still read.
            r.seek(SeekFrom::Start(2_999_000)).unwrap();
            let tail = read(&mut r, 1000);
            assert!(tail.iter().enumerate().all(|(i, &b)| b == (2_999_000 + i) as u8));
            assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0);
            assert_eq!(l.length(), Some(3_000_000), "the end found where the bytes stop (says {says})");
        }
    }

    #[test]
    fn a_body_that_breaks_at_the_real_end_learns_it_from_the_answer_past_it() {
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
    fn a_network_that_drops_mid_song_is_asked_again_not_taken_for_the_end() {
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

    /// Songs being opened for nothing (a mix made again, a song measured ahead and let go) wait on the same
    /// song's bytes as the one the player needs, and give up as the engine moves on: the one still wanted
    /// is woken when the bytes come all the same, rather than sleeping out its timeout while they sit
    /// there (a phone's music stopped at the end of a song, the next one never heard).
    #[test]
    fn readers_that_give_up_leave_the_one_still_waiting_to_be_woken_by_the_bytes() {
        /// Answers only once the test lets it, so every reader is waiting when the others give up.
        struct Late(Arc<Counting>, Arc<AtomicBool>);
        impl ByteSource for Late {
            fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
                while !self.1.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                self.0.open(url, from)
            }
        }
        let blocked = |l: &Loader| l.0.state.lock().blocked;
        let until = |l: &Loader, n: u32, what: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while blocked(l) != n {
                assert!(Instant::now() < deadline, "{what}: {} readers blocked, not {n}", blocked(l));
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let answer = Arc::new(AtomicBool::new(false));
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
        until(&l, 9, "the player's reader and eight opened for nothing all wait on the bytes");
        stop.store(true, Ordering::Release);
        l.nudge();
        let gave_up = others.into_iter().map(|o| o.join().unwrap()).filter(|&e| e).count();
        assert_eq!(gave_up, 8, "the readers nobody wants give up");
        assert_eq!(blocked(&l), 1, "the player's reader still waits, and is counted as waiting");
        let answered = Instant::now();
        answer.store(true, Ordering::Release);
        let (n, at) = player.join().unwrap();
        assert_eq!(n.expect("the bytes"), 16);
        let took = at - answered;
        assert!(took < Duration::from_secs(3), "read as the bytes came, not after the {READ_TIMEOUT:?} timeout: {took:?}");
    }

    #[test]
    fn a_reader_nobody_wants_any_more_stops_waiting_and_moves_the_fetch_no_more() {
        /// Slow to answer from anywhere but the start.
        struct Slow(Arc<Counting>);
        impl ByteSource for Slow {
            fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
                if from > 0 {
                    std::thread::sleep(Duration::from_millis(1500));
                }
                self.0.open(url, from)
            }
        }
        let s = server(5_000_000);
        let l = Loader::start(Arc::new(Slow(s.clone())), "song".into(), LOAD, Some(50_000), None);
        let stop = Arc::new(AtomicBool::new(false));
        let mut r = l.reader_until(stop.clone());
        read(&mut r, 1000);
        r.seek(SeekFrom::Start(4_000_000)).unwrap();
        let (l2, stop2) = (l.clone(), stop.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            stop2.store(true, Ordering::Release);
            l2.nudge();
        });
        let t = Instant::now();
        assert!(r.read(&mut [0u8; 16]).is_err(), "given up");
        assert!(t.elapsed() < Duration::from_millis(1000), "at once, not once the bytes came: {:?}", t.elapsed());
        assert!(r.read(&mut [0u8; 16]).is_err(), "and for good");
    }

    #[test]
    fn a_server_without_ranges_that_ends_before_the_place_asked_for_is_the_end() {
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

    /// A server that never answers, or answers and never sends a byte, until the request is called off
    /// (as OkHttp's cancelled call fails): counts the requests made and the ones called off.
    #[derive(Default)]
    struct Silent {
        headers: bool,
        asked: AtomicU64,
        called_off: Arc<AtomicU64>,
    }

    struct Nothing(Arc<AtomicBool>, Arc<AtomicU64>);

    impl Nothing {
        fn wait(&self) {
            let until = Instant::now() + Duration::from_secs(30);
            while !self.0.load(Ordering::Acquire) {
                assert!(Instant::now() < until, "never called off");
                std::thread::sleep(Duration::from_millis(1));
            }
            self.1.fetch_add(1, Ordering::Relaxed);
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
            self.asked.fetch_add(1, Ordering::Relaxed);
            let off = Arc::new(AtomicBool::new(false));
            let o = off.clone();
            cancel.on_cancel(move || o.store(true, Ordering::Release));
            let nothing = Nothing(off, self.called_off.clone());
            if self.headers {
                nothing.wait();
                return Err("Canceled".into());
            }
            Ok(Body { start: from, len: Some(1_000_000), reader: Box::new(nothing) })
        }
    }

    fn called_off(s: &Silent, n: u64) {
        let until = Instant::now() + Duration::from_secs(10);
        while s.called_off.load(Ordering::Relaxed) < n {
            assert!(Instant::now() < until, "{} of {n} called off", s.called_off.load(Ordering::Relaxed));
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_song_let_go_calls_off_its_request_that_never_answers_or_never_sends() {
        for headers in [true, false] {
            let s = Arc::new(Silent { headers, ..Silent::default() });
            let l = Loader::start(s.clone(), "ext-1".into(), LOAD, Some(3_000), None);
            let until = Instant::now() + Duration::from_secs(10);
            while s.asked.load(Ordering::Relaxed) == 0 {
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(1));
            }
            drop(l);
            called_off(&s, 1);
            assert_eq!(s.asked.load(Ordering::Relaxed), 1, "and not asked again (headers {headers})");
        }
    }

    #[test]
    fn a_request_with_no_answer_in_time_is_called_off_as_timed_out() {
        let s = Silent { headers: true, ..Silent::default() };
        let cancel = Cancel::stalling_after(100);
        let t = Instant::now();
        let got = open_watched(&s, "ext-1", None, 0, &cancel);
        assert_eq!(got.err(), Some(OpenError::TimedOut));
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        // And a body whose bytes stop: its read fails as timed out, so the loader asks again.
        let s = Silent { headers: false, ..Silent::default() };
        let cancel = Cancel::stalling_after(100);
        let Ok(mut b) = open_watched(&s, "ext-1", None, 0, &cancel) else { panic!("the headers came") };
        let e = b.reader.read(&mut [0u8; 16]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }

    /// No answer in time: the song fails at once, of the network (no error status), not after asking
    /// again and again while the player waits on it.
    #[test]
    fn a_song_whose_server_does_not_answer_in_time_fails_at_once_as_the_network_s_failure() {
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
    fn past_max_asking_the_oldest_request_to_the_same_source_is_called_off() {
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
        // One answered makes room: the next asked calls nobody off.
        asking.answered(&calls[5]);
        ask(1, 100);
        assert_eq!(off.lock().len(), 2);
    }
}
