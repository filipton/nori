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
    /// The ring's music was dropped (seek, jump): an output with a buffer of seconds drops its own too;
    /// [`Feed::flushed`] marks where the new music starts.
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
    /// Hold only a fraction of a second (equalizer tuning) or the deep buffer again. Told before the
    /// flush that comes with it (no flush if [`AudioOutput::resizes`]) and before starting; the ring is
    /// kept at [`SHALLOW_US`] meanwhile.
    fn shallow(&mut self, _on: bool) {}
    /// [`AudioOutput::shallow`] applies in place without dropping what the device holds (a phone's
    /// AudioTrack): the engine then resizes the ring in place too, with no flush or dip.
    fn resizes(&self) -> bool {
        false
    }
    /// How deep a shallow device must be where it plays (a Bluetooth output needs far more than a
    /// speaker), once known. None: [`SHALLOW_US`] and the device's own say.
    fn shallow_depth(&self) -> Option<ShallowDepth> {
        None
    }
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
/// Ring depth while tuning (`nori_player::pipeline::Player::shallow_us`): with a shallow device a band
/// moved is heard within a quarter second. Topped up at half.
pub const SHALLOW_US: i64 = 80_000;
/// What a shallow device needs ([`AudioOutput::shallow_depth`]), µs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShallowDepth {
    /// Most the device holds while shallow, its latency included.
    pub device_us: i64,
    /// Ring depth that keeps it fed: one of its top-ups.
    pub ring_us: i64,
}

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
    /// Frames written and read since creation; monotonic.
    write: AtomicU64,
    read: AtomicU64,
    /// Flush: frames before this are dropped.
    discard: AtomicU64,
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
            discard: AtomicU64::new(0),
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
        let w = r.write.load(Ordering::Acquire);
        // Read after `write`, so music written after a flush comes with the flush.
        let discard = r.discard.load(Ordering::Acquire);
        if discard != self.seen {
            self.seen = discard;
            self.flushed = true;
        }
        let at = r.read.load(Ordering::Acquire).max(discard);
        let n = w.saturating_sub(at).min(want);
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
        for i in 0..n {
            if self.gain != self.target {
                self.gain += self.step;
                if (self.step > 0.0 && self.gain > self.target) || (self.step < 0.0 && self.gain < self.target) || self.step == 0.0 {
                    self.gain = self.target;
                }
            }
            let slot = ((at + i) % r.frames) as usize * ch;
            let o = i as usize * ch;
            for c in 0..ch {
                out[o + c] = conv(f32::from_bits(r.slots[slot + c].load(Ordering::Relaxed)) * self.gain);
            }
        }
        out[n as usize * ch..].fill(S::default());
        r.read.store(at + n, Ordering::Release);
        if n < want && !r.ended.load(Ordering::Relaxed) && w > 0 {
            r.underruns.fetch_add(1, Ordering::Relaxed);
        }
        // At the low mark: wake the engine for the next burst, once.
        if w - (at + n) <= r.low.load(Ordering::Relaxed) && r.waiting.load(Ordering::Relaxed) && r.waiting.swap(false, Ordering::AcqRel) {
            r.engine.unpark();
        }
        n as usize
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

/// The [`Track`] under the engine's sink: the ring, the device, and the ring-frame-to-song-time map.
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
    /// End of each written stretch (frames since the flush, song time), and the start of the first
    /// stretch still ahead of the playhead.
    marks: VecDeque<(u64, f64)>,
    from: (u64, f64),
    written: (u64, f64),
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
    /// [`AudioOutput::shallow_depth`].
    pub(crate) fn shallow_depth(&self) -> Option<ShallowDepth> {
        self.output.shallow_depth()
    }

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
            from: (0, 0.0),
            written: (0, 0.0),
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
        self.marks.clear();
        self.from = (0, 0.0);
        self.written = (0, 0.0);
        self.untold = false;
        self.held_ramp = None;
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

    /// The ring frame (since the flush) at song time `media`.
    fn frame_of(&self, media: f64) -> u64 {
        let mut before = self.from;
        for &m in self.marks.iter().chain(std::iter::once(&self.written)) {
            if media <= m.1 {
                let span = m.1 - before.1;
                let k = if span > 0.0 { ((media - before.1) / span).clamp(0.0, 1.0) } else { 0.0 };
                return before.0 + ((m.0 - before.0) as f64 * k).round() as u64;
            }
            before = m;
        }
        self.written.0
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

    /// Song time at ring frame `p` (since the flush).
    fn media_at(&mut self, p: u64) -> f64 {
        while let Some(&m) = self.marks.front() {
            if m.0 > p {
                break;
            }
            self.from = m;
            self.marks.pop_front();
        }
        match self.marks.front() {
            Some(&(end, media)) if end > self.from.0 => self.from.1 + (p - self.from.0) as f64 / (end - self.from.0) as f64 * (media - self.from.1),
            _ => self.from.1,
        }
    }
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
        let put = |at: u64, samples: &mut dyn Iterator<Item = f32>| {
            let mut n = 0u64;
            let mut c = 0;
            let mut slot = ((at % r.frames) as usize) * ch;
            for v in samples {
                r.slots[slot + c].store(v.to_bits(), Ordering::Relaxed);
                c += 1;
                if c == ch {
                    c = 0;
                    n += 1;
                    slot = (((at + n) % r.frames) as usize) * ch;
                }
            }
            n
        };
        let floats = |b: &[u8]| f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let frames = match (self.resampler.as_mut(), f.encoding) {
            (None, Encoding::Pcm16) => put(w, &mut data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)),
            (None, Encoding::Float) => put(w, &mut data.chunks_exact(4).map(floats)),
            (Some(rs), _) => {
                let in_frames = data.len() / f.frame_bytes();
                let need = ((in_frames as u64 * d.rate as u64 / f.rate as u64) as usize + 4) * ch * 4;
                if self.converted.len() < need {
                    self.converted.resize(need, 0);
                }
                let Some((_, made)) = rs.process(data, f.encoding.media3(), &mut self.converted, Encoding::FLOAT) else { return };
                put(w, &mut self.converted[..made].chunks_exact(4).map(floats))
            }
        };
        r.write.store(w + frames, Ordering::Release);
        self.written.0 += frames;
        self.written.1 += media;
        if self.untold {
            self.since_flush += frames;
            if self.since_flush as i64 >= d.rate as i64 * TELL_FLUSH_US / 1_000_000 {
                self.tell_flush();
            }
        }
        // Merge stretches at the same pace (at 1x without resampling a song is one mark).
        let pace = |from: (u64, f64), to: (u64, f64)| (to.1 - from.1) / (to.0 - from.0).max(1) as f64;
        let before = self.marks.len().checked_sub(2).map_or(self.from, |i| self.marks[i]);
        match self.marks.back_mut() {
            Some(last) if frames > 0 && (pace(before, *last) - media / frames as f64).abs() < 1e-3 => *last = self.written,
            _ => self.marks.push_back(self.written),
        }
    }

    fn played_media(&mut self) -> f64 {
        let (Some(r), Some(d)) = (self.ring.clone(), self.device) else { return 0.0 };
        let latency = self.output.latency_us() * d.rate as u64 / 1_000_000;
        let p = r.read_at().saturating_sub(latency).saturating_sub(self.base);
        self.media_at(p)
    }

    fn is_empty(&self) -> bool {
        self.ring.as_ref().is_none_or(|r| r.filled() == 0) && !self.output.holding()
    }

    fn flush(&mut self) {
        if let Some(r) = &self.ring {
            let w = r.write.load(Ordering::Relaxed);
            r.discard.store(w, Ordering::Release);
            r.ended.store(false, Ordering::Release);
            self.base = w;
            self.untold = true;
            self.since_flush = 0;
        }
        self.marks.clear();
        self.from = (0, 0.0);
        self.written = (0, 0.0);
        if let (Some(f), Some(d)) = (self.format, self.device) {
            if self.resampler.is_some() {
                self.resampler = Resampler::new(f.rate as i32, f.channels as i32, d.rate as i32, d.channels as i32);
            }
        }
    }

    /// Scales what the ring still holds of song time `from..to`. What the device already took stays.
    fn rescale(&mut self, from: f64, to: f64, ratio: f32) {
        let Some(r) = self.ring.clone() else { return };
        let ch = r.channels;
        let (a, b) = (self.base + self.frame_of(from.max(0.0)), self.base + self.frame_of(to));
        let (a, b) = (a.max(r.read_at()), b.min(r.write.load(Ordering::Acquire)));
        for f in a..b {
            let slot = (f % r.frames) as usize * ch;
            for s in &r.slots[slot..slot + ch] {
                s.store((f32::from_bits(s.load(Ordering::Relaxed)) * ratio).to_bits(), Ordering::Relaxed);
            }
        }
    }

    fn source_bits(&mut self, bits: u32) {
        self.bits = bits;
    }

    fn resizes(&self) -> bool {
        self.output.resizes()
    }

    fn depth(&mut self, capacity_us: i64) {
        let shallow = capacity_us < BUFFER_US;
        if shallow != self.shallow {
            self.shallow = shallow;
            self.output.shallow(shallow);
        }
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

    /// Keeps the feed for pulling by hand.
    struct Hand(Arc<parking_lot::Mutex<Option<Feed>>>);

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
            0
        }
        fn close(&mut self) {}
    }

    const F: Format = Format { rate: 1000, channels: 1, encoding: Encoding::Pcm16 };

    fn pcm(v: &[i16]) -> Vec<u8> {
        v.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn playhead_follows_pace_changes() {
        let feed = Arc::new(parking_lot::Mutex::new(None));
        let mut t = RingTrack::new(Box::new(Hand(feed.clone())));
        t.open(F);
        // 100 frames for 100 of song time, then 100 for 200 (2x speed).
        t.write(&pcm(&[1000; 100]), 100.0);
        t.write(&pcm(&[1000; 100]), 200.0);
        let mut out = vec![0f32; 150];
        assert_eq!(feed.lock().as_mut().unwrap().pull(&mut out), 150);
        assert!((t.played_media() - 200.0).abs() < 1e-9, "{}", t.played_media());
        assert!((out[0] - 1000.0 / 32768.0).abs() < 1e-6);
        t.flush();
        assert_eq!(t.played_media(), 0.0);
        assert!(t.is_empty(), "a flush drops what was unplayed");
        let mut out = vec![1f32; 10];
        assert_eq!(feed.lock().as_mut().unwrap().pull(&mut out), 0);
        assert!(out.iter().all(|&v| v == 0.0), "silence when there is nothing");
    }

    #[test]
    fn pull_reports_flush() {
        let feed = Arc::new(parking_lot::Mutex::new(None));
        let mut t = RingTrack::new(Box::new(Hand(feed.clone())));
        t.open(F);
        t.write(&pcm(&[1000; 100]), 100.0);
        let mut f = feed.lock().take().unwrap();
        let mut out = vec![0f32; 10];
        f.pull(&mut out);
        assert!(!f.flushed(), "nothing was dropped yet");
        t.flush();
        t.write(&pcm(&[-1000; 100]), 100.0);
        assert_eq!(f.pull(&mut out), 10);
        assert!(f.flushed(), "the pull after a flush says so");
        assert!(out.iter().all(|&v| v < 0.0), "and holds only the new music");
        f.pull(&mut out);
        assert!(!f.flushed(), "once");
    }

    #[test]
    fn rescale_touches_only_that_songs_ring_music() {
        let feed = Arc::new(parking_lot::Mutex::new(None));
        let mut t = RingTrack::new(Box::new(Hand(feed.clone())));
        t.open(F);
        // Two songs of 100 frames, the second at 2x speed.
        t.write(&pcm(&[16384; 100]), 100.0);
        t.write(&pcm(&[16384; 100]), 200.0);
        let mut out = vec![0f32; 300];
        let mut f = feed.lock();
        let f = f.as_mut().unwrap();
        assert_eq!(f.pull(&mut out[..40]), 40);
        // Halve what is left of the first song.
        t.rescale(0.0, 100.0, 0.5);
        // The second from song time 200 (ring frame 150).
        t.rescale(200.0, f64::MAX, 0.25);
        assert_eq!(f.pull(&mut out[40..200]), 160);
        assert!(out[..40].iter().all(|&v| v == 0.5), "pulled before the change: as it was");
        assert!(out[40..100].iter().all(|&v| v == 0.25), "the rest of the first song at its new volume");
        assert!(out[100..150].iter().all(|&v| v == 0.5), "the second untouched up to where it changes");
        assert!(out[150..200].iter().all(|&v| v == 0.125), "and scaled from there");
    }

    #[test]
    fn fade_runs_in_pulls() {
        let feed = Arc::new(parking_lot::Mutex::new(None));
        let mut t = RingTrack::new(Box::new(Hand(feed.clone())));
        t.open(F);
        t.write(&pcm(&[16384; 200]), 200.0);
        t.ramp(None, 0.0, 100);
        let mut out = vec![0f32; 200];
        feed.lock().as_mut().unwrap().pull(&mut out);
        assert!((out[0] - 0.5 * 0.99).abs() < 1e-3, "{}", out[0]);
        assert!((out[49] - 0.25).abs() < 1e-2, "half way down: {}", out[49]);
        assert!(out[100..].iter().all(|&v| v == 0.0), "down and staying down");
    }
}
