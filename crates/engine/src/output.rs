//! The sound card side. A platform implements [`AudioOutput`] and calls [`Feed::pull`] from the device
//! thread. The feed reads a lock-free ring of float samples (device rate and channels) that the engine
//! fills in bursts: pulling never blocks, allocates or locks. [`RingTrack`] converts the chain's audio
//! into the ring, resampling only when the device will not take the stream's rate, and maps ring frames
//! back to song time for the playhead.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::Thread;

pub use nori_player::outputs::OutputKind;

use nori_player::automix::resample::Resampler;
use nori_player::burst::{BUFFER_US, LOW_US};
use nori_player::pcm::{Encoding, Format};
use nori_player::pipeline::Track;

/// What a device plays: float samples, interleaved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFormat {
    pub rate: u32,
    pub channels: usize,
    /// Bit-perfect output: the song's own bits per sample, for a device that can take them. 0 otherwise.
    pub bits: u32,
}

/// The device the music goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub kind: OutputKind,
    pub name: String,
}

/// Told the new device whenever it changes (from any thread).
pub type DeviceWatch = Box<dyn Fn(Device) + Send + Sync>;

/// A sound card, or anything that takes music like one (a file). Called only on the engine's thread;
/// the device thread only calls [`Feed::pull`].
pub trait AudioOutput: Send {
    /// Registers `changed`, told the device now and whenever the system moves the music (headphones,
    /// Bluetooth), so each device can have its own sound. Optional.
    fn watch(&mut self, _changed: DeviceWatch) {}
    /// Picks the device format closest to `want`. Nothing plays yet.
    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String>;
    /// The device pulls from `feed` from now on; it starts paused.
    fn start(&mut self, feed: Feed) -> Result<(), String>;
    /// Stops pulling; the device may sleep.
    fn pause(&mut self);
    fn resume(&mut self);
    /// How long a sample pulled now takes to be heard, µs.
    fn latency_us(&self) -> u64;
    /// Whether the device takes float ([`Feed::pull`]) rather than 16-bit ([`Feed::pull_i16`]). With
    /// high quality output, songs are then decoded and carried in float.
    fn takes_float(&mut self) -> bool {
        false
    }
    /// High quality output switched: a device opening in either format opens in float from its next
    /// opening only while on (the engine reopens it at the next song).
    fn float(&mut self, _on: bool) {}
    /// The ring's music was dropped (seek, jump) or made again from before what the device took (a sound
    /// change): an output with a buffer of seconds drops its own too and gives back what it did not play
    /// ([`Feed::rewind`]); [`Feed::flushed`] marks where the new music starts.
    fn flush(&mut self) {}
    /// Fades from `from` (or where it is) to `target` over `ms`. An output holding seconds runs it on
    /// the device's volume, where it is heard at once, and returns true; otherwise the ring runs it.
    fn ramp(&mut self, _from: Option<f32>, _target: f32, _ms: i64) -> bool {
        false
    }
    /// The device still holds unplayed music taken from the ring.
    fn holding(&self) -> bool {
        false
    }
    /// The device pulls in bursts of seconds: the engine sleeps until the pull that crosses the low
    /// mark wakes it, with no timer.
    fn bursts(&self) -> bool {
        false
    }
    /// Hold only a fraction of a second (equalizer tuning), so a sound change is heard without the device
    /// dropping what it holds, or the deep buffer again. Also told before starting.
    fn shallow(&mut self, _on: bool) {}
    /// The device stopped and could not reopen (a dead sound server); the engine checks after
    /// [`Feed::wake_engine`], stops, reports it and releases the output.
    fn failed(&mut self) -> Option<String> {
        None
    }
    fn close(&mut self);
}

/// Ring fill at which the engine is woken: a little under the burst's low mark, so the burst's own
/// count (which includes the device) agrees it is time.
pub const WAKE_LOW_US: i64 = LOW_US - 250_000;
/// A device holding more than this takes too long to play out for a sound change to wait: it drops
/// what it holds and the change starts where it is.
const HELD_US: i64 = 250_000;
/// How far before the device's play head such a change starts: what its clock reading may be ahead
/// of it. The device gives back exactly what it did not play ([`Feed::rewind`]).
const REWIND_EARLY_US: i64 = 100_000;

/// Ring room beyond the deep buffer: resampler rounding and a device's first pull.
const SLACK_US: i64 = 2_000_000;
/// A flush reaches the device ([`AudioOutput::flush`]) once this much new music is in the ring, or at
/// the turn's end: told at once, a deep device found the ring empty and waited a tick (a phone's track
/// waits for a quarter second of music before starting).
const TELL_FLUSH_US: i64 = 300_000;

/// Shared by the engine thread (sole writer) and the device thread (sole reader).
pub(crate) struct Ring {
    /// Float samples as bits: no data race even when a flush lets the writer overrun the reader.
    slots: Box<[AtomicU32]>,
    frames: u64,
    channels: usize,
    rate: u32,
    bits: u32,
    /// Frames written and read since creation. `write` goes back when music not yet taken is replaced.
    write: AtomicU64,
    read: AtomicU64,
    /// The furthest frame the reader's pull may take, stored before it looks at `write`: music before
    /// it may be in the device already, music after it can still be replaced.
    limit: AtomicU64,
    /// Flush: frames before this are dropped; a pull finding it moved says so ([`Feed::flushed`]).
    discard: AtomicU64,
    flushes: AtomicU64,
    /// The engine sleeps until the ring falls to `low` frames; the reader wakes it once.
    waiting: AtomicBool,
    low: AtomicU64,
    engine: Thread,
    /// A fade run per sample by the reader: from (NaN: current), to, length, and a generation bumped
    /// per request.
    gain_from: AtomicU32,
    gain_target: AtomicU32,
    ramp_frames: AtomicU32,
    ramp_gen: AtomicU32,
    /// The music is over: running dry is not an underrun.
    ended: AtomicBool,
    underruns: AtomicU64,
}

impl Ring {
    fn new(format: OutputFormat, engine: Thread) -> Ring {
        let frames = (format.rate as i64 * (BUFFER_US + SLACK_US) / 1_000_000) as u64;
        let slots: Box<[AtomicU32]> = (0..frames as usize * format.channels).map(|_| AtomicU32::new(0)).collect();
        RING_BYTES.fetch_add(std::mem::size_of_val(&*slots) as u64, Ordering::Relaxed);
        Ring {
            slots,
            frames,
            channels: format.channels,
            rate: format.rate,
            bits: format.bits,
            write: AtomicU64::new(0),
            read: AtomicU64::new(0),
            limit: AtomicU64::new(0),
            discard: AtomicU64::new(0),
            flushes: AtomicU64::new(0),
            waiting: AtomicBool::new(false),
            low: AtomicU64::new(0),
            engine,
            gain_from: AtomicU32::new(f32::NAN.to_bits()),
            gain_target: AtomicU32::new(1f32.to_bits()),
            ramp_frames: AtomicU32::new(0),
            ramp_gen: AtomicU32::new(0),
            ended: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
        }
    }

    /// The reader's position, after any flush.
    fn read_at(&self) -> u64 {
        self.read.load(Ordering::Acquire).max(self.discard.load(Ordering::Acquire))
    }

    /// Frames written and not yet read.
    fn filled(&self) -> u64 {
        self.write.load(Ordering::Acquire).saturating_sub(self.read_at())
    }

    fn ramp(&self, from: Option<f32>, target: f32, ms: i64) {
        self.gain_from.store(from.unwrap_or(f32::NAN).to_bits(), Ordering::Relaxed);
        self.gain_target.store(target.to_bits(), Ordering::Relaxed);
        self.ramp_frames.store((ms.max(0) as u64 * self.rate as u64 / 1000) as u32, Ordering::Relaxed);
        self.ramp_gen.fetch_add(1, Ordering::Release);
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        RING_BYTES.fetch_sub(std::mem::size_of_val(&*self.slots) as u64, Ordering::Relaxed);
    }
}

/// Bytes held by all rings, for the perf report. Process-wide: the report reads memory per process.
static RING_BYTES: AtomicU64 = AtomicU64::new(0);

/// Bytes held by all rings now.
pub fn ring_bytes() -> u64 {
    RING_BYTES.load(Ordering::Relaxed)
}

/// The device thread's end of the ring.
pub struct Feed {
    ring: Arc<Ring>,
    gain: f32,
    target: f32,
    step: f32,
    gen: u32,
    /// The last flush seen, and whether one happened since [`Feed::flushed`] was asked.
    seen: u64,
    flushed: bool,
}

impl Feed {
    fn new(ring: Arc<Ring>) -> Feed {
        Feed { ring, gain: 1.0, target: 1.0, step: 0.0, gen: 0, seen: 0, flushed: false }
    }

    /// Whether a pull found a flush since last asked: the last pull's frames are new music, and what the
    /// device held before should go. A pull never mixes the two.
    pub fn flushed(&mut self) -> bool {
        std::mem::take(&mut self.flushed)
    }

    pub fn format(&self) -> OutputFormat {
        OutputFormat { rate: self.ring.rate, channels: self.ring.channels, bits: self.ring.bits }
    }

    /// Fills `out` (interleaved) with music, then silence. Returns the frames of music. Lock- and
    /// allocation-free, for a real-time thread.
    pub fn pull(&mut self, out: &mut [f32]) -> usize {
        self.pull_as(out, |v| v)
    }

    /// [`Feed::pull`] into 16-bit samples, for a device that takes nothing else.
    pub fn pull_i16(&mut self, out: &mut [i16]) -> usize {
        self.pull_as(out, |v| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
    }

    fn pull_as<S: Copy + Default>(&mut self, out: &mut [S], conv: impl Fn(f32) -> S) -> usize {
        let r = &*self.ring;
        let ch = r.channels;
        let want = (out.len() / ch) as u64;
        // Said before `write` is read: the engine replaces nothing this pull may take ([`RingTrack::freeze`]).
        let limit = r.read.load(Ordering::Acquire).max(r.discard.load(Ordering::Acquire)) + want;
        r.limit.store(limit, Ordering::SeqCst);
        let w = r.write.load(Ordering::SeqCst);
        // Read after `write`, so music written after a flush comes with the flush.
        let flushes = r.flushes.load(Ordering::Acquire);
        if flushes != self.seen {
            self.seen = flushes;
            self.flushed = true;
        }
        let at = r.read.load(Ordering::Acquire).max(r.discard.load(Ordering::Acquire));
        let n = w.saturating_sub(at).min(limit.saturating_sub(at));
        let gen = r.ramp_gen.load(Ordering::Acquire);
        if gen != self.gen {
            self.gen = gen;
            let from = f32::from_bits(r.gain_from.load(Ordering::Relaxed));
            if !from.is_nan() {
                self.gain = from;
            }
            self.target = f32::from_bits(r.gain_target.load(Ordering::Relaxed));
            let frames = r.ramp_frames.load(Ordering::Relaxed);
            self.step = if frames == 0 { self.target - self.gain } else { (self.target - self.gain) / frames as f32 };
        }
        let slot = |f: u64| (f % r.frames) as usize * ch;
        let mut i = 0;
        // A fade, frame by frame.
        while i < n && self.gain != self.target {
            self.gain += self.step;
            if (self.step > 0.0 && self.gain > self.target) || (self.step < 0.0 && self.gain < self.target) || self.step == 0.0 {
                self.gain = self.target;
            }
            let (s, o) = (slot(at + i), i as usize * ch);
            for c in 0..ch {
                out[o + c] = conv(f32::from_bits(r.slots[s + c].load(Ordering::Relaxed)) * self.gain);
            }
            i += 1;
        }
        // Then at one level, up to the ring's end and from its start.
        let g = self.gain;
        while i < n {
            let (s, o) = (slot(at + i), i as usize * ch);
            let run = (n - i).min(r.frames - (at + i) % r.frames) as usize * ch;
            for (v, slot) in out[o..o + run].iter_mut().zip(&r.slots[s..s + run]) {
                *v = conv(f32::from_bits(slot.load(Ordering::Relaxed)) * g);
            }
            i += (run / ch) as u64;
        }
        out[n as usize * ch..].fill(S::default());
        r.read.store(at + n, Ordering::Release);
        r.limit.store(at + n, Ordering::Release);
        if n < want && !r.ended.load(Ordering::Relaxed) && w > 0 {
            r.underruns.fetch_add(1, Ordering::Relaxed);
        }
        // At the low mark: wake the engine for the next burst, once.
        if w.saturating_sub(at + n) <= r.low.load(Ordering::Relaxed) && r.waiting.load(Ordering::Relaxed) && r.waiting.swap(false, Ordering::AcqRel) {
            r.engine.unpark();
        }
        n as usize
    }

    /// Takes back `frames` of the music pulled (a device that dropped what it held and did not play),
    /// never before the last flush's new music.
    pub fn rewind(&mut self, frames: u64) {
        let r = &*self.ring;
        let back = r.read.load(Ordering::Acquire).saturating_sub(frames).max(r.discard.load(Ordering::Acquire));
        r.read.store(back, Ordering::Release);
    }

    /// Frames pulled since the first frame of the last flush's new music: more than a pull since the
    /// flush took when the music was made again from before where the device got to (a sound change)
    /// rather than jumped. What [`Feed::rewind`] can give back.
    pub fn behind(&self) -> u64 {
        let r = &*self.ring;
        r.read.load(Ordering::Acquire).saturating_sub(r.discard.load(Ordering::Acquire))
    }

    /// Wakes the engine, e.g. for [`AudioOutput::failed`].
    pub fn wake_engine(&self) {
        self.ring.engine.unpark();
    }

    /// The engine waits for the low mark (a pull that clears this woke it). For test devices.
    pub fn engine_waits(&self) -> bool {
        self.ring.waiting.load(Ordering::Acquire)
    }

    /// Frames of music waiting in the ring.
    pub fn available(&self) -> usize {
        self.ring.filled() as usize
    }

    /// The music is over: the ring holds the last of it.
    pub fn ending(&self) -> bool {
        self.ring.ended.load(Ordering::Acquire)
    }

    /// The music is over and all pulled.
    pub fn finished(&self) -> bool {
        self.ring.ended.load(Ordering::Acquire) && self.ring.filled() == 0
    }
}

/// Where a written stretch ends: ring frames and sink frames since the flush, and song time.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Stretch {
    ring: u64,
    sink: u64,
    media: f64,
}

/// The point at ring frame `ring` between stretch ends `a` and `b`.
fn between(a: Stretch, b: Stretch, ring: u64) -> Stretch {
    let span = b.ring.saturating_sub(a.ring);
    let k = if span > 0 { (ring.clamp(a.ring, b.ring) - a.ring) as f64 / span as f64 } else { 0.0 };
    Stretch { ring, sink: a.sink + ((b.sink - a.sink) as f64 * k).round() as u64, media: a.media + (b.media - a.media) * k }
}

/// The [`Track`] under the engine's sink: the ring, the device, and the map from ring frames to sink
/// frames and song time.
pub(crate) struct RingTrack {
    output: Box<dyn AudioOutput>,
    ring: Option<Arc<Ring>>,
    device: Option<OutputFormat>,
    /// What the device was asked for (it may have given something else).
    asked: Option<OutputFormat>,
    format: Option<Format>,
    resampler: Option<Resampler>,
    converted: Vec<u8>,
    engine: Thread,
    /// The ring's write position at the last flush.
    base: u64,
    /// End of each written stretch at its pace (at 1x without resampling a song is one), the start of
    /// the first still ahead of the playhead, and the end of what was written.
    marks: VecDeque<Stretch>,
    from: Stretch,
    written: Stretch,
    /// After a cut: ring frames blended so far from what was there into what is written, of how many.
    blend: Option<(u64, u64)>,
    playing: bool,
    /// Whether the device takes float, once asked.
    float: Option<bool>,
    /// Why the device would not open, until reported.
    pub failed: Option<String>,
    /// Bit-perfect: the device opens at each song's own format and bits, reopening between songs that
    /// differ (`Track::must_reopen`).
    pub(crate) exact: bool,
    /// Bits per sample of the next stream.
    bits: u32,
    /// Highest device rate, Hz (0: the song's own); ignored when bit-perfect.
    pub(crate) max_rate: u32,
    /// High quality output is on, and was when the device opened.
    float_on: bool,
    opened_float: bool,
    /// As last told to [`AudioOutput::shallow`].
    shallow: bool,
    /// A flush not yet told to the device ([`TELL_FLUSH_US`]), frames written since, and a fade to apply
    /// with it.
    untold: bool,
    since_flush: u64,
    held_ramp: Option<(Option<f32>, f32, i64)>,
}

impl RingTrack {
    pub(crate) fn new(output: Box<dyn AudioOutput>) -> RingTrack {
        RingTrack {
            output,
            ring: None,
            device: None,
            asked: None,
            format: None,
            resampler: None,
            converted: Vec::new(),
            engine: std::thread::current(),
            base: 0,
            marks: VecDeque::with_capacity(64),
            from: Stretch::default(),
            written: Stretch::default(),
            blend: None,
            playing: false,
            float: None,
            failed: None,
            exact: false,
            bits: 0,
            max_rate: 0,
            float_on: false,
            opened_float: false,
            shallow: false,
            untold: false,
            since_flush: 0,
            held_ramp: None,
        }
    }

    /// The device format for `format`: as is (with bits) when bit-perfect, else the rate capped within its
    /// family.
    fn wanted(&self, format: Format) -> OutputFormat {
        if self.exact {
            return OutputFormat { rate: format.rate, channels: format.channels, bits: self.bits };
        }
        OutputFormat { rate: nori_player::policy::capped_rate(format.rate, self.max_rate), channels: format.channels, bits: 0 }
    }

    /// Whether the device is open.
    pub(crate) fn opened(&self) -> bool {
        self.device.is_some()
    }

    /// Releases the device; the next stream reopens it.
    pub(crate) fn release(&mut self) {
        if self.device.take().is_some() {
            self.output.close();
        }
        self.asked = None;
        self.ring = None;
        self.format = None;
        self.resampler = None;
        self.restart_map();
        self.untold = false;
        self.held_ramp = None;
    }

    fn restart_map(&mut self) {
        self.marks.clear();
        self.from = Stretch::default();
        self.written = Stretch::default();
        self.blend = None;
    }

    /// Starts the resampler afresh (after a flush or cut).
    fn restart_resampler(&mut self) {
        if let (Some(f), Some(d), true) = (self.format, self.device, self.resampler.is_some()) {
            self.resampler = Resampler::new(f.rate as i32, f.channels as i32, d.rate as i32, d.channels as i32);
        }
    }

    /// Tells the device of a pending flush, with the fade asked for since.
    fn tell_flush(&mut self) {
        if !std::mem::take(&mut self.untold) {
            return;
        }
        self.output.flush();
        if let Some((from, target, ms)) = self.held_ramp.take() {
            self.ramp_now(from, target, ms);
        }
    }

    /// End of the engine's turn: a pending flush is told now.
    pub(crate) fn told(&mut self) {
        self.tell_flush();
    }

    /// High quality output on or off, from the device's next opening ([`Track::must_reopen`]).
    pub(crate) fn set_float(&mut self, on: bool) {
        self.float_on = on;
        self.output.float(on);
    }

    /// The device holds a fraction of a second (equalizer tuning) or the deep buffer.
    pub(crate) fn shallow(&mut self, on: bool) {
        if on != self.shallow {
            self.shallow = on;
            self.output.shallow(on);
        }
    }

    /// Whether the device must reopen for `format`: another rate, channels or (bit-perfect) bits than it
    /// was asked for, or opened 16-bit before high quality output was switched on. A device that gave
    /// another format than asked is fed through the resampler instead.
    fn reopens(&self, format: Format) -> bool {
        self.device.is_some() && (self.asked != Some(self.wanted(format)) || (self.float_on && !self.opened_float))
    }

    /// Whether the device takes float; asked once.
    pub(crate) fn takes_float(&mut self) -> bool {
        *self.float.get_or_insert_with(|| self.output.takes_float())
    }

    /// Music in the ring, µs.
    pub(crate) fn filled_us(&self) -> i64 {
        match (&self.ring, self.device) {
            (Some(r), Some(d)) => (r.filled() as i128 * 1_000_000 / d.rate as i128) as i64,
            _ => 0,
        }
    }

    pub(crate) fn latency_us(&self) -> i64 {
        self.output.latency_us() as i64
    }

    /// The device pulls in bursts ([`AudioOutput::bursts`]).
    pub(crate) fn bursts(&self) -> bool {
        self.output.bursts()
    }

    /// The engine sleeps until the ring holds `us` or less.
    pub(crate) fn wake_at(&self, us: i64) {
        if let Some(r) = &self.ring {
            r.low.store(us.max(0) as u64 * r.rate as u64 / 1_000_000, Ordering::Relaxed);
            r.waiting.store(true, Ordering::Release);
        }
    }

    /// Fades from `from` (or where it is) to `target` over `ms`, on the device if it fades itself, else
    /// in the pulls.
    pub(crate) fn ramp(&mut self, from: Option<f32>, target: f32, ms: i64) {
        if self.untold && self.ring.is_some() {
            // The device still plays pre-flush music: the fade goes with the flush; a start level
            // applies at once.
            if let Some(v) = from {
                self.ramp_now(Some(v), v, 0);
            }
            self.held_ramp = Some((None, target, ms));
            return;
        }
        self.ramp_now(from, target, ms);
    }

    fn ramp_now(&mut self, from: Option<f32>, target: f32, ms: i64) {
        if let Some(r) = &self.ring {
            if !self.output.ramp(from, target, ms) {
                r.ramp(from, target, ms);
            }
        }
    }

    /// Marks the music over (no underruns counted past it).
    pub(crate) fn set_ended(&self, ended: bool) {
        if let Some(r) = &self.ring {
            r.ended.store(ended, Ordering::Release);
        }
    }

    /// Why the device failed, once: it would not open, or stopped and would not reopen.
    pub(crate) fn take_failure(&mut self) -> Option<String> {
        self.failed.take().map(|e| format!("the output would not open: {e}")).or_else(|| self.output.failed().map(|e| format!("the output stopped: {e}")))
    }

    pub(crate) fn underruns(&self) -> u64 {
        self.ring.as_ref().map_or(0, |r| r.underruns.load(Ordering::Relaxed))
    }

    /// The written stretch at ring frame `p` (since the flush).
    fn at_ring(&self, p: u64) -> Stretch {
        let mut before = self.from;
        for &m in self.marks.iter() {
            if p <= m.ring {
                return between(before, m, p);
            }
            before = m;
        }
        Stretch { ring: p, ..before }
    }

    /// The stretch the device plays at, letting go of those before it but for [`REWIND_EARLY_US`].
    fn played_at(&mut self) -> Stretch {
        let (p, _) = self.heard();
        let early = self.device.map_or(0, |d| (REWIND_EARLY_US * d.rate as i64 / 1_000_000) as u64);
        while self.marks.front().is_some_and(|m| m.ring + early <= p) {
            self.from = self.marks.pop_front().expect("checked");
        }
        self.at_ring(p)
    }

    /// The ring frame (since the flush) sink frame `sink` was written to.
    fn ring_of(&self, sink: u64) -> u64 {
        let mut before = self.from;
        for &m in self.marks.iter() {
            if sink <= m.sink {
                let span = m.sink - before.sink;
                let k = if span > 0 { (sink.max(before.sink) - before.sink) as f64 / span as f64 } else { 0.0 };
                return before.ring + ((m.ring - before.ring) as f64 * k).round() as u64;
            }
            before = m;
        }
        self.written.ring
    }

    /// The ring frame (since the flush) the device has played to, and the frames it holds past it.
    fn heard(&self) -> (u64, u64) {
        let (Some(r), Some(d)) = (self.ring.as_ref(), self.device) else { return (0, 0) };
        let held = self.output.latency_us() * d.rate as u64 / 1_000_000;
        let taken = r.read_at().saturating_sub(self.base);
        (taken.saturating_sub(held), held.min(taken))
    }
}

/// Writes `samples` (whole frames, each sample's float by `value`) into `r` from frame `at`, the first
/// frames of a `blend` (done, of) blended into what the slots held; returns the frames written.
fn put<const W: usize>(r: &Ring, at: u64, blend: Option<(u64, u64)>, samples: &[[u8; W]], value: impl Fn([u8; W]) -> f32) -> u64 {
    let ch = r.channels;
    let frames = (samples.len() / ch) as u64;
    let slot = |f: u64| (f % r.frames) as usize * ch;
    let mut done = 0;
    if let Some((from, of)) = blend {
        // What the device would have played there, blended into what replaces it.
        while done < frames && from + done < of {
            let s = slot(at + done);
            for c in 0..ch {
                let old = f32::from_bits(r.slots[s + c].load(Ordering::Relaxed));
                let v = nori_player::pipeline::blended(old, value(samples[done as usize * ch + c]), (from + done) as usize, of as usize);
                r.slots[s + c].store(v.to_bits(), Ordering::Relaxed);
            }
            done += 1;
        }
    }
    // Up to the ring's end, then from its start.
    while done < frames {
        let s = slot(at + done);
        let run = (frames - done).min(r.frames - (at + done) % r.frames) as usize * ch;
        let from = done as usize * ch;
        for (slot, &b) in r.slots[s..s + run].iter().zip(&samples[from..from + run]) {
            slot.store(value(b).to_bits(), Ordering::Relaxed);
        }
        done += (run / ch) as u64;
    }
    frames
}

impl Track for RingTrack {
    fn open(&mut self, format: Format) {
        self.format = Some(format);
        let want = self.wanted(format);
        if self.reopens(format) {
            // Another format: a new device (the old one played out, `Track::must_reopen`).
            self.release();
            self.format = Some(format);
        }
        if self.device.is_none() {
            // The stream picks the format; mixed songs arrive converted by the transition engine.
            let opened = self.output.open(want).and_then(|d| {
                let ring = Arc::new(Ring::new(d, self.engine.clone()));
                self.output.start(Feed::new(ring.clone()))?;
                Ok((d, ring))
            });
            match opened {
                Ok((d, ring)) => {
                    self.opened_float = self.float_on;
                    self.device = Some(d);
                    self.asked = Some(want);
                    self.ring = Some(ring);
                    self.base = 0;
                    if self.playing {
                        self.output.resume();
                    }
                }
                Err(e) => self.failed = Some(e),
            }
        }
        self.resampler = self.device.filter(|d| d.rate != format.rate || d.channels != format.channels).and_then(|d| {
            Resampler::new(format.rate as i32, format.channels as i32, d.rate as i32, d.channels as i32)
        });
    }

    fn queued_bytes(&self) -> usize {
        let (Some(r), Some(d), Some(f)) = (&self.ring, self.device, self.format) else { return 0 };
        (r.filled() as u128 * f.rate as u128 / d.rate as u128) as usize * f.frame_bytes()
    }

    fn write(&mut self, data: &[u8], media: f64) {
        let (Some(r), Some(d), Some(f)) = (self.ring.clone(), self.device, self.format) else { return };
        let w = r.write.load(Ordering::Relaxed);
        let ch = d.channels;
        let frames = match (self.resampler.as_mut(), f.encoding) {
            (None, Encoding::Pcm16) => put(&r, w, self.blend, data.as_chunks::<2>().0, |b| i16::from_le_bytes(b) as f32 / 32768.0),
            (None, Encoding::Float) => put(&r, w, self.blend, data.as_chunks::<4>().0, f32::from_le_bytes),
            (Some(rs), _) => {
                let in_frames = data.len() / f.frame_bytes();
                let need = ((in_frames as u64 * d.rate as u64 / f.rate as u64) as usize + 4) * ch * 4;
                if self.converted.len() < need {
                    self.converted.resize(need, 0);
                }
                let Some((_, made)) = rs.process(data, f.encoding.media3(), &mut self.converted, Encoding::FLOAT) else { return };
                put(&r, w, self.blend, self.converted[..made].as_chunks::<4>().0, f32::from_le_bytes)
            }
        };
        self.blend = self.blend.and_then(|(done, of)| (done + frames < of).then_some((done + frames, of)));
        r.write.store(w + frames, Ordering::Release);
        let before = self.written;
        self.written = Stretch { ring: before.ring + frames, sink: before.sink + (data.len() / f.frame_bytes()) as u64, media: before.media + media };
        if self.untold {
            self.since_flush += frames;
            if self.since_flush as i64 >= d.rate as i64 * TELL_FLUSH_US / 1_000_000 {
                self.tell_flush();
            }
        }
        // Merge stretches at the same pace (at 1x without resampling a song is one mark).
        let pace = |a: Stretch, b: Stretch| (b.media - a.media) / (b.ring - a.ring).max(1) as f64;
        let start = self.marks.len().checked_sub(2).map_or(self.from, |i| self.marks[i]);
        match self.marks.back_mut() {
            Some(last) if frames > 0 && (pace(start, *last) - media / frames as f64).abs() < 1e-3 => *last = self.written,
            _ => self.marks.push_back(self.written),
        }
    }

    fn played_media(&mut self) -> f64 {
        self.played_at().media
    }

    fn played(&mut self) -> u64 {
        self.played_at().sink
    }

    /// A device holding little plays on: the first frame no pull can have taken, fenced so none takes
    /// it before the cut. One holding more than [`HELD_US`] drops what it holds: a little before what
    /// it has played.
    fn freeze(&mut self) -> u64 {
        let (Some(r), Some(d)) = (self.ring.clone(), self.device) else { return self.written.sink };
        let (heard, held) = self.heard();
        if held as i64 * 1_000_000 > HELD_US * d.rate as i64 {
            let early = (REWIND_EARLY_US * d.rate as i64 / 1_000_000) as u64;
            return self.at_ring(heard.saturating_sub(early)).sink;
        }
        // `write` lowered past any pull's reach, then checked against a pull that read it before: each
        // side stores before it loads, so one sees the other.
        let end = self.base + self.written.ring;
        let mut at = r.limit.load(Ordering::SeqCst).max(r.read_at());
        loop {
            let fence = at.clamp(self.base, end);
            r.write.store(fence, Ordering::SeqCst);
            let limit = r.limit.load(Ordering::SeqCst);
            if limit <= fence || fence == end {
                at = fence;
                break;
            }
            at = limit;
        }
        let p = at - self.base;
        let sink = self.at_ring(p).sink;
        // The first sink frame written at or past the fence.
        if self.ring_of(sink) < p {
            sink + 1
        } else {
            sink
        }
    }

    fn cut(&mut self, at: u64) -> f64 {
        let Some(r) = self.ring.clone() else { return self.written.media };
        let end = self.written;
        let p = self.ring_of(at).min(end.ring);
        let cut = if p >= end.ring { end } else { self.at_ring(p) };
        while self.marks.back().is_some_and(|m| m.ring >= p) {
            self.marks.pop_back();
        }
        if p > self.from.ring {
            self.marks.push_back(cut);
        }
        self.written = cut;
        let w = self.base + p;
        r.write.store(w, Ordering::SeqCst);
        if w < r.read_at() {
            // Before what the device took: it drops what it holds and plays on from where it got to.
            r.discard.store(w, Ordering::Release);
            r.flushes.fetch_add(1, Ordering::AcqRel);
            r.ended.store(false, Ordering::Release);
            self.untold = true;
            self.since_flush = 0;
            self.blend = None;
        } else {
            let blend = self.device.map_or(0, |d| d.rate as i64 * nori_player::pipeline::BLEND_US / 1_000_000) as u64;
            self.blend = (end.ring > p).then_some((0, blend.min(end.ring - p)));
        }
        self.restart_resampler();
        cut.media
    }

    fn is_empty(&self) -> bool {
        self.ring.as_ref().is_none_or(|r| r.filled() == 0) && !self.output.holding()
    }

    fn flush(&mut self) {
        if let Some(r) = &self.ring {
            let w = r.write.load(Ordering::Relaxed);
            r.discard.store(w, Ordering::Release);
            r.flushes.fetch_add(1, Ordering::AcqRel);
            r.ended.store(false, Ordering::Release);
            self.base = w;
            self.untold = true;
            self.since_flush = 0;
        }
        self.restart_map();
        self.restart_resampler();
    }

    fn source_bits(&mut self, bits: u32) {
        self.bits = bits;
    }

    /// A stream needing a reopen waits for the device to play out: marked as the end, so it is played
    /// whole with no underrun counted.
    fn must_reopen(&mut self, format: Format) -> bool {
        let reopen = self.reopens(format);
        if reopen {
            self.set_ended(true);
        }
        reopen
    }

    fn play(&mut self) {
        self.playing = true;
        if self.device.is_some() {
            self.output.resume();
        }
    }

    fn pause(&mut self) {
        self.playing = false;
        if self.device.is_some() {
            self.output.pause();
        }
    }
}

impl Drop for RingTrack {
    fn drop(&mut self) {
        self.output.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps the feed for pulling by hand; holds what the test says, µs.
    struct Hand(Arc<parking_lot::Mutex<Option<Feed>>>, Arc<AtomicU64>);

    impl AudioOutput for Hand {
        fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
            Ok(want)
        }
        fn start(&mut self, feed: Feed) -> Result<(), String> {
            *self.0.lock() = Some(feed);
            Ok(())
        }
        fn pause(&mut self) {}
        fn resume(&mut self) {}
        fn latency_us(&self) -> u64 {
            self.1.load(Ordering::Relaxed)
        }
        fn close(&mut self) {}
    }

    /// A track over a [`Hand`] and its feed.
    fn by_hand() -> (RingTrack, Feed, Arc<AtomicU64>) {
        let (feed, held) = (Arc::new(parking_lot::Mutex::new(None)), Arc::new(AtomicU64::new(0)));
        let mut t = RingTrack::new(Box::new(Hand(feed.clone(), held.clone())));
        t.open(F);
        let f = feed.lock().take().expect("started");
        (t, f, held)
    }

    const F: Format = Format { rate: 1000, channels: 1, encoding: Encoding::Pcm16 };

    fn pcm(v: &[i16]) -> Vec<u8> {
        v.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn playhead_follows_pace_changes() {
        let (mut t, mut f, _) = by_hand();
        // 100 frames for 100 of song time, then 100 for 200 (2x speed).
        t.write(&pcm(&[1000; 100]), 100.0);
        t.write(&pcm(&[1000; 100]), 200.0);
        let mut out = vec![0f32; 150];
        assert_eq!(f.pull(&mut out), 150);
        assert!((t.played_media() - 200.0).abs() < 1e-9, "{}", t.played_media());
        assert!((out[0] - 1000.0 / 32768.0).abs() < 1e-6);
        t.flush();
        assert_eq!(t.played_media(), 0.0);
        assert!(t.is_empty(), "a flush drops what was unplayed");
        let mut out = vec![1f32; 10];
        assert_eq!(f.pull(&mut out), 0);
        assert!(out.iter().all(|&v| v == 0.0), "silence when there is nothing");
    }

    #[test]
    fn pull_reports_flush() {
        let (mut t, mut f, _) = by_hand();
        t.write(&pcm(&[1000; 100]), 100.0);
        let mut out = vec![0f32; 10];
        f.pull(&mut out);
        assert!(!f.flushed(), "nothing was dropped yet");
        t.flush();
        t.write(&pcm(&[-1000; 100]), 100.0);
        assert_eq!(f.pull(&mut out), 10);
        assert!(f.flushed(), "the pull after a flush says so");
        assert_eq!(f.behind(), 10, "a jump: nothing before what that pull took");
        assert!(out.iter().all(|&v| v < 0.0), "and holds only the new music");
        f.pull(&mut out);
        assert!(!f.flushed(), "once");
    }

    #[test]
    fn cut_ahead_of_device_blends_in() {
        let (mut t, mut f, _) = by_hand();
        t.write(&pcm(&[16384; 200]), 200.0);
        let mut out = vec![0f32; 200];
        assert_eq!(f.pull(&mut out[..40]), 40);
        let at = t.freeze();
        assert_eq!(at, 40, "the first frame no pull took");
        assert_eq!(f.pull(&mut out[40..60]), 0, "and none takes it before the cut");
        assert_eq!(t.cut(at), 40.0, "the song time before it");
        t.write(&pcm(&[-16384; 100]), 100.0);
        assert_eq!(f.pull(&mut out[40..140]), 100);
        let blend = (F.rate as i64 * nori_player::pipeline::BLEND_US / 1_000_000) as usize;
        for k in 0..blend {
            assert!((out[40 + k] - nori_player::pipeline::blended(0.5, -0.5, k, blend)).abs() < 1e-6, "frame {k} of the blend: {}", out[40 + k]);
        }
        assert!(out[40 + blend..140].iter().all(|&v| v == -0.5), "then the new music");
        assert!((t.played_media() - 140.0).abs() < 1e-9, "{}", t.played_media());
    }

    #[test]
    fn cut_behind_device_takes_back_what_it_did_not_play() {
        let (mut t, mut f, held) = by_hand();
        t.write(&pcm(&[16384; 1000]), 1000.0);
        let mut out = vec![0f32; 600];
        assert_eq!(f.pull(&mut out), 600);
        // It played 300 of the 600 it took.
        held.store(300_000, Ordering::Relaxed);
        let at = t.freeze();
        assert!((150..=300).contains(&at), "from a little before what it played: {at}");
        t.cut(at);
        let new: Vec<i16> = (0..700).map(|k| k as i16).collect();
        t.write(&pcm(&new), 700.0);
        let mut out = vec![0f32; 100];
        assert_eq!(f.pull(&mut out), 100);
        assert!(f.flushed(), "the device is told to drop what it holds");
        assert_eq!(f.behind(), 700 - at, "and can go back to the first frame made again");
        // 700 taken, 300 played.
        f.rewind(400);
        assert_eq!(f.pull(&mut out), 100);
        assert_eq!((out[0] * 32768.0).round() as u64, 300 - at, "and plays on from where it got to, in the new music");
    }

    #[test]
    fn pull_after_a_cut_behind_waits_for_music() {
        let (mut t, mut f, held) = by_hand();
        t.write(&pcm(&[16384; 1000]), 1000.0);
        let mut out = vec![0f32; 600];
        f.pull(&mut out);
        held.store(300_000, Ordering::Relaxed);
        let at = t.freeze();
        t.cut(at);
        assert_eq!(f.pull(&mut out), 0);
    }

    /// A sound change between a new stream's announcement and its first buffer (the ring full) has
    /// nothing of that stream to make again: what the ring holds of the one before plays on, whole.
    #[test]
    fn change_before_a_new_streams_music_keeps_the_ring() {
        use nori_player::engine::Downstream;
        use nori_player::pipeline::{ChainSettings, Sink, Sound};
        let (feed, held) = (Arc::new(parking_lot::Mutex::new(None)), Arc::new(AtomicU64::new(0)));
        let mut track = RingTrack::new(Box::new(Hand(feed.clone(), held)));
        // Both streams on a device at 1 kHz: the second converts, the device stays.
        track.max_rate = 1000;
        let mut sink = Sink::new(nori_player::burst::BUFFER_US, ChainSettings::default(), track);
        sink.configure(Format { rate: 2000, ..F });
        assert_eq!(sink.handle_buffer(&pcm(&[1000; 2000]), 0, 0), (true, 4000));
        sink.configure(F);
        let f = feed.lock().take().expect("started");
        let held = f.available();
        assert!(held > 900, "the first stream, resampled to the device: {held}");
        sink.change(ChainSettings { sound: Sound { preamp_db: -6.0, ..Sound::default() }, ..ChainSettings::default() });
        assert_eq!(f.available(), held, "all of it still there");
    }

    #[test]
    fn new_format_after_a_change_plays_the_input_made_again() {
        use nori_player::engine::Downstream;
        use nori_player::pipeline::{ChainSettings, Sink};
        let (feed, held) = (Arc::new(parking_lot::Mutex::new(None)), Arc::new(AtomicU64::new(0)));
        let mut track = RingTrack::new(Box::new(Hand(feed.clone(), held)));
        track.max_rate = 1000;
        let mut sink = Sink::new(nori_player::burst::BUFFER_US, ChainSettings::default(), track);
        sink.configure(F);
        assert_eq!(sink.handle_buffer(&pcm(&[1000; 3000]), 0, 0), (true, 6000));
        let mut f = feed.lock().take().expect("started");
        let mut out = vec![0f32; 1000];
        assert_eq!(f.pull(&mut out), 1000);
        sink.change(ChainSettings { speed: 2.0, ..ChainSettings::default() });
        // The next song, in another format the device converts.
        sink.configure(Format { rate: 2000, ..F });
        sink.fill();
        let mut out = vec![0f32; 3000];
        let rest = f.pull(&mut out);
        assert!((900..=1100).contains(&rest), "the last 2000 frames at twice the speed: {rest}");
    }

    #[test]
    fn fade_runs_in_pulls() {
        let (mut t, mut f, _) = by_hand();
        t.write(&pcm(&[16384; 200]), 200.0);
        t.ramp(None, 0.0, 100);
        let mut out = vec![0f32; 200];
        f.pull(&mut out);
        assert!((out[0] - 0.5 * 0.99).abs() < 1e-3, "{}", out[0]);
        assert!((out[49] - 0.25).abs() < 1e-2, "half way down: {}", out[49]);
        assert!(out[100..].iter().all(|&v| v == 0.0), "down and staying down");
    }
}
