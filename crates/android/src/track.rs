//! nori-engine's output on Android (`nori_engine::AudioOutput`): the engine's ring poured into an
//! AudioTrack in bursts, the way the ExoPlayer path's sink feeds its deep AudioTrack buffer.
//!
//! One thread of its own, which sleeps between bursts. The track holds one of the engine's bursts and a
//! little more ([`TRACK_US`]); the thread wakes when about a second of it is left ([`LOW_US`]) and moves
//! everything the ring has into it in one go, which runs the ring past its own low mark, so the engine
//! is woken in the same moment to decode the next burst. While music plays the two wake together about
//! every ten seconds, and nothing else of this output runs; the engine keeps no timer of its own for the
//! ring (`AudioOutput::bursts`), since between top-ups it does not run down.
//!
//! The thread wakes only for:
//! - the track running down to its low mark: one timed sleep, computed from the track's clock;
//! - a command from the engine (play, pause, a flush, a fade): the engine unparks it;
//! - filling the track after a start, a resume or a flush, every 20 ms until it is full (a fraction of a
//!   second while the engine decodes its first burst), backing off to a second while the engine has
//!   nothing yet (the network is slow);
//! - a fade, every 16 ms while it runs (`nori_player::transport::FADE_TICK_MS`);
//! - the device's clock settling after a start: twice in the first second, so the playhead is the
//!   device's own and not a guess;
//! - the end of the music: once, when the track has played its last frame.
//!
//! A track that dies (the sound server restarted, the device went away under it) refuses its writes
//! with an error rather than taking nothing: it is opened again in its place, as ExoPlayer recovers from
//! a write that failed, and what it held is lost. One that would not open again is the engine's to hear
//! of ([`AudioOutput::failed`]): it stops and says so, and the writer sleeps until it is let go.
//!
//! Paused, or at the end, it sleeps until the engine says something.
//!
//! While the equalizer is tuned the engine keeps its ring shallow (`nori_engine::output::SHALLOW_US`) and
//! says so ([`AudioOutput::shallow`]). The track is never opened again for it: it is opened deep once, in
//! power saving mode, and only the part of its buffer it may fill changes, at once
//! (`AudioTrack.setBufferSizeInFrames`, [`Sink::resize`]): [`SHALLOW_TRACK_US`] while tuned on the phone's
//! speaker, topped up once per half of that, so a band moved is heard within a quarter of a second (the
//! ring's and the track's together), more on an output that needs more (below); all of it again when the
//! screen closes. Neither way drops anything or stops the track, so
//! neither is heard: made shallow, the seconds it holds play out first (the engine makes the music again
//! behind its dip for the first band moved before they have, `nori_engine`'s `Worker::apply`); made deep,
//! it is filled up from the next burst on. Opening it again at the other size, as it once was, was a
//! stutter each way: a new track takes its time to start, and a shallow one went to another mixer.
//!
//! How shallow is the output's to say, not a constant's. The writer's clock counts music from the moment
//! the track lets it go (the play head as the device presents it), so what the output holds past the
//! track - its own latency, a Bluetooth link's couple of hundred milliseconds - is counted as the track's,
//! and a track made as shallow as for the phone's speaker held nothing at all on a pair of Bluetooth
//! headphones: the platform lets `setBufferSizeInFrames` go down to 16 frames whatever the output needs,
//! where a track opened anew is given at least `AudioTrack.getMinBufferSize` for it. So the shallow size
//! is [`shallow_marks`]: topped up while it still holds the output's latency, the least the platform would
//! give a new track there and a wake's lateness ([`Needs`]), which on the speaker is the 160 ms it always
//! was. The writer then watches, only while shallow: the latency it sees (what the track let go against
//! what was heard) and the track's underruns, and grows for either, never shrinking again for that
//! output. The engine keeps its ring as deep as one of those top-ups ([`AudioOutput::shallow_depth`]).
//!
//! What the platform does with a buffer made smaller: nothing but that. The track stays on the output its
//! performance mode chose when it was built (the deep buffer mixer, for power saving, on a phone that has
//! one), whose periods and latency do not change, so the time from the track to the ear is that output's
//! in both sizes; the sound server takes no more from the track per period than before, the writer only
//! tops it up more often. The start threshold (Android 12 on) is kept inside the size, or a track flushed
//! while shallow would wait for more than it may hold; before 12 a track starts once full at its size.
//! Deep, the track is exactly what it was before: the same buffer, mode and wakes, so the battery is too.
//! The writer runs at audio priority, as ExoPlayer's playback thread does: with the equalizer screen
//! drawing at 120 frames a second, a thread at the normal priority is woken late often enough for a
//! shallow track to run dry.
//!
//! A sound server may give a smaller buffer than asked, or say it gave the size asked and take less. The
//! writer is timed by what the track holds, never by what is pulled for it, and a track that refuses a
//! write it had room for is counted as the size it held then: a small track is topped up once per half of
//! what it holds, and the log says so when it opens. It still wakes no more often than that buffer
//! demands, and the engine still once per burst.
//!
//! With seconds of music inside the track, what the ring does to samples as they are pulled is heard
//! seconds later. So the fades run at the track's volume (the engine hands them over through
//! `AudioOutput::ramp`), and a flush empties the track as well as the ring (`AudioOutput::flush`, with
//! `Feed::flushed` saying which pull holds the new music). ReplayGain is on each song's samples before
//! they reach the ring (`TransitionEngine::set_gain`); a change of the settings scales what the ring
//! still holds, and the seconds already in the track play out as they were.
//!
//! No JNI here: the AudioTrack is a [`Sink`], so the tests below run the whole thing on a simulated
//! track and a virtual clock. `player.rs` has the real one.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{JoinHandle, Thread};
use std::time::Duration;

use nori_engine::{AudioOutput, DeviceWatch, Feed, OutputFormat, ShallowDepth};
use nori_player::burst::BUFFER_US;
use nori_player::transport::{fade_step, FADE_TICK_MS};
use parking_lot::Mutex;

/// The track is topped up again when this much music is left in it.
pub(crate) const LOW_US: i64 = 1_000_000;
/// How much the track holds: one of the engine's bursts, the low mark, and half a second for what the
/// ear moves on while a burst is decoded. A top-up then takes all the ring has, so the ring always runs
/// down past its own low mark and wakes the engine at the same moment: the engine has no timer of its
/// own for it (`AudioOutput::bursts`).
pub(crate) const TRACK_US: i64 = BUFFER_US + LOW_US + 500_000;
/// How much the track holds while the equalizer is tuned: topped up at half of it, it keeps between 80
/// and 160 ms, which with the ring's 40 to 80 ms before it is what a band moved takes to be heard.
pub(crate) const SHALLOW_TRACK_US: i64 = 160_000;
/// How late the writer may wake while shallow and still find the output fed: part of the low mark.
const SHALLOW_LATE_US: i64 = 40_000;
/// The deepest a shallow track grows for an output that keeps running dry: past this the equalizer screen
/// might as well have the deep buffer.
const SHALLOW_MOST_US: i64 = 1_500_000;
/// A latency seen this much past the one planned for makes the track deeper.
const LAG_STEP_US: i64 = 20_000;
/// The samples moved per write, in bytes.
pub(crate) const CHUNK_BYTES: usize = 128 * 1024;
/// How often the track is filled while it is being filled after a start or a flush.
const FILL_TICK_MS: u64 = 20;
/// The longest the filling waits between looks while the engine has nothing to give.
const STARVED_MAX_MS: u64 = 1_000;
/// After a start, when the device's clock is read again: its first readings come late.
const SETTLE_MS: [i64; 2] = [250, 1_000];
/// The track's start threshold, as it is opened (`RustPlayer.openTrack`) and made shallow
/// (`JavaTrack::resize`): after a flush it plays nothing until it holds this much, or all it may hold.
const START_US: i64 = 250_000;
/// What a track that starts only once full (before Android 12) is made to hold after a flush, until it
/// has started: a quarter of a second, as a track from Android 12 on starts with (its start threshold).
/// Deep, it waited for all of its eleven seconds to be decoded and written first.
const PRIMING_US: i64 = 250_000;

/// What the output needs of an AudioTrack.
pub(crate) trait Sink: Send {
    /// Where samples are put before they are written: the same memory for the sink's whole life,
    /// [`CHUNK_BYTES`] long.
    fn staging(&mut self) -> &mut [f32];
    /// Writes bytes `from..from + len` of the staging memory, as much as fits without waiting. The
    /// bytes taken, or the error the track answered with (it is dead and must be opened again).
    fn write(&mut self, from: usize, len: usize) -> Result<usize, i32>;
    fn play(&mut self);
    fn pause(&mut self);
    /// Drops what was written and not yet played. Only while paused or stopped.
    fn flush(&mut self);
    /// Plays what was written to its end, then stops: for a track that would otherwise wait to be
    /// full before it starts, at the end of the music.
    fn stop(&mut self);
    fn set_volume(&mut self, volume: f32);
    /// Frames heard since the last flush, and the monotonic time (ns) that was true at; none while the
    /// device cannot say.
    fn heard(&mut self, playing: bool) -> Option<(u64, i64)>;
    /// From now on the track holds at most `frames` (`AudioTrack.setBufferSizeInFrames`), up to the buffer
    /// it was opened with, and starts after a flush once it holds a quarter of a second or all of that.
    /// Nothing it holds is dropped and it keeps playing: a size under what it holds only stops it taking
    /// more until it has played down to it. Returns the size it gave.
    fn resize(&mut self, frames: u64) -> u64;
    /// What the output the track plays on now says of itself ([`Route`]): read as the track is made
    /// shallow, where the headphones connected since it was opened count.
    fn route(&mut self) -> Route {
        Route::default()
    }
    /// Times the track ran dry since it was made (`AudioTrack.getUnderrunCount`); none where it cannot say.
    fn underruns(&mut self) -> Option<u64> {
        None
    }
    /// Frames the sound server took from the track since the last flush (the play head, ahead of what the
    /// device presents by the output's own latency); none where it cannot say.
    fn consumed(&mut self) -> Option<u64> {
        None
    }
    fn release(&mut self);
}

/// What an output says of itself, for the shallow track's size ([`shallow_marks`]).
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Route {
    /// The least the platform gives a new track there (`AudioTrack.getMinBufferSize`), frames: what it
    /// holds the output must be able to take from the track at once.
    pub min_frames: Option<u64>,
    /// The output's own latency past the track (`AudioTrack.getLatency`, less the track's buffer), frames.
    pub latency_frames: Option<u64>,
    /// Where it plays, in the log's words: "Bluetooth", "the phone speaker".
    pub name: Option<&'static str>,
}

/// What the output needs of a shallow track, as it said and as the writer saw it, frames.
#[derive(Default, Clone, Copy, Debug)]
struct Needs {
    /// Music the track let go and the ear has not heard: the output's own latency. The writer's clock
    /// counts it as the track's.
    lag: u64,
    /// What the track must still hold for the output's next pull: the least the platform gives a new
    /// track there, and more each time the track ran dry.
    pull: u64,
    /// Where it plays, for the log; a new name is a new output, whose needs are its own.
    name: Option<&'static str>,
    /// Times the track ran dry, as last read.
    underruns: Option<u64>,
    /// Times it grew for running dry, on this output.
    grown: u32,
}

/// The shallow track's marks for an output with `lag` past the track and pulls of `pull` (frames, at
/// `rate`): topped up at the low mark, while it still holds the latency, one pull and a wake's lateness
/// (never under half of [`SHALLOW_TRACK_US`]), with a quarter of that again or half of
/// [`SHALLOW_TRACK_US`], whichever is more. `(low, capacity)`, counted as the writer's clock counts. An
/// output that says nothing of itself (the phone's speaker, fed from the deep buffer mixer) gets the
/// 80/160 ms it always had.
pub(crate) fn shallow_marks(rate: u32, lag: u64, pull: u64) -> (u64, u64) {
    let f = |us: i64| (rate as i64 * us / 1_000_000) as u64;
    let half = f(SHALLOW_TRACK_US / 2);
    let low = (lag + pull + f(SHALLOW_LATE_US)).max(half).min(f(SHALLOW_MOST_US));
    let top = half.max(low / 4);
    (low, low + top)
}

/// What the shallow track found it needs, for the engine (`AudioOutput::shallow_depth`): µs, 0 until known.
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

/// A sink as it was opened: how many frames its buffer holds, and whether it only starts once that
/// buffer is full (Android before 12, which has no start threshold).
pub(crate) struct Opened {
    pub sink: Box<dyn Sink>,
    pub frames: u64,
    pub starts_full: bool,
}

/// Opens the device: `frames` is the buffer asked for.
pub(crate) trait Opener: Send {
    fn open(&mut self, format: OutputFormat, float: bool, frames: u64) -> Result<Opened, String>;
}

/// The engine's end of the ring as the writer uses it: `nori_engine::Feed`, or a simulated one.
pub(crate) trait Ring: Send {
    fn available(&self) -> usize;
    fn pull(&mut self, out: &mut [f32]) -> usize;
    fn pull_i16(&mut self, out: &mut [i16]) -> usize;
    fn flushed(&mut self) -> bool;
    fn ending(&self) -> bool;
    /// Wakes the engine now: the output failed.
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

/// CLOCK_MONOTONIC in ns: the clock the device's timestamps are on (Java's `System.nanoTime`).
// Both fields are 32 bits on a 32-bit ABI.
#[allow(clippy::unnecessary_cast)]
pub(crate) fn mono_ns() -> i64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a plain system call writing into the struct handed to it.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as i64 * 1_000_000_000 + t.tv_nsec as i64
}

/// An AudioTrack's play head, which the platform gives as 32 bits that wrap (after six hours at
/// 192 kHz, a day at 44.1): the last value read and the wraps counted since the last flush make it a
/// count that only grows. Made anew at every flush, which sets the head back to nought.
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

/// The track's clock: moved by the writer, read by the engine (through `latency_us`) and by the
/// screen, from any thread.
#[derive(Default)]
pub(crate) struct Clock(Mutex<Counts>);

#[derive(Default, Clone, Copy)]
struct Counts {
    rate: u32,
    /// Frames pulled for the track since the last flush, written or about to be.
    ahead: u64,
    /// Frames the track took since the last flush.
    given: u64,
    /// Frames heard at `at_ns`, and whether the count moves on from there.
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
    /// Frames between what was pulled and what has been heard: how far behind the ring the ear is.
    pub(crate) fn latency_frames(&self, now_ns: i64) -> u64 {
        let c = *self.0.lock();
        c.ahead.saturating_sub(c.heard_at(now_ns))
    }

    pub(crate) fn latency_us(&self, now_ns: i64) -> u64 {
        let rate = self.0.lock().rate.max(1) as u64;
        self.latency_frames(now_ns) * 1_000_000 / rate
    }

    /// Frames the track took and has not played yet: what it holds, without what is pulled and still
    /// waiting to go in. What the next top-up is timed by.
    fn in_track(&self, now_ns: i64) -> u64 {
        let c = *self.0.lock();
        c.given.saturating_sub(c.heard_at(now_ns))
    }

    /// Frames heard by now.
    fn heard_now(&self, now_ns: i64) -> u64 {
        self.0.lock().heard_at(now_ns)
    }

    fn update(&self, f: impl FnOnce(&mut Counts)) {
        f(&mut self.0.lock());
    }

    /// Whether the count moves on.
    fn running(&self) -> bool {
        self.0.lock().running
    }

    /// The count stops where it is now (a pause asked for).
    fn freeze(&self, now_ns: i64) {
        self.update(|c| {
            c.heard = c.heard_at(now_ns);
            c.at_ns = now_ns;
            c.running = false;
        });
    }

    /// The count moves on from here (the track was started).
    fn run(&self, now_ns: i64) {
        self.update(|c| {
            c.heard = c.heard_at(now_ns);
            c.at_ns = now_ns;
            c.running = true;
        });
    }

    /// What the device says it has played, taken over when it makes sense.
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

/// What the engine asked of the writer since it last looked.
#[derive(Default)]
pub(crate) struct Control {
    pub playing: bool,
    pub ramp: Option<(Option<f32>, f32, i64)>,
    pub stop: bool,
    /// The track is to be shallow ([`SHALLOW_TRACK_US`]), from now on.
    pub shallow: bool,
}

/// The frames a track is asked for: its deep size, or the shallow one while the equalizer is tuned.
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

/// The writer thread's state: what moves music from the ring into the track, one step per wake.
pub(crate) struct Writer<R: Ring> {
    ring: R,
    sink: Box<dyn Sink>,
    /// What opens a track in place of one that died, as it was asked for the first one.
    opener: Arc<Mutex<Box<dyn Opener>>>,
    format: OutputFormat,
    asked: u64,
    /// The buffer the track was opened with, which its size moves inside.
    allocated: u64,
    /// Why the track died and would not open again, for the engine ([`AudioOutput::failed`]).
    failure: Arc<Mutex<Option<String>>>,
    /// No track at all: nothing is written until the output is let go.
    dead: bool,
    /// A track was opened in place of a dead one: it fills from the next look.
    revived: bool,
    clock: Arc<Clock>,
    bytes: Arc<AtomicU64>,
    channels: usize,
    rate: u32,
    float: bool,
    /// 24-bit samples, packed: a song of more than 16 bits played as it is (bit-perfect).
    packed: bool,
    /// What the track holds: the buffer it gave, or less once it has refused a write with room left.
    capacity: u64,
    /// The track was flushed holding music and has not been heard playing since. The platform lets go of
    /// what a flush dropped only at its mixer's next period (AudioFlinger flushes a track still pausing
    /// there, not in the call), and until then a write finds that much less room: a write it refuses then
    /// says nothing of its size ([`Writer::refused`]).
    flushed_full: bool,
    /// The track is topped up again when this much is left in it: [`LOW_US`], or half of a buffer too
    /// small for that.
    low: u64,
    starts_full: bool,
    /// Bytes at the start of the staging memory the track did not take yet.
    staged: (usize, usize),
    playing: bool,
    /// Filling the track to full: after a start, a resume or a flush.
    filling: bool,
    /// Stopped at the end of the music, the track playing out what it has.
    drained: bool,
    volume: f32,
    fade: Option<Fade>,
    /// When the track was last started, and how many of the settling readings were taken since.
    started_ns: i64,
    settled: usize,
    /// How long the next look waits while the ring has nothing to give.
    starved_ms: u64,
    /// The track is kept shallow, and whether the engine wants it so.
    shallow: bool,
    wants_shallow: bool,
    /// What the output needs of the shallow track.
    needs: Needs,
    /// A track that starts only once full holds [`PRIMING_US`] after a flush until it has started.
    priming: bool,
    /// The shallow track's size as found, for the engine.
    depth: Arc<Depth>,
}

impl<R: Ring> Writer<R> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(ring: R, opened: Opened, reopen: Reopen, format: OutputFormat, float: bool, clock: Arc<Clock>, bytes: Arc<AtomicU64>, depth: Arc<Depth>) -> Writer<R> {
        let rate = format.rate;
        clock.update(|c| *c = Counts { rate, ..Counts::default() });
        said_small(opened.frames, reopen.frames, rate);
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
            depth,
        }
    }

    fn frame_bytes(&self) -> usize {
        self.channels * sample_bytes(self.float, self.packed)
    }

    /// The track holds `frames` from now on, and is topped up at the low mark that goes with it.
    fn holds(&mut self, frames: u64) {
        self.capacity = frames.max(1);
        self.low = low_mark(frames, self.rate);
    }

    /// The track made shallow or deep as the engine wants it, in place: what it holds plays on. Shallow,
    /// it takes nothing more until it has played down to its new low mark, as deep as the output it plays
    /// on needs ([`shallow_marks`]); deep, it is topped up at the next look, from what the ring has and
    /// the engine's next burst.
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

    /// What the output the track plays on says of itself, taken into its needs: another output than the
    /// last one starts from what it says; the same one keeps what was seen of it too (it grows, never
    /// shrinks). The underruns are counted from here.
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

    /// The track made as shallow as the output needs now, in place, and the engine told how deep its ring
    /// is to be; `why` is what changed since it was last made so, for the log.
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

    /// While shallow, at a wake the writer made anyway: the output's latency as seen (what the track let
    /// go against what the ear heard), and its underruns. A latency past the one planned for, or a track
    /// that ran dry, makes it deeper, for good on this output.
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

    /// One wake: does what the engine asked and what the track needs, and says how long to sleep
    /// (ms; `None` until the engine says something).
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
                // The engine pauses when its own clock says the fade is over; this thread may have woken a
                // step late and not reached the end yet. Finish it, so the track stops at the fade's target
                // (silence) rather than a step short of it.
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
            // Paused and played again before this thread woke: the pause stopped the clock at once, on the
            // engine's thread (`TrackOutput::pause`), but never reached the track, which played on. So does
            // its clock, from the device's word; left stopped, it counted the track's seconds as still in it,
            // and the track ran dry while the player said it played.
            self.clock.run(now_ns);
            self.read_clock();
        }
        // A flush shows in the next pull, even one that takes nothing: the track follows it at once.
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

    /// The track starts (or starts again): filled to full first, the clock from the device's word.
    fn start(&mut self, now_ns: i64) {
        self.sink.play();
        self.clock.run(now_ns);
        self.filling = true;
        self.started_ns = now_ns;
        self.settled = 0;
        self.starved_ms = FILL_TICK_MS;
    }

    /// The ring was flushed: so is the track, and it fills again from the new music. One that starts only
    /// once full is made to hold a quarter of a second until it has ([`PRIMING_US`]).
    fn restart(&mut self, now_ns: i64) {
        log("emptied for the music that follows");
        self.sink.pause();
        self.sink.flush();
        self.flushed_full |= self.clock.0.lock().given > 0;
        if self.starts_full && !self.shallow && !self.priming {
            let got = self.sink.resize(self.frames(PRIMING_US).min(self.allocated));
            self.holds(got);
            self.priming = true;
        }
        self.staged = (0, 0);
        self.drained = false;
        self.clock.update(|c| {
            c.ahead = 0;
            c.given = 0;
            c.heard = 0;
            c.at_ns = now_ns;
            c.running = false;
        });
        if self.playing {
            self.start(now_ns);
        } else {
            self.filling = true;
        }
    }

    fn read_clock(&mut self) {
        if let Some((frames, ns)) = self.sink.heard(self.playing) {
            // Music written since the flush heard: the platform has let go of what the flush dropped.
            if frames > 0 && frames <= self.clock.0.lock().given {
                self.flushed_full = false;
            }
            self.clock.anchor(frames, ns, self.playing);
            self.watched(frames);
        }
    }

    /// The perf build's watch: the device's own count against what it was given, at a wake this thread
    /// made anyway (nori_perf::invariants). Timed by the clock now: a reading that stopped moving keeps its
    /// old time.
    fn watched(&self, presented: u64) {
        if nori_perf::invariants::on() {
            let given = self.clock.0.lock().given;
            nori_perf::invariants::track_seen(mono_ns() / 1_000_000, self.playing && !self.dead, given, presented, self.rate);
        }
    }

    /// A wake that does not read the device's clock (nothing is due): in the perf build it is read for the
    /// watch all the same, so a track whose count stands still is seen whatever the writer thinks of it.
    fn look_for_the_watch(&mut self) {
        if nori_perf::invariants::on() {
            if let Some((frames, _)) = self.sink.heard(self.playing) {
                self.watched(frames);
            }
        }
    }

    /// Tops the track up when it is due, and says when to look again.
    fn fill(&mut self, now_ns: i64) -> Option<u64> {
        let ms = |frames: u64, rate: u32| frames * 1000 / rate.max(1) as u64;
        // Timed by what the track holds, not by what is pulled for it: a chunk left over from a track
        // that was full is not music the track can play, and counted in, a track too small to take much
        // was looked at again every fill tick.
        let fill = self.clock.in_track(now_ns);
        if self.drained {
            if self.ring.available() == 0 {
                return None;
            }
            // More music after the end was drained (the queue grew in its last seconds): the stopped
            // track is started again from nothing, all of the old music having been heard.
            self.restart(now_ns);
        } else if !self.filling && fill > self.low {
            self.look_for_the_watch();
            return Some(ms(fill - self.low, self.rate) + 1);
        }
        self.read_clock();
        if self.priming && self.playing && self.sink.heard(true).is_some_and(|(frames, _)| frames > 0) {
            // Started: deep again, in place, filled from here on.
            let got = self.sink.resize(self.allocated);
            self.holds(got);
            self.priming = false;
            self.filling = true;
        }
        if self.shallow && self.playing {
            if self.filling {
                // Filling from empty after a start or a flush is not the output running dry.
                self.needs.underruns = self.sink.underruns();
            } else {
                self.watch_output(now_ns);
            }
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
            self.filling = false;
            self.starved_ms = FILL_TICK_MS;
            // Never sooner than a fill tick: a track that says it is full with less than the low mark in
            // it would otherwise be asked again every millisecond.
            return Some((ms(fill.saturating_sub(self.low), self.rate) + 1).max(FILL_TICK_MS));
        }
        if self.ring.ending() && self.ring.available() == 0 {
            // The last of the music is in the track. One that starts only when full is told to play
            // what it has; either way the next look is when it has played out.
            self.filling = false;
            if self.starts_full && !self.drained {
                self.sink.stop();
                self.drained = true;
            }
            return Some(ms(self.clock.latency_frames(now_ns), self.rate) + 1);
        }
        // The ring had less than the track has room for. Between bursts that is only the engine decoding
        // the next one, and the track has plenty; while filling, or low, the engine is still decoding its
        // first burst or waiting for the network: look again soon, less often the longer it takes.
        if !self.filling && fill > self.low {
            return Some(ms(fill - self.low, self.rate) + 1);
        }
        let mut wait = self.starved_ms;
        self.starved_ms = (self.starved_ms * 2).min(STARVED_MAX_MS);
        if fill > 0 {
            // Never past half of what the track still holds. A ring kept shallower than the track (the
            // equalizer tuned) never fills it, so filling never ends; backing off to a second while each
            // look moved half a second in, the track ran dry about once a second.
            wait = wait.min((ms(fill, self.rate) / 2).max(FILL_TICK_MS));
        }
        Some(wait)
    }

    /// Moves what the ring has into the track, as much as it has room for. True when the track is
    /// full.
    fn top_up(&mut self, now_ns: i64) -> bool {
        let fb = self.frame_bytes();
        // 24-bit samples are pulled as floats and packed in place, so a chunk is what the floats fit.
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
            // Counted before the pull: the ring's read position moves in it, and the engine must never
            // see the ear ahead of where it is.
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
                // SAFETY: the staging memory is f32s, so it is aligned for i16 and twice as many of them
                // fit; the slice lives no longer than the borrow of the staging it was made from.
                let halves = unsafe { std::slice::from_raw_parts_mut(staging.as_mut_ptr() as *mut i16, staging.len() * 2) };
                self.ring.pull_i16(&mut halves[..samples])
            };
            if self.ring.flushed() {
                // The pull began the new music: what the track held of the old goes.
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

    /// Writes what is staged. False when the track would not take all of it (it is full), or died.
    fn write_staged(&mut self, now_ns: i64) -> bool {
        let (from, len) = self.staged;
        let taken = match self.sink.write(from, len) {
            Ok(n) => n.min(len),
            Err(code) => {
                self.revive(now_ns, code);
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

    /// The track would not take everything offered, which is only ever offered when the buffer it said it
    /// has has room for it: it holds less than it said (a sound server that gives less than asked, and
    /// says the size asked). What it holds now is at most what it can hold, since the ear lags what the
    /// track has let go: that is its size from now on, so the writer sleeps until that much has played
    /// down instead of asking again every fill tick. A small difference is the clock's, and left alone.
    ///
    /// Not after a flush of a track that held music, until it has been heard playing again: the platform
    /// still counts what the flush dropped until its mixer's next period, so the first writes find the
    /// track as full as it was. Taken for its size, a skip made just after a top-up counted a track of
    /// seconds as a tenth of one, under the start threshold it must fill before it plays after a flush:
    /// it never played again, while the engine's ring stood full and the player said it played. It is
    /// written to again at the next look instead. Never counted under that threshold either, which a
    /// track that holds so little could never start from.
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
}

impl<R: Ring> Writer<R> {
    /// The track refused a write with an error: it is dead. Another is opened in its place and filled
    /// from the ring; what the dead one held is lost, and the ear is where the ring is. One that will
    /// not open is the engine's to hear of.
    fn revive(&mut self, now_ns: i64, code: i32) {
        log(&format!("the AudioTrack failed a write ({code}): opening another"));
        self.reopen(now_ns, self.asked);
    }

    /// Another track in place of this one, asked for `frames`, empty and started as after a flush; one
    /// that will not open is the engine's to hear of.
    fn reopen(&mut self, now_ns: i64, frames: u64) {
        self.sink.release();
        self.asked = frames;
        let opened = self.opener.lock().open(self.format, self.float, frames);
        match opened {
            Ok(o) => {
                self.sink = o.sink;
                said_small(o.frames, self.asked, self.rate);
                self.allocated = o.frames;
                self.holds(o.frames);
                // Opened at its whole size: made shallow again if it is to be, its underruns its own.
                self.shallow = false;
                self.needs.underruns = None;
                if self.wants_shallow {
                    self.resize();
                }
                self.starts_full = o.starts_full;
                self.staged = (0, 0);
                self.drained = false;
                self.flushed_full = false;
                self.revived = true;
                self.sink.set_volume(self.volume);
                // The new track counts its frames from nought, as after a flush.
                self.clock.update(|c| {
                    c.ahead = 0;
                    c.given = 0;
                    c.heard = 0;
                    c.at_ns = now_ns;
                    c.running = false;
                });
                if self.playing {
                    self.start(now_ns);
                } else {
                    self.filling = true;
                }
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

/// What the writer needs to open a track again: the opener, the size asked for, and where to say it
/// could not.
pub(crate) struct Reopen {
    pub opener: Arc<Mutex<Box<dyn Opener>>>,
    pub frames: u64,
    pub failure: Arc<Mutex<Option<String>>>,
}

/// Shared by the output (on the engine's thread), its writer thread and the app's doors.
#[derive(Default)]
pub(crate) struct Shared {
    control: Mutex<Control>,
    writer: Mutex<Option<Thread>>,
    pub clock: Arc<Clock>,
    /// Bytes handed to the track since the output was made, for the test bridge.
    pub bytes: Arc<AtomicU64>,
    /// Told whenever the track's route changes.
    pub watch: Mutex<Option<DeviceWatch>>,
    /// Why the track died and would not open again, until the engine has heard.
    failure: Arc<Mutex<Option<String>>>,
    /// How deep the shallow track found it must be, for the engine.
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
    /// `float` is the high quality output setting: the track takes float samples, else 16-bit ones.
    pub(crate) fn new(opener: Box<dyn Opener>, float: bool, shared: Arc<Shared>) -> TrackOutput {
        TrackOutput { opener: Arc::new(Mutex::new(opener)), float, shared, format: None, thread: None }
    }
}

impl AudioOutput for TrackOutput {
    fn watch(&mut self, changed: DeviceWatch) {
        *self.shared.watch.lock() = Some(changed);
    }

    /// The song's own rate as it is (a DAC asked for bit-perfect gets exactly that), in mono or stereo.
    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
        // Past 192 kHz, halved within its family (352.8 kHz to 176.4, not 192), the ring converting.
        let f = OutputFormat { rate: nori_player::policy::capped_rate(want.rate, 192_000).clamp(8_000, 192_000), channels: want.channels.clamp(1, 2), bits: want.bits };
        self.format = Some(f);
        Ok(f)
    }

    fn start(&mut self, feed: Feed) -> Result<(), String> {
        let format = self.format.ok_or("the output was not opened")?;
        self.close();
        // The engine's thread, which starts the output, decodes what the track is fed: it takes the
        // writer's priority too.
        audio_priority("the engine");
        // Opened deep, always: while tuned it is made shallow in place, before anything is written.
        let shallow = self.shared.control.lock().shallow;
        let frames = track_frames(format.rate, false);
        let opened = self.opener.lock().open(format, self.float, frames).inspect_err(|e| log(&format!("the AudioTrack would not open: {e}")))?;
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

    /// Android's AudioTrack takes float at any rate; whether it gets it is the setting's.
    fn takes_float(&mut self) -> bool {
        true
    }

    /// The setting as it is now: the next track is opened for it.
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

    /// The track's size changes at once, in place ([`Sink::resize`]).
    fn shallow(&mut self, on: bool) {
        self.shared.tell(|c| c.shallow = on);
    }

    fn resizes(&self) -> bool {
        true
    }

    /// As deep as the writer found the output it plays on needs, once it has looked.
    fn shallow_depth(&self) -> Option<ShallowDepth> {
        self.shared.depth.get()
    }

    /// Seconds of music sit in the track after the ring has run empty: the end is when it has played them.
    fn holding(&self) -> bool {
        self.shared.clock.latency_frames(mono_ns()) > 0
    }

    /// The ring is emptied into the track every ten seconds or so, and stands still in between.
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

/// One line in the app's log.
fn log(message: &str) {
    nori_core::alog::info(&format!("rust track: {message}"));
}

/// Whether a track for `format` takes 24-bit samples, packed: a song of more than 16 bits handed over as
/// it is (bit-perfect, `OutputFormat::bits`), when the setting does not ask for float.
pub(crate) fn packed24(format: OutputFormat, float: bool) -> bool {
    !float && format.bits > 16
}

/// Bytes one sample takes in the track.
pub(crate) fn sample_bytes(float: bool, packed: bool) -> usize {
    if float {
        4
    } else if packed {
        3
    } else {
        2
    }
}

/// The first `samples` floats of `staging` as 24-bit samples, three bytes each, packed from its start in
/// place: each is read before anything is written over it, as the bytes written trail those read. A
/// song's own 24 bits come back exactly (the ring carries them as floats, which hold 24 bits).
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

/// The low mark for a track of `frames`: [`LOW_US`], or half of a track too small for that, so the
/// writer wakes once per half of what it holds and never more often than the buffer demands.
fn low_mark(frames: u64, rate: u32) -> u64 {
    ((rate as i64 * LOW_US / 1_000_000) as u64).min(frames / 2)
}

/// A track that gave less than was asked is said in the log once, as it is opened: it is topped up more
/// often, which is what its wakeups are.
fn said_small(frames: u64, asked: u64, rate: u32) {
    if frames < asked {
        let ms = |f: u64| f * 1000 / rate.max(1) as u64;
        log(&format!("the AudioTrack holds {} ms of the {} ms asked: topped up every {} ms or so", ms(frames), ms(asked), ms(frames - low_mark(frames, rate)).max(FILL_TICK_MS)));
    }
}

/// The calling thread at Android's `THREAD_PRIORITY_AUDIO`, as `Process.setThreadPriority` sets it (a
/// nice value, which an app may lower this far).
/// Elsewhere (the tests on a Mac) there is no thread id to give a nice value to, and nothing to do.
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
            }
        }

        fn refill(&mut self) {
            let want = match self.engine {
                Engine::Bursts => self.burst,
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
    fn while_music_plays_the_track_is_topped_up_about_every_ten_seconds_with_the_engine_woken_at_once() {
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
        // Every top-up took the whole ring past its low mark: each woke the engine for its next burst, so it
        // needs no timer of its own.
        assert_eq!(refills, wakes, "one burst decoded for every top-up");
    }

    #[test]
    fn a_track_that_gives_less_than_asked_is_topped_up_once_per_half_of_it() {
        // A sound server that gives a third of a second of the eleven and a half asked. Timed by what was
        // pulled for it rather than what it held, the writer looked every fill tick: fifty wakes a second.
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
    fn a_track_that_holds_less_than_it_says_is_counted_as_what_it_holds() {
        // Says it holds the eleven and a half seconds asked, and takes half a second.
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
    fn a_song_s_24_bit_samples_are_packed_in_place_bit_for_bit() {
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
    fn the_play_head_counts_on_past_its_32_bits() {
        let mut h = HeadCount::default();
        assert_eq!(h.read(10), 10);
        assert_eq!(h.read(u32::MAX - 5), u32::MAX as u64 - 5);
        assert_eq!(h.read(20), (1u64 << 32) + 20, "the wrap is counted, not read as the start again");
        assert_eq!(h.read(30), (1u64 << 32) + 30);
    }

    #[test]
    fn a_track_that_dies_is_opened_again_and_the_music_goes_on() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(15_000);
        let wakes = s.wakes;
        s.track.lock().dead = true;
        // Its next top-up finds it dead: another is opened and filled, and the music goes on from the ring.
        s.run(15_000);
        let t = s.track.lock();
        assert_eq!(t.reopened, 1, "one track opened in place of the dead one");
        assert!(t.buffered > 0 && t.started, "the new one is filled and playing: {} buffered, started {}", t.buffered, t.started);
        assert!(s.wakes - wakes < 10, "no retrying every millisecond: {} wakes", s.wakes - wakes);
        assert!(s.failure.lock().is_none());
    }

    #[test]
    fn a_track_that_dies_and_will_not_open_again_is_the_engine_s_to_hear_of() {
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
    fn a_start_fills_the_track_before_anything_else_and_then_sleeps() {
        let mut s = Sim::new(600, false, true);
        s.play();
        s.run(1_500);
        let t = s.track.lock();
        assert_eq!(t.buffered + t.played, t.capacity, "filled to full, so a track that waits for that starts");
        assert!(t.ready && t.played > 0, "and it plays");
        drop(t);
        // One wake to fill it, two to read the device's clock as it settles.
        assert!(s.wakes <= 4, "the first second and a half took {} wakes", s.wakes);
        let next = s.next.unwrap() - s.now();
        assert!(next > 8_000 * MS, "then it sleeps until the track is low: {} ms", next / MS);
    }

    #[test]
    fn the_clock_says_how_far_behind_the_ring_the_ear_is() {
        let mut s = Sim::new(600, true, false);
        s.play();
        s.run(3_333);
        let t = s.track.lock();
        let latency = s.clock.latency_frames(s.now());
        assert!(latency.abs_diff(t.buffered) <= RATE as u64 / 1000, "latency {latency} frames, {} in the track", t.buffered);
        assert_eq!(t.written_bytes, (t.buffered + t.played) * 8, "float stereo: eight bytes a frame");
    }

    #[test]
    fn a_flush_empties_the_track_and_it_fills_with_the_new_music() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(4_000);
        s.ring.lock().flush(-0.25);
        // The engine's flush wakes the writer.
        s.wake();
        let t = s.track.lock();
        assert_eq!(t.flushes, 1);
        assert!(t.last < 0.0, "what the track holds now is the new music");
        assert_eq!(t.played, 0, "counted again from the flush");
        assert_eq!(s.clock.latency_frames(s.now()), t.buffered, "and the clock with it");
    }

    /// The S22's silence after next was pressed fast and long: every skip is a flush of a track the songs
    /// from the stream cache had just filled, and the platform lets go of what a flush dropped only at its
    /// mixer's next period, so the write straight after it finds the track still full of the old music. A
    /// track that refused a write so was once taken to hold no more than that (a tenth of a second), under
    /// the quarter of a second it must hold before it starts after a flush: it never started again, the
    /// engine's ring stood full, and the player said it played. A flush of the same track (the equalizer
    /// switched) kept the size; only a new track, the output opened again, brought the music back.
    #[test]
    fn next_pressed_fast_over_a_full_track_leaves_it_playing() {
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
                // Each song from the cache: its first burst is in the ring at once.
                s.ring.lock().flush(if k % 2 == 0 { -0.25 } else { 0.25 });
                // The engine's flush wakes the writer, now or a moment later.
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

    /// A pause stops the track's clock at once, on the engine's thread (`TrackOutput::pause`), so the ear is
    /// read where it stopped. Played again before the writer woke, the writer never saw the pause: the track
    /// played on, and so must the clock that times its top-ups, or it stands with the track's seconds
    /// counted as still in it and the track runs dry while the player says it plays.
    #[test]
    fn a_pause_taken_back_before_the_writer_woke_leaves_the_music_playing() {
        let mut s = Sim::new(600, false, false);
        s.play();
        s.run(3_000);
        // `TrackOutput::pause` and `resume`, one after the other.
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

    /// A flush on a track that starts only once full (before Android 12), the engine decoding the new
    /// music half a second at a time: the track is heard again within a moment, not once all of its
    /// eleven seconds have been decoded and written; then it is deep again, and never runs dry.
    #[test]
    fn a_track_that_starts_only_full_is_heard_again_at_once_after_a_flush() {
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
    fn paused_it_writes_nothing_and_sleeps_until_told() {
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
    fn a_fade_runs_at_the_track_s_volume_and_ticks_only_while_it_lasts() {
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
    fn a_ring_kept_shallower_than_the_track_never_lets_it_run_dry() {
        // The phone's report: the equalizer screen's shallow buffer reached the engine's ring at a song's
        // start, and from there the ring held half a second, topped up on the engine's 200 ms timer, while
        // the track was the deep one. Filling the track after that flush never ended, and the writer
        // backed off to a second between looks while each moved half a second in: a gap about once a
        // second.
        let mut s = Sim::new(600, false, false);
        s.ring.lock().kept(Engine::Timer { cap: RATE as usize / 2, every: 200 * MS });
        s.ring.lock().flush(0.25);
        s.late = 20 * MS;
        s.play();
        s.run(60_000);
        assert_eq!(s.track.lock().underruns, 0, "never runs dry");
    }

    /// The ring the engine keeps while the equalizer is tuned, in frames.
    fn shallow_ring() -> usize {
        (RATE as i64 * nori_engine::output::SHALLOW_US / 1_000_000) as usize
    }

    #[test]
    fn tuned_the_track_is_made_shallow_in_place_and_never_runs_dry_for_a_late_writer_and_a_jittery_mixer() {
        let mut s = Sim::new(600, false, false);
        // A mixer reading 20 ms at a time, up to 8 ms late; a writer woken up to 30 ms late.
        s.mixed(20, 8);
        s.late = 30 * MS;
        s.play();
        s.run(15_000);
        // The equalizer screen: the engine keeps its ring shallow and says so; a band moved before the
        // deep seconds have played out makes the music again where the ear is (a flush), and from then on
        // the engine wakes up to 15 ms late whenever the ring runs down to half.
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
        // The screen closed: deep again at once, and in bursts.
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

    /// The equalizer screen opened and closed `rounds` times over a track that records what it plays,
    /// read by a jittery mixer and written by a late writer: the longest silence heard and the frames
    /// heard out of their order.
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
            // Opened while the track is full, or while the deep seconds are still playing out.
            s.control.shallow = true;
            s.ring.lock().kept(Engine::Shallow { cap: shallow_ring(), late: 15 * MS });
            s.wake();
            assert_eq!(s.track.lock().size, track_frames(RATE, true), "round {round}: shallow at once");
            // What the ring and the track held of the deep buffer plays out first; then a band moved is
            // heard within a quarter of a second.
            s.run(25_000);
            s.deepest = 0;
            s.run(5_000);
            let ms = s.deepest * 1000 / RATE as u64;
            assert!(ms <= 250, "round {round}: a band moved is heard {ms} ms later at most");
            s.control.shallow = false;
            s.ring.lock().kept(Engine::Bursts);
            s.wake();
            assert_eq!(s.track.lock().size, deep, "round {round}: deep at once");
            // Closed again before the deep buffer has filled, once in a while.
            s.run(if round % 2 == 0 { 20_000 } else { 3_000 });
        }
        let t = s.track.lock();
        assert_eq!((t.reopened, t.flushes, t.underruns), (0, 0, 0), "never opened again, emptied or run dry");
        assert_eq!(t.resizes as usize, rounds * 2);
        assert!(t.played > 0 && t.last_played.is_some());
        (t.longest_silence, t.jumps)
    }

    #[test]
    fn the_equalizer_screen_opening_and_closing_is_not_heard_either_way() {
        for starts_full in [false, true] {
            let (silence, jumps) = tuned_back_and_forth(starts_full, 6);
            let ms = silence as f64 * 1000.0 / RATE as f64;
            assert!(ms <= 5.0, "starts full {starts_full}: {ms} ms of silence at a switch");
            assert_eq!(jumps, 0, "starts full {starts_full}: every frame heard, in order");
        }
    }

    #[test]
    fn a_track_made_shallow_plays_what_it_holds_and_takes_more_once_below_its_new_size() {
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
    fn a_track_that_died_while_tuned_is_opened_deep_and_made_shallow_again() {
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

    /// Tuned over a Bluetooth output ([`Sim::bluetooth`], which says what it is or not) after 15 s played
    /// deep: made shallow in place, a band moved made again at once (a flush) as the engine does, and
    /// the engine's ring following what the writer found. Returns the sim, playing shallow.
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
    fn tuned_over_bluetooth_the_track_is_as_deep_as_the_output_needs_and_never_runs_dry() {
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
        // Closed: deep again, in place.
        s.control.shallow = false;
        s.ring.lock().kept(Engine::Bursts);
        s.wake();
        assert_eq!(s.track.lock().size, track_frames(RATE, false));
        s.run(30_000);
        assert_eq!(s.track.lock().underruns, underruns);
    }

    #[test]
    fn tuned_over_an_output_that_says_nothing_the_track_grows_for_what_it_sees_and_stops_running_dry() {
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
    fn the_shallow_marks_are_the_speaker_s_for_an_output_that_needs_no_more_and_grow_with_what_it_does() {
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

    /// An AudioTrack playing on the real clock, keeping every sample written since its last flush.
    #[derive(Default)]
    struct Live {
        written: Vec<i16>,
        played_before: u64,
        since: Option<std::time::Instant>,
        flushes: u32,
        volumes: Vec<f32>,
        /// As a phone's track is (the S22's): it holds only the buffer it was opened with (`bounded`), plays
        /// nothing after a flush until it holds its start threshold, and a flush straight after a pause of
        /// the music is done only at the mixer's next period (`defer`), what it dropped taking up room until
        /// then. Off, it takes everything and plays at once.
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

        /// The mixer's period since a pause came round: a flush waiting for it is done.
        fn mixed(&mut self) {
            if self.pausing_until.is_some_and(|t| std::time::Instant::now() >= t) {
                self.pausing_until = None;
                self.stale = 0;
            }
        }

        /// Started, the flush before done, and holding what it must to play: the clock runs from now.
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
    fn the_engine_plays_through_it_sample_for_sample_and_its_fades_and_jumps_reach_the_track() {
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

        // A jump to the second song a second in: the track is emptied, and holds the new music only.
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

        // A pause fades at the track's volume, then the track stops.
        live.lock().volumes.clear();
        engine.pause();
        assert!(wait(5, || live.lock().since.is_none()), "paused");
        let l = live.lock();
        // A fade, not a cut: more than one step, only ever down. How many steps fit in the 200 ms depends
        // on how often a loaded machine wakes the writer, so the count is not the test.
        assert!(l.volumes.len() >= 2, "the fade out ran in steps: {:?}", l.volumes);
        assert!(l.volumes.windows(2).all(|w| w[1] <= w[0]), "only ever down: {:?}", l.volumes);
        assert_eq!(l.volumes.last(), Some(&0.0));
        drop(l);

        // Played on to the end of the queue: over only once the track has played its last frame.
        engine.play();
        assert!(wait(6, || engine.status().state == nori_engine::State::Ended), "the queue ends");
        let l = live.lock();
        assert!(l.played() + RATE as u64 / 10 >= l.written.len() as u64 / 2, "{} of {} frames heard at the end", l.played(), l.written.len() / 2);
        drop(l);
        engine.stop();
    }

    /// Songs as WAV files on the disk, as the stream cache keeps them: opened and decoded at once.
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

    /// The phone's report, end to end: the equalizer and AutoMix on, next pressed a dozen times and more,
    /// most a few to two hundred milliseconds apart and every fourth once the track has been filled to the
    /// brim again, through songs the stream cache has whole; the engine on its own thread and
    /// this output over a track that behaves as a phone's does (its buffer, its start threshold, a flush
    /// done at the mixer's next period). The music must be heard again after the last press, and go on.
    #[test]
    fn the_engine_through_it_plays_on_after_next_is_pressed_fast_and_long() {
        const SECS: u32 = 30;
        let ms = SECS as i64 * 1000;
        let ids: Vec<String> = (0..20).map(|k| format!("s{k}")).collect();
        let files = Arc::new(Files { dir: nori_testdir::TempDir::new("nori-android-skips"), ms });
        // One song's bytes under every name.
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
        // Measured already, as the core's measurer does it off the engine's thread: the simulated app would
        // measure whole songs on it.
        for id in &ids {
            let a = nori_player::types::TrackAnalysis { song_id: id.clone(), analysis_version: nori_player::automix::ANALYSIS_VERSION, duration_ms: ms, bpm: 120.0, bpm_confidence: 1.0, lufs: -14.0, silence_end_ms: ms, mixramp_end_ms: ms, outro_start_ms: ms - 8_000, ..Default::default() };
            app.analyses.insert(id.clone(), a);
        }
        let bands = vec![nori_player::dsp::Band { kind: nori_player::dsp::PEAKING, freq: 1000.0, gain_db: 6.0, q: 1.0, channel: 0 }];
        let settings = nori_engine::Settings { sound: nori_engine::Sound { bands, ..Default::default() }, auto_mix: true, ..Default::default() };
        let config = nori_engine::Config { settings, ..Default::default() };
        let engine = nori_engine::Engine::start(OnDisk(files.clone()), app, queue, Box::new(output), None, config, |_| {});
        engine.queue_changed();
        // The place is read four times a second, as the screen asks for it while it is open.
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
            // As the app skips: a jump to the song after the one on the screen.
            engine.go_to(k, 0);
            if k % 4 == 0 {
                // A breath between presses: the song pressed to fills the track to the brim again.
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
    fn at_the_end_a_track_that_starts_only_full_is_told_to_play_what_it_has() {
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
