//! The track heard sample by sample: an AudioTrack on an output on a virtual clock ([`Air`]), fed by a
//! ring whose music the engine changes in place at a sound change ([`Tape`]), against what the music is.
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
    /// Frames the mixer took since the last flush, and each period's: (presented from, taken before,
    /// taken).
    consumed: u64,
    mixes: VecDeque<(i64, u64, u64)>,
    started_ns: i64,
    /// Each frame's number as the device presented it, by device frame; one run per flush.
    traces: Vec<Vec<(u64, f32)>>,
}

impl Wire {
    fn new(frames: u64) -> Wire {
        Wire { opened: frames, size: frames, threshold: (RATE as u64 / 4).min(frames), volume: 1.0, applied: 1.0, traces: vec![Vec::new()], ..Wire::default() }
    }
}

/// The output: every `period` the mixer takes a period from the track and presents it `delay` later.
struct Air {
    period: u64,
    delay: i64,
    next_mix: i64,
    origin: i64,
    wire: Arc<Mutex<Wire>>,
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
        self.next_mix = now + ns_of(self.period);
        let mut w = self.wire.lock();
        if !w.started {
            return;
        }
        let need = w.threshold.min(w.size) as usize;
        if !w.ready && w.queue.len() >= need.max(1) {
            w.ready = true;
        }
        if !w.ready {
            return;
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
        w.consumed += n as u64;
        w.mixes.push_back((at, before, n as u64));
        if w.mixes.len() > 64 {
            w.mixes.pop_front();
        }
        w.applied = to;
    }
}

/// The app's side of a [`Wire`].
struct WireSink {
    wire: Arc<Mutex<Wire>>,
    staging: Vec<f32>,
    now: Arc<AtomicI64>,
    /// getTimestamp's first readings come this long after a start; its times are off by up to `jitter` ns,
    /// as the output reported the period presented.
    stamp_after: i64,
    jitter: i64,
    /// Bluetooth: the output says what it presented once per packet of this many ns, not per period; a
    /// reading with no new packet since the last is the last one's frames at the current time (Android's
    /// "device stall time corrected using current time").
    packet: i64,
    last_packet: i64,
}

impl WireSink {
    fn now(&self) -> i64 {
        self.now.load(Ordering::Relaxed)
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
}

impl Sink for WireSink {
    fn staging(&mut self) -> &mut [f32] {
        &mut self.staging
    }
    fn write(&mut self, from: usize, len: usize) -> Result<usize, i32> {
        let mut w = self.wire.lock();
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
        w.consumed = 0;
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
        Some((self.wire.lock().consumed, now))
    }
    /// Keeps what it holds past a smaller size, as `setBufferSizeInFrames` does.
    fn resize(&mut self, frames: u64) -> u64 {
        let mut w = self.wire.lock();
        w.size = frames.clamp(16, w.opened);
        w.threshold = (RATE as u64 / 4).min(w.size);
        w.size
    }
    fn consumed(&mut self) -> Option<u64> {
        Some(self.wire.lock().consumed)
    }
    fn release(&mut self) {}
}

/// Opens the one [`Wire`] on the [`Air`].
struct AirOpener(Option<WireSink>);

impl Opener for AirOpener {
    fn open(&mut self, _format: OutputFormat, _float: bool, frames: u64) -> Result<Opened, String> {
        let sink = self.0.take().ok_or("one track")?;
        assert_eq!(sink.wire.lock().opened, frames, "deep");
        Ok(Opened { sink: Box::new(sink), frames, starts_full: false })
    }
}

/// The engine's ring, its music a function of the frame and the sound it was made with.
struct Tape {
    read: u64,
    discard: u64,
    written: u64,
    /// From which frame each sound was made, its level, and whether it was blended in over
    /// [`nori_player::pipeline::BLEND_US`] from what the ring held there.
    sounds: Vec<(u64, f32, bool)>,
    untold: bool,
    flushed: bool,
}

impl Tape {
    fn amp(&self, i: u64) -> f32 {
        self.amp_of(self.sounds.len(), i)
    }

    /// The level of frame `i` as the first `n` sounds made it.
    fn amp_of(&self, n: usize, i: u64) -> f32 {
        let blend = (RATE as i64 * nori_player::pipeline::BLEND_US / 1_000_000) as u64;
        let k = self.sounds[..n].iter().rposition(|s| s.0 <= i).expect("the first sound is from frame 0");
        match self.sounds[k] {
            (from, amp, true) if i - from < blend => nori_player::pipeline::blended(self.amp_of(k, i), amp, (i - from) as usize, blend as usize),
            (_, amp, _) => amp,
        }
    }

    fn frame(&self, i: u64) -> [f32; 2] {
        let v = (std::f64::consts::TAU * TONE_HZ * i as f64 / RATE as f64).sin() as f32;
        [self.amp(i) * v, (i + 1) as f32]
    }

    /// A sound change as the engine makes it (`nori_engine` `RingTrack::freeze`/`cut`): from the first
    /// frame no pull took, blended from what was there (before any change made at that frame).
    fn change(&mut self, amp: f32) {
        let at = self.read;
        self.sounds.retain(|s| s.0 < at);
        self.sounds.push((at, amp, true));
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
    fn pulled(&self) -> Pulled {
        let tape = self.clone();
        Box::new(move || {
            let t = tape.lock();
            t.read.max(t.discard)
        })
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
}

const SPEAKER: Output = Output { period_ms: 20, delay_ms: 40, stamp_after_ms: 60, jitter_us: 50, packet_us: 0 };
/// A2DP on a Galaxy S22: a long way to the ear, no timestamp for a while after a start, then one per
/// packet with milliseconds of jitter, stall-corrected between packets.
const BLUETOOTH: Output = Output { period_ms: 20, delay_ms: 250, stamp_after_ms: 400, jitter_us: 3_000, packet_us: 23_220 };
const OUTPUTS: [Output; 5] = [SPEAKER, Output { period_ms: 5, delay_ms: 10, ..SPEAKER }, Output { period_ms: 40, delay_ms: 80, ..SPEAKER }, Output { delay_ms: 150, ..BLUETOOTH }, BLUETOOTH];

struct Rig {
    writer: Writer<Arc<Mutex<Tape>>>,
    tape: Arc<Mutex<Tape>>,
    air: Air,
    control: Control,
    clock: Arc<Clock>,
    now: Arc<AtomicI64>,
    next: Option<i64>,
    wakes: u32,
    /// Once checked: the worst difference, frames, between the engine's play head and what was presented.
    head_error: Option<u64>,
}

impl Rig {
    fn new(out: Output) -> Rig {
        let start = 1_000 * MS;
        let now = Arc::new(AtomicI64::new(start));
        let clock = Arc::new(Clock::default());
        let tape = Arc::new(Mutex::new(Tape { read: 0, discard: 0, written: frames_of(10_200 * MS), sounds: vec![(0, 0.5, false)], untold: false, flushed: false }));
        let frames = track_frames(RATE, false);
        let wire = Arc::new(Mutex::new(Wire::new(frames)));
        let sink = WireSink { wire: wire.clone(), staging: vec![0.0; CHUNK_BYTES / 4], now: now.clone(), stamp_after: out.stamp_after_ms * MS, jitter: out.jitter_us * 1_000, packet: out.packet_us * 1_000, last_packet: -1 };
        let mut opener = AirOpener(Some(sink));
        let format = OutputFormat { rate: RATE, channels: 2, bits: 0 };
        let opened = opener.open(format, true, frames).expect("opens");
        let reopen = Reopen { opener: Arc::new(Mutex::new(Box::new(opener))), frames, failure: Arc::new(Mutex::new(None)) };
        let writer = Writer::new(tape.clone(), opened, reopen, format, true, clock.clone(), Arc::new(AtomicU64::new(0)));
        let period = frames_of(out.period_ms * MS);
        let air = Air { period, delay: out.delay_ms * MS, next_mix: start + ns_of(period), origin: start, wire, heard: Vec::new() };
        Rig { writer, tape, air, control: Control::default(), clock, now, next: Some(start), wakes: 0, head_error: None }
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
    /// the frame presented now.
    fn check_head(&mut self, now: i64) {
        if !self.control.playing || self.head_error.is_none() {
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

    /// The app came in sight (`Engine::set_shallow`) or left it.
    fn in_sight(&mut self, on: bool) {
        self.control.shallow = on;
        self.wake();
    }

    /// Paused with the engine's fade, and resumed after `ms`.
    fn pause_for(&mut self, ms: i64) {
        self.control.ramp = Some((None, 0.0, 100));
        self.wake();
        self.run(100);
        // `TrackOutput::pause`.
        self.clock.freeze(self.now());
        self.control.playing = false;
        self.wake();
        self.run(ms);
        self.control.playing = true;
        self.control.ramp = Some((Some(0.0), 1.0, 100));
        self.wake();
    }

    /// Music in the track not yet presented, ms.
    fn held_ms(&self) -> i64 {
        ns_of(self.clock.latency_frames(self.now())) / MS
    }

    /// Times the track was emptied.
    fn flushes(&self) -> usize {
        self.air.wire.lock().traces.len() - 1
    }
}

/// What was heard, measured.
#[derive(Debug)]
struct Heard {
    /// Frames skipped or played twice, all told (by their numbers), silence left out.
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
    // Frame numbers heard at full level (whole, and the next one after them), less their place among the
    // frames not silent.
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

/// Plays 10 s deep, runs `script`, then 8 s more; measures from 1 s before the script.
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

/// Every frame heard once, in order, at its level, with no gap or click, the engine's play head where
/// the ear is, and the track never emptied.
fn assert_seamless(what: &str, out: Output, r: &Rig, h: &Heard) {
    assert_eq!(h.slip, 0, "{what}: every frame heard once, in order: {h:?}");
    assert_eq!(h.gap, 0, "{what}: no silence: {h:?}");
    assert!(h.level_db.0 > -0.5 && h.level_db.1 < 0.5, "{what}: no dip or bump: {h:?}");
    assert!(h.click < 1e-3, "{what}: no click: {h:?}");
    // As far off as the output's timestamps are (a packet, stall-corrected).
    let off = frames_of(MS / 10 + (out.jitter_us + out.packet_us) * 1_000);
    assert!(r.head_error.is_some_and(|e| e <= off), "{what}: the engine's play head is where the ear is: {:?} frames off", r.head_error);
    assert_eq!(r.flushes(), 0, "{what}: never emptied");
    let w = r.air.wire.lock();
    assert!(w.started && w.volume == 1.0, "{what}: playing");
}

/// Since the track was last emptied, the mixer took every frame once, in order (whatever its volume).
fn every_frame_once(r: &Rig) -> bool {
    r.air.wire.lock().traces.last().expect("a run").windows(2).all(|p| p[1].1 == p[0].1 + 1.0)
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
    let latency = r.clock.latency_frames(r.now());
    let t = r.tape.lock();
    t.amp(t.read - latency)
}

#[test]
fn background_changes_wait_for_the_deep_track() {
    // Out of sight, a change is made in the ring past what the track holds: seamless, heard once the
    // track has played what it held.
    for out in [SPEAKER, BLUETOOTH] {
        let mut at = 0;
        let (r, h) = heard_after(out, |r| {
            at = r.now();
            r.change(0.6);
            r.run(5_000);
        });
        let what = format!("{out:?}");
        assert_seamless(&what, out, &r, &h);
        let ms = heard_in_ms(&r, at, 0.5, 0.6);
        assert!(ms >= LOW_US / 1_000 && ms <= TRACK_US / 1_000 + out.delay_ms + out.period_ms, "{what}: heard after {ms} ms, once the track played what it held");
        assert_eq!(heard_amp(&r), 0.6, "{what}: the change is heard");
    }
}

#[test]
fn in_sight_changes_are_heard_soon() {
    // In sight and drained to shallow, a change at moments through the track's top-up cycle: in place,
    // seamless, heard within the output's own latency and a shallow track's 120 ms besides.
    for out in OUTPUTS {
        let mut worst = 0;
        for k in 0..8 {
            let mut at = 0;
            let (r, h) = heard_after(out, |r| {
                r.in_sight(true);
                r.run(12_000 + k * 17);
                at = r.now();
                r.change(0.6);
            });
            let what = format!("in sight on {out:?}, {k}");
            assert_seamless(&what, out, &r, &h);
            worst = worst.max(heard_in_ms(&r, at, 0.5, 0.6));
        }
        let late = worst - out.delay_ms - out.period_ms;
        assert!(late <= SHALLOW_TRACK_US / 1_000, "{out:?}: heard {late} ms after the output's own latency at worst");
    }

    // Changes one after another, faster than the track takes music (each made where the last was, in its
    // blend) and slower: every step heard, no click.
    for (out, step_ms) in [(SPEAKER, 2), (SPEAKER, 40), (BLUETOOTH, 3), (BLUETOOTH, 100)] {
        let what = format!("changed in {step_ms} ms steps on {out:?}");
        let (r, h) = heard_after(out, |r| {
            r.in_sight(true);
            r.run(12_000);
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
fn coming_in_sight_drains_the_track() {
    // The app comes in sight with seconds in the track: it takes no more until it has played down to
    // shallow, waking no more than for its bursts, and is never emptied. A change meanwhile is made in
    // the ring and heard where the track runs out of what it held; one after that, soon.
    for out in OUTPUTS {
        let what = format!("{out:?}");
        let (mut at, mut held, mut wakes, mut later) = (0, 0, 0, 0);
        let (r, h) = heard_after(out, |r| {
            while r.held_ms() < 8_000 {
                r.run(100);
            }
            r.in_sight(true);
            held = r.held_ms();
            assert!(held > 5_000, "{what}: seconds held");
            let woke = r.wakes;
            r.run(50);
            at = r.now();
            r.change(0.6);
            r.run(500);
            r.change(0.65);
            r.run(held - 900);
            wakes = r.wakes - woke;
            r.run(1_400);
            later = r.now();
            r.change(0.7);
        });
        assert_seamless(&what, out, &r, &h);
        let ms = heard_in_ms(&r, at, 0.5, 0.6);
        assert!(ms >= held - 300 && ms <= held + out.delay_ms + 100, "{what}: heard after {ms} ms, where the {held} ms held ran out");
        assert!(wakes <= 12, "{what}: {wakes} wakes while it drained");
        let ms = heard_in_ms(&r, later, 0.65, 0.7);
        assert!(ms <= out.delay_ms + out.period_ms + 120, "{what}: drained, heard after {ms} ms");
        assert_eq!(heard_amp(&r), 0.7, "{what}: the last change is heard");
    }
}

#[test]
fn leaving_sight_fills_the_track() {
    // Out of sight again, shallow or still draining: the track is written further at once, with no gap,
    // then topped up once a burst.
    for (out, in_sight_ms) in [(SPEAKER, 15_000), (BLUETOOTH, 15_000), (SPEAKER, 3_000), (BLUETOOTH, 3_000)] {
        let what = format!("{out:?} after {in_sight_ms} ms in sight");
        let mut wakes = 0;
        let (r, h) = heard_after(out, |r| {
            r.in_sight(true);
            r.run(in_sight_ms);
            r.in_sight(false);
            r.run(100);
            assert!(r.held_ms() > 9_000, "{what}: deep at once, {} ms held", r.held_ms());
            r.run(15_000);
            let woke = r.wakes;
            r.run(60_000);
            wakes = r.wakes - woke;
        });
        assert_seamless(&what, out, &r, &h);
        assert!(wakes <= 8, "{what}: a wake every ten seconds or so: {wakes}");
    }
}

#[test]
fn pause_and_jump_in_each_state() {
    // Deep, in sight and drained, draining, filling again after leaving sight: a pause (the engine
    // fading out first) plays on from where it was; a jump empties the track for the new music, played
    // from its first frame.
    type Into = fn(&mut Rig);
    let states: [(&str, Into); 4] = [
        ("deep", |_| {}),
        ("shallow", |r| {
            r.in_sight(true);
            r.run(12_000);
        }),
        ("draining", |r| {
            r.in_sight(true);
            r.run(1_000);
        }),
        ("filling", |r| {
            r.in_sight(true);
            r.run(12_000);
            r.in_sight(false);
            r.run(10);
        }),
    ];
    for out in [SPEAKER, BLUETOOTH] {
        for (state, into) in states {
            let what = format!("{state} on {out:?}");
            let (r, h) = heard_after(out, |r| {
                into(r);
                r.change(0.6);
                r.pause_for(2_000);
            });
            assert_eq!(r.flushes(), 0, "{what}: never emptied");
            assert!(every_frame_once(&r), "{what}: paused, on from the frame it paused at");
            assert!(h.click < 2e-3, "{what}: no click: {h:?}");

            let mut jumped = 0;
            let (r, _) = heard_after(out, |r| {
                into(r);
                let mut t = r.tape.lock();
                t.jump();
                jumped = t.read;
                drop(t);
                r.wake();
            });
            assert_eq!(r.flushes(), 1, "{what}: emptied once for the jump");
            let first = r.air.wire.lock().traces.last().expect("a run")[0].1 as u64 - 1;
            assert_eq!(first, jumped, "{what}: from its first frame");
            assert!(every_frame_once(&r), "{what}: and on, every frame once");
        }
    }
}
