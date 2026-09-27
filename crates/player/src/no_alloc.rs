//! Steady-state playback must not allocate: an allocation per buffer is a lock, a search and cache
//! misses tens of times a second for as long as music plays, on the thread that feeds the output. The
//! test binary counts every allocation made on the calling thread, and each per-buffer path is run
//! past its warm-up (the first buffers may size the reused buffers) and then required to make none.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use crate::automix::analysis::Analyzer;
use crate::automix::{mixer, plan};
use crate::dsp::{Band, Equalizer};
use crate::engine::{Downstream, Heard, Host, Plan, StreamFormat, TransitionEngine};
use crate::heard::{HeardTracker, PlayerNow};
use crate::pcm::{Encoding, Format};
use crate::silence::SilenceSkipper;
use crate::speed::SpeedPitch;
use crate::types::AutoMixSettings;

struct Counting;

thread_local! {
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

/// An output that takes everything and keeps nothing, like an AudioTrack with room.
struct Sink {
    bytes: usize,
}

impl Downstream for Sink {
    type Config = u32;
    fn configure(&mut self, _: &u32, _: Option<Format>) {}
    fn handle_buffer(&mut self, data: &[u8], from: usize, _: i64) -> (bool, usize) {
        self.bytes += data.len() - from;
        (true, data.len() - from)
    }
    fn handle_discontinuity(&mut self) {}
    /// A playhead two seconds behind what was written, as a deep track has.
    fn position_us(&mut self, _: bool) -> i64 {
        (FMT.us(self.bytes) - 2_000_000).max(0)
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
    StreamFormat { id: Some(id.into()), format: Some(f) }
}

/// Feeds `data` from `from_us` in decoder-sized buffers; the allocations made after the first `warm` buffers.
fn feed(e: &mut TransitionEngine<u32>, d: &mut Sink, h: &mut App, data: &[u8], from_us: i64, warm: usize) -> u64 {
    allocating(e, d, h, data, from_us, warm).iter().map(|&(_, a)| a).sum()
}

/// The buffers after the first `warm` that allocated, and how many times each did.
fn allocating(e: &mut TransitionEngine<u32>, d: &mut Sink, h: &mut App, data: &[u8], from_us: i64, warm: usize) -> Vec<(usize, u64)> {
    let mut found = Vec::new();
    for (i, c) in data.chunks(CHUNK).enumerate() {
        let pts = from_us + FMT.us(i * CHUNK);
        let a = allocations(|| {
            e.handle_buffer(d, h, c, pts);
            e.position_us(d, h, false);
            e.has_pending_data();
        });
        if i >= warm && a > 0 {
            found.push((i, a));
        }
    }
    found
}

#[test]
fn passing_straight_through_allocates_nothing() {
    // At a song's ReplayGain the buffer is scaled in a copy from the pool, which is sized once.
    for (analyse, gain) in [(false, 1.0), (true, 1.0), (false, 0.5), (true, 0.5)] {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Sink { bytes: 0 }, App { plan: None, analyse });
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        e.set_gain(gain);
        assert_eq!(feed(&mut e, &mut d, &mut h, &tone(20.0, 440.0), 0, 8), 0, "analysing: {analyse}, at {gain}");
    }
}

#[test]
fn converting_another_rate_allocates_nothing() {
    let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Sink { bytes: 0 }, App { plan: None, analyse: false });
    e.configure(&mut d, &mut h, stream("a", FMT), 1);
    feed(&mut e, &mut d, &mut h, &tone(0.5, 440.0), 0, 0);
    e.configure(&mut d, &mut h, stream("b", Format { rate: 48_000, ..FMT }), 2);
    e.handle_discontinuity(&mut d, &mut h);
    let b: Vec<u8> = tone(10.0, 300.0);
    let mut n = 0;
    for (i, c) in b.chunks(CHUNK).enumerate() {
        let a = allocations(|| {
            e.handle_buffer(&mut d, &mut h, c, 1_000_000 + i as i64 * 20_000);
        });
        if i >= 8 {
            n += a;
        }
    }
    assert_eq!(n, 0);
}

#[test]
fn holding_and_mixing_allocate_nothing_per_buffer() {
    holding_and_mixing(1.0, 1.0);
}

#[test]
fn holding_and_mixing_songs_at_their_own_volumes_allocate_nothing_per_buffer() {
    holding_and_mixing(0.5, 0.8);
}

/// A crossfade from a song at volume `a` into one at `b`.
fn holding_and_mixing(a: f32, b: f32) {
    let s = AutoMixSettings { max_transition_s: 6.0, ..Default::default() };
    let t = plan::plan(None, None, 60_000, 60_000, &s);
    let p = Plan {
        incoming_id: "b".into(),
        out_start_us: 4_000_000,
        duration_us: t.duration_ms * 1000,
        in_skip_us: 0,
        mixer: mixer::params(&t),
        tempo_ratio: 1.0,
        keep_pitch: true,
        ramp_us: 0,
        out_loop_us: 0,
    };
    let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Sink { bytes: 0 }, App { plan: Some(p), analyse: false });
    e.configure(&mut d, &mut h, stream("a", FMT), 1);
    e.set_gain(a);
    // Up to the hold, then the held ending (the first buffers into the hold size its storage).
    feed(&mut e, &mut d, &mut h, &tone(4.0, 440.0), 0, 0);
    let held = feed(&mut e, &mut d, &mut h, &tone(6.0, 440.0), 4_000_000, 16);
    e.configure(&mut d, &mut h, stream("b", FMT), 2);
    e.handle_discontinuity(&mut d, &mut h);
    e.set_gain(b);
    let mixed = allocating(&mut e, &mut d, &mut h, &tone(12.0, 330.0), 10_000_000, 16);
    assert_eq!(held, 0, "holding");
    // The one buffer the mix ends in hands the rest of itself on in a chunk of a new size and closes
    // the mix: a few allocations once per transition. Every other buffer makes none.
    let end = (t.duration_ms as usize * RATE as usize / 1000 * FMT.frame_bytes()) / CHUNK;
    assert!(mixed.iter().all(|&(i, a)| i.abs_diff(end) <= 1 && a <= 8), "allocating buffers {mixed:?}, the mix ends in {end}");
}

#[test]
fn the_sound_chain_allocates_nothing() {
    let mut eq = Equalizer::new(RATE, 2);
    let bands = [Band { kind: 0, freq: 1000.0, gain_db: 4.0, q: 1.0, channel: 0 }, Band { kind: 1, freq: 90.0, gain_db: 3.0, q: 0.7, channel: 0 }];
    eq.configure(&bands, -3.0, -6.0);
    eq.configure_output(0.1, false, -1.0, 120.0, 5.0);
    let x: Vec<i16> = tone(2.0, 440.0).chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    let mut y = vec![0i16; CHUNK / 2];
    eq.process_i16(&x[..CHUNK / 2], &mut y);
    let n = allocations(|| {
        for c in x.chunks_exact(CHUNK / 2) {
            eq.process_i16(c, &mut y);
        }
    });
    assert_eq!(n, 0);
}

#[test]
fn speed_and_silence_allocate_nothing_once_warm() {
    let x = tone(20.0, 440.0);
    let mut sp = SpeedPitch::new(RATE, 2, Encoding::Pcm16);
    sp.set(1.25, 1.1);
    sp.flush();
    let mut out = Vec::with_capacity(1 << 16);
    let mut run = |f: &mut dyn FnMut(&[u8], &mut Vec<u8>)| {
        let mut n = 0;
        for (i, c) in x.chunks(CHUNK).enumerate() {
            let a = allocations(|| {
                f(c, &mut out);
                out.clear();
            });
            if i >= 16 {
                n += a;
            }
        }
        n
    };
    assert_eq!(run(&mut |c, o| sp.process(c, o)), 0, "speed and pitch");
    let mut si = SilenceSkipper::new(RATE, 2);
    let quiet: Vec<u8> = x.iter().enumerate().map(|(i, &b)| if (i / 40_000) % 3 == 0 { 0 } else { b }).collect();
    let mut n = 0;
    for (i, c) in quiet.chunks(CHUNK).enumerate() {
        let a = allocations(|| {
            si.process(c, &mut out);
            out.clear();
        });
        if i >= 16 {
            n += a;
        }
    }
    assert_eq!(n, 0, "silence skipping");
}

#[test]
fn analysing_allocates_nothing_per_buffer() {
    let x: Vec<f32> = tone(30.0, 440.0).chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect();
    // Both channels alike (the mono path), and panned (the side read for the vocal curve).
    let panned: Vec<f32> = x.iter().enumerate().map(|(i, v)| if i % 2 == 0 { *v } else { *v * 0.5 }).collect();
    for x in [&x, &panned] {
        let mut a = Analyzer::new(RATE, 30_000);
        let mut n = 0;
        for (i, c) in x.chunks(CHUNK / 2).enumerate() {
            let k = allocations(|| a.feed_interleaved(c, 2, |v| v));
            if i >= 16 {
                n += k;
            }
        }
        assert_eq!(n, 0);
    }
}

#[test]
fn asking_what_is_heard_allocates_nothing() {
    let mut t = HeardTracker::new();
    t.set_queue([("a".to_string(), 200_000), ("b".to_string(), 180_000)]);
    let h = Heard {
        id: Some("a".into()),
        us: 190_000_000,
        at_ms: 0,
        until_us: 194_000_000,
        mixing: false,
        next_id: Some("b".into()),
        next_from_us: 5_000_000,
        next_rate: 1.0,
        from_id: Some("a".into()),
        audible_us: 194_000_000,
    };
    t.at(&h, PlayerNow { now_ms: 0, playing: true, on: Some("b"), position_ms: 0 });
    // Eight seconds of frames, through the moment the mix becomes audible (4 s in) and the ear moves
    // from a to b: the one copy of an id is that change, nothing per frame.
    let n = allocations(|| {
        for ms in (16..8_000).step_by(16) {
            t.at(&h, PlayerNow { now_ms: ms, playing: true, on: Some("b"), position_ms: ms });
        }
    });
    assert_eq!(n, 1);
}

/// The seek bar's place (held while the page is a song behind, run on while reconnecting) and a volume
/// fade's tick are asked every frame and every 16 ms: neither may allocate.
#[test]
fn the_playhead_and_a_fade_tick_allocate_nothing() {
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
fn decoding_a_packet_allocates_nothing_after_the_first() {
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
    assert_eq!(n, 0, "allocations while decoding");
}

#[test]
fn decoding_opus_allocates_nothing_after_the_first_packets() {
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
    assert_eq!(n, 0, "allocations while decoding Opus");
}
