//! Handovers heard sample by sample: AudioTracks on one output on a virtual clock ([`Air`]), fed by a
//! ring whose music the engine makes again at a sound change ([`Tape`]), against what the music is.
//!
//! The ring's frame `i` is `[amp(i) * sin(2π 100 Hz i), i + 1]`: the left channel is the music (its
//! level the sound setting it was made with), the right numbers the frame, so what the device presents
//! says which frame it is.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use super::*;

const RATE: u32 = 48_000;
const MS: i64 = 1_000_000;
const TONE_HZ: f64 = 100.0;

fn frames_of(ns: i64) -> u64 {
    (ns as i128 * RATE as i128 / 1_000_000_000) as u64
}

fn ns_of(frames: u64) -> i64 {
    (frames as i128 * 1_000_000_000 / RATE as i128) as i64
}

/// An AudioTrack as AudioFlinger plays it: written frames queue; the mixer takes a period of them at a
/// time once it has started (a start threshold after a start or flush), at the volume ramped over that
/// period from the one before.
#[derive(Default)]
struct Wire {
    opened: u64,
    size: u64,
    threshold: u64,
    queue: VecDeque<[f32; 2]>,
    started: bool,
    ready: bool,
    volume: f32,
    applied: f32,
    /// Frames the mixer took since the last flush, its first mix's, and each period's: (presented from,
    /// taken before, taken).
    consumed: u64,
    first_mix: u64,
    mixes: VecDeque<(i64, u64, u64)>,
    started_ns: i64,
    released: bool,
    /// Every write fails, as on a track the platform tore down.
    dead: bool,
    session: i32,
    /// Each frame's number as the device presented it, by device frame; one run per flush.
    traces: Vec<Vec<(u64, f32)>>,
}

impl Wire {
    fn new(frames: u64, session: i32) -> Wire {
        Wire { opened: frames, size: frames, threshold: (RATE as u64 / 4).min(frames), volume: 1.0, applied: 1.0, session, traces: vec![Vec::new()], ..Wire::default() }
    }
}

/// The output: every `period` the mixer takes a period from each playing track and presents it `delay`
/// later.
struct Air {
    period: u64,
    delay: i64,
    next_mix: i64,
    origin: i64,
    wires: Arc<Mutex<Vec<Arc<Mutex<Wire>>>>>,
    /// What the ear hears, by device frame from `origin`.
    heard: Vec<[f32; 2]>,
}

impl Air {
    fn mix(&mut self, now: i64) {
        let at = now + self.delay;
        let first = frames_of(at - self.origin) as usize;
        let p = self.period as usize;
        if self.heard.len() < first + p {
            self.heard.resize(first + p, [0.0; 2]);
        }
        for w in self.wires.lock().iter() {
            let mut w = w.lock();
            if !w.started || w.released {
                continue;
            }
            let need = w.threshold.min(w.size) as usize;
            if !w.ready && w.queue.len() >= need.max(1) {
                w.ready = true;
            }
            if !w.ready {
                continue;
            }
            let n = p.min(w.queue.len());
            let (from, to) = (w.applied, w.volume);
            for j in 0..n {
                let g = from + (to - from) * (j + 1) as f32 / p as f32;
                let s = w.queue.pop_front().expect("counted");
                let h = &mut self.heard[first + j];
                h[0] += s[0] * g;
                h[1] += s[1] * g;
                w.traces.last_mut().expect("one").push(((first + j) as u64, s[1]));
            }
            let before = w.consumed;
            if before == 0 {
                w.first_mix = n as u64;
            }
            w.consumed += n as u64;
            w.mixes.push_back((at, before, n as u64));
            if w.mixes.len() > 64 {
                w.mixes.pop_front();
            }
            w.applied = to;
        }
        self.next_mix = now + ns_of(self.period);
    }
}

/// The app's side of a [`Wire`].
struct WireSink {
    wire: Arc<Mutex<Wire>>,
    staging: Vec<f32>,
    now: Arc<AtomicI64>,
    /// getTimestamp's first readings come this long after a start; its times are off by up to `jitter` ns,
    /// as the output reported the period presented (the same for every track).
    stamp_after: i64,
    jitter: i64,
    /// Bluetooth: the output says what it presented once per packet of this many ns, not per period; a
    /// reading with no new packet since the last is the last one's frames at the current time (Android's
    /// "device stall time corrected using current time").
    packet: i64,
    last_packet: i64,
    heads: Heads,
}

/// How a track's play head counts what the mixer took from it.
#[derive(Clone, Copy, Debug)]
enum Heads {
    /// Exactly.
    Taken,
    /// Never its first mix (released a period late, the timestamps lagging it).
    Late,
    /// In steps of this many µs from its start (released per Bluetooth packet).
    Steps(i64),
    /// This many µs ahead (a resampler's look-ahead).
    Ahead(i64),
}

impl Heads {
    fn of(self, w: &Wire) -> u64 {
        match self {
            Heads::Taken => w.consumed,
            Heads::Late => w.consumed - w.first_mix,
            Heads::Steps(us) => w.consumed / frames_of(us * 1_000) * frames_of(us * 1_000),
            Heads::Ahead(us) if w.consumed > 0 => w.consumed + frames_of(us * 1_000),
            Heads::Ahead(_) => 0,
        }
    }
}

impl WireSink {
    fn now(&self) -> i64 {
        self.now.load(Ordering::Relaxed)
    }
}

impl Sink for WireSink {
    fn staging(&mut self) -> &mut [f32] {
        &mut self.staging
    }
    fn write(&mut self, from: usize, len: usize) -> Result<usize, i32> {
        let mut w = self.wire.lock();
        if w.dead {
            return Err(-6);
        }
        let frames = (w.size as usize).saturating_sub(w.queue.len()).min(len / 8);
        for k in 0..frames {
            let i = from / 4 + k * 2;
            w.queue.push_back([self.staging[i], self.staging[i + 1]]);
        }
        Ok(frames * 8)
    }
    fn play(&mut self) {
        let mut w = self.wire.lock();
        if !w.started {
            w.started = true;
            w.started_ns = self.now();
        }
    }
    fn pause(&mut self) {
        let mut w = self.wire.lock();
        w.started = false;
        w.ready = false;
    }
    fn flush(&mut self) {
        let mut w = self.wire.lock();
        assert!(!w.started, "a track is only flushed paused");
        w.queue.clear();
        (w.consumed, w.first_mix) = (0, 0);
        w.mixes.clear();
        w.traces.push(Vec::new());
    }
    fn stop(&mut self) {}
    fn set_volume(&mut self, volume: f32) {
        self.wire.lock().volume = volume;
    }
    /// As the real one: the timestamp while playing, else the play head.
    fn heard(&mut self, playing: bool) -> Option<(u64, i64)> {
        let now = self.now();
        if playing {
            if let Some(s) = self.stamp() {
                return Some(s);
            }
        }
        Some((self.heads.of(&self.wire.lock()), now))
    }
    fn resize(&mut self, frames: u64) -> u64 {
        let mut w = self.wire.lock();
        w.size = frames.clamp(16, w.opened);
        w.threshold = (RATE as u64 / 4).min(w.size);
        w.size
    }
    fn consumed(&mut self) -> Option<u64> {
        Some(self.heads.of(&self.wire.lock()))
    }
    fn stamp(&mut self) -> Option<(u64, i64)> {
        let now = self.now();
        let w = self.wire.lock();
        if !w.started || now < w.started_ns + self.stamp_after {
            return None;
        }
        let off = |at: i64| ((at as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 33) as i64 % (2 * self.jitter + 1) - self.jitter;
        if self.packet == 0 {
            // The first frame of the period presented last, and when, off by a little.
            let &(at, before, _) = w.mixes.iter().rev().find(|m| m.0 <= now)?;
            return Some((before, at + off(at)));
        }
        let at = now / self.packet * self.packet;
        let &(mixed, before, n) = w.mixes.iter().rev().find(|m| m.0 <= at)?;
        let frames = before + frames_of(at - mixed).min(n);
        if std::mem::replace(&mut self.last_packet, at) == at {
            return Some((frames, now));
        }
        Some((frames, (at + off(at)).min(now)))
    }
    fn session(&mut self) -> i32 {
        self.wire.lock().session
    }
    fn release(&mut self) {
        self.wire.lock().released = true;
    }
}

/// Opens [`Wire`]s on the [`Air`]; the second track only while `beside_opens`, dead while `beside_dies`.
struct AirOpener {
    wires: Arc<Mutex<Vec<Arc<Mutex<Wire>>>>>,
    now: Arc<AtomicI64>,
    stamp_after: i64,
    jitter: i64,
    packet: i64,
    beside_opens: Arc<std::sync::atomic::AtomicBool>,
    /// How a second track's play head counts.
    heads: Heads,
    beside_dies: Arc<std::sync::atomic::AtomicBool>,
}

impl AirOpener {
    fn wire(&mut self, frames: u64, session: i32, heads: Heads) -> Opened {
        let wire = Arc::new(Mutex::new(Wire::new(frames, session)));
        let mut wires = self.wires.lock();
        wires.push(wire.clone());
        let sink = WireSink { wire, staging: vec![0.0; CHUNK_BYTES / 4], now: self.now.clone(), stamp_after: self.stamp_after, jitter: self.jitter, packet: self.packet, last_packet: -1, heads };
        Opened { sink: Box::new(sink), frames, starts_full: false }
    }
}

impl Opener for AirOpener {
    fn open(&mut self, _format: OutputFormat, _float: bool, frames: u64) -> Result<Opened, String> {
        Ok(self.wire(frames, 7, Heads::Taken))
    }
    fn beside(&mut self, _format: OutputFormat, _float: bool, frames: u64, session: i32) -> Result<Opened, String> {
        assert_eq!(session, 7, "in the first track's audio session");
        if !self.beside_opens.load(Ordering::Relaxed) {
            return Err("no room for another track".into());
        }
        let opened = self.wire(frames, session, self.heads);
        self.wires.lock().last().expect("just opened").lock().dead = self.beside_dies.load(Ordering::Relaxed);
        Ok(opened)
    }
}

/// The engine's ring, its music a function of the frame and the sound it was made with.
struct Tape {
    read: u64,
    discard: u64,
    written: u64,
    /// From which frame each sound was made, its level, and the level it was blended from over
    /// [`nori_player::pipeline::BLEND_US`] (a change the ring took in place).
    sounds: Vec<(u64, f32, Option<f32>)>,
    untold: bool,
    flushed: bool,
    /// A sound screen is open (`Engine::set_tuning`).
    tuned: bool,
    now: Arc<AtomicI64>,
    clock: Arc<Clock>,
}

impl Tape {
    fn amp(&self, i: u64) -> f32 {
        let blend = (RATE as i64 * nori_player::pipeline::BLEND_US / 1_000_000) as u64;
        let k = self.sounds.iter().rposition(|s| s.0 <= i).expect("the first sound is from frame 0");
        let (from, amp, old) = self.sounds[k];
        match old {
            Some(old) if i - from < blend => nori_player::pipeline::blended(old, amp, (i - from) as usize, blend as usize),
            _ => amp,
        }
    }

    fn frame(&self, i: u64) -> [f32; 2] {
        let v = (std::f64::consts::TAU * TONE_HZ * i as f64 / RATE as f64).sin() as f32;
        [self.amp(i) * v, (i + 1) as f32]
    }

    /// A sound change as the engine makes it (`nori_engine` `RingTrack::freeze`/`cut`): tuned, a device
    /// holding more than a quarter second not yet mixed drops what it holds, the music made again from a
    /// little before what it played; otherwise the ring changes from the first frame no pull took, blended.
    fn change(&mut self, amp: f32) {
        let now = self.now.load(Ordering::Relaxed);
        let held = self.clock.latency_frames(now).saturating_sub(self.clock.mixed_us() * RATE as u64 / 1_000_000);
        let old = self.amp(self.read);
        if self.tuned && held > frames_of(250 * MS) {
            let cut = (self.read.saturating_sub(held + frames_of(100 * MS))).max(self.discard);
            self.sounds.retain(|s| s.0 < cut);
            self.sounds.push((cut, amp, None));
            self.discard = cut;
            self.untold = true;
        } else {
            let at = self.read;
            self.sounds.retain(|s| s.0 < at);
            self.sounds.push((at, amp, Some(old)));
        }
    }

    /// A jump: the ring's music goes, the next starts where it was written to.
    fn jump(&mut self) {
        self.discard = self.written;
        self.read = self.written;
        self.written += frames_of(10_200 * MS);
        self.untold = true;
    }
}

impl Ring for Arc<Mutex<Tape>> {
    fn available(&self) -> usize {
        let t = self.lock();
        t.written.saturating_sub(t.read.max(t.discard)) as usize
    }
    fn pull(&mut self, out: &mut [f32]) -> usize {
        let mut t = self.lock();
        if std::mem::take(&mut t.untold) {
            t.flushed = true;
        }
        let at = t.read.max(t.discard);
        let n = ((out.len() / 2) as u64).min(t.written.saturating_sub(at));
        for k in 0..n {
            let f = t.frame(at + k);
            out[k as usize * 2..k as usize * 2 + 2].copy_from_slice(&f);
        }
        t.read = at + n;
        // The engine's burst when the ring runs low.
        if t.written - t.read <= frames_of(1_750 * MS) {
            t.written += frames_of(10_200 * MS);
        }
        n as usize
    }
    fn pull_i16(&mut self, _out: &mut [i16]) -> usize {
        unreachable!("float tracks")
    }
    fn flushed(&mut self) -> bool {
        std::mem::take(&mut self.lock().flushed)
    }
    fn ending(&self) -> bool {
        false
    }
    fn wake_engine(&self) {}
    fn rewind(&mut self, frames: u64) {
        let mut t = self.lock();
        t.read = t.read.saturating_sub(frames).max(t.discard);
    }
    fn behind(&self) -> u64 {
        let t = self.lock();
        t.read.saturating_sub(t.discard)
    }
}

/// How the output behaves.
#[derive(Clone, Copy, Debug)]
struct Output {
    period_ms: i64,
    delay_ms: i64,
    stamp_after_ms: i64,
    jitter_us: i64,
    packet_us: i64,
    /// How a second track's play head counts.
    heads: Heads,
}

const SPEAKER: Output = Output { period_ms: 20, delay_ms: 40, stamp_after_ms: 60, jitter_us: 50, packet_us: 0, heads: Heads::Taken };
/// A2DP on a Galaxy S22: a long way to the ear, no timestamp for a while after a start, then one per
/// packet with milliseconds of jitter, stall-corrected between packets.
const BLUETOOTH: Output = Output { period_ms: 20, delay_ms: 250, stamp_after_ms: 400, jitter_us: 3_000, packet_us: 23_220, heads: Heads::Taken };

struct Rig {
    writer: Writer<Arc<Mutex<Tape>>>,
    tape: Arc<Mutex<Tape>>,
    air: Air,
    control: Control,
    clock: Arc<Clock>,
    now: Arc<AtomicI64>,
    next: Option<i64>,
    wakes: u32,
    beside_opens: Arc<std::sync::atomic::AtomicBool>,
    beside_dies: Arc<std::sync::atomic::AtomicBool>,
    /// Once checked: the worst difference, frames, between the engine's play head and what was presented.
    head_error: Option<u64>,
}

impl Rig {
    fn new(out: Output) -> Rig {
        let start = 1_000 * MS;
        let now = Arc::new(AtomicI64::new(start));
        let clock = Arc::new(Clock::default());
        let tape = Arc::new(Mutex::new(Tape { read: 0, discard: 0, written: frames_of(10_200 * MS), sounds: vec![(0, 0.5, None)], untold: false, flushed: false, tuned: false, now: now.clone(), clock: clock.clone() }));
        let wires = Arc::new(Mutex::new(Vec::new()));
        let beside_opens = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let beside_dies = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut opener = AirOpener { wires: wires.clone(), now: now.clone(), stamp_after: out.stamp_after_ms * MS, jitter: out.jitter_us * 1_000, packet: out.packet_us * 1_000, beside_opens: beside_opens.clone(), heads: out.heads, beside_dies: beside_dies.clone() };
        let format = OutputFormat { rate: RATE, channels: 2, bits: 0 };
        let frames = track_frames(RATE, false);
        let opened = opener.open(format, true, frames).expect("opens");
        let reopen = Reopen { opener: Arc::new(Mutex::new(Box::new(opener))), frames, failure: Arc::new(Mutex::new(None)) };
        let writer = Writer::new(tape.clone(), opened, reopen, format, true, clock.clone(), Arc::new(AtomicU64::new(0)));
        let period = frames_of(out.period_ms * MS);
        let air = Air { period, delay: out.delay_ms * MS, next_mix: start + ns_of(period), origin: start, wires, heard: Vec::new() };
        Rig { writer, tape, air, control: Control::default(), clock, now, next: Some(start), wakes: 0, beside_opens, beside_dies, head_error: None }
    }

    fn now(&self) -> i64 {
        self.now.load(Ordering::Relaxed)
    }

    fn wake(&mut self) {
        self.wakes += 1;
        let now = self.now();
        self.next = self.writer.step(now, &mut self.control).map(|ms| now + ms as i64 * MS);
    }

    /// Runs the clock for `ms`: the mixer every period, the writer when it asked to wake.
    fn run(&mut self, ms: i64) {
        let end = self.now() + ms * MS;
        loop {
            let to = [self.next, Some(self.air.next_mix), Some(end)].into_iter().flatten().min().expect("the end");
            self.now.store(to, Ordering::Relaxed);
            if self.air.next_mix == to {
                self.air.mix(to);
                self.check_head(to);
            }
            if self.next.is_some_and(|n| n <= to) {
                self.wake();
            }
            if to >= end {
                break;
            }
        }
    }

    /// The engine's play head (what the ring gave, less what the writer says is not yet heard) against
    /// the frame presented now, when one track alone is heard.
    fn check_head(&mut self, now: i64) {
        if !self.control.playing || self.writer.handover.is_some() || self.head_error.is_none() {
            return;
        }
        let d = frames_of(now - self.air.origin) as usize;
        let Some(&[_, number]) = self.air.heard.get(d) else { return };
        if number < 1.0 || number.fract() != 0.0 {
            return;
        }
        let read = self.tape.lock().read;
        let head = read.saturating_sub(self.clock.latency_frames(now));
        self.head_error = self.head_error.max(Some(head.abs_diff(number as u64 - 1)));
    }

    fn play(&mut self) {
        self.control.playing = true;
        self.wake();
    }

    fn change(&mut self, amp: f32) {
        self.tape.lock().change(amp);
        // The engine tells the output at the end of its turn (`TrackOutput::flush`: an unpark).
        self.wake();
    }

    /// A sound screen opened or closed (`Engine::set_tuning`); opened, the music is made again as it is.
    fn tune(&mut self, on: bool) {
        self.control.shallow = on;
        let mut t = self.tape.lock();
        t.tuned = on;
        if on {
            let amp = t.sounds.last().expect("a sound").1;
            t.change(amp);
        }
        drop(t);
        self.wake();
    }

    fn open_tracks(&self) -> usize {
        self.air.wires.lock().iter().filter(|w| !w.lock().released).count()
    }
}

/// What was heard, measured.
#[derive(Debug)]
struct Heard {
    /// Frames skipped or played twice, all told, among the frames heard alone (by their numbers),
    /// silence left out.
    slip: i64,
    /// The longest silence, frames.
    gap: u64,
    /// The level of each 10 ms against the music's lowest and highest, dB.
    level_db: (f64, f64),
    /// The largest jump in the music's slope (second difference), full scale.
    click: f32,
}

/// Measures the heard music from device frame `from` to `to` against the levels `amps` it may have.
fn measure(heard: &[[f32; 2]], from: usize, to: usize, amps: (f32, f32)) -> Heard {
    let heard = &heard[from..to];
    let silent = |h: &[f32; 2]| h[0] == 0.0 && h[1] == 0.0;
    // Frame numbers heard alone at full level (whole, and the next one after them), less their place among
    // the frames not silent.
    let sounding: Vec<&[f32; 2]> = heard.iter().filter(|h| !silent(h)).collect();
    let offsets: Vec<i64> = sounding.windows(2).enumerate().filter(|(_, p)| p[0][1] >= 1.0 && p[0][1].fract() == 0.0 && p[1][1] == p[0][1] + 1.0).map(|(k, p)| p[0][1] as i64 - k as i64).collect();
    let slip = offsets.windows(2).map(|p| (p[1] - p[0]).abs()).sum();
    let (mut gap, mut run) = (0, 0);
    for h in heard {
        run = if silent(h) { run + 1 } else { 0 };
        gap = gap.max(run);
    }
    // 10 ms is a whole period of the tone: its RMS is amp / √2.
    let w = RATE as usize / 100;
    let (mut low, mut high) = (f64::MAX, f64::MIN);
    for k in (0..heard.len() - w).step_by(w / 4) {
        let rms = (heard[k..k + w].iter().map(|h| (h[0] as f64).powi(2)).sum::<f64>() / w as f64).sqrt() * std::f64::consts::SQRT_2;
        low = low.min(20.0 * (rms / amps.0 as f64).log10());
        high = high.max(20.0 * (rms / amps.1 as f64).log10());
    }
    let click = heard.windows(3).map(|v| (v[2][0] - 2.0 * v[1][0] + v[0][0]).abs()).fold(0.0, f32::max);
    Heard { slip, gap, level_db: (low, high), click }
}

/// Each handover's alignment: the frame numbers the track the music left and the one that took it
/// presented at the same device frame differ by this many frames (from the frames each played at full
/// level, as the mixer took them).
fn alignments(air: &Air) -> Vec<i64> {
    let wires = air.wires.lock();
    // Each run of a track's frames (one per flush): device frame, and frame number less device frame.
    let runs: Vec<Vec<(u64, i64)>> = wires
        .iter()
        .flat_map(|w| w.lock().traces.clone())
        .map(|trace| trace.windows(2).filter(|p| p[0].1 >= 1.0 && p[0].1.fract() == 0.0 && p[1].1 == p[0].1 + 1.0 && p[1].0 == p[0].0 + 1).map(|p| (p[0].0, p[0].1 as i64 - p[0].0 as i64)).collect())
        .filter(|run: &Vec<(u64, i64)>| !run.is_empty())
        .collect();
    let mut out = Vec::new();
    for (k, joined) in runs.iter().enumerate() {
        let (start, offset) = joined[0];
        // Another run heard in the half second before this one began: the one it took over from.
        for left in runs.iter().enumerate().filter(|(j, _)| *j != k).map(|(_, run)| run) {
            if let Some(&(_, was)) = left.iter().rev().find(|(d, _)| *d < start && start - d < frames_of(500 * MS)) {
                out.push(offset - was);
            }
        }
    }
    out
}

/// Plays 10 s, runs `script` (changes and the like), then 8 s more; measures from 1 s before the script.
fn heard_after(out: Output, script: impl FnOnce(&mut Rig)) -> (Rig, Heard) {
    let mut r = Rig::new(out);
    r.play();
    r.run(10_000);
    let from = frames_of(r.now() - r.air.origin) as usize - RATE as usize;
    r.head_error = Some(0);
    script(&mut r);
    r.run(8_000);
    let to = frames_of(r.now() - r.air.origin) as usize;
    let amps = {
        let t = r.tape.lock();
        let levels = t.sounds.iter().map(|s| s.1);
        (levels.clone().fold(0.5, f32::min), levels.fold(0.5, f32::max))
    };
    let h = measure(&r.air.heard, from, to, amps);
    (r, h)
}

/// One track left, the deep one, playing at full volume, no handover under way.
fn assert_home(what: &str, r: &Rig) {
    assert_eq!(r.open_tracks(), 1, "{what}: one track left");
    assert!(r.writer.handover.is_none(), "{what}: the handover is over");
    let wires = r.air.wires.lock();
    let w = wires.iter().find(|w| !w.lock().released).expect("one").lock();
    assert!(w.started && w.opened == track_frames(RATE, false) && w.volume == 1.0, "{what}: the deep track, playing");
}

fn assert_seamless(what: &str, out: Output, r: &Rig, h: &Heard) {
    assert_eq!(h.slip, 0, "{what}: every frame heard once, in order: {h:?}");
    assert_eq!(h.gap, 0, "{what}: no silence: {h:?}");
    assert!(h.level_db.0 > -0.5 && h.level_db.1 < 0.5, "{what}: no dip or bump: {h:?}");
    assert!(h.click < 1e-3, "{what}: no click: {h:?}");
    // As far off as the output's timestamps are (a packet, stall-corrected), and a second track's play head.
    let head_us = match out.heads {
        Heads::Taken => 0,
        Heads::Late => out.period_ms * 1_000,
        Heads::Steps(us) | Heads::Ahead(us) => us,
    };
    let off = frames_of(MS / 10 + (out.jitter_us + out.packet_us + head_us) * 1_000);
    assert!(r.head_error.is_some_and(|e| e <= off), "{what}: the engine's play head is where the ear is: {:?} frames off", r.head_error);
    assert_home(what, r);
}

/// Ms from `at_ns` until the ear hears the music halfway from level `old` to `new`.
fn heard_in_ms(r: &Rig, at_ns: i64, old: f32, new: f32) -> i64 {
    let w = RATE as usize / 100;
    let from = frames_of(at_ns - r.air.origin) as usize;
    let half = (old + new) / 2.0;
    let k = (from..r.air.heard.len() - w)
        .find(|&k| {
            let rms = (r.air.heard[k..k + w].iter().map(|h| (h[0] as f64).powi(2)).sum::<f64>() / w as f64).sqrt() * std::f64::consts::SQRT_2;
            (rms as f32 - half) * (new - old).signum() >= 0.0
        })
        .expect("heard");
    ns_of((k + w / 2 - from) as u64) / MS
}

/// The sound the ear hears now.
fn heard_amp(r: &Rig) -> f32 {
    let t = r.tape.lock();
    t.amp(t.read - r.clock.latency_frames(r.now()))
}

#[test]
fn changes_wait_for_a_deep_track() {
    // Untuned, a change is made in the ring past what the track holds: no second track, seamless, heard
    // once the track has played what it held.
    for out in [SPEAKER, BLUETOOTH] {
        let mut at = 0;
        let (r, h) = heard_after(out, |r| {
            at = r.now();
            r.change(0.6);
            r.run(5_000);
        });
        let what = format!("{out:?}");
        assert_seamless(&what, out, &r, &h);
        assert_eq!(r.air.wires.lock().len(), 1, "{what}: no second track");
        let ms = heard_in_ms(&r, at, 0.5, 0.6);
        assert!(ms >= LOW_US / 1_000 && ms <= TRACK_US / 1_000 + out.delay_ms + out.period_ms, "{what}: heard after {ms} ms, once the track played what it held");
        assert_eq!(heard_amp(&r), 0.6, "{what}: the change is heard");
    }
}

/// Outputs the second track meets, and whether it can be lined up with the first: periods, latency,
/// Bluetooth's timestamps, and play heads that do not say what the mixer took (a new track's first mix
/// never counted, counted per Bluetooth packet, a resampler's look-ahead).
const OUTPUTS: [(Output, bool); 9] = [
    (SPEAKER, true),
    (Output { period_ms: 5, delay_ms: 10, ..SPEAKER }, true),
    (Output { period_ms: 40, delay_ms: 80, ..SPEAKER }, true),
    (Output { delay_ms: 150, ..BLUETOOTH }, true),
    (BLUETOOTH, true),
    (Output { heads: Heads::Late, ..BLUETOOTH }, false),
    (Output { heads: Heads::Steps(23_220), ..BLUETOOTH }, false),
    (Output { heads: Heads::Ahead(1_000), ..BLUETOOTH }, true),
    (Output { heads: Heads::Ahead(5_000), ..BLUETOOTH }, false),
];

#[test]
fn tuning_drops_the_deep_buffer_at_once() {
    // A sound screen opened: the deep track hands the music to a second track and takes it back shallow,
    // a change made meanwhile riding along; where the two can't be lined up the track is emptied
    // instead. Never a frame heard twice or lost; seamless where lined up; every change after in place.
    for (out, lines_up) in OUTPUTS {
        let (mut wakes, mut at, mut tracks) = (0, 0, 0);
        let (mut r, h) = heard_after(out, |r| {
            r.tune(true);
            wakes = r.wakes;
            r.run(50);
            at = r.now();
            r.change(0.6);
            r.run(3_000);
            wakes = r.wakes - wakes;
            tracks = r.air.wires.lock().len();
            r.change(0.7);
        });
        let what = format!("{out:?}");
        assert_eq!(h.slip, 0, "{what}: every frame heard once, in order: {h:?}");
        assert_eq!(heard_amp(&r), 0.7, "{what}: the last change is heard");
        assert_eq!(r.air.wires.lock().len(), tracks, "{what}: the change once shallow made in place");
        if lines_up {
            assert_seamless(&what, out, &r, &h);
            let aligned = alignments(&r.air);
            assert!(aligned.len() >= 2 && aligned.iter().all(|a| a.abs() <= 1), "{what}: over to the second track and back, to a frame: {aligned:?}");
            // The output's own latency, the second track's first timestamp, a few mixes, and a little more.
            let ms = heard_in_ms(&r, at, 0.5, 0.6);
            assert!(ms <= out.delay_ms + out.stamp_after_ms + 5 * out.period_ms + 100, "{what}: the change is heard after {ms} ms");
        } else {
            let wires = r.air.wires.lock();
            assert!(wires[1..].iter().all(|w| w.lock().traces.iter().flatten().all(|t| t.1 == 0.0)), "{what}: no second track played music");
            drop(wires);
            assert!(h.gap < frames_of(out.delay_ms * MS + 400 * MS), "{what}: the gap of a track emptied: {h:?}");
            assert_home(&what, &r);
        }
        assert!(wakes <= 300, "{what}: {wakes} wakes for it");
        r.tune(false);
        r.run(15_000);
        let wakes = r.wakes;
        r.run(60_000);
        assert!(r.wakes - wakes <= 8, "{what}: closed, a wake every ten seconds or so: {}", r.wakes - wakes);
    }

    // A slider dragged as the screen opens, its steps 40 or 100 ms apart: every step heard.
    for (out, step_ms) in [(SPEAKER, 40), (BLUETOOTH, 40), (BLUETOOTH, 100)] {
        let what = format!("dragged in {step_ms} ms steps on {out:?}");
        let (r, h) = heard_after(out, |r| {
            r.tune(true);
            for k in 1..=25 {
                r.change(0.5 + 0.01 * k as f32);
                r.run(step_ms);
            }
        });
        assert_seamless(&what, out, &r, &h);
        assert_eq!(heard_amp(&r), 0.75, "{what}: the last change is heard");
    }
}

#[test]
fn handover() {
    // Paused (the engine fading out first) and resumed at moments through a handover.
    for after_ms in [20, 120, 200, 260, 400, 700] {
        let (r, h) = heard_after(SPEAKER, |r| {
            r.tune(true);
            r.change(0.6);
            r.run(after_ms);
            r.control.ramp = Some((None, 0.0, 100));
            r.wake();
            r.run(100);
            // `TrackOutput::pause`.
            r.clock.freeze(r.now());
            r.control.playing = false;
            r.wake();
            r.run(2_000);
            assert_eq!(r.open_tracks(), 1, "after {after_ms} ms: one track left");
            r.control.playing = true;
            r.control.ramp = Some((Some(0.0), 1.0, 100));
            r.wake();
        });
        let what = format!("paused {after_ms} ms into it");
        assert_eq!(h.slip, 0, "{what}: on from the frame it paused at: {h:?}");
        // The first period after the pause ramps from the volume the last one before it was mixed at.
        assert!(h.click < 2e-3, "{what}: no click: {h:?}");
        assert_home(&what, &r);
        assert_eq!(heard_amp(&r), 0.6, "{what}: the change is heard");
    }

    // A jump at moments through a handover: the new music, from its start, on the deep track.
    for after_ms in [20, 120, 200, 260, 400, 700] {
        let mut jumped = 0;
        let (r, _) = heard_after(SPEAKER, |r| {
            r.tune(true);
            r.run(after_ms);
            let mut t = r.tape.lock();
            t.jump();
            jumped = t.read;
            drop(t);
            r.wake();
        });
        let what = format!("jumped {after_ms} ms into it");
        assert_home(&what, &r);
        // What the deep track played since it was emptied for the jump.
        let wires = r.air.wires.lock();
        let home = wires[0].lock();
        let numbers: Vec<u64> = home.traces.last().expect("a run").iter().map(|t| t.1 as u64 - 1).collect();
        assert_eq!(numbers[0], jumped, "{what}: from its first frame");
        assert!(numbers.windows(2).all(|p| p[1] == p[0] + 1), "{what}: and on, every frame once");
    }
}

#[test]
fn second_track() {
    // No second track: the deep one is emptied and refilled as before, nothing lost or heard twice.
    let (r, h) = heard_after(SPEAKER, |r| {
        r.beside_opens.store(false, Ordering::Relaxed);
        r.tune(true);
        r.change(0.6);
    });
    assert_eq!(h.slip, 0, "on from where it was: {h:?}");
    assert_home("no second track", &r);
    let wires = r.air.wires.lock();
    assert_eq!((wires.len(), wires[0].lock().traces.len()), (1, 2), "the one track, emptied once");
    drop(wires);
    assert_eq!(heard_amp(&r), 0.6, "the change is heard");

    // A second track that dies as it joins: it goes at once, and the deep one is emptied and refilled as
    // with no second track.
    let (r, h) = heard_after(SPEAKER, |r| {
        r.beside_dies.store(true, Ordering::Relaxed);
        r.tune(true);
        r.change(0.6);
        r.run(100);
        assert!(r.writer.handover.is_none() && r.open_tracks() == 1, "the dead track let go at once");
    });
    assert_eq!(h.slip, 0, "on from where it was: {h:?}");
    assert_home("a dead second track", &r);
    assert_eq!(heard_amp(&r), 0.6, "the change is heard");
}

#[test]
fn tuned_changes_are_heard_soon() {
    // A sound screen open (a shallow track), a change at moments through its top-up cycle: in place,
    // seamless, heard within the output's own latency and what the track holds besides (a top-up and a
    // late wake on the speaker, a quarter more of Bluetooth's long way).
    for out in [SPEAKER, BLUETOOTH] {
        let mut worst = 0;
        for k in 0..8 {
            let (mut at, mut tracks) = (0, 0);
            let (r, h) = heard_after(out, |r| {
                r.tune(true);
                r.run(3_000 + k * 17);
                tracks = r.air.wires.lock().len();
                at = r.now();
                r.change(0.6);
            });
            let what = format!("tuning on {out:?}, {k}");
            assert_seamless(&what, out, &r, &h);
            assert_eq!(r.air.wires.lock().len(), tracks, "{what}: the one track");
            worst = worst.max(heard_in_ms(&r, at, 0.5, 0.6));
        }
        let late = worst - out.delay_ms - out.period_ms;
        assert!(late <= if out.packet_us > 0 { 120 } else { 90 }, "{out:?}: heard {late} ms after the output's own latency at worst");
    }
}
