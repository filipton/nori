//! The sound card's side. A platform writes one small [`AudioOutput`]: open a device, and from the
//! device's own thread call [`Feed::pull`] for every buffer it wants. The feed reads a lock-free ring
//! that one engine thread fills in bursts; pulling never blocks, never allocates and never takes a
//! lock, so the device thread cannot be held up by anything the player does.
//!
//! The ring holds float samples at the device's rate and channel count. The engine's output below the
//! sound chain ([`RingTrack`]) converts the chain's audio (16-bit, or float for high quality output)
//! into it, resampling only when the device would not take the stream's own rate, and keeps the map
//! from ring frames back to song time that the playhead is read through.

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
    /// The bits per sample of the song, as its file stores them, when the song's own samples are to
    /// reach the device as they are (bit-perfect output): a device that can takes them at that depth.
    /// 0 otherwise.
    pub bits: u32,
}

/// Where the music goes: the kind of device, and the name it gives itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub kind: OutputKind,
    pub name: String,
}

/// Called by an output whenever the device the music goes to changes (from any thread).
pub type DeviceWatch = Box<dyn Fn(Device) + Send + Sync>;

/// A sound card, or anything that takes the music the way one does (a file). Called only from the
/// engine's thread; the device's own thread only ever calls [`Feed::pull`].
pub trait AudioOutput: Send {
    /// From now on `changed` is told which device the music goes to: once when the output knows, and
    /// again whenever the system moves it (headphones plugged in, a Bluetooth device connected), so
    /// the core can give each device its own sound. An output that cannot tell says nothing.
    fn watch(&mut self, _changed: DeviceWatch) {}
    /// Picks the device's format, as close to `want` as it goes. Nothing plays yet.
    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String>;
    /// From now on the device pulls every buffer it plays from `feed`; it starts paused.
    fn start(&mut self, feed: Feed) -> Result<(), String>;
    /// Stops pulling, and lets the device sleep.
    fn pause(&mut self);
    fn resume(&mut self);
    /// How long a sample pulled now takes to be heard, µs.
    fn latency_us(&self) -> u64;
    /// Whether the device plays float samples as they are ([`Feed::pull`]) rather than 16-bit ones
    /// ([`Feed::pull_i16`]). With high quality output on, songs are then decoded and carried to it in
    /// float; otherwise the chain runs in 16 bits, as Android's does without float output.
    fn takes_float(&mut self) -> bool {
        false
    }
    /// High quality output was switched on or off: a device that plays float or 16-bit as it is opened
    /// opens in float from its next opening only while it is on. The engine opens it again for the next
    /// song when it was opened without.
    fn float(&mut self, _on: bool) {}
    /// The music the ring held was dropped (a seek, a jump): what the device itself still holds of it is
    /// stale too. Called on the engine's thread after the ring let it go; an output with a buffer of its
    /// own worth hearing (seconds, not milliseconds) wakes its thread here, and [`Feed::flushed`] says
    /// where the new music starts.
    fn flush(&mut self) {}
    /// A volume fade from `from` (or wherever the volume is) to `target` over `ms`. An output whose device
    /// holds seconds of music runs it at the device (its own volume), where it is heard when asked for,
    /// and says true; otherwise the ring runs it on the samples it hands out.
    fn ramp(&mut self, _from: Option<f32>, _target: f32, _ms: i64) -> bool {
        false
    }
    /// Whether the device still holds music it took from the ring and has not played: with a buffer of
    /// seconds, the music is not over when the ring runs empty.
    fn holding(&self) -> bool {
        false
    }
    /// Whether the device takes the ring's music in bursts of seconds rather than a steady trickle. The
    /// ring then does not run down between its pulls, so the engine sleeps until the pull that crosses
    /// the low mark wakes it, with no timer guessing when that will be.
    fn bursts(&self) -> bool {
        false
    }
    /// Whether the device should hold no more than a fraction of a second of music from now on (the
    /// equalizer is being tuned, and a band moved is to be heard at once) or its deep buffer again. Told
    /// just before the flush that comes with it ([`AudioOutput::flush`]), or with none for a device that
    /// [`AudioOutput::resizes`], and before the device is started; the ring is kept as shallow then
    /// ([`SHALLOW_US`]). A device whose buffer is a few milliseconds anyway has nothing to do.
    fn shallow(&mut self, _on: bool) {}
    /// Whether [`AudioOutput::shallow`] takes effect at once over the same device, which keeps what it
    /// holds and plays on (a phone's AudioTrack, whose buffer size moves inside the one it was opened
    /// with). The engine then changes its ring's depth in place too, with no flush and no dip, so the
    /// equalizer screen opening or closing is not heard; otherwise both are made again behind a dip.
    fn resizes(&self) -> bool {
        false
    }
    /// How deep a device kept shallow found it must be for where it plays, once it knows: a Bluetooth
    /// output has a latency and pulls of its own far beyond a phone speaker's, and a track kept as shallow
    /// as for the speaker runs dry there. The engine keeps its ring at least [`ShallowDepth::ring_us`]
    /// while tuned, and counts [`ShallowDepth::device_us`] as the shallow device's when it tells a band
    /// moved that is heard as it is from one made again. None: [`SHALLOW_US`] and the device's own say.
    fn shallow_depth(&self) -> Option<ShallowDepth> {
        None
    }
    /// The device stopped taking music and could not be opened again (a sound server that died): asked
    /// by the engine whenever it looks, and woken for with [`Feed::wake_engine`]. The engine then stops,
    /// says so, and lets the output go, so the next play opens a new one where the music was.
    fn failed(&mut self) -> Option<String> {
        None
    }
    /// Lets the device go.
    fn close(&mut self);
}

/// How far above empty the ring is when the engine is woken to fill it: a little under the burst's
/// low mark, so the burst's own count (which includes what the device holds) agrees it is time.
pub const WAKE_LOW_US: i64 = LOW_US - 250_000;
/// How much the ring holds while the equalizer is tuned (`nori_player::pipeline::Player::shallow_us`):
/// with a device kept as shallow ([`AudioOutput::shallow`]), a band moved is heard within a quarter of a
/// second. The engine is woken to top it up when half of it is left.
pub const SHALLOW_US: i64 = 80_000;
/// What a device kept shallow needs ([`AudioOutput::shallow_depth`]), µs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShallowDepth {
    /// The most music the device holds while shallow, counted as its clock counts it (the output's own
    /// latency in it).
    pub device_us: i64,
    /// The ring that keeps it fed: what one of the device's top-ups takes from it.
    pub ring_us: i64,
}

/// The ring's room beyond the sink's deep buffer: the resampler's rounding and a device's first pull.
const SLACK_US: i64 = 2_000_000;
/// A flush is told to the device ([`AudioOutput::flush`]) once this much of the music that follows it
/// is in the ring, or at the end of the engine's turn if that comes first: a device with a buffer of
/// seconds empties it when told, and starts again from what the ring has then. Told at once, it found
/// the ring empty and waited a tick for the music (and a phone's track waits for a quarter of a second
/// of it before it starts).
const TELL_FLUSH_US: i64 = 300_000;

/// Shared between the engine's thread (the only writer) and the device's (the only reader).
pub(crate) struct Ring {
    /// Float samples as their bits: plain loads and stores on every machine that matters, and no
    /// data race even when a flush lets the writer run over what the reader is still looking at.
    slots: Box<[AtomicU32]>,
    frames: u64,
    channels: usize,
    rate: u32,
    bits: u32,
    /// Frames written and read since the ring was made; they only grow.
    write: AtomicU64,
    read: AtomicU64,
    /// A flush: everything before this frame is dropped unplayed.
    discard: AtomicU64,
    /// The engine sleeps until the ring runs down to `low` frames: the reader wakes it then, once.
    waiting: AtomicBool,
    low: AtomicU64,
    engine: Thread,
    /// A volume fade the reader runs sample by sample: where from (NaN: wherever it is), where to,
    /// over how many frames, and a count that moves whenever a new one is asked for.
    gain_from: AtomicU32,
    gain_target: AtomicU32,
    ramp_frames: AtomicU32,
    ramp_gen: AtomicU32,
    /// The music is over: running dry now is the end, not an underrun.
    ended: AtomicBool,
    /// Pulls that found the ring short while music was due.
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

    /// Where the reader is, a flush taken into account.
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

/// Bytes every ring alive holds, for the perf report's memory line.
static RING_BYTES: AtomicU64 = AtomicU64::new(0);

/// Bytes the engines' rings hold now: twelve seconds of float samples each.
pub fn ring_bytes() -> u64 {
    RING_BYTES.load(Ordering::Relaxed)
}

/// The device thread's end of the ring. Owned by the device's callback; see [`Feed::pull`].
pub struct Feed {
    ring: Arc<Ring>,
    gain: f32,
    target: f32,
    step: f32,
    gen: u32,
    /// The flush the last pull saw, and whether one happened since [`Feed::flushed`] was last asked.
    seen: u64,
    flushed: bool,
}

impl Feed {
    fn new(ring: Arc<Ring>) -> Feed {
        Feed { ring, gain: 1.0, target: 1.0, step: 0.0, gen: 0, seen: 0, flushed: false }
    }

    /// Whether the ring was flushed since this was last asked, as the pulls found it: the frames the
    /// last pull returned (if any) are then the new music's, and what the device held from before it
    /// should go. A pull never mixes the two.
    pub fn flushed(&mut self) -> bool {
        std::mem::take(&mut self.flushed)
    }

    pub fn format(&self) -> OutputFormat {
        OutputFormat { rate: self.ring.rate, channels: self.ring.channels, bits: self.ring.bits }
    }

    /// Fills `out` (interleaved, the device's channels) with the music, silence past what there is.
    /// Returns the frames of music it held. Lock-free and allocation-free: made for the device's own
    /// real-time thread.
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
        // Read after the write position: a pull that sees music written after a flush sees the flush.
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
        // Run down to the low mark: the engine's next burst is due. One wake per burst.
        if w - (at + n) <= r.low.load(Ordering::Relaxed) && r.waiting.load(Ordering::Relaxed) && r.waiting.swap(false, Ordering::AcqRel) {
            r.engine.unpark();
        }
        n as usize
    }

    /// Wakes the engine now, for something the device must tell it at once ([`AudioOutput::failed`]).
    pub fn wake_engine(&self) {
        self.ring.engine.unpark();
    }

    /// Whether the engine sleeps until a pull takes the ring down to its low mark: a pull that finds it
    /// so, and leaves it not so, woke it. For a test's device on a clock it moves by hand.
    pub fn engine_waits(&self) -> bool {
        self.ring.waiting.load(Ordering::Acquire)
    }

    /// Frames of music waiting in the ring.
    pub fn available(&self) -> usize {
        self.ring.filled() as usize
    }

    /// The music is over: what is left in the ring is the last of it.
    pub fn ending(&self) -> bool {
        self.ring.ended.load(Ordering::Acquire)
    }

    /// The music is over and everything was pulled: a file stops writing here.
    pub fn finished(&self) -> bool {
        self.ring.ended.load(Ordering::Acquire) && self.ring.filled() == 0
    }
}

/// The [`Track`] under the engine's sink: the ring, the device, and the way back from ring frames
/// to song time.
pub(crate) struct RingTrack {
    output: Box<dyn AudioOutput>,
    ring: Option<Arc<Ring>>,
    device: Option<OutputFormat>,
    /// What the device was asked for when it was opened (it may have given something else).
    asked: Option<OutputFormat>,
    format: Option<Format>,
    resampler: Option<Resampler>,
    converted: Vec<u8>,
    engine: Thread,
    /// The ring's write position at the last flush: frames from here on are this timeline's.
    base: u64,
    /// Where each stretch of written frames ends (frames since the flush) and the song time it takes
    /// the playhead to, and the start of the first stretch still ahead of the playhead.
    marks: VecDeque<(u64, f64)>,
    from: (u64, f64),
    written: (u64, f64),
    playing: bool,
    /// Whether the device plays float, once asked.
    float: Option<bool>,
    /// Why the device would not open, until the engine has said so.
    pub failed: Option<String>,
    /// Each song reaches the device as it is (bit-perfect): the device is opened in the song's own bits
    /// too, never converted, and opened again when the next song's differ, once what it holds of the song
    /// before has played (`Track::must_reopen`).
    pub(crate) exact: bool,
    /// The bits per sample of the song whose stream is configured next.
    bits: u32,
    /// The highest rate the device is opened at, Hz (0: the song's own); not for a song played as it is.
    pub(crate) max_rate: u32,
    /// High quality output is on, and whether it was when the device was opened.
    float_on: bool,
    opened_float: bool,
    /// The device is kept shallow ([`AudioOutput::shallow`]), as last told.
    shallow: bool,
    /// A flush the device has not been told of yet ([`TELL_FLUSH_US`]), the frames written since it,
    /// and a fade asked for meanwhile, which the device takes with the flush.
    untold: bool,
    since_flush: u64,
    held_ramp: Option<(Option<f32>, f32, i64)>,
}

impl RingTrack {
    /// What the device found it needs while shallow ([`AudioOutput::shallow_depth`]).
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

    /// The device's format for a stream in `format`: its own, with its bits when it goes out exactly, and
    /// otherwise its rate held under the maximum (halved within its family, the ring's resampler converting).
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

    /// The device is let go (a long pause); the next stream opens it again.
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

    /// The device is told of the flush now, and takes the fade asked for since with it.
    fn tell_flush(&mut self) {
        if !std::mem::take(&mut self.untold) {
            return;
        }
        self.output.flush();
        if let Some((from, target, ms)) = self.held_ramp.take() {
            self.ramp_now(from, target, ms);
        }
    }

    /// The end of the engine's turn: a flush not told yet is told now, with whatever came after it.
    pub(crate) fn told(&mut self) {
        self.tell_flush();
    }

    /// High quality output on or off: from the device's next opening, which the next song brings when it
    /// was opened without ([`Track::must_reopen`]).
    pub(crate) fn set_float(&mut self, on: bool) {
        self.float_on = on;
        self.output.float(on);
    }

    /// Whether the device must be opened again for a stream in `format` before it plays: a stream the
    /// engine hands over at another rate or channel count than the device was asked for (the transition
    /// engine converts only what it mixes, and lets the output follow a song that begins with nothing
    /// overlapping it), or at other bits played as it is; or opened in 16 bits with high quality output on
    /// since. A device that would not take what it was asked is converted to by the ring's resampler.
    fn reopens(&self, format: Format) -> bool {
        self.device.is_some() && (self.asked != Some(self.wanted(format)) || (self.float_on && !self.opened_float))
    }

    /// Whether the device plays float samples as they are; asked of it once.
    pub(crate) fn takes_float(&mut self) -> bool {
        *self.float.get_or_insert_with(|| self.output.takes_float())
    }

    /// Music in the ring, µs of the device's time.
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

    /// The engine goes to sleep until the ring holds no more than `us` of music.
    pub(crate) fn wake_at(&self, us: i64) {
        if let Some(r) = &self.ring {
            r.low.store((us.max(0) as u64 * r.rate as u64 / 1_000_000) as u64, Ordering::Relaxed);
            r.waiting.store(true, Ordering::Release);
        }
    }

    /// A volume fade from `from` (or wherever the volume is) to `target` over `ms`: run by the device
    /// when it says it does fades itself, otherwise by the device thread from its next pull.
    pub(crate) fn ramp(&mut self, from: Option<f32>, target: f32, ms: i64) {
        if self.untold && self.ring.is_some() {
            // The device still plays what it held before the flush: the fade is for the music after it,
            // and goes with the flush. A level to start from is taken at once.
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

    /// The ring frame (since the flush) the music reaches song time `media` at, from the stretches
    /// written: the map the playhead is read through, the other way round.
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

    /// Whether the music is over (no gaps are counted past it).
    pub(crate) fn set_ended(&self, ended: bool) {
        if let Some(r) = &self.ring {
            r.ended.store(ended, Ordering::Release);
        }
    }

    /// Why the device failed, once: it would not open, or it stopped taking music and would not open again.
    pub(crate) fn take_failure(&mut self) -> Option<String> {
        self.failed.take().map(|e| format!("the output would not open: {e}")).or_else(|| self.output.failed().map(|e| format!("the output stopped: {e}")))
    }

    pub(crate) fn underruns(&self) -> u64 {
        self.ring.as_ref().map_or(0, |r| r.underruns.load(Ordering::Relaxed))
    }

    /// Song time up to ring frame `p` (since the flush), from the stretches written.
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
            // A stream in another format gets a device of its own (the one before has played out,
            // `Track::must_reopen`), its playhead from nought.
            self.release();
            self.format = Some(format);
        }
        if self.device.is_none() {
            // The stream picks the device's format. Songs mixed into it arrive converted to it by the
            // transition engine; one that begins alone at another rate opens the device again above.
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
        // Stretches at the same pace are one: at one times speed, with no resampling, the whole song
        // is a single mark.
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
            // Told once the music after it is there to start from.
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

    /// What the ring holds of it and the device has not pulled yet is scaled where it lies. A frame the
    /// device pulls in the same instant may come out at either volume, which nobody can hear; what the
    /// device itself already holds (a phone's track holds seconds) stays as it was.
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

    /// Played as it is, a stream in another format than the device's waits for what the device holds to
    /// play out: the ring says that is the end of the music, so the device plays all of it and counts no
    /// gap.
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

    /// An output that only remembers the feed, for pulling by hand.
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
    fn the_playhead_follows_song_time_through_a_change_of_pace() {
        let feed = Arc::new(parking_lot::Mutex::new(None));
        let mut t = RingTrack::new(Box::new(Hand(feed.clone())));
        t.open(F);
        // 100 frames standing for 100 of the song, then 100 standing for 200 (twice the speed).
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
    fn a_pull_says_when_the_music_before_it_was_flushed() {
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
    fn a_new_volume_reaches_the_music_the_ring_still_holds_of_that_song() {
        let feed = Arc::new(parking_lot::Mutex::new(None));
        let mut t = RingTrack::new(Box::new(Hand(feed.clone())));
        t.open(F);
        // Two songs of 100 frames each, the second at twice the speed (100 frames for 200 of song).
        t.write(&pcm(&[16384; 100]), 100.0);
        t.write(&pcm(&[16384; 100]), 200.0);
        let mut out = vec![0f32; 300];
        let mut f = feed.lock();
        let f = f.as_mut().unwrap();
        assert_eq!(f.pull(&mut out[..40]), 40);
        // The first song's volume halves: what is left of it in the ring, and nothing of the next.
        t.rescale(0.0, 100.0, 0.5);
        // The second one's from half way into it (song time 200 is ring frame 150).
        t.rescale(200.0, f64::MAX, 0.25);
        assert_eq!(f.pull(&mut out[40..200]), 160);
        assert!(out[..40].iter().all(|&v| v == 0.5), "pulled before the change: as it was");
        assert!(out[40..100].iter().all(|&v| v == 0.25), "the rest of the first song at its new volume");
        assert!(out[100..150].iter().all(|&v| v == 0.5), "the second untouched up to where it changes");
        assert!(out[150..200].iter().all(|&v| v == 0.125), "and scaled from there");
    }

    #[test]
    fn a_fade_is_run_by_the_puller() {
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
