//! Steady-state playback must not allocate on the audio thread. The test binary counts allocations
//! per thread; each per-buffer path runs past a warm-up and must then make none.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use crate::automix::analysis::Analyzer;
use crate::automix::plan;
use crate::dsp::{Band, Equalizer};
use crate::engine::{Downstream, Heard, Host, Plan, StreamFormat, TransitionEngine};
use crate::heard::{HeardTracker, PlayerNow};
use crate::pcm::{Encoding, Format};
use crate::silence::SilenceSkipper;
use crate::speed::SpeedPitch;
use crate::types::AutoMixSettings;

struct Counting;

thread_local! {
    // Global by necessity: the allocator has no other place to count.
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations `f` made on this thread.
fn allocations(f: impl FnOnce()) -> u64 {
    let before = ALLOCS.with(Cell::get);
    f();
    ALLOCS.with(Cell::get) - before
}

/// Allocations of `f` over `chunk`-sized pieces of `data`, after the first 16.
fn steady<T>(data: &[T], chunk: usize, mut f: impl FnMut(&[T])) -> u64 {
    data.chunks(chunk).enumerate().map(|(i, c)| allocations(|| f(c)) * (i >= 16) as u64).sum()
}

const RATE: u32 = 44_100;
const FMT: Format = Format { rate: RATE, channels: 2, encoding: Encoding::Pcm16 };
const CHUNK: usize = 4608;

fn tone(secs: f64, hz: f64) -> Vec<u8> {
    (0..(RATE as f64 * secs) as usize)
        .flat_map(|i| {
            let v = ((i as f64 / RATE as f64 * hz * std::f64::consts::TAU).sin() * 9000.0) as i16;
            [v, v]
        })
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

/// Takes everything, with a playhead 2 s behind what was written.
struct Sink {
    bytes: usize,
}

impl Downstream for Sink {
    fn configure(&mut self, _: Format) {}
    fn handle_buffer(&mut self, data: &[u8], from: usize, _: i64) -> (bool, usize) {
        self.bytes += data.len() - from;
        (true, data.len() - from)
    }
    fn handle_discontinuity(&mut self) {}
    fn position_us(&mut self, _: bool) -> Option<i64> {
        Some((FMT.us(self.bytes) - 2_000_000).max(0))
    }
}

struct App {
    plan: Option<Plan>,
    analyse: bool,
}

impl Host for App {
    fn plan_for(&mut self, _: &str) -> Option<Plan> {
        self.plan.clone()
    }
    fn wants_analysis(&mut self, _: &str) -> Option<u64> {
        self.analyse.then_some(0)
    }
    fn analysed(&mut self, _: &str, _: Analyzer, _: usize, _: u64, _: u32) {}
    fn now_ms(&self) -> i64 {
        0
    }
}

fn stream(id: &str, f: Format) -> StreamFormat {
    StreamFormat { id: crate::engine::StreamId { song: id.into(), serial: id.as_bytes()[0] as u64 }, format: f }
}

/// Feeds `data` from `from_us`; total allocations after the first `warm` buffers.
fn feed(e: &mut TransitionEngine, d: &mut Sink, h: &mut App, data: &[u8], from_us: i64, warm: usize) -> u64 {
    allocating(e, d, h, data, from_us, warm).iter().map(|&(_, a)| a).sum()
}

/// (buffer index, allocations) of the buffers after the first `warm` that allocated.
fn allocating(e: &mut TransitionEngine, d: &mut Sink, h: &mut App, data: &[u8], from_us: i64, warm: usize) -> Vec<(usize, u64)> {
    let mut found = Vec::new();
    for (i, c) in data.chunks(CHUNK).enumerate() {
        let pts = from_us + FMT.us(i * CHUNK);
        let a = allocations(|| {
            e.handle_buffer(d, h, c, pts);
            e.position_us(d, h, false);
        });
        if i >= warm && a > 0 {
            found.push((i, a));
        }
    }
    found
}

#[test]
fn pass_through() {
    // With a gain the buffer is scaled in a pooled copy.
    for (analyse, gain) in [(false, 1.0), (true, 1.0), (false, 0.5), (true, 0.5)] {
        let (mut e, mut d, mut h) = (TransitionEngine::new(), Sink { bytes: 0 }, App { plan: None, analyse });
        e.configure(&mut d, &mut h, stream("a", FMT));
        e.set_gain(gain);
        assert_eq!(feed(&mut e, &mut d, &mut h, &tone(20.0, 440.0), 0, 8), 0, "analysing: {analyse}, at {gain}");
    }
}

#[test]
fn rate_conversion() {
    let (mut e, mut d, mut h) = (TransitionEngine::new(), Sink { bytes: 0 }, App { plan: None, analyse: false });
    e.configure(&mut d, &mut h, stream("a", FMT));
    feed(&mut e, &mut d, &mut h, &tone(0.5, 440.0), 0, 0);
    e.configure(&mut d, &mut h, stream("b", Format { rate: 48_000, ..FMT }));
    e.handle_discontinuity(&mut d, &mut h);
    assert_eq!(feed(&mut e, &mut d, &mut h, &tone(10.0, 300.0), 1_000_000, 8), 0);
}

#[test]
fn holding_and_mixing() {
    // Equal gains, and each song at its own gain.
    for (a, b) in [(1.0, 1.0), (0.5, 0.8)] {
        let s = AutoMixSettings { max_transition_s: 6.0, ..Default::default() };
        let t = plan::plan(None, None, 60_000, 60_000, &s);
        let p = Plan {
            incoming_id: "b".into(),
            out_start_us: 4_000_000,
            duration_us: t.duration_ms * 1000,
            in_skip_us: 0,
            mixer: t.clone(),
            tempo_ratio: 1.0,
            keep_pitch: true,
            ramp_us: 0,
            out_loop_us: 0,
        };
        let (mut e, mut d, mut h) = (TransitionEngine::new(), Sink { bytes: 0 }, App { plan: Some(p), analyse: false });
        e.configure(&mut d, &mut h, stream("a", FMT));
        e.set_gain(a);
        feed(&mut e, &mut d, &mut h, &tone(4.0, 440.0), 0, 0);
        let held = feed(&mut e, &mut d, &mut h, &tone(6.0, 440.0), 4_000_000, 16);
        e.configure(&mut d, &mut h, stream("b", FMT));
        e.handle_discontinuity(&mut d, &mut h);
        e.set_gain(b);
        let mixed = allocating(&mut e, &mut d, &mut h, &tone(12.0, 330.0), 10_000_000, 16);
        assert_eq!(held, 0, "holding");
        // The buffer the mix ends in may allocate a few times, once per transition.
        let end = (t.duration_ms as usize * RATE as usize / 1000 * FMT.frame_bytes()) / CHUNK;
        assert!(mixed.iter().all(|&(i, a)| i.abs_diff(end) <= 1 && a <= 8), "allocating buffers {mixed:?}, the mix ends in {end}");
    }
}

#[test]
fn parametric_equalizer() {
    let mut eq = Equalizer::new(RATE, 2);
    let bands = [Band { kind: 0, freq: 1000.0, gain_db: 4.0, q: 1.0, channel: 0 }, Band { kind: 1, freq: 90.0, gain_db: 3.0, q: 0.7, channel: 0 }];
    eq.configure(&bands, -3.0, -6.0);
    eq.configure_output(0.1, false, -1.0, 120.0, 5.0);
    let x: Vec<i16> = tone(2.0, 440.0).chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    let mut y = vec![0i16; CHUNK / 2];
    assert_eq!(steady(&x, CHUNK / 2, |c| eq.process_i16(c, &mut y[..c.len()])), 0);
}

#[test]
fn graphic_equalizer_and_effects() {
    let mut eq = Equalizer::new(RATE, 2);
    eq.configure_graphic(&[3.0, 5.0, 2.0, 0.0, -2.0, -4.0, 0.0, 2.0, 4.0, 6.0, 3.0, 1.0, 0.0, -1.0, 2.0], -6.0, 3.0);
    let fx = crate::dsp::Effects {
        bass_boost_db: 6.0,
        compressor: Some(crate::compressor::CompressorPreset::Strong.settings()),
        expander: Some(crate::compressor::ExpanderSettings { threshold_db: -30.0, ratio: 4.0, ..Default::default() }),
        loudness: Some(crate::contour::Loudness { reference_phon: 80.0, volume_db: -25.0 }),
        virtualizer: 0.7,
        boost_db: 4.0,
    };
    eq.configure_effects(&fx);
    eq.configure_output(0.0, true, -1.0, 120.0, 5.0);
    let x: Vec<i16> = tone(4.0, 440.0).chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    let mut y = vec![0i16; CHUNK / 2];
    assert_eq!(steady(&x, CHUNK / 2, |c| eq.process_i16(c, &mut y[..c.len()])), 0, "16-bit, dithered");
    let xf: Vec<f32> = x.iter().map(|v| *v as f32 / 32768.0).collect();
    let mut yf = vec![0f32; CHUNK / 2];
    assert_eq!(steady(&xf, CHUNK / 2, |c| eq.process_f32(c, &mut yf[..c.len()])), 0, "float");
}

#[test]
fn speed_and_silence_skipping() {
    let x = tone(20.0, 440.0);
    let mut out = Vec::with_capacity(1 << 17);
    let mut sp = SpeedPitch::new(RATE, 2, Encoding::Pcm16);
    sp.set(1.25, 1.1);
    sp.flush();
    let mut run = |f: &mut dyn FnMut(&[u8], &mut Vec<u8>), data: &[u8], chunk: usize| {
        steady(data, chunk, |c| {
            f(c, &mut out);
            out.clear();
        })
    };
    assert_eq!(run(&mut |c, o| sp.process(c, o), &x, CHUNK), 0, "speed and pitch");
    let quiet: Vec<u8> = x.iter().enumerate().map(|(i, &b)| if (i / 40_000) % 3 == 0 { 0 } else { b }).collect();
    let mut si = SilenceSkipper::new(RATE, 2, false);
    assert_eq!(run(&mut |c, o| si.process(c, o), &quiet, CHUNK), 0, "silence skipping");
    let quiet_f: Vec<u8> = quiet.chunks_exact(2).flat_map(|c| (i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).to_le_bytes()).collect();
    let mut si = SilenceSkipper::new(RATE, 2, true);
    assert_eq!(run(&mut |c, o| si.process(c, o), &quiet_f, CHUNK * 2), 0, "float silence skipping");
}

#[test]
fn sing_masker() {
    use crate::sing::{bands, Masker, Placed, VocalMask};
    let x = tone(8.0, 440.0);
    // Vocals in every other frame, so the masked and the passed-through paths both run.
    let row = |k: usize| vec![if k.is_multiple_of(2) { 200u8 } else { 0 }; bands()];
    let mask = std::sync::Arc::new(VocalMask::new(43.0, (0..400).flat_map(row).collect()));
    let masks = [Placed { at: 0..i64::MAX, mask }];
    let mut m = Masker::new(RATE, 2, Encoding::Pcm16, 0.2);
    let mut out = Vec::with_capacity(1 << 16);
    let mut pts = 0;
    let made = steady(&x, CHUNK, |c| {
        m.process(c, pts, 1.0, &masks, &mut out);
        pts += FMT.us(c.len());
        out.clear();
    });
    assert_eq!(made, 0);
    // A mark's copy reuses the buffers.
    let mut kept = m.clone();
    assert_eq!(allocations(|| kept.clone_from(&m)), 0);
}

#[test]
fn analysis() {
    let x: Vec<f32> = tone(30.0, 440.0).chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect();
    // Mono-alike and panned (the side feeds the vocal curve).
    let panned: Vec<f32> = x.iter().enumerate().map(|(i, v)| if i % 2 == 0 { *v } else { *v * 0.5 }).collect();
    for x in [&x, &panned] {
        let mut a = Analyzer::new(RATE, 30_000);
        assert_eq!(steady(x, CHUNK / 2, |c| a.feed_interleaved(c, 2, |v| v)), 0);
    }
}

#[test]
fn heard_tracker() {
    let mut t = HeardTracker::new();
    let find = |s: crate::heard::StreamAt| Some(if s == crate::heard::StreamAt::Serial(1) { (0, 200_000) } else { (1, 180_000) });
    let h = Heard { id: Some(1), us: 190_000_000, at_ms: 0, until_us: 194_000_000, mixing: false, next_from_us: 5_000_000, next_rate: 1.0, from: Some(1), audible_us: 194_000_000 };
    t.at(&h, PlayerNow { now_ms: 0, playing: true, on: Some(2), position_ms: 0 }, &find);
    // 8 s of frames across the takeover (4 s in).
    let n = allocations(|| {
        for ms in (16..8_000).step_by(16) {
            t.at(&h, PlayerNow { now_ms: ms, playing: true, on: Some(2), position_ms: ms }, &find);
        }
    });
    assert_eq!(n, 0);
}

/// Asked every frame / every 16 ms.
#[test]
fn playhead_and_fade_tick() {
    use crate::heard::{Playhead, Seen};
    let mut t = HeardTracker::new();
    t.set_queue([("a".to_string(), 200_000), ("b".to_string(), 180_000)]);
    let mut p = Playhead::new();
    let mut sum = 0i64;
    let mut vol = 0f32;
    let n = allocations(|| {
        for ms in (16..8_000).step_by(16) {
            let index = if ms > 4_000 { Some(1) } else { Some(0) };
            sum += p.show(&t, Seen { index, ms, changed: false }, Some(0), ms);
            sum += p.run_on(ms, true);
            vol += crate::transport::fade_step(1.0, 0.0, 16, ms, 600).0;
        }
    });
    assert_eq!(n, 0);
    assert!(sum > 0 && vol > 0.0);
}

#[test]
fn mp3_decoding() {
    use crate::decode::{Codec, Decoder};
    let file = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/tone440.mp3")).unwrap();
    let frames = crate::sim::mp3_frames(&file);
    let mut d = Decoder::new(Codec::Mp3, 44_100, 2, None, true).unwrap();
    let mut out = vec![0i16; 1152 * 2];
    let mut outf = vec![0f32; 1152 * 2];
    for f in &frames[..4] {
        d.decode_i16(f, &mut out).unwrap();
    }
    let n = allocations(|| {
        for f in &frames[4..] {
            d.decode_i16(f, &mut out).unwrap();
        }
        for f in &frames[4..] {
            d.decode_f32(f, &mut outf).unwrap();
        }
    });
    assert_eq!(n, 0);
}

#[test]
fn opus_decoding() {
    use crate::decode::{Codec, Decoder};
    let file = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/tone440.opus")).unwrap();
    let (setup, packets) = crate::sim::ogg_opus(&file);
    let mut d = Decoder::new(Codec::Opus, 48_000, 2, Some(&setup), false).unwrap();
    let mut out = vec![0i16; 5760 * 2];
    for p in &packets[..4] {
        d.decode_i16(p, &mut out).unwrap();
    }
    let n = allocations(|| {
        for p in &packets[4..] {
            d.decode_i16(p, &mut out).unwrap();
        }
    });
    assert_eq!(n, 0);
}
