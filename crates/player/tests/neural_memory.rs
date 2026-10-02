//! What "Better beat detection" holds on the heap while it measures a song, as nori-engine's measurer runs it: the
//! song decoded piece by piece into the classical analyser and the ends kept for the model, the model loaded, and
//! its two windows run. Counted by a global allocator of this binary's own (the live bytes and their peak), so the
//! figures are the Rust heap's exactly, not the process's RSS.
//!
//! Needs the model: `NORI_BEAT_THIS=<beat-this-small0-v1.onnx> cargo test -p nori-player --features neural-beats
//! --test neural_memory -- --nocapture` (the export with its weights in it, as the app's build makes it for the
//! assets: core/build/generated/beatModel). Without it, or without the feature, the test says so and passes.

#![cfg(feature = "neural-beats")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use nori_player::automix::analysis::Analyzer;
use nori_player::automix::beats::{self, Ends, MixEnd};
use nori_player::automix::neural::BeatThis;

struct Peak;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

fn grew(by: isize) {
    let now = LIVE.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every call is the system allocator's own; only a count is kept beside it.
unsafe impl GlobalAlloc for Peak {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grew(l.size() as isize);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            grew(l.size() as isize);
        }
        p
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, size) };
        if !q.is_null() {
            grew(size as isize - l.size() as isize);
        }
        q
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
    }
}

#[global_allocator]
static A: Peak = Peak;

/// The process's resident memory now, MB (what a PSS reading follows), from /proc.
fn rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix("VmRSS:")).and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<f64>().ok()).unwrap_or(0.0) / 1024.0
}

/// Freed memory back to the system, as nori-engine's measurer does once the model has read a song
/// (`arriving::give_memory_back`).
fn give_back() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only returns free memory to the system.
    unsafe {
        libc::malloc_trim(0);
    }
}

fn mb(b: isize) -> f64 {
    b as f64 / 1_048_576.0
}

/// The peak starts again from what is live now; answers what is live.
fn mark() -> isize {
    let now = LIVE.load(Ordering::Relaxed);
    PEAK.store(now, Ordering::Relaxed);
    now
}

fn peak() -> isize {
    PEAK.load(Ordering::Relaxed)
}

/// A song of `secs` at 44.1 kHz stereo with a beat in it (a click every half second over a quiet tone), made a
/// piece at a time as a decoder hands it over: never whole in memory.
fn pieces(secs: usize, mut each: impl FnMut(&[f32])) {
    let rate = 44_100usize;
    let mut buf = vec![0f32; 4096 * 2];
    let total = secs * rate;
    let mut at = 0;
    while at < total {
        let n = 4096.min(total - at);
        for i in 0..n {
            let t = at + i;
            let click = if t % (rate / 2) < 400 { 0.6 * (1.0 - (t % (rate / 2)) as f32 / 400.0) } else { 0.0 };
            let v = click + 0.05 * (t as f32 * 2.0 * std::f32::consts::PI * 220.0 / rate as f32).sin();
            buf[2 * i] = v;
            buf[2 * i + 1] = v;
        }
        each(&buf[..2 * n]);
        at += n;
    }
}

/// One song measured with the model, as the measurer does it: the peak of each step over what was live before it.
fn measure(model: &BeatThis, secs: usize) -> [isize; 3] {
    let base = mark();
    let mut a = Analyzer::new(44_100, secs as u64 * 1000);
    let mut ends = Ends::new(44_100);
    pieces(secs, |x| {
        a.feed_interleaved(x, 2, |v| v);
        ends.feed(x, 2);
    });
    let decoded = peak() - base;
    drop(a);
    let before = mark();
    let rate = ends.rate();
    let mut windows = 0;
    for end in [MixEnd::Intro, MixEnd::Outro] {
        let (x, start_ms) = match end {
            MixEnd::Intro => (ends.head().to_vec(), 0),
            MixEnd::Outro => {
                let (x, s) = ends.tail();
                (x.to_vec(), s)
            }
        };
        let n = (30 * rate as usize).min(x.len());
        let piece = if end == MixEnd::Intro { &x[..n] } else { &x[x.len() - n..] };
        let g = beats::read(model, piece, rate, end, start_ms).expect("the model runs");
        assert!(g.is_some_and(|g| (g.bpm - 120.0).abs() < 2.0), "a click every half second is 120 bpm: {g:?}");
        windows += 1;
    }
    assert_eq!(windows, 2);
    let run = peak() - before;
    drop(ends);
    let left = LIVE.load(Ordering::Relaxed) - base;
    [decoded, run, left]
}

#[test]
fn beat_model_heap_is_bounded() {
    let Ok(path) = std::env::var("NORI_BEAT_THIS") else {
        eprintln!("no model in NORI_BEAT_THIS: skipped");
        return;
    };
    let bytes = std::fs::read(&path).expect("the model file");
    eprintln!("resident {:.1} MB before the model", rss_mb());
    let base = mark();
    let model = BeatThis::from_bytes(&bytes).expect("the model loads");
    drop(bytes);
    let loaded = LIVE.load(Ordering::Relaxed) - base;
    let load_peak = peak() - base;
    eprintln!("model: {:.1} MB held once loaded, {:.1} MB at the peak of loading", mb(loaded), mb(load_peak));
    let mut runs = Vec::new();
    for secs in [360, 900] {
        let [decoded, run, left] = measure(&model, secs);
        eprintln!(
            "{} min song: decode into the analyser and the ends {:.1} MB, the two windows {:.1} MB over them, {:.1} MB left after",
            secs / 60,
            mb(decoded),
            mb(run),
            mb(left)
        );
        runs.push(run);
        let held = rss_mb();
        give_back();
        eprintln!("  resident {held:.1} MB after the song, {:.1} MB once freed memory is handed back", rss_mb());
    }
    drop(model);
    let after = LIVE.load(Ordering::Relaxed) - base;
    eprintln!("after the model is dropped: {:.1} MB", mb(after));
    // The model's run does not grow with the song: both windows are 30 s whatever its length.
    assert!((runs[1] - runs[0]).abs() < 4 << 20, "a longer song costs the model no more: {runs:?}");
}
