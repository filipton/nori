//! Per-frame calls (lyrics clock, draw keys) must not allocate. The test binary's global allocator counts
//! allocations per thread (a global allocator must be a static).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use crate::lyrics::{Line, LyricClock, LyricTiming, Step, Word};

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

/// Allocations made by `f` on this thread.
fn allocations(f: impl FnOnce()) -> u64 {
    let before = ALLOCS.with(Cell::get);
    f();
    ALLOCS.with(Cell::get) - before
}

#[test]
fn lyric_clock_does_not_allocate() {
    // 80 lines of five timed words, 2.5 s apart.
    let lines = (0..80i64).map(|i| {
        let start = 5_000 + i * 2_500;
        let words = (0..5u32).map(|k| Word { start_ms: start + k as i64 * 400, end_ms: start + k as i64 * 400 + 350, start: k * 6, end: k * 6 + 5 }).collect();
        Line { start_ms: start, len: 29, words, ..Default::default() }
    });
    let clock = LyricClock::new(LyricTiming::new(true, true, lines), 0);
    let mut seen = 0i64;
    let n = allocations(|| {
        // Every other frame of the song with and without sweep, plus nudge, forced refresh and tap.
        for sweep in [true, false] {
            let mut t = 0;
            while t < 210_000 {
                seen ^= clock.advance(t, sweep, sweep, t % 50_000 == 0).pack();
                seen ^= clock.backing_sung().to_bits() as i64 ^ clock.shown_ms();
                seen ^= std::hint::black_box(Step::unpack(seen)).frame.active as i64;
                t += 33;
            }
            clock.nudge(1);
            seen ^= clock.tap(3) ^ clock.shown().pack();
            clock.nudge(0);
        }
    });
    std::hint::black_box(seen);
    assert_eq!(n, 0);
}

#[test]
fn draw_keys_do_not_allocate() {
    let url = "https://m.example/rest/getCoverArt.view?id=al-1&size=320";
    let mut key = String::with_capacity(256);
    let mut seen = 0i64;
    let n = allocations(|| {
        for i in 0..1_000 {
            let t = i as f32 / 1_000.0;
            seen ^= crate::sleeve::band_key(t, 1.0, 0.9 * t, 0.5);
            crate::cover::palette_key(&mut key, url, i % 2 == 0, i % 3 == 0);
        }
    });
    std::hint::black_box((seen, &key));
    assert_eq!(n, 0);
}
