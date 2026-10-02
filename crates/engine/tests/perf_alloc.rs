//! The test binary's allocator: the system's, counting allocations and bytes, and the heap's peak, for
//! the perf report ([`counts`], [`peak`]). Process-wide by nature (a global allocator).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

struct Counted;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
/// Bytes held now and at most since [`restart_peak`], by threads not excluded.
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

thread_local! {
    static EXCLUDED: Cell<bool> = const { Cell::new(false) };
}

/// Counts `delta` bytes held, unless this thread is excluded.
fn hold(delta: i64) {
    if EXCLUDED.try_with(Cell::get).unwrap_or(true) {
        return;
    }
    let live = LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// SAFETY: every call is the system allocator's own; only counts are kept beside it.
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
        hold(l.size() as i64);
        unsafe { System.alloc(l) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
        hold(l.size() as i64);
        unsafe { System.alloc_zeroed(l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size as u64, Ordering::Relaxed);
        hold(size as i64 - l.size() as i64);
        unsafe { System.realloc(p, l, size) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        hold(-(l.size() as i64));
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static COUNTED: Counted = Counted;

/// Allocations so far, and their bytes.
pub fn counts() -> (u64, u64) {
    (ALLOCATIONS.load(Ordering::Relaxed), BYTES.load(Ordering::Relaxed))
}

/// This thread's allocations stay out of [`peak`] (a test's own buffers).
pub fn exclude_this_thread() {
    EXCLUDED.with(|e| e.set(true));
}

/// The peak counts from what is held now.
pub fn restart_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// Most bytes held at once since [`restart_peak`], by threads not excluded.
pub fn peak() -> i64 {
    PEAK.load(Ordering::Relaxed)
}
