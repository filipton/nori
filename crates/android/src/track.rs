//! nori-engine's Android output (`AudioOutput`): a writer thread moves the engine's ring into a deep
//! AudioTrack in bursts.
//!
//! The track holds a burst plus margin ([`TRACK_US`]). The writer sleeps until [`LOW_US`] is left, then
//! moves the whole ring in at once, which drops the ring below its low mark and wakes the engine for its
//! next burst: about one wake each every ten seconds, and no engine timer (`AudioOutput::bursts`). Other
//! wakes: engine commands (unpark), filling after start/flush (every [`FILL_TICK_MS`], backing off while
//! starved), fades (every `FADE_TICK_MS`; fades run at the track volume since seconds sit in the track),
//! two clock readings after a start ([`SETTLE_MS`]), and the end of the music.
//!
//! A track that fails a write is dead: it is reopened and refilled; if that fails the engine is told
//! ([`AudioOutput::failed`]).
//!
//! While the equalizer is open (`AudioOutput::shallow`) the same track is resized in place with
//! `setBufferSizeInFrames` ([`Sink::resize`]) rather than reopened (a new track stutters). The shallow
//! size comes from the route ([`shallow_marks`]: latency, min buffer, wake lateness) and grows when the
//! writer sees more latency or underruns ([`Needs`]); the engine sizes its ring from it
//! ([`AudioOutput::shallow_depth`]). The writer runs at audio priority so a shallow track does not run dry.
//!
//! A track that takes less than it claims is re-sized to what it held when it refused a write
//! ([`Writer::refused`]); writes are timed by what the track holds, not by what was pulled.
//!
//! No JNI here: the AudioTrack is a [`Sink`] (player.rs has the real one), so the tests run on a
//! simulated track and virtual clock.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{JoinHandle, Thread};
use std::time::Duration;

use nori_engine::{AudioOutput, DeviceWatch, Feed, OutputFormat, ShallowDepth};
use nori_player::burst::BUFFER_US;
use nori_player::transport::{fade_step, FADE_TICK_MS};
use parking_lot::Mutex;

/// Top-up threshold.
pub(crate) const LOW_US: i64 = 1_000_000;
/// Track size: a burst, the low mark, and 0.5 s of playback during the next decode. A top-up then empties
/// the ring below its own low mark, waking the engine (`AudioOutput::bursts`).
pub(crate) const TRACK_US: i64 = BUFFER_US + LOW_US + 500_000;
/// Shallow track size on the speaker: 80-160 ms, plus the ring's 40-80 ms, is the equalizer's latency.
pub(crate) const SHALLOW_TRACK_US: i64 = 160_000;
/// Wake lateness allowed for while shallow.
const SHALLOW_LATE_US: i64 = 40_000;
/// Largest a shallow track grows.
const SHALLOW_MOST_US: i64 = 1_500_000;
/// Observed latency this far above plan makes the shallow track deeper.
const LAG_STEP_US: i64 = 20_000;
/// Bytes per write.
pub(crate) const CHUNK_BYTES: usize = 128 * 1024;
/// Fill interval after a start or flush.
const FILL_TICK_MS: u64 = 20;
/// Longest fill back-off while the ring is empty.
const STARVED_MAX_MS: u64 = 1_000;
/// Deep and playing, less than this in the track with an empty ring is logged: the engine is late.
const LATE_US: i64 = 1_000_000;
/// Clock re-reads after a start (the device's first timestamps are late).
const SETTLE_MS: [i64; 2] = [250, 1_000];
/// Start threshold (`RustPlayer.openTrack`, `JavaTrack::resize`).
const START_US: i64 = 250_000;
/// Size a starts-when-full track (pre-Android 12) gets after a flush until it starts, so it starts as
/// soon as a 12+ track would instead of after filling eleven seconds.
const PRIMING_US: i64 = 250_000;

/// The AudioTrack as the writer uses it.
pub(crate) trait Sink: Send {
    /// Staging memory, [`CHUNK_BYTES`] long, fixed for the sink's life.
    fn staging(&mut self) -> &mut [f32];
    /// Non-blocking write of staging bytes `from..from + len`: bytes taken, or the error code (the track
    /// is dead).
    fn write(&mut self, from: usize, len: usize) -> Result<usize, i32>;
    fn play(&mut self);
    fn pause(&mut self);
    /// Drops unplayed data. Only while paused or stopped.
    fn flush(&mut self);
    /// Plays out what was written, then stops (for a starts-when-full track at the end of the music).
    fn stop(&mut self);
    fn set_volume(&mut self, volume: f32);
    /// Frames presented since the last flush and the CLOCK_MONOTONIC ns of that reading.
    fn heard(&mut self, playing: bool) -> Option<(u64, i64)>;
    /// `setBufferSizeInFrames` (up to the opened size); keeps playing and drops nothing. Returns the size
    /// given.
    fn resize(&mut self, frames: u64) -> u64;
    /// The current output route.
    fn route(&mut self) -> Route {
        Route::default()
    }
    /// `getUnderrunCount`.
    fn underruns(&mut self) -> Option<u64> {
        None
    }
    /// Frames the mixer took since the last flush (the play head, ahead of presentation by the output's
    /// latency).
    fn consumed(&mut self) -> Option<u64> {
        None
    }
    fn release(&mut self);
}

/// What an output route reports, for [`shallow_marks`].
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Route {
    /// `getMinBufferSize` in frames: roughly the output's pull size.
    pub min_frames: Option<u64>,
    /// Output latency past the track (`getLatency` minus the track buffer), frames.
    pub latency_frames: Option<u64>,
    /// For the log: "Bluetooth", "the phone speaker".
    pub name: Option<&'static str>,
}

/// What the shallow track needs on the current output, reported and observed, in frames.
#[derive(Default, Clone, Copy, Debug)]
struct Needs {
    /// Output latency: counted as in the track by the writer's clock.
    lag: u64,
    /// Output pull size, grown on every underrun.
    pull: u64,
    /// Output name; a new name resets the needs.
    name: Option<&'static str>,
    /// Underrun count at the last reading.
    underruns: Option<u64>,
    /// Times grown for underruns.
    grown: u32,
}

/// `(low, capacity)` of the shallow track for an output with latency `lag` and pull size `pull`
/// (frames): low covers lag + pull + wake lateness (at least half of [`SHALLOW_TRACK_US`], at most
/// [`SHALLOW_MOST_US`]); capacity adds max(low / 4, half of [`SHALLOW_TRACK_US`]). An output reporting
/// nothing gets 80/160 ms.
pub(crate) fn shallow_marks(rate: u32, lag: u64, pull: u64) -> (u64, u64) {
    let f = |us: i64| (rate as i64 * us / 1_000_000) as u64;
    let half = f(SHALLOW_TRACK_US / 2);
    let low = (lag + pull + f(SHALLOW_LATE_US)).max(half).min(f(SHALLOW_MOST_US));
    let top = half.max(low / 4);
    (low, low + top)
}

/// The shallow track's size and ring depth for the engine (`AudioOutput::shallow_depth`), µs; 0 until known.
#[derive(Default)]
pub(crate) struct Depth {
    device_us: AtomicI64,
    ring_us: AtomicI64,
}

impl Depth {
    fn set(&self, device_us: i64, ring_us: i64) {
        self.device_us.store(device_us, Ordering::Relaxed);
        self.ring_us.store(ring_us, Ordering::Relaxed);
    }

    pub(crate) fn get(&self) -> Option<ShallowDepth> {
        let (device_us, ring_us) = (self.device_us.load(Ordering::Relaxed), self.ring_us.load(Ordering::Relaxed));
        (device_us > 0).then_some(ShallowDepth { device_us, ring_us })
    }
}

/// An opened sink, its buffer in frames, and whether it starts only when full (pre-Android 12).
pub(crate) struct Opened {
    pub sink: Box<dyn Sink>,
    pub frames: u64,
    pub starts_full: bool,
}

/// Opens a track with a buffer of `frames`.
pub(crate) trait Opener: Send {
    fn open(&mut self, format: OutputFormat, float: bool, frames: u64) -> Result<Opened, String>;
}

/// Smallest track [`open_fitting`] tries.
const FITTING_MIN_US: i64 = 1_000_000;

/// Opens a track of `frames`, halving on failure down to [`FITTING_MIN_US`]: the sound server has a few
/// MB per app, and 11.5 s of 96 kHz float (8.8 MB) fails with -12.
pub(crate) fn open_fitting(opener: &mut dyn Opener, format: OutputFormat, float: bool, frames: u64) -> Result<Opened, String> {
    let least = (format.rate as i64 * FITTING_MIN_US / 1_000_000) as u64;
    let mut ask = frames;
    loop {
        match opener.open(format, float, ask) {
            Ok(o) => return Ok(o),
            Err(e) if ask / 2 >= least => {
                log(&format!("the AudioTrack would not open at {} ms ({e}): asking for half", ask * 1000 / format.rate.max(1) as u64));
                ask /= 2;
            }
            Err(e) => return Err(e),
        }
    }
}

/// The ring's read end: `nori_engine::Feed`, or a fake in tests.
pub(crate) trait Ring: Send {
    fn available(&self) -> usize;
    fn pull(&mut self, out: &mut [f32]) -> usize;
    fn pull_i16(&mut self, out: &mut [i16]) -> usize;
    fn flushed(&mut self) -> bool;
    fn ending(&self) -> bool;
    fn wake_engine(&self);
}

impl Ring for Feed {
    fn available(&self) -> usize {
        Feed::available(self)
    }
    fn pull(&mut self, out: &mut [f32]) -> usize {
        Feed::pull(self, out)
    }
    fn pull_i16(&mut self, out: &mut [i16]) -> usize {
        Feed::pull_i16(self, out)
    }
    fn flushed(&mut self) -> bool {
        Feed::flushed(self)
    }
    fn ending(&self) -> bool {
        Feed::ending(self)
    }
    fn wake_engine(&self) {
        Feed::wake_engine(self)
    }
}

/// CLOCK_MONOTONIC ns, the clock of AudioTimestamp (`System.nanoTime`).
#[allow(clippy::unnecessary_cast)] // 32-bit fields on 32-bit ABIs.
pub(crate) fn mono_ns() -> i64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a plain system call writing into the struct handed to it.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as i64 * 1_000_000_000 + t.tv_nsec as i64
}

/// Unwraps the 32-bit play head (wraps after 6 h at 192 kHz). Reset at every flush.
#[derive(Default, Clone, Copy)]
pub(crate) struct HeadCount {
    last: u32,
    wraps: u64,
}

impl HeadCount {
    pub(crate) fn read(&mut self, raw: u32) -> u64 {
        if raw < self.last {
            self.wraps += 1;
        }
        self.last = raw;
        self.wraps << 32 | raw as u64
    }
}

/// The track's frame counts: written by the writer, read by the engine (`latency_us`) from any thread.
#[derive(Default)]
pub(crate) struct Clock(Mutex<Counts>);

/// Frame counts since the last flush.
#[derive(Default, Clone, Copy)]
struct Counts {
    rate: u32,
    /// Pulled from the ring (written or staged).
    ahead: u64,
    /// Taken by the track.
    given: u64,
    /// Presented at `at_ns`; extrapolated from there while `running`.
    heard: u64,
    at_ns: i64,
    running: bool,
}

impl Counts {
    fn heard_at(&self, now_ns: i64) -> u64 {
        let moved = if self.running && self.rate > 0 { ((now_ns - self.at_ns).max(0) as i128 * self.rate as i128 / 1_000_000_000) as u64 } else { 0 };
        (self.heard + moved).min(self.given)
    }
}

impl Clock {
    /// Frames pulled but not yet presented.
    pub(crate) fn latency_frames(&self, now_ns: i64) -> u64 {
        let c = *self.0.lock();
        c.ahead.saturating_sub(c.heard_at(now_ns))
    }

    pub(crate) fn latency_us(&self, now_ns: i64) -> u64 {
        let rate = self.0.lock().rate.max(1) as u64;
        self.latency_frames(now_ns) * 1_000_000 / rate
    }

    /// Frames in the track, not yet presented (staged frames excluded). Times the next top-up.
    fn in_track(&self, now_ns: i64) -> u64 {
        let c = *self.0.lock();
        c.given.saturating_sub(c.heard_at(now_ns))
    }

    fn heard_now(&self, now_ns: i64) -> u64 {
        self.0.lock().heard_at(now_ns)
    }

    fn update(&self, f: impl FnOnce(&mut Counts)) {
        f(&mut self.0.lock());
    }

    fn running(&self) -> bool {
        self.0.lock().running
    }

    fn given(&self) -> u64 {
        self.0.lock().given
    }

    /// Resets all counts to zero at `now_ns`, stopped (after a flush or reopen).
    fn reset(&self, now_ns: i64) {
        self.update(|c| *c = Counts { rate: c.rate, at_ns: now_ns, ..Counts::default() });
    }

    /// Stops extrapolating (pause).
    fn freeze(&self, now_ns: i64) {
        self.update(|c| {
            c.heard = c.heard_at(now_ns);
            c.at_ns = now_ns;
            c.running = false;
        });
    }

    /// Extrapolates from now (track started).
    fn run(&self, now_ns: i64) {
        self.update(|c| {
            c.heard = c.heard_at(now_ns);
            c.at_ns = now_ns;
            c.running = true;
        });
    }

    /// Adopts the device's presented count, unless it exceeds what was given.
    fn anchor(&self, frames: u64, at_ns: i64, running: bool) {
        self.update(|c| {
            if frames <= c.given {
                c.heard = frames;
                c.at_ns = at_ns;
                c.running = running;
            }
        });
    }
}

/// The engine's commands to the writer.
#[derive(Default)]
pub(crate) struct Control {
    pub playing: bool,
    /// A fade: (from, or the current volume; to; ms).
    pub ramp: Option<(Option<f32>, f32, i64)>,
    pub stop: bool,
    pub shallow: bool,
}

/// Track size in frames, deep or shallow.
pub(crate) fn track_frames(rate: u32, shallow: bool) -> u64 {
    (rate as i64 * if shallow { SHALLOW_TRACK_US } else { TRACK_US } / 1_000_000) as u64
}

#[derive(Clone, Copy)]
struct Fade {
    from: f32,
    to: f32,
    start_ms: i64,
    ms: i32,
}

/// The writer thread's state; [`Writer::step`] runs once per wake.
pub(crate) struct Writer<R: Ring> {
    ring: R,
    sink: Box<dyn Sink>,
    /// Reopens a dead track.
    opener: Arc<Mutex<Box<dyn Opener>>>,
    format: OutputFormat,
    /// Frames requested at open.
    asked: u64,
    /// Frames the track was opened with; resizes stay within it.
    allocated: u64,
    /// Why the track could not be reopened, for [`AudioOutput::failed`].
    failure: Arc<Mutex<Option<String>>>,
    /// No track: nothing more is done.
    dead: bool,
    /// Just reopened: refill on the next fill tick.
    revived: bool,
    clock: Arc<Clock>,
    bytes: Arc<AtomicU64>,
    channels: usize,
    rate: u32,
    float: bool,
    /// Packed 24-bit samples (bit-perfect >16-bit songs).
    packed: bool,
    /// Frames the track holds: its size, or less once it refused a write it had room for.
    capacity: u64,
    /// Flushed while holding music and not heard playing since. AudioFlinger applies such a flush only at
    /// the mixer's next period, so a refused write then says nothing about capacity ([`Writer::refused`]).
    flushed_full: bool,
    /// Top-up threshold: [`LOW_US`] or half a smaller track.
    low: u64,
    starts_full: bool,
    /// (offset, len) of staged bytes not yet taken.
    staged: (usize, usize),
    playing: bool,
    /// Filling to full after a start or flush.
    filling: bool,
    /// Stopped at the end of the music, playing out.
    drained: bool,
    volume: f32,
    fade: Option<Fade>,
    started_ns: i64,
    /// Settle readings ([`SETTLE_MS`]) done since the last start.
    settled: usize,
    /// Next wait while the ring is empty.
    starved_ms: u64,
    shallow: bool,
    wants_shallow: bool,
    needs: Needs,
    /// Holding [`PRIMING_US`] after a flush until a starts-when-full track starts.
    priming: bool,
    depth: Arc<Depth>,
    /// Deep: underrun count at the last top-up ([`Writer::watch_deep`]).
    deep_underruns: Option<u64>,
    /// Deep: "engine is late" already logged, until full again.
    late_logged: bool,
}

impl<R: Ring> Writer<R> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(ring: R, opened: Opened, reopen: Reopen, format: OutputFormat, float: bool, clock: Arc<Clock>, bytes: Arc<AtomicU64>, depth: Arc<Depth>) -> Writer<R> {
        let rate = format.rate;
        clock.update(|c| *c = Counts { rate, ..Counts::default() });
        log_if_smaller(opened.frames, reopen.frames, rate);
        Writer {
            ring,
            sink: opened.sink,
            opener: reopen.opener,
            format,
            asked: reopen.frames,
            allocated: opened.frames,
            failure: reopen.failure,
            dead: false,
            revived: false,
            clock,
            bytes,
            channels: format.channels,
            rate,
            float,
            packed: packed24(format, float),
            capacity: opened.frames.max(1),
            flushed_full: false,
            low: low_mark(opened.frames, rate),
            starts_full: opened.starts_full,
            staged: (0, 0),
            playing: false,
            filling: true,
            drained: false,
            volume: 1.0,
            fade: None,
            started_ns: 0,
            settled: SETTLE_MS.len(),
            starved_ms: FILL_TICK_MS,
            shallow: false,
            wants_shallow: false,
            needs: Needs::default(),
            priming: false,
            deep_underruns: None,
            late_logged: false,
            depth,
        }
    }

    fn frame_bytes(&self) -> usize {
        self.channels * sample_bytes(self.float, self.packed)
    }

    /// Sets the capacity and the matching low mark.
    fn holds(&mut self, frames: u64) {
        self.capacity = frames.max(1);
        self.low = low_mark(frames, self.rate);
    }

    /// Resizes in place to shallow ([`shallow_marks`]) or deep, as the engine wants; buffered audio keeps
    /// playing.
    fn resize(&mut self) {
        self.shallow = self.wants_shallow;
        self.priming = false;
        if self.shallow {
            self.look_at_route();
            self.make_shallow(None);
            return;
        }
        let got = self.sink.resize(self.allocated);
        log(&format!("deep again in place: {} ms of the {} ms opened", self.ms(got), self.ms(self.allocated)));
        self.holds(got);
    }

    fn ms(&self, frames: u64) -> u64 {
        frames * 1000 / self.rate.max(1) as u64
    }

    fn frames(&self, us: i64) -> u64 {
        (self.rate as i64 * us / 1_000_000) as u64
    }

    /// Merges the route's report into `needs` (reset on a new output, otherwise only grown) and restarts
    /// underrun counting.
    fn look_at_route(&mut self) {
        let route = self.sink.route();
        let said_lag = route.latency_frames.unwrap_or(0).min(self.frames(SHALLOW_MOST_US));
        let said_pull = route.min_frames.unwrap_or(0).min(self.frames(SHALLOW_MOST_US));
        if route.name != self.needs.name {
            self.needs = Needs { name: route.name, ..Needs::default() };
        }
        self.needs.lag = self.needs.lag.max(said_lag);
        self.needs.pull = self.needs.pull.max(said_pull);
        self.needs.underruns = self.sink.underruns();
    }

    /// Resizes to the shallow marks for `needs` and publishes the depth for the engine; `why` (for the
    /// log) is what made it grow.
    fn make_shallow(&mut self, why: Option<String>) {
        let (low, capacity) = shallow_marks(self.rate, self.needs.lag, self.needs.pull);
        let got = self.sink.resize(capacity.min(self.allocated));
        self.capacity = got.max(1);
        self.low = low.min(got / 2 + got / 4);
        self.depth.set((self.capacity * 1_000_000 / self.rate.max(1) as u64) as i64, ((self.capacity - self.low) * 1_000_000 / self.rate.max(1) as u64) as i64);
        let to = self.needs.name.map_or(String::new(), |n| format!(" for {n}"));
        let line = match why {
            Some(why) => format!("shallow {} ms{to}, grown: {why}", self.ms(got)),
            None => {
                let mut parts = Vec::new();
                if self.needs.lag > 0 {
                    parts.push(format!("its latency is {} ms", self.ms(self.needs.lag)));
                }
                if self.needs.pull > 0 {
                    parts.push(format!("its pulls are {} ms", self.ms(self.needs.pull)));
                }
                let because = if parts.is_empty() { String::new() } else { format!(": {}", parts.join(", ")) };
                format!("shallow {} ms{to}{because}", self.ms(got))
            }
        };
        log(&format!("{line} (topped up at {} ms, of the {} ms opened)", self.ms(self.low), self.ms(self.allocated)));
        nori_perf::invariants::tuning_said(&line);
    }

    /// Shallow: grows the track for new underruns or for observed latency (consumed - presented) above
    /// plan. Never shrinks on the same output.
    fn watch_output(&mut self, now_ns: i64) {
        if let Some(n) = self.sink.underruns() {
            match self.needs.underruns.replace(n) {
                Some(was) if n > was => {
                    self.needs.grown += 1;
                    let more = (self.needs.pull / 2).max(self.frames(SHALLOW_TRACK_US / 2));
                    self.needs.pull = (self.needs.pull + more).min(self.frames(SHALLOW_MOST_US));
                    let why = format!("it ran dry {} more time{}, its pulls are counted as {} ms", n - was, if n - was == 1 { "" } else { "s" }, self.ms(self.needs.pull));
                    self.make_shallow(Some(why));
                    return;
                }
                _ => {}
            }
        }
        if let Some(consumed) = self.sink.consumed() {
            let heard = self.clock.heard_now(now_ns);
            let lag = consumed.saturating_sub(heard).min(self.frames(SHALLOW_MOST_US));
            if lag > self.needs.lag + self.frames(LAG_STEP_US) {
                self.needs.lag = lag;
                let why = format!("its latency is seen to be {} ms", self.ms(lag));
                self.make_shallow(Some(why));
            }
        }
    }

    /// Deep: logs underruns since the last top-up (not counting fills after a start or flush).
    fn watch_deep(&mut self, now_ns: i64) {
        let now = self.sink.underruns();
        let was = std::mem::replace(&mut self.deep_underruns, now);
        if self.filling {
            return;
        }
        if let (Some(n), Some(was)) = (now, was) {
            if n > was {
                let fill = self.clock.in_track(now_ns);
                log(&format!("the AudioTrack ran dry {} more time{} (it holds {} ms now, {} ms in the ring)", n - was, if n - was == 1 { "" } else { "s" }, self.ms(fill), self.ms(self.ring.available() as u64)));
            }
        }
    }

    /// One wake: applies the engine's commands and tops the track up. Returns ms to sleep; None to sleep
    /// until unparked.
    pub(crate) fn step(&mut self, now_ns: i64, c: &mut Control) -> Option<u64> {
        if self.dead {
            return None;
        }
        let now_ms = now_ns / 1_000_000;
        self.wants_shallow = c.shallow;
        if self.shallow != self.wants_shallow {
            self.resize();
        }
        let mut wake: Option<u64> = None;
        let mut at = |ms: u64| wake = Some(wake.map_or(ms, |w| w.min(ms)));
        if let Some((from, to, ms)) = c.ramp.take() {
            self.fade = Some(Fade { from: from.unwrap_or(self.volume), to, start_ms: now_ms, ms: ms.clamp(0, i32::MAX as i64) as i32 });
        }
        if let Some(f) = self.fade {
            let (v, done) = fade_step(f.from, f.to, f.start_ms, now_ms, f.ms);
            self.sink.set_volume(v);
            self.volume = v;
            if done {
                self.fade = None;
            } else {
                at(FADE_TICK_MS as u64);
            }
        }
        if c.playing != self.playing {
            self.playing = c.playing;
            log(if self.playing { "plays" } else { "pauses" });
            if self.playing {
                self.start(now_ns);
            } else {
                // The engine pauses when its clock ends the fade; this thread may be a step behind. Finish
                // the fade so the track pauses at its target, not a step short.
                if let Some(f) = self.fade.take() {
                    self.sink.set_volume(f.to);
                    self.volume = f.to;
                }
                self.sink.pause();
                self.clock.freeze(now_ns);
                if let Some((frames, ns)) = self.sink.heard(false) {
                    self.clock.anchor(frames, ns, false);
                }
            }
        } else if self.playing && !self.clock.running() {
            // Paused and resumed before this wake: `TrackOutput::pause` froze the clock, but the track kept
            // playing. Restart the clock, or top-ups stop and the track runs dry.
            self.clock.run(now_ns);
            self.read_clock();
        }
        // An empty pull surfaces a pending ring flush.
        self.ring.pull(&mut []);
        if self.ring.flushed() {
            self.restart(now_ns);
        }
        if !self.playing {
            return wake;
        }
        if self.settled < SETTLE_MS.len() {
            let due = self.started_ns + SETTLE_MS[self.settled] * 1_000_000;
            if now_ns >= due {
                self.settled += 1;
                self.read_clock();
            }
            if let Some(ms) = SETTLE_MS.get(self.settled) {
                at(((self.started_ns + ms * 1_000_000 - now_ns).max(0) / 1_000_000) as u64 + 1);
            }
        }
        if let Some(ms) = self.fill(now_ns) {
            at(ms);
        }
        wake
    }

    /// Plays, fills to full, and schedules the settle readings.
    fn start(&mut self, now_ns: i64) {
        self.sink.play();
        self.clock.run(now_ns);
        self.filling = true;
        self.started_ns = now_ns;
        self.settled = 0;
        self.starved_ms = FILL_TICK_MS;
    }

    /// The track is empty (flushed or new): resets counts and refills, playing if it was.
    fn refill_from_empty(&mut self, now_ns: i64) {
        self.staged = (0, 0);
        self.drained = false;
        self.clock.reset(now_ns);
        if self.playing {
            self.start(now_ns);
        } else {
            self.filling = true;
        }
    }

    /// The ring was flushed: flushes the track too. A starts-when-full track is primed to [`PRIMING_US`].
    fn restart(&mut self, now_ns: i64) {
        log("emptied for the music that follows");
        self.sink.pause();
        self.sink.flush();
        self.flushed_full |= self.clock.given() > 0;
        if self.starts_full && !self.shallow && !self.priming {
            let got = self.sink.resize(self.frames(PRIMING_US).min(self.allocated));
            self.holds(got);
            self.priming = true;
        }
        self.refill_from_empty(now_ns);
    }

    fn read_clock(&mut self) {
        if let Some((frames, ns)) = self.sink.heard(self.playing) {
            // Post-flush music is being heard: the platform has applied the flush.
            if frames > 0 && frames <= self.clock.given() {
                self.flushed_full = false;
            }
            self.clock.anchor(frames, ns, self.playing);
            self.report_to_perf_watch(frames);
        }
    }

    /// Perf build: reports presented vs given frames (nori_perf::invariants).
    fn report_to_perf_watch(&self, presented: u64) {
        if nori_perf::invariants::on() {
            nori_perf::invariants::track_seen(mono_ns() / 1_000_000, self.playing && !self.dead, self.clock.given(), presented, self.rate);
        }
    }

    /// Perf build: reads the device clock for the watch even on wakes that don't need it, so a stalled
    /// count is caught.
    fn read_clock_for_perf_watch(&mut self) {
        if nori_perf::invariants::on() {
            if let Some((frames, _)) = self.sink.heard(self.playing) {
                self.report_to_perf_watch(frames);
            }
        }
    }

    /// Tops the track up when due; returns ms until the next look.
    fn fill(&mut self, now_ns: i64) -> Option<u64> {
        let ms = |frames: u64, rate: u32| frames * 1000 / rate.max(1) as u64;
        // Timed by what the track holds, not by what was pulled: staged leftovers can't play.
        let fill = self.clock.in_track(now_ns);
        if self.drained {
            if self.ring.available() == 0 {
                return None;
            }
            // Music arrived after the end drained (the queue grew): restart from empty.
            self.restart(now_ns);
        } else if !self.filling && fill > self.low {
            self.read_clock_for_perf_watch();
            return Some(ms(fill - self.low, self.rate) + 1);
        }
        self.read_clock();
        if self.priming && self.playing && self.sink.heard(true).is_some_and(|(frames, _)| frames > 0) {
            // Started: back to the full size.
            let got = self.sink.resize(self.allocated);
            self.holds(got);
            self.priming = false;
            self.filling = true;
        }
        if self.shallow && self.playing {
            if self.filling {
                // Underruns while filling from empty don't count.
                self.needs.underruns = self.sink.underruns();
            } else {
                self.watch_output(now_ns);
            }
        } else if self.playing {
            self.watch_deep(now_ns);
        }
        let full = self.top_up(now_ns);
        if self.dead {
            return None;
        }
        if std::mem::take(&mut self.revived) {
            return Some(FILL_TICK_MS);
        }
        let fill = self.clock.in_track(now_ns);
        if full {
            if std::mem::take(&mut self.late_logged) {
                log("full again");
            }
            self.filling = false;
            self.starved_ms = FILL_TICK_MS;
            // At least a fill tick: a "full" track below its low mark would otherwise spin.
            return Some((ms(fill.saturating_sub(self.low), self.rate) + 1).max(FILL_TICK_MS));
        }
        if self.ring.ending() && self.ring.available() == 0 {
            // All music is in the track: a starts-when-full track is told to play out; look again when done.
            self.filling = false;
            if self.starts_full && !self.drained {
                self.sink.stop();
                self.drained = true;
            }
            return Some(ms(self.clock.latency_frames(now_ns), self.rate) + 1);
        }
        // The ring ran out before the track filled: between bursts the track has plenty; otherwise poll
        // with back-off.
        if !self.filling && fill > self.low {
            return Some(ms(fill - self.low, self.rate) + 1);
        }
        if !self.filling && !self.shallow && !self.late_logged && fill < self.frames(LATE_US) {
            // Deep and low with an empty ring: the engine is behind (network or CPU).
            self.late_logged = true;
            log(&format!("down to {} ms with nothing more to give it: the engine is late", ms(fill, self.rate)));
        }
        let mut wait = self.starved_ms;
        self.starved_ms = (self.starved_ms * 2).min(STARVED_MAX_MS);
        if fill > 0 {
            // Never more than half of what the track holds: a ring shallower than the track never fills
            // it, and a full back-off let it run dry.
            wait = wait.min((ms(fill, self.rate) / 2).max(FILL_TICK_MS));
        }
        Some(wait)
    }

    /// Moves as much of the ring into the track as fits. True when the track is full.
    fn top_up(&mut self, now_ns: i64) -> bool {
        let fb = self.frame_bytes();
        // 24-bit samples are pulled as floats and packed in place: a chunk is as many frames as floats fit.
        let chunk_frames = CHUNK_BYTES / if self.packed { self.channels * 4 } else { fb };
        loop {
            if self.staged.1 > 0 && !self.write_staged(now_ns) {
                return true;
            }
            let room = self.capacity.saturating_sub(self.clock.latency_frames(now_ns));
            if room == 0 {
                return true;
            }
            let n = (room as usize).min(self.ring.available()).min(chunk_frames);
            if n == 0 {
                return false;
            }
            // Counted before the pull so the engine never sees the position ahead of the truth.
            self.clock.update(|c| c.ahead += n as u64);
            let samples = n * self.channels;
            let got = if self.float {
                self.ring.pull(&mut self.sink.staging()[..samples])
            } else if self.packed {
                let staging = self.sink.staging();
                let got = self.ring.pull(&mut staging[..samples]);
                pack24(staging, got * self.channels);
                got
            } else {
                let staging = self.sink.staging();
                // SAFETY: f32 memory is aligned for i16 and holds twice as many; the slice lives within the borrow.
                let halves = unsafe { std::slice::from_raw_parts_mut(staging.as_mut_ptr() as *mut i16, staging.len() * 2) };
                self.ring.pull_i16(&mut halves[..samples])
            };
            if self.ring.flushed() {
                // This pull started the new music: flush the old from the track.
                self.restart(now_ns);
                self.clock.update(|c| c.ahead = got as u64);
            } else if got < n {
                self.clock.update(|c| c.ahead -= (n - got) as u64);
            }
            if got == 0 {
                return false;
            }
            self.staged = (0, got * fb);
            if !self.write_staged(now_ns) {
                return true;
            }
        }
    }

    /// Writes what is staged. False when the track is full or died.
    fn write_staged(&mut self, now_ns: i64) -> bool {
        let (from, len) = self.staged;
        let taken = match self.sink.write(from, len) {
            Ok(n) => n.min(len),
            Err(code) => {
                self.reopen(now_ns, code);
                return false;
            }
        };
        let fb = self.frame_bytes();
        self.clock.update(|c| c.given += (taken / fb) as u64);
        self.bytes.fetch_add(taken as u64, Ordering::Relaxed);
        self.staged = if taken < len { (from + taken, len - taken) } else { (0, 0) };
        if taken < len {
            self.refused(now_ns);
        }
        taken == len
    }

    /// The track refused data it claimed room for: it holds less than it says. Its current fill becomes
    /// its capacity (small differences are clock error and ignored), floored at the start threshold so
    /// it can still start.
    ///
    /// Skipped after a flush of a full track until it is heard playing: the platform still counts the
    /// flushed frames until the mixer's next period. Taking that as capacity once shrank a track below
    /// its start threshold, and it never played again (fast skipping on a Galaxy S22).
    fn refused(&mut self, now_ns: i64) {
        if self.flushed_full {
            return;
        }
        let least = self.frames(START_US).min(self.capacity);
        let holds = self.clock.in_track(now_ns).max(least);
        if holds + self.capacity / 8 < self.capacity {
            log(&format!("the AudioTrack took no more at {} ms of the {} ms it said it holds: counted as {} ms", holds * 1000 / self.rate as u64, self.capacity * 1000 / self.rate as u64, holds * 1000 / self.rate as u64));
            self.holds(holds);
        }
    }

    /// A write failed with `code`: the track is dead. Opens a new one and refills it from the ring (what
    /// the old one held is lost); if that fails, tells the engine.
    fn reopen(&mut self, now_ns: i64, code: i32) {
        log(&format!("the AudioTrack failed a write ({code}): opening another"));
        self.sink.release();
        let opened = open_fitting(&mut **self.opener.lock(), self.format, self.float, self.asked);
        match opened {
            Ok(o) => {
                self.sink = o.sink;
                log_if_smaller(o.frames, self.asked, self.rate);
                self.allocated = o.frames;
                self.holds(o.frames);
                // Opened deep: shallow again if wanted, underruns counted afresh.
                self.shallow = false;
                self.needs.underruns = None;
                if self.wants_shallow {
                    self.resize();
                }
                self.starts_full = o.starts_full;
                self.flushed_full = false;
                self.revived = true;
                self.sink.set_volume(self.volume);
                self.refill_from_empty(now_ns);
            }
            Err(e) => {
                log(&format!("the AudioTrack would not open again: {e}"));
                *self.failure.lock() = Some(e);
                self.dead = true;
                self.ring.wake_engine();
            }
        }
    }
}

/// What the writer needs to reopen a dead track.
pub(crate) struct Reopen {
    pub opener: Arc<Mutex<Box<dyn Opener>>>,
    pub frames: u64,
    pub failure: Arc<Mutex<Option<String>>>,
}

/// State shared by the output (engine thread), the writer thread and the natives.
#[derive(Default)]
pub(crate) struct Shared {
    control: Mutex<Control>,
    writer: Mutex<Option<Thread>>,
    pub clock: Arc<Clock>,
    /// Bytes written since creation, for the test bridge.
    pub bytes: Arc<AtomicU64>,
    /// Called on route changes.
    pub watch: Mutex<Option<DeviceWatch>>,
    failure: Arc<Mutex<Option<String>>>,
    depth: Arc<Depth>,
}

impl Shared {
    fn tell(&self, f: impl FnOnce(&mut Control)) {
        f(&mut self.control.lock());
        if let Some(t) = &*self.writer.lock() {
            t.unpark();
        }
    }
}

/// The engine's output over an AudioTrack.
pub(crate) struct TrackOutput {
    opener: Arc<Mutex<Box<dyn Opener>>>,
    float: bool,
    shared: Arc<Shared>,
    format: Option<OutputFormat>,
    thread: Option<JoinHandle<()>>,
}

impl TrackOutput {
    /// `float`: the high quality setting (float samples, else 16-bit).
    pub(crate) fn new(opener: Box<dyn Opener>, float: bool, shared: Arc<Shared>) -> TrackOutput {
        TrackOutput { opener: Arc::new(Mutex::new(opener)), float, shared, format: None, thread: None }
    }
}

impl AudioOutput for TrackOutput {
    fn watch(&mut self, changed: DeviceWatch) {
        *self.shared.watch.lock() = Some(changed);
    }

    /// The song's own rate (bit-perfect for DACs), mono or stereo.
    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
        // Above 192 kHz: halved within its family (352.8 to 176.4 kHz), the ring resampling.
        let f = OutputFormat { rate: nori_player::policy::capped_rate(want.rate, 192_000).clamp(8_000, 192_000), channels: want.channels.clamp(1, 2), bits: want.bits };
        self.format = Some(f);
        Ok(f)
    }

    fn start(&mut self, feed: Feed) -> Result<(), String> {
        let format = self.format.ok_or("the output was not opened")?;
        self.close();
        // Called on the engine thread, which decodes for the track: audio priority too.
        audio_priority("the engine");
        // Always opened deep; the writer makes it shallow in place before writing if needed.
        let shallow = self.shared.control.lock().shallow;
        let frames = track_frames(format.rate, false);
        let opened = open_fitting(&mut **self.opener.lock(), format, self.float, frames).inspect_err(|e| log(&format!("the AudioTrack would not open: {e}")))?;
        *self.shared.control.lock() = Control { shallow, ..Control::default() };
        *self.shared.failure.lock() = None;
        let reopen = Reopen { opener: self.opener.clone(), frames, failure: self.shared.failure.clone() };
        let writer = Writer::new(feed, opened, reopen, format, self.float, self.shared.clock.clone(), self.shared.bytes.clone(), self.shared.depth.clone());
        let shared = self.shared.clone();
        let t = std::thread::Builder::new().name("nori-track".into()).spawn(move || run(writer, shared)).map_err(|e| e.to_string())?;
        *self.shared.writer.lock() = Some(t.thread().clone());
        self.thread = Some(t);
        Ok(())
    }

    fn pause(&mut self) {
        self.shared.clock.freeze(mono_ns());
        self.shared.tell(|c| c.playing = false);
    }

    fn resume(&mut self) {
        self.shared.tell(|c| c.playing = true);
    }

    fn latency_us(&self) -> u64 {
        self.shared.clock.latency_us(mono_ns())
    }

    fn takes_float(&mut self) -> bool {
        true
    }

    /// Applies to the next track opened.
    fn float(&mut self, on: bool) {
        self.float = on;
    }

    fn flush(&mut self) {
        self.shared.tell(|_| {});
    }

    fn ramp(&mut self, from: Option<f32>, target: f32, ms: i64) -> bool {
        self.shared.tell(|c| c.ramp = Some((from, target, ms)));
        true
    }

    /// Resized in place ([`Sink::resize`]).
    fn shallow(&mut self, on: bool) {
        self.shared.tell(|c| c.shallow = on);
    }

    fn resizes(&self) -> bool {
        true
    }

    fn shallow_depth(&self) -> Option<ShallowDepth> {
        self.shared.depth.get()
    }

    /// Unplayed music is still in the track.
    fn holding(&self) -> bool {
        self.shared.clock.latency_frames(mono_ns()) > 0
    }

    fn bursts(&self) -> bool {
        true
    }

    fn failed(&mut self) -> Option<String> {
        self.shared.failure.lock().take()
    }

    fn close(&mut self) {
        if let Some(t) = self.thread.take() {
            self.shared.tell(|c| c.stop = true);
            let _ = t.join();
            *self.shared.writer.lock() = None;
        }
    }
}

impl Drop for TrackOutput {
    fn drop(&mut self) {
        self.close();
    }
}

fn log(message: &str) {
    nori_core::alog::info(&format!("rust track: {message}"));
}

/// Whether the track takes packed 24-bit samples: a >16-bit song, bit-perfect, without the float setting.
pub(crate) fn packed24(format: OutputFormat, float: bool) -> bool {
    !float && format.bits > 16
}

pub(crate) fn sample_bytes(float: bool, packed: bool) -> usize {
    if float {
        4
    } else if packed {
        3
    } else {
        2
    }
}

/// Converts the first `samples` floats of `staging` in place to packed little-endian 24-bit (exact for
/// 24-bit sources). Safe in place because output bytes trail input bytes.
fn pack24(staging: &mut [f32], samples: usize) {
    let samples = samples.min(staging.len());
    let floats = staging.as_mut_ptr();
    let bytes = floats as *mut u8;
    for k in 0..samples {
        // SAFETY: k is inside the staging memory; float k is read before its bytes (and any after them)
        // are written over, since sample k's bytes go to 3k..3k + 3, at or before 4k.
        let v = unsafe { floats.add(k).read() };
        let b = ((v * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32).to_le_bytes();
        // SAFETY: 3k + 3 <= 4k + 4, inside the staging memory.
        unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), bytes.add(k * 3), 3) };
    }
}

/// [`LOW_US`], or half of a smaller track.
fn low_mark(frames: u64, rate: u32) -> u64 {
    ((rate as i64 * LOW_US / 1_000_000) as u64).min(frames / 2)
}

/// Logs a track opened smaller than asked (it will wake more often).
fn log_if_smaller(frames: u64, asked: u64, rate: u32) {
    if frames < asked {
        let ms = |f: u64| f * 1000 / rate.max(1) as u64;
        log(&format!("the AudioTrack holds {} ms of the {} ms asked: topped up every {} ms or so", ms(frames), ms(asked), ms(frames - low_mark(frames, rate)).max(FILL_TICK_MS)));
    }
}

/// Sets the calling thread to `THREAD_PRIORITY_AUDIO` (a nice value, as `Process.setThreadPriority`).
/// No-op off Linux/Android.
#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn audio_priority(_who: &str) {}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn audio_priority(who: &str) {
    const THREAD_PRIORITY_AUDIO: libc::c_int = -16;
    // SAFETY: plain system calls on the calling thread.
    let set = unsafe { libc::setpriority(libc::PRIO_PROCESS as _, libc::gettid() as _, THREAD_PRIORITY_AUDIO) };
    if set != 0 {
        log(&format!("{who}'s thread kept its priority: {}", std::io::Error::last_os_error()));
    }
}

fn run<R: Ring>(mut w: Writer<R>, shared: Arc<Shared>) {
    audio_priority("the writer");
    loop {
        let mut c = {
            let mut g = shared.control.lock();
            Control { playing: g.playing, ramp: g.ramp.take(), stop: g.stop, shallow: g.shallow }
        };
        if c.stop {
            log("released");
            w.sink.release();
            return;
        }
        match w.step(mono_ns(), &mut c) {
            Some(ms) => std::thread::park_timeout(Duration::from_millis(ms)),
            None => std::thread::park(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_player::burst::LOW_US as RING_LOW_US;
    use std::collections::VecDeque;

    const RATE: u32 = 48_000;
    const MS: i64 = 1_000_000;

    /// An AudioTrack played on a virtual clock: what was written plays at the rate while it is started,
    /// once it is full when it starts only full.
    #[derive(Default)]
    struct Track {
        /// The buffer it was opened with, and the part of it that may be filled (`setBufferSizeInFrames`).
        capacity: u64,
        size: u64,
        starts_full: bool,
        buffered: u64,
        played: u64,
        started: bool,
        ready: bool,
        stopping: bool,
        volume: f32,
        flushes: u32,
        stops: u32,
        underruns: u32,
        written_bytes: u64,
        /// The value of the last sample written, to tell the music before a flush from the music after.
        last: f32,
        /// The sound server died under it: every write is refused with ERROR_DEAD_OBJECT.
        dead: bool,
        /// Tracks opened in place of a dead one, and whether another may be.
        reopened: u32,
        refuse_open: bool,
        /// Every frame written and not yet played, as the ring numbered it, while `record` is on; frames
        /// played out of their order, and the longest silence heard once music had started.
        record: bool,
        queued: VecDeque<f32>,
        last_played: Option<f32>,
        jumps: u32,
        heard_any: bool,
        silent_run: u64,
        longest_silence: u64,
        resizes: u32,
        /// The output past the track: what the device presents lags what it took by this many frames.
        latency: u64,
        /// What it says of itself.
        route: Route,
        /// The start threshold (Android 12 on, `setStartThresholdInFrames`): after a flush, and when new,
        /// it plays nothing until it holds this much (or its whole size, if that is less).
        threshold: u64,
        filling_up: bool,
        /// As AudioFlinger does it: a track paused while playing is let go only by the mixer's next period,
        /// and a flush made before then is done there. Until then the frames the flush dropped still count
        /// against the room a write finds (the client's count of what the server holds moves only when the
        /// server has looked), though none of them is played.
        defers: bool,
        pausing: bool,
        stale: u64,
    }

    struct FakeSink {
        track: Arc<Mutex<Track>>,
        staging: Vec<f32>,
        float: bool,
        now: Arc<AtomicU64>,
    }

    impl Sink for FakeSink {
        fn staging(&mut self) -> &mut [f32] {
            &mut self.staging
        }
        fn write(&mut self, from: usize, len: usize) -> Result<usize, i32> {
            let mut t = self.track.lock();
            if t.dead {
                return Err(-6);
            }
            let fb = if self.float { 8 } else { 4 };
            let frames = (t.size.saturating_sub(t.buffered + t.stale) as usize).min(len / fb);
            if t.record && self.float {
                let first = from / 4;
                for k in 0..frames {
                    let v = self.staging[first + k * 2];
                    t.queued.push_back(v);
                }
            }
            if frames > 0 {
                // SAFETY: the staging memory is f32s, aligned for i16, and `from` lies inside it.
                let sample: i16 = unsafe { *(self.staging.as_ptr() as *const i16).add(from / 2) };
                t.last = if self.float { self.staging[from / 4] } else { f32::from(sample) / 32767.0 };
            }
            t.buffered += frames as u64;
            t.written_bytes += (frames * fb) as u64;
            Ok(frames * fb)
        }
        fn play(&mut self) {
            let mut t = self.track.lock();
            t.started = true;
            t.stopping = false;
            t.ready = t.buffered >= t.need();
            if t.ready {
                t.filling_up = false;
            }
        }
        fn pause(&mut self) {
            let mut t = self.track.lock();
            t.pausing = t.defers && t.started;
            t.started = false;
            t.ready = false;
            t.heard_any = false;
            t.silent_run = 0;
        }
        fn flush(&mut self) {
            let mut t = self.track.lock();
            assert!(!t.started, "a track is only flushed paused");
            if t.pausing {
                t.stale = t.buffered;
            }
            t.filling_up = true;
            t.buffered = 0;
            t.played = 0;
            t.queued.clear();
            t.last_played = None;
            t.flushes += 1;
        }
        fn stop(&mut self) {
            let mut t = self.track.lock();
            t.stopping = true;
            t.stops += 1;
        }
        fn set_volume(&mut self, volume: f32) {
            self.track.lock().volume = volume;
        }
        fn heard(&mut self, _playing: bool) -> Option<(u64, i64)> {
            let t = self.track.lock();
            Some((t.played.saturating_sub(t.latency), self.now.load(Ordering::Relaxed) as i64))
        }
        fn route(&mut self) -> Route {
            self.track.lock().route
        }
        fn underruns(&mut self) -> Option<u64> {
            Some(self.track.lock().underruns as u64)
        }
        fn consumed(&mut self) -> Option<u64> {
            Some(self.track.lock().played)
        }
        /// As the platform does it: at least 16 frames, at most the buffer opened.
        fn resize(&mut self, frames: u64) -> u64 {
            let mut t = self.track.lock();
            t.size = frames.clamp(16, t.capacity);
            t.resizes += 1;
            t.size
        }
        fn release(&mut self) {}
    }

    impl Track {
        /// What it must hold before it plays: all of it when it starts only full, its start threshold after
        /// a flush, nothing more once it has started.
        fn need(&self) -> u64 {
            if self.starts_full {
                self.size
            } else if self.filling_up {
                self.threshold.min(self.size)
            } else {
                0
            }
        }

        /// The mixer's period came round: a pause asked for is done, and a flush waiting for it.
        fn mix(&mut self) {
            self.pausing = false;
            self.stale = 0;
        }

        fn advance(&mut self, frames: u64) {
            // A flush the mixer has not done yet: it has not looked at the track since, nor played from it.
            if !self.started || self.stale > 0 {
                return;
            }
            if !self.ready && (self.buffered >= self.need() || self.stopping) {
                self.ready = true;
                self.filling_up = false;
            }
            if !self.ready {
                self.silence(frames);
                return;
            }
            let n = frames.min(self.buffered);
            if n < frames && !self.stopping {
                self.underruns += 1;
            }
            self.buffered -= n;
            self.played += n;
            for _ in 0..n.min(self.queued.len() as u64) {
                let v = self.queued.pop_front();
                if let (Some(was), Some(v)) = (self.last_played, v) {
                    if v != was + 1.0 {
                        self.jumps += 1;
                    }
                }
                self.last_played = v;
            }
            if n > 0 {
                self.heard_any = true;
                self.silent_run = 0;
            }
            if !self.stopping {
                self.silence(frames - n);
            }
        }

        /// `frames` of silence where music was due, once music had started.
        fn silence(&mut self, frames: u64) {
            if self.heard_any && frames > 0 {
                self.silent_run += frames;
                self.longest_silence = self.longest_silence.max(self.silent_run);
            }
        }
    }

    /// A small generator of numbers that look random, the same every run.
    struct Dice(u64);

    impl Dice {
        /// A number from 0 to `max`.
        fn roll(&mut self, max: i64) -> i64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            if max <= 0 {
                0
            } else {
                (self.0 % (max as u64 + 1)) as i64
            }
        }
    }

    /// How the simulated engine keeps the ring filled when it is not in bursts.
    #[derive(Clone, Copy)]
    enum Engine {
        /// A whole burst decoded the moment a pull runs the ring down to its low mark, as the engine does
        /// while music plays.
        Bursts,
        /// The ring topped up to `cap` frames, woken by the pull that runs it down to half of that, and
        /// up to `late` ns after it: the engine while the equalizer is tuned.
        Shallow { cap: usize, late: i64 },
        /// The ring topped up to `cap` frames every `every` ns, whatever the pulls do: the engine before
        /// it knew its ring was shallow, on its 200 ms timer.
        Timer { cap: usize, every: i64 },
        /// Nothing decoded however low the ring runs: the engine kept from its work (bytes late from the
        /// network, its thread off the CPU).
        Stalled,
    }

    /// The engine's ring, simulated: filled as `engine` says, until `left` frames of music have been
    /// decoded.
    struct FakeRing {
        available: usize,
        left: u64,
        low: usize,
        burst: usize,
        refills: u32,
        flushed: bool,
        value: f32,
        engine_woken: u32,
        engine: Engine,
        /// When the engine fills the ring next, if it is due to.
        due: Option<i64>,
        now: Arc<AtomicU64>,
        dice: Dice,
        /// Each frame pulled carries its number (from 1, counted since the last flush) instead of `value`:
        /// for a float track that records what it plays.
        counting: bool,
        pulled: u64,
    }

    impl FakeRing {
        fn new(music_s: u64, now: Arc<AtomicU64>) -> FakeRing {
            let low = (RATE as i64 * (RING_LOW_US - 250_000) / 1_000_000) as usize;
            // A burst is ten seconds counted from the ear, which moves on while it is decoded: a little more.
            let burst = (RATE as i64 * (BUFFER_US + 200_000) / 1_000_000) as usize;
            let mut r = FakeRing { available: 0, left: music_s * RATE as u64, low, burst, refills: 0, flushed: false, value: 0.5, engine_woken: 0, engine: Engine::Bursts, due: None, now, dice: Dice(7), counting: false, pulled: 0 };
            r.refill();
            r
        }

        /// From now on the engine fills it as `engine` says, starting now.
        fn kept(&mut self, engine: Engine) {
            self.engine = engine;
            match engine {
                Engine::Bursts => {
                    self.low = (RATE as i64 * (RING_LOW_US - 250_000) / 1_000_000) as usize;
                    self.due = None;
                }
                Engine::Shallow { cap, .. } => {
                    self.low = cap / 2;
                    self.due = None;
                }
                Engine::Timer { every, .. } => self.due = Some(self.now.load(Ordering::Relaxed) as i64 + every),
                Engine::Stalled => self.due = None,
            }
        }

        fn refill(&mut self) {
            let want = match self.engine {
                Engine::Bursts | Engine::Stalled => self.burst,
                Engine::Shallow { cap, .. } | Engine::Timer { cap, .. } => cap.saturating_sub(self.available),
            };
            let n = (want as u64).min(self.left) as usize;
            self.left -= n as u64;
            self.available += n;
            self.refills += 1;
        }

        /// The engine's turn, when it was due.
        fn turn(&mut self, now: i64) {
            self.due = None;
            self.refill();
            if let Engine::Timer { every, .. } = self.engine {
                self.due = Some(now + every);
            }
        }

        fn take(&mut self, frames: usize) -> usize {
            let n = frames.min(self.available);
            self.available -= n;
            if self.available <= self.low && self.left > 0 {
                match self.engine {
                    Engine::Bursts => self.refill(),
                    Engine::Shallow { late, .. } if self.due.is_none() => {
                        let now = self.now.load(Ordering::Relaxed) as i64;
                        self.due = Some(now + self.dice.roll(late));
                    }
                    _ => {}
                }
            }
            n
        }

        /// A seek: what the ring held goes, and a burst of other music comes.
        fn flush(&mut self, value: f32) {
            self.available = 0;
            self.pulled = 0;
            self.flushed = true;
            self.value = value;
            self.refill();
        }
    }

    impl Ring for Arc<Mutex<FakeRing>> {
        fn available(&self) -> usize {
            self.lock().available
        }
        fn pull(&mut self, out: &mut [f32]) -> usize {
            let mut r = self.lock();
            let n = r.take(out.len() / 2);
            if r.counting {
                for f in out[..n * 2].as_chunks_mut::<2>().0 {
                    r.pulled += 1;
                    *f = [r.pulled as f32; 2];
                }
            } else {
                out[..n * 2].fill(r.value);
            }
            n
        }
        fn pull_i16(&mut self, out: &mut [i16]) -> usize {
            let mut r = self.lock();
            let n = r.take(out.len() / 2);
            out[..n * 2].fill((r.value * 32767.0) as i16);
            n
        }
        fn flushed(&mut self) -> bool {
            std::mem::take(&mut self.lock().flushed)
        }
        fn ending(&self) -> bool {
            let r = self.lock();
            r.left == 0
        }
        fn wake_engine(&self) {
            self.lock().engine_woken += 1;
        }
    }

    /// The sound server's mixer, reading the track `period` ns at a time, each read up to `late` ns
    /// after it was due: the jittery consumer a busy phone is.
    struct Mixer {
        period: i64,
        late: i64,
        due: i64,
        next: i64,
        dice: Dice,
        /// A Bluetooth output: every `every` ns or so it takes nothing for `stall` ns (a range), then all
        /// it missed at once.
        bursts: Option<Bursts>,
    }

    #[derive(Clone, Copy)]
    struct Bursts {
        every: (i64, i64),
        stall: (i64, i64),
        /// Taking nothing until then, and ns of music owed since.
        until: Option<i64>,
        owed: i64,
        next: i64,
    }

    impl Mixer {
        /// The ns of music the mixer takes from the track at its read at `now`.
        fn take(&mut self, now: i64) -> i64 {
            let Some(b) = self.bursts.as_mut() else { return self.period };
            if b.until.is_some_and(|u| now < u) {
                b.owed += self.period;
                return 0;
            }
            let took = self.period + std::mem::take(&mut b.owed);
            b.until = None;
            if now >= b.next {
                let stall = b.stall.0 + self.dice.roll(b.stall.1 - b.stall.0);
                b.until = Some(now + stall);
                b.next = now + stall + b.every.0 + self.dice.roll(b.every.1 - b.every.0);
            }
            took
        }
    }

    struct Sim {
        writer: Writer<Arc<Mutex<FakeRing>>>,
        ring: Arc<Mutex<FakeRing>>,
        track: Arc<Mutex<Track>>,
        clock: Arc<Clock>,
        now: Arc<AtomicU64>,
        control: Control,
        wakes: u32,
        next: Option<i64>,
        failure: Arc<Mutex<Option<String>>>,
        /// The writer's thread is woken up to this many ns after it asked to be (a busy phone).
        late: i64,
        dice: Dice,
        mixer: Option<Mixer>,
        /// The most music between the ring's writing end and the ear, as the simulation went, frames.
        deepest: u64,
        /// What the writer found the shallow track needs, as the engine reads it.
        depth: Arc<Depth>,
        /// The platform's mixer period, ns, and when it next comes round ([`Track::mix`]); none: a pause and a
        /// flush are done at once.
        period: Option<i64>,
        mix_at: i64,
    }

    /// Opens the simulated track again, empty, as a new AudioTrack in place of a dead one.
    struct FakeOpener {
        track: Arc<Mutex<Track>>,
        float: bool,
        now: Arc<AtomicU64>,
    }

    impl Opener for FakeOpener {
        fn open(&mut self, _format: OutputFormat, float: bool, frames: u64) -> Result<Opened, String> {
            assert_eq!(float, self.float);
            let mut t = self.track.lock();
            if t.refuse_open {
                return Err("no sound server".into());
            }
            let starts_full = t.starts_full;
            *t = Track { capacity: frames, size: frames, starts_full, volume: 1.0, reopened: t.reopened + 1, underruns: t.underruns, record: t.record, latency: t.latency, route: t.route, threshold: t.threshold, defers: t.defers, filling_up: true, ..Track::default() };
            let sink = FakeSink { track: self.track.clone(), staging: vec![0.0; CHUNK_BYTES / 4], float, now: self.now.clone() };
            Ok(Opened { sink: Box::new(sink), frames, starts_full })
        }
    }

    /// A sound server with memory for tracks of at most `most` frames: anything bigger is refused.
    struct Cramped(FakeOpener, u64, Vec<u64>);

    impl Opener for Cramped {
        fn open(&mut self, format: OutputFormat, float: bool, frames: u64) -> Result<Opened, String> {
            self.2.push(frames);
            if frames > self.1 {
                return Err("not enough memory".into());
            }
            self.0.open(format, float, frames)
        }
    }

    #[test]
    fn open_fitting_halves_until_it_fits() {
        let track = Arc::new(Mutex::new(Track::default()));
        let fake = FakeOpener { track: track.clone(), float: true, now: Arc::new(AtomicU64::new(0)) };
        let format = OutputFormat { rate: 96_000, channels: 2, bits: 0 };
        let asked = track_frames(96_000, false);
        let mut o = Cramped(fake, asked / 3, Vec::new());
        let opened = open_fitting(&mut o, format, true, asked).expect("opened smaller");
        assert_eq!(o.2, vec![asked, asked / 2, asked / 4]);
        assert_eq!(opened.frames, asked / 4);
        // Not below one second.
        let fake = FakeOpener { track, float: true, now: Arc::new(AtomicU64::new(0)) };
        let mut o = Cramped(fake, 1_000, Vec::new());
        assert!(open_fitting(&mut o, format, true, asked).is_err());
        assert!(o.2.iter().all(|f| *f >= 96_000), "{:?}", o.2);
    }

    impl Sim {
        fn new(music_s: u64, float: bool, starts_full: bool) -> Sim {
            Sim::granted(music_s, float, starts_full, TRACK_US, TRACK_US)
        }

        /// A track that says it holds `said_us` of music and holds `holds_us`, whatever was asked of it.
        fn granted(music_s: u64, float: bool, starts_full: bool, said_us: i64, holds_us: i64) -> Sim {
            Sim::asked(music_s, float, starts_full, said_us, holds_us, TRACK_US)
        }

        /// [`Sim::granted`], the track having been asked for `asked_us`.
        fn asked(music_s: u64, float: bool, starts_full: bool, said_us: i64, holds_us: i64, asked_us: i64) -> Sim {
            let now = Arc::new(AtomicU64::new(1_000 * MS as u64));
            let ring = Arc::new(Mutex::new(FakeRing::new(music_s, now.clone())));
            let capacity = (RATE as i64 * said_us / 1_000_000) as u64;
            let holds = (RATE as i64 * holds_us / 1_000_000) as u64;
            let track = Arc::new(Mutex::new(Track { capacity: holds, size: holds, starts_full, volume: 1.0, filling_up: true, ..Track::default() }));
            let sink = FakeSink { track: track.clone(), staging: vec![0.0; CHUNK_BYTES / 4], float, now: now.clone() };
            let clock = Arc::new(Clock::default());
            let format = OutputFormat { rate: RATE, channels: 2, bits: 0 };
            let opened = Opened { sink: Box::new(sink), frames: capacity, starts_full };
            let opener = FakeOpener { track: track.clone(), float, now: now.clone() };
            let failure = Arc::new(Mutex::new(None));
            let asked = (RATE as i64 * asked_us / 1_000_000) as u64;
            let reopen = Reopen { opener: Arc::new(Mutex::new(Box::new(opener))), frames: asked, failure: failure.clone() };
            let depth = Arc::new(Depth::default());
            let writer = Writer::new(ring.clone(), opened, reopen, format, float, clock.clone(), Arc::new(AtomicU64::new(0)), depth.clone());
            let control = Control { shallow: asked < track_frames(RATE, false), ..Control::default() };
            Sim { writer, ring, track, clock, now, control, wakes: 0, next: Some(0), failure, late: 0, dice: Dice(11), mixer: None, deepest: 0, depth, period: None, mix_at: 0 }
        }

        fn now(&self) -> i64 {
            self.now.load(Ordering::Relaxed) as i64
        }

        /// The track is read by a mixer `period_ms` at a time, each read up to `late_ms` late.
        fn mixed(&mut self, period_ms: i64, late_ms: i64) {
            let at = self.now() + period_ms * MS;
            self.mixer = Some(Mixer { period: period_ms * MS, late: late_ms * MS, due: at, next: at, dice: Dice(5), bursts: None });
        }

        /// A Bluetooth output: read 20 ms at a time, but every 300 to 600 ms it takes nothing for 100 to
        /// 200 ms and then all of that at once; what it presents lags what it took by `latency_ms`; and
        /// it says so of itself, or nothing (`says`).
        fn bluetooth(&mut self, latency_ms: i64, says: bool) {
            self.mixed(20, 4);
            let now = self.now();
            if let Some(m) = self.mixer.as_mut() {
                m.bursts = Some(Bursts { every: (300 * MS, 600 * MS), stall: (100 * MS, 200 * MS), until: None, owed: 0, next: now + 300 * MS });
            }
            let mut t = self.track.lock();
            t.latency = (RATE as i64 * latency_ms / 1000) as u64;
            t.route = if says {
                // What getMinBufferSize gives for such an output: about its latency.
                Route { min_frames: Some(t.latency), latency_frames: Some(t.latency), name: Some("Bluetooth") }
            } else {
                Route::default()
            };
        }

        /// The engine keeps its shallow ring as deep as the writer found it needs, as nori-engine does
        /// (`Worker::follow_depth`).
        fn follow_depth(&mut self) {
            let Some(d) = self.depth.get() else { return };
            let cap = (RATE as i64 * d.ring_us.max(nori_engine::output::SHALLOW_US) / 1_000_000) as usize;
            let mut r = self.ring.lock();
            if let Engine::Shallow { cap: was, late } = r.engine {
                if was != cap {
                    r.kept(Engine::Shallow { cap, late });
                }
            }
        }

        /// The writer wakes now.
        fn wake(&mut self) {
            self.wakes += 1;
            let now = self.now();
            let late = self.dice.roll(self.late);
            self.next = self.writer.step(now, &mut self.control).map(|ms| now + ms as i64 * MS + late);
        }

        /// Runs the virtual clock for `ms`, the writer waking whenever it asked to, the engine filling the
        /// ring when it is due to, and the mixer reading the track.
        fn run(&mut self, ms: i64) {
            let end = self.now() + ms * MS;
            loop {
                let due = self.ring.lock().due;
                let mix = self.mixer.as_ref().map(|m| m.next);
                let period = self.period.map(|_| self.mix_at);
                let to = [self.next, due, mix, period, Some(end)].into_iter().flatten().min().expect("the end at least");
                let step = (to - self.now()).max(0);
                if self.mixer.is_none() {
                    self.track.lock().advance((step as u128 * RATE as u128 / 1_000_000_000) as u64);
                }
                self.now.store(to as u64, Ordering::Relaxed);
                if let Some(m) = self.mixer.as_mut().filter(|m| m.next == to) {
                    let took = m.take(to);
                    if took > 0 {
                        self.track.lock().advance((took as u128 * RATE as u128 / 1_000_000_000) as u64);
                    }
                    m.due += m.period;
                    m.next = m.due + m.dice.roll(m.late);
                }
                if let Some(p) = self.period.filter(|_| self.mix_at == to) {
                    self.track.lock().mix();
                    self.mix_at = to + p;
                }
                if due == Some(to) {
                    self.ring.lock().turn(to);
                }
                if self.next.is_some_and(|n| n <= to) {
                    self.wake();
                }
                let held = self.ring.lock().available as u64 + self.track.lock().buffered;
                self.deepest = self.deepest.max(held);
                if to >= end {
                    break;
                }
            }
        }

        fn play(&mut self) {
            self.control.playing = true;
            self.wake();
        }
    }

    #[test]
    fn playing_wakes_once_per_burst() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(5_000);
        let (wakes, refills) = (s.wakes, s.ring.lock().refills);
        s.run(120_000);
        let t = s.track.lock();
        assert_eq!(t.underruns, 0, "never runs dry");
        let wakes = s.wakes - wakes;
        let refills = s.ring.lock().refills - refills;
        assert!((11..=14).contains(&wakes), "the writer woke {wakes} times in two minutes");
        assert!((11..=14).contains(&refills), "the engine decoded {refills} bursts in two minutes");
        assert_eq!(refills, wakes, "one burst decoded for every top-up");
    }

    #[test]
    fn stalled_engine_is_logged() {
        let said = |what: &str| nori_core::alog::recent().iter().any(|(_, l)| l.starts_with("rust track: ") && l.contains(what));
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(30_000);
        assert_eq!(s.track.lock().underruns, 0);
        s.ring.lock().kept(Engine::Stalled);
        s.run(20_000);
        assert!(s.track.lock().underruns > 0, "the stalled engine let it run dry");
        assert!(said("with nothing more to give it: the engine is late"), "said as it ran low");
        assert!(said("the AudioTrack ran dry"), "the gap said at the next top-up");
        let mut r = s.ring.lock();
        r.kept(Engine::Bursts);
        r.refill();
        drop(r);
        s.run(20_000);
        assert!(said("full again"), "and when it is over");
    }

    #[test]
    fn small_track_tops_up_once_per_half() {
        // 300 ms granted of 11.5 s asked. Regression: timing by pulled frames woke every fill tick.
        let mut s = Sim::granted(600, false, false, 300_000, 300_000);
        s.play();
        s.run(5_000);
        let wakes = s.wakes;
        s.run(120_000);
        let wakes = s.wakes - wakes;
        assert_eq!(s.track.lock().underruns, 0, "never runs dry");
        assert!((700..=900).contains(&wakes), "once per 150 ms, what the buffer demands: {wakes} wakes in two minutes");
    }

    #[test]
    fn track_holding_less_than_claimed_is_resized() {
        // Claims 11.5 s, takes 0.5 s.
        let mut s = Sim::granted(600, true, false, TRACK_US, 500_000);
        s.play();
        s.run(5_000);
        let wakes = s.wakes;
        s.run(120_000);
        let wakes = s.wakes - wakes;
        assert_eq!(s.track.lock().underruns, 0, "never runs dry");
        assert!(wakes <= 600, "about once per quarter of a second once its size is known: {wakes} wakes in two minutes");
    }

    #[test]
    fn pack24_is_exact() {
        let values = [0i32, 1, -1, 8_388_607, -8_388_608, 123_456, -654_321, 42];
        let mut staging: Vec<f32> = values.iter().map(|&v| v as f32 / 8_388_608.0).collect();
        staging.resize(16, 0.0);
        pack24(&mut staging, values.len());
        // SAFETY: the floats' memory, read as bytes, inside its length.
        let bytes = unsafe { std::slice::from_raw_parts(staging.as_ptr() as *const u8, values.len() * 3) };
        for (k, &v) in values.iter().enumerate() {
            let b = &bytes[k * 3..k * 3 + 3];
            assert_eq!(i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8, v, "sample {k}");
        }
        assert_eq!(sample_bytes(false, packed24(OutputFormat { rate: 96_000, channels: 2, bits: 24 }, false)), 3);
        assert_eq!(sample_bytes(true, packed24(OutputFormat { rate: 96_000, channels: 2, bits: 24 }, true)), 4, "float asked for: float");
        assert!(!packed24(OutputFormat { rate: 44_100, channels: 2, bits: 16 }, false));
    }

    #[test]
    fn head_count_unwraps() {
        let mut h = HeadCount::default();
        assert_eq!(h.read(10), 10);
        assert_eq!(h.read(u32::MAX - 5), u32::MAX as u64 - 5);
        assert_eq!(h.read(20), (1u64 << 32) + 20, "the wrap is counted, not read as the start again");
        assert_eq!(h.read(30), (1u64 << 32) + 30);
    }

    #[test]
    fn dead_track_is_reopened() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(15_000);
        let wakes = s.wakes;
        s.track.lock().dead = true;
        s.run(15_000);
        let t = s.track.lock();
        assert_eq!(t.reopened, 1, "one track opened in place of the dead one");
        assert!(t.buffered > 0 && t.started, "the new one is filled and playing: {} buffered, started {}", t.buffered, t.started);
        assert!(s.wakes - wakes < 10, "no retrying every millisecond: {} wakes", s.wakes - wakes);
        assert!(s.failure.lock().is_none());
    }

    #[test]
    fn dead_track_that_wont_reopen_fails_the_output() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(15_000);
        {
            let mut t = s.track.lock();
            t.dead = true;
            t.refuse_open = true;
        }
        s.run(15_000);
        assert!(s.failure.lock().is_some(), "the failure is kept for the engine");
        assert_eq!(s.ring.lock().engine_woken, 1, "and the engine woken to hear it");
        assert_eq!(s.next, None, "the writer sleeps until it is let go");
    }

    #[test]
    fn start_fills_then_sleeps() {
        let mut s = Sim::new(600, false, true);
        s.play();
        s.run(1_500);
        let t = s.track.lock();
        assert_eq!(t.buffered + t.played, t.capacity, "filled to full, so a track that waits for that starts");
        assert!(t.ready && t.played > 0, "and it plays");
        drop(t);
        // One fill wake and two settle readings.
        assert!(s.wakes <= 4, "the first second and a half took {} wakes", s.wakes);
        let next = s.next.unwrap() - s.now();
        assert!(next > 8_000 * MS, "then it sleeps until the track is low: {} ms", next / MS);
    }

    #[test]
    fn latency_is_what_the_track_holds() {
        let mut s = Sim::new(600, true, false);
        s.play();
        s.run(3_333);
        let t = s.track.lock();
        let latency = s.clock.latency_frames(s.now());
        assert!(latency.abs_diff(t.buffered) <= RATE as u64 / 1000, "latency {latency} frames, {} in the track", t.buffered);
        assert_eq!(t.written_bytes, (t.buffered + t.played) * 8, "float stereo: eight bytes a frame");
    }

    #[test]
    fn flush_refills_with_new_music() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(4_000);
        s.ring.lock().flush(-0.25);
        s.wake();
        let t = s.track.lock();
        assert_eq!(t.flushes, 1);
        assert!(t.last < 0.0, "what the track holds now is the new music");
        assert_eq!(t.played, 0, "counted again from the flush");
        assert_eq!(s.clock.latency_frames(s.now()), t.buffered, "and the clock with it");
    }

    /// Regression (Galaxy S22): fast skips flush a full track whose flush the platform applies only at the
    /// next mixer period; the refused writes shrank the capacity below the start threshold and the track
    /// never started again.
    #[test]
    fn fast_skips_over_full_track_keep_playing() {
        for round in 0..40u64 {
            let mut s = Sim::new(600, false, false);
            let mut dice = Dice(1 + round * 7919);
            let period = (10 + dice.roll(30)) * MS;
            s.period = Some(period);
            s.mix_at = s.now() + period;
            {
                let mut t = s.track.lock();
                t.threshold = RATE as u64 / 4;
                t.defers = true;
            }
            s.play();
            s.run(3_000 + dice.roll(8_000));
            let presses = 10 + dice.roll(10);
            for k in 0..presses {
                // A cached song: its first burst is ready at once; the writer wakes up to 3 ms later.
                s.ring.lock().flush(if k % 2 == 0 { -0.25 } else { 0.25 });
                let late = dice.roll(3) * MS;
                s.next = Some(s.now() + late);
                s.run(2 + dice.roll(200));
            }
            s.run(2_000);
            let (played, capacity) = (s.track.lock().played, s.writer.capacity);
            s.run(6_000);
            let t = s.track.lock();
            assert!(
                t.played >= played + 5 * RATE as u64,
                "round {round}, {presses} presses: the music plays on after them ({} ms heard in six seconds; the writer counts the track as {} ms, {} buffered, playing {})",
                (t.played - played) * 1000 / RATE as u64,
                capacity * 1000 / RATE as u64,
                t.buffered * 1000 / RATE as u64,
                t.ready,
            );
            assert_eq!(capacity, t.capacity, "round {round}: the room a flush left for a moment is not the track's size");
        }
    }

    /// Regression: `TrackOutput::pause` freezes the clock at once; resumed before the writer woke, the
    /// clock stayed frozen and the track ran dry.
    #[test]
    fn pause_undone_before_wake_keeps_playing() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(3_000);
        // `TrackOutput::pause` then `resume`.
        s.clock.freeze(s.now());
        s.control.playing = false;
        s.control.playing = true;
        s.wake();
        let played = s.track.lock().played;
        s.run(30_000);
        let t = s.track.lock();
        assert_eq!(t.underruns, 0, "never runs dry");
        assert!(t.played >= played + 29 * RATE as u64, "thirty seconds heard: {} ms", (t.played - played) * 1000 / RATE as u64);
    }

    /// Pre-Android 12 track (starts when full): audible within 60 ms of a flush, then deep again.
    #[test]
    fn starts_full_track_resumes_quickly_after_flush() {
        let mut s = Sim::new(600, false, true);
        s.play();
        s.run(4_000);
        {
            let mut r = s.ring.lock();
            r.kept(Engine::Timer { cap: RATE as usize / 2, every: 100 * MS });
            r.flush(-0.25);
        }
        s.wake();
        let flushed = s.now();
        while s.track.lock().played == 0 && s.now() - flushed < 5_000 * MS {
            s.run(5);
        }
        let silent = (s.now() - flushed) / MS;
        assert!(silent <= 60, "heard again {silent} ms after the flush");
        s.ring.lock().kept(Engine::Bursts);
        s.run(30_000);
        let t = s.track.lock();
        assert_eq!(t.size, t.capacity, "deep again");
        assert_eq!(t.underruns, 0, "never ran dry");
    }

    #[test]
    fn paused_writes_nothing_and_sleeps() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(2_000);
        s.control.playing = false;
        s.wake();
        assert_eq!(s.next, None, "paused, nothing is due");
        let (written, latency) = (s.track.lock().written_bytes, s.clock.latency_frames(s.now()));
        s.now.fetch_add(60_000 * MS as u64, Ordering::Relaxed);
        assert_eq!(s.clock.latency_frames(s.now()), latency, "the clock stands still");
        s.wake();
        assert_eq!(s.track.lock().written_bytes, written);
        assert_eq!(s.next, None);
    }

    #[test]
    fn fade_ticks_only_while_running() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(1_000);
        let wakes = s.wakes;
        s.control.ramp = Some((None, 0.0, 160));
        s.wake();
        s.run(400);
        assert_eq!(s.track.lock().volume, 0.0);
        let ticks = s.wakes - wakes;
        assert!((10..=13).contains(&ticks), "{ticks} wakes for a 160 ms fade");
        let next = s.next.unwrap() - s.now();
        assert!(next > 5_000 * MS, "and none after it");
    }

    #[test]
    fn shallow_ring_never_starves_deep_track() {
        // Regression: a 0.5 s ring on a 200 ms timer never filled the deep track, and the fill back-off
        // grew to a second: a gap every second.
        let mut s = Sim::new(600, false, false);
        s.ring.lock().kept(Engine::Timer { cap: RATE as usize / 2, every: 200 * MS });
        s.ring.lock().flush(0.25);
        s.late = 20 * MS;
        s.play();
        s.run(60_000);
        assert_eq!(s.track.lock().underruns, 0, "never runs dry");
    }

    /// The engine's shallow ring, frames.
    fn shallow_ring() -> usize {
        (RATE as i64 * nori_engine::output::SHALLOW_US / 1_000_000) as usize
    }

    #[test]
    fn shallow_in_place_survives_jitter() {
        let mut s = Sim::new(600, false, false);
        // Mixer: 20 ms periods, up to 8 ms late; writer up to 30 ms late.
        s.mixed(20, 8);
        s.late = 30 * MS;
        s.play();
        s.run(15_000);
        // Equalizer opened: shallow ring, a band move flushes, the engine refills up to 15 ms late.
        s.control.shallow = true;
        s.wake();
        {
            let t = s.track.lock();
            assert_eq!((t.reopened, t.capacity, t.size), (0, track_frames(RATE, false), track_frames(RATE, true)), "the same track, made shallow in place");
        }
        {
            let mut r = s.ring.lock();
            r.kept(Engine::Shallow { cap: shallow_ring(), late: 15 * MS });
            r.flush(0.25);
        }
        s.wake();
        let (wakes, underruns) = (s.wakes, s.track.lock().underruns);
        s.deepest = 0;
        s.run(60_000);
        assert_eq!(s.track.lock().underruns, underruns, "never runs dry");
        let ms = s.deepest * 1000 / RATE as u64;
        assert!(ms <= 250, "a band moved is heard {ms} ms later at most");
        let wakes = s.wakes - wakes;
        assert!(wakes <= 60 * 16, "about once per 80 ms: {wakes} wakes in a minute");
        // Closed: deep again, in bursts.
        s.control.shallow = false;
        s.ring.lock().kept(Engine::Bursts);
        s.wake();
        {
            let t = s.track.lock();
            assert_eq!((t.reopened, t.size), (0, track_frames(RATE, false)), "deep again, in place");
        }
        s.run(15_000);
        let wakes = s.wakes;
        s.run(60_000);
        assert_eq!(s.track.lock().underruns, underruns);
        assert!(s.wakes - wakes <= 8, "back to a wake every ten seconds or so: {}", s.wakes - wakes);
    }

    /// Toggles shallow/deep `rounds` times under jitter; returns the longest silence and out-of-order frames.
    fn tuned_back_and_forth(starts_full: bool, rounds: usize) -> (u64, u32) {
        let mut s = Sim::new(900, true, starts_full);
        s.ring.lock().counting = true;
        s.track.lock().record = true;
        s.mixed(20, 8);
        s.late = 30 * MS;
        s.play();
        s.run(15_000);
        let deep = track_frames(RATE, false);
        for round in 0..rounds {
            s.control.shallow = true;
            s.ring.lock().kept(Engine::Shallow { cap: shallow_ring(), late: 15 * MS });
            s.wake();
            assert_eq!(s.track.lock().size, track_frames(RATE, true), "round {round}: shallow at once");
            // After the deep buffer plays out, latency is at most 250 ms.
            s.run(25_000);
            s.deepest = 0;
            s.run(5_000);
            let ms = s.deepest * 1000 / RATE as u64;
            assert!(ms <= 250, "round {round}: a band moved is heard {ms} ms later at most");
            s.control.shallow = false;
            s.ring.lock().kept(Engine::Bursts);
            s.wake();
            assert_eq!(s.track.lock().size, deep, "round {round}: deep at once");
            // Every other round, reopened before the deep buffer refills.
            s.run(if round % 2 == 0 { 20_000 } else { 3_000 });
        }
        let t = s.track.lock();
        assert_eq!((t.reopened, t.flushes, t.underruns), (0, 0, 0), "never opened again, emptied or run dry");
        assert_eq!(t.resizes as usize, rounds * 2);
        assert!(t.played > 0 && t.last_played.is_some());
        (t.longest_silence, t.jumps)
    }

    #[test]
    fn shallow_deep_switches_are_seamless() {
        for starts_full in [false, true] {
            let (silence, jumps) = tuned_back_and_forth(starts_full, 6);
            let ms = silence as f64 * 1000.0 / RATE as f64;
            assert!(ms <= 5.0, "starts full {starts_full}: {ms} ms of silence at a switch");
            assert_eq!(jumps, 0, "starts full {starts_full}: every frame heard, in order");
        }
    }

    #[test]
    fn shallowing_plays_out_what_it_holds() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(3_000);
        let held = s.track.lock().buffered;
        assert!(held > track_frames(RATE, true) * 10, "seconds in the track");
        s.control.shallow = true;
        s.ring.lock().kept(Engine::Shallow { cap: shallow_ring(), late: 0 });
        s.wake();
        let written = s.track.lock().written_bytes;
        s.run(1_000);
        let t = s.track.lock();
        assert_eq!((t.reopened, t.flushes), (0, 0), "nothing dropped");
        assert_eq!(t.written_bytes, written, "nothing more while it holds more than its new size");
        assert!(t.buffered < held, "and it plays on");
    }

    #[test]
    fn dead_shallow_track_reopens_shallow() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(3_000);
        s.control.shallow = true;
        s.ring.lock().kept(Engine::Shallow { cap: shallow_ring(), late: 0 });
        s.wake();
        s.track.lock().dead = true;
        s.run(12_000);
        let t = s.track.lock();
        assert_eq!(t.reopened, 1);
        assert_eq!((t.capacity, t.size), (track_frames(RATE, false), track_frames(RATE, true)));
    }

    /// 15 s deep over Bluetooth ([`Sim::bluetooth`]), then shallow with a flush; returns the sim.
    fn tuned_over_bluetooth(says: bool) -> Sim {
        let mut s = Sim::new(900, false, false);
        s.bluetooth(200, says);
        s.late = 30 * MS;
        s.play();
        s.run(15_000);
        assert_eq!(s.track.lock().underruns, 0, "deep, Bluetooth never runs it dry");
        s.control.shallow = true;
        s.wake();
        {
            let t = s.track.lock();
            assert_eq!((t.reopened, t.capacity), (0, track_frames(RATE, false)), "the same track, made shallow in place");
            assert!(t.size < t.capacity / 10, "and shallow: {} ms", t.size * 1000 / RATE as u64);
        }
        {
            let mut r = s.ring.lock();
            r.kept(Engine::Shallow { cap: shallow_ring(), late: 15 * MS });
            r.flush(0.25);
        }
        s.follow_depth();
        s.wake();
        s
    }

    #[test]
    fn shallow_sized_for_reported_bluetooth_route() {
        let mut s = tuned_over_bluetooth(true);
        let bt = RATE as u64 / 5;
        let (_, capacity) = shallow_marks(RATE, bt, bt);
        assert_eq!(s.track.lock().size, capacity, "sized for the output's latency and pulls");
        let d = s.depth.get().expect("the engine is told how deep");
        assert!(d.ring_us >= nori_engine::output::SHALLOW_US && d.device_us == (capacity * 1_000_000 / RATE as u64) as i64, "{d:?}");
        let (underruns, wakes) = (s.track.lock().underruns, s.wakes);
        s.deepest = 0;
        for _ in 0..60 {
            s.run(1_000);
            s.follow_depth();
        }
        let t = s.track.lock();
        assert_eq!(t.underruns, underruns, "never runs dry, its 200 ms pulled at once and all");
        assert_eq!((t.reopened, t.size), (0, capacity), "never opened again, never grown");
        drop(t);
        let ms = s.deepest * 1000 / RATE as u64;
        assert!(ms <= 700, "a band moved reaches the output {ms} ms later at most (and its own 200 ms after that)");
        let wakes = s.wakes - wakes;
        assert!(wakes <= 60 * 12, "{wakes} wakes in a minute");
        s.control.shallow = false;
        s.ring.lock().kept(Engine::Bursts);
        s.wake();
        assert_eq!(s.track.lock().size, track_frames(RATE, false));
        s.run(30_000);
        assert_eq!(s.track.lock().underruns, underruns);
    }

    #[test]
    fn shallow_grows_for_unreported_latency() {
        let mut s = tuned_over_bluetooth(false);
        assert_eq!(s.track.lock().size, track_frames(RATE, true), "at first, as for the speaker");
        for _ in 0..20 {
            s.run(1_000);
            s.follow_depth();
        }
        let (underruns, size) = {
            let t = s.track.lock();
            (t.underruns, t.size)
        };
        assert!(underruns <= 10, "it grew within a few underruns: {underruns}");
        assert!(size > track_frames(RATE, true) * 2, "for the latency it saw and the pulls it missed: {} ms", size * 1000 / RATE as u64);
        for _ in 0..60 {
            s.run(1_000);
            s.follow_depth();
        }
        let t = s.track.lock();
        assert_eq!(t.underruns, underruns, "and then never ran dry");
        assert_eq!((t.reopened, t.size), (0, size), "never grown again, never shrunk, never opened again");
    }

    #[test]
    fn shallow_marks_by_route() {
        let f = |ms: u64| ms * RATE as u64 / 1000;
        assert_eq!(shallow_marks(RATE, 0, 0), (f(80), f(160)), "the speaker's, as before");
        assert_eq!(shallow_marks(RATE, f(10), f(20)), (f(80), f(160)));
        let (low, capacity) = shallow_marks(RATE, f(200), f(200));
        assert_eq!(low, f(440), "the latency, a pull and a late wake");
        assert_eq!(capacity, f(550));
        let (low, capacity) = shallow_marks(RATE, f(5_000), f(5_000));
        assert_eq!(low, f(1_500), "never past the most");
        assert!(capacity < f(2_000));
    }

    /// A fake AudioTrack on the real clock, keeping every sample since the last flush.
    #[derive(Default)]
    struct Live {
        written: Vec<i16>,
        played_before: u64,
        since: Option<std::time::Instant>,
        flushes: u32,
        volumes: Vec<f32>,
        /// Phone-like behaviour: bounded buffer, start threshold after a flush, and a flush right after a
        /// pause applied only after `defer` (the dropped frames take room until then).
        bounded: bool,
        size: u64,
        threshold: u64,
        defer: Duration,
        playing: bool,
        filling_up: bool,
        pausing_until: Option<std::time::Instant>,
        stale: u64,
    }

    impl Live {
        fn played(&self) -> u64 {
            let running = self.since.map_or(0, |t| (t.elapsed().as_secs_f64() * RATE as f64) as u64);
            (self.played_before + running).min(self.written.len() as u64 / 2)
        }

        fn buffered(&self) -> u64 {
            (self.written.len() as u64 / 2).saturating_sub(self.played())
        }

        /// Applies a deferred flush once its period has passed.
        fn mixed(&mut self) {
            if self.pausing_until.is_some_and(|t| std::time::Instant::now() >= t) {
                self.pausing_until = None;
                self.stale = 0;
            }
        }

        /// Starts the clock once playing, flushed and above the threshold.
        fn start_if_filled(&mut self) {
            self.mixed();
            if self.playing && self.since.is_none() && self.stale == 0 && (!self.filling_up || self.buffered() >= self.threshold) {
                self.filling_up = false;
                self.since = Some(std::time::Instant::now());
            }
        }
    }

    struct LiveSink(Arc<Mutex<Live>>, Vec<f32>);

    impl Sink for LiveSink {
        fn staging(&mut self) -> &mut [f32] {
            &mut self.1
        }
        fn write(&mut self, from: usize, len: usize) -> Result<usize, i32> {
            // SAFETY: the staging memory is f32s, aligned for i16, and the writer keeps the range inside it.
            let samples = unsafe { std::slice::from_raw_parts((self.1.as_ptr() as *const u8).add(from) as *const i16, len / 2) };
            let mut l = self.0.lock();
            l.mixed();
            let take = if l.bounded { (l.size.saturating_sub(l.buffered() + l.stale) as usize * 2).min(samples.len()) } else { samples.len() };
            l.written.extend_from_slice(&samples[..take]);
            l.start_if_filled();
            Ok(take * 2)
        }
        fn play(&mut self) {
            let mut l = self.0.lock();
            l.playing = true;
            l.start_if_filled();
        }
        fn pause(&mut self) {
            let mut l = self.0.lock();
            if l.since.is_some() && !l.defer.is_zero() {
                l.pausing_until = Some(std::time::Instant::now() + l.defer);
            }
            l.played_before = l.played();
            l.since = None;
            l.playing = false;
        }
        fn flush(&mut self) {
            let mut l = self.0.lock();
            if l.pausing_until.is_some_and(|t| std::time::Instant::now() < t) {
                l.stale = l.buffered();
            }
            l.written.clear();
            l.played_before = 0;
            l.flushes += 1;
            l.filling_up = l.threshold > 0;
        }
        fn stop(&mut self) {}
        fn set_volume(&mut self, volume: f32) {
            self.0.lock().volumes.push(volume);
        }
        fn heard(&mut self, _playing: bool) -> Option<(u64, i64)> {
            let mut l = self.0.lock();
            l.start_if_filled();
            Some((l.played(), mono_ns()))
        }
        fn resize(&mut self, frames: u64) -> u64 {
            frames
        }
        fn release(&mut self) {}
    }

    struct LiveOpener(Arc<Mutex<Live>>);

    impl Opener for LiveOpener {
        fn open(&mut self, format: OutputFormat, float: bool, frames: u64) -> Result<Opened, String> {
            assert_eq!((format.rate, format.channels, float), (RATE, 2, false));
            {
                let mut l = self.0.lock();
                l.size = frames;
                l.filling_up = l.threshold > 0;
            }
            Ok(Opened { sink: Box::new(LiveSink(self.0.clone(), vec![0.0; CHUNK_BYTES / 4])), frames, starts_full: false })
        }
    }

    /// Songs as WAV files in memory, streamed through a byte source.
    struct Wavs(Vec<(String, Arc<Vec<u8>>)>);

    impl nori_engine::ByteSource for Wavs {
        fn open(&self, url: &str, from: u64) -> Result<nori_engine::Body, nori_engine::OpenError> {
            let f = self.0.iter().find(|(id, _)| id == url).ok_or("no such song")?.1.clone();
            let len = f.len() as u64;
            Ok(nori_engine::Body { start: from, len: Some(len), reader: Box::new(std::io::Cursor::new(f[from as usize..].to_vec())) })
        }
    }

    struct Songs(Arc<Wavs>);

    impl nori_engine::Library for Songs {
        fn locate(&mut self, id: &str) -> Result<nori_engine::Located, String> {
            Ok(nori_engine::Located { source: nori_engine::Source::Url { url: id.to_string(), bytes: self.0.clone() }, hint: Some("wav".into()), duration_ms: Some(3_000), estimated: false })
        }
        fn about(&self, id: &str) -> nori_player::transitions::WindowSong {
            nori_player::transitions::WindowSong { id: id.to_string(), title: id.to_string(), duration_ms: 3_000, ..Default::default() }
        }
    }

    fn tone(secs: u32, hz: f64) -> Vec<i16> {
        (0..secs * RATE).flat_map(|i| {
            let v = ((std::f64::consts::TAU * hz * i as f64 / RATE as f64).sin() * 12_000.0) as i16;
            [v, v / 2]
        }).collect()
    }

    fn wav(samples: &[i16]) -> Vec<u8> {
        let data = samples.len() as u32 * 2;
        let mut w = Vec::new();
        for part in [&b"RIFF"[..], &(36 + data).to_le_bytes(), b"WAVEfmt ", &16u32.to_le_bytes(), &1u16.to_le_bytes(), &2u16.to_le_bytes()] {
            w.extend_from_slice(part);
        }
        for part in [&RATE.to_le_bytes()[..], &(RATE * 4).to_le_bytes(), &4u16.to_le_bytes(), &16u16.to_le_bytes(), b"data", &data.to_le_bytes()] {
            w.extend_from_slice(part);
        }
        w.extend(samples.iter().flat_map(|v| v.to_le_bytes()));
        w
    }

    fn wait(secs: u64, mut done: impl FnMut() -> bool) -> bool {
        let until = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < until {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn engine_end_to_end_samples_seek_fade() {
        let songs = [tone(3, 440.0), tone(3, 660.0)];
        let wavs = Arc::new(Wavs(vec![("a".into(), Arc::new(wav(&songs[0]))), ("b".into(), Arc::new(wav(&songs[1])))]));
        let live = Arc::new(Mutex::new(Live::default()));
        let shared = Arc::new(Shared::default());
        let output = TrackOutput::new(Box::new(LiveOpener(live.clone())), false, shared.clone());
        let queue = nori_engine::SharedQueue::default();
        queue.0.lock().set(vec!["a".into(), "b".into()], Some(0), false, 0);
        let mut app = nori_player::sim::App::new();
        app.prefs = nori_player::sim::prefs_off();
        let settings = nori_engine::Settings { fade_ms: 200, ..Default::default() };
        let config = nori_engine::Config { settings, ..Default::default() };
        let engine = nori_engine::Engine::start(Songs(wavs), app, queue, Box::new(output), None, config, |_| {});
        engine.queue_changed();
        engine.play_at(0, 0);

        assert!(wait(5, || live.lock().played() > RATE as u64 / 2), "it plays");
        {
            let l = live.lock();
            assert!(l.written.len() >= songs[0].len(), "the whole of a short song went into the track at once");
            assert!(l.written[..songs[0].len()] == songs[0][..], "sample for sample, the song's own");
        }
        assert_eq!(engine.status().state, nori_engine::State::Playing, "the ring ran empty, the track did not: still playing");

        // Seek to song 2 at 1 s: the track holds only the new music.
        engine.play_at(1, 1_000);
        assert!(
            wait(5, || {
                let l = live.lock();
                l.flushes == 1 && l.written.len() > RATE as usize
            }),
            "the jump empties the track"
        );
        {
            let l = live.lock();
            let from = RATE as usize * 2;
            assert!(l.written[..RATE as usize] == songs[1][from..from + RATE as usize], "the new music from where it was asked");
        }
        assert!(wait(5, || engine.status().position_ms >= 1_200), "and the playhead follows the track's clock");
        let at = engine.status().position_ms;
        assert!(at < 2_500, "a second and a bit into the song, not further: {at}");

        // Pause fades the track volume, then pauses.
        live.lock().volumes.clear();
        engine.pause();
        assert!(wait(5, || live.lock().since.is_none()), "paused");
        let l = live.lock();
        // Several monotonic steps (the count depends on machine load).
        assert!(l.volumes.len() >= 2, "the fade out ran in steps: {:?}", l.volumes);
        assert!(l.volumes.windows(2).all(|w| w[1] <= w[0]), "only ever down: {:?}", l.volumes);
        assert_eq!(l.volumes.last(), Some(&0.0));
        drop(l);

        // Ended only once the track played its last frame.
        engine.play();
        assert!(wait(6, || engine.status().state == nori_engine::State::Ended), "the queue ends");
        let l = live.lock();
        assert!(l.played() + RATE as u64 / 10 >= l.written.len() as u64 / 2, "{} of {} frames heard at the end", l.played(), l.written.len() / 2);
        drop(l);
        engine.stop();
    }

    /// Songs as WAV files on disk.
    struct Files {
        dir: nori_testdir::TempDir,
        ms: i64,
    }

    struct OnDisk(Arc<Files>);

    impl nori_engine::Library for OnDisk {
        fn locate(&mut self, id: &str) -> Result<nori_engine::Located, String> {
            Ok(nori_engine::Located { source: nori_engine::Source::File(self.0.dir.join(format!("{id}.wav"))), hint: Some("wav".into()), duration_ms: Some(self.0.ms), estimated: false })
        }
        fn about(&self, id: &str) -> nori_player::transitions::WindowSong {
            nori_player::transitions::WindowSong { id: id.to_string(), title: id.to_string(), duration_ms: self.0.ms, ..Default::default() }
        }
    }

    /// End to end regression for [`fast_skips_over_full_track_keep_playing`]: equalizer and AutoMix on,
    /// 12-15 skips 5-200 ms apart (every fourth after the track refilled), on a phone-like track.
    #[test]
    fn engine_plays_on_after_fast_skips() {
        const SECS: u32 = 30;
        let ms = SECS as i64 * 1000;
        let ids: Vec<String> = (0..20).map(|k| format!("s{k}")).collect();
        let files = Arc::new(Files { dir: nori_testdir::TempDir::new("nori-android-skips"), ms });
        // One file, hard-linked under every id.
        let first = files.dir.join("s0.wav");
        std::fs::write(&first, wav(&tone(SECS, 330.0))).expect("a song on the disk");
        for id in &ids[1..] {
            std::fs::hard_link(&first, files.dir.join(format!("{id}.wav"))).expect("the song under another name");
        }
        let live = Arc::new(Mutex::new(Live { bounded: true, threshold: RATE as u64 / 4, defer: Duration::from_millis(40), ..Live::default() }));
        let output = TrackOutput::new(Box::new(LiveOpener(live.clone())), false, Arc::new(Shared::default()));
        let queue = nori_engine::SharedQueue::default();
        queue.0.lock().set(ids.clone(), Some(0), false, 0);
        let mut app = nori_player::sim::App::new();
        app.prefs = nori_player::transitions::TransitionPrefs { auto_mix: true, keep_albums: false, ..nori_player::sim::prefs_off() };
        // Pre-measured, so the simulated app doesn't analyse on the engine thread.
        for id in &ids {
            let a = nori_player::types::TrackAnalysis { song_id: id.clone(), analysis_version: nori_player::automix::ANALYSIS_VERSION, duration_ms: ms, bpm: 120.0, bpm_confidence: 1.0, lufs: -14.0, silence_end_ms: ms, mixramp_end_ms: ms, outro_start_ms: ms - 8_000, ..Default::default() };
            app.analyses.insert(id.clone(), a);
        }
        let bands = vec![nori_player::dsp::Band { kind: nori_player::dsp::PEAKING, freq: 1000.0, gain_db: 6.0, q: 1.0, channel: 0 }];
        let settings = nori_engine::Settings { sound: nori_engine::Sound { bands, ..Default::default() }, auto_mix: true, ..Default::default() };
        let config = nori_engine::Config { settings, ..Default::default() };
        let engine = nori_engine::Engine::start(OnDisk(files.clone()), app, queue, Box::new(output), None, config, |_| {});
        engine.queue_changed();
        // Position polled 4x/s, as the screen does.
        engine.position_updates(Some(Duration::from_millis(250)));
        engine.play_at(0, 0);
        let brim = || {
            let l = live.lock();
            l.buffered() + RATE as u64 / 10 >= l.size
        };
        assert!(wait(5, brim), "the track filled to the brim");
        assert!(wait(5, || live.lock().played() > 0), "and playing");
        let mut dice = Dice(0x5EED);
        let presses = 12 + dice.roll(4) as usize;
        for k in 1..=presses {
            engine.go_to(k, 0);
            if k % 4 == 0 {
                wait(3, brim);
            } else {
                std::thread::sleep(Duration::from_millis(5 + dice.roll(195) as u64));
            }
        }
        let plays = wait(8, || live.lock().played() > 2 * RATE as u64);
        let (status, l) = (engine.status(), live.lock());
        assert!(
            plays,
            "{presses} presses: music after them ({} ms heard of {} ms written since the last flush, the track playing {}; the engine {:?} on {:?} at {} ms)",
            l.played() * 1000 / RATE as u64,
            l.written.len() as u64 / 2 * 1000 / RATE as u64,
            l.since.is_some(),
            status.state,
            status.index,
            status.position_ms,
        );
        drop(l);
        assert_eq!(status.index, Some(presses), "on the song the last press asked for");
        let at = status.position_ms;
        assert!(wait(5, || engine.status().position_ms >= at + 1_000), "and the place moves on from {at} ms: {:?}", engine.status());
        engine.stop();
    }

    #[test]
    fn starts_full_track_is_stopped_at_the_end() {
        let mut s = Sim::new(4, false, true);
        s.play();
        s.run(10_000);
        let t = s.track.lock();
        assert_eq!(t.stops, 1, "stopped once, to play the last of it out");
        assert_eq!(t.played, 4 * RATE as u64, "every frame heard");
        drop(t);
        assert_eq!(s.next, None, "and then nothing more to do");
    }
}
