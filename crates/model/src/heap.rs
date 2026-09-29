//! Live Rust heap bytes for the perf report, counted by [`Counting`] when a client installs it as the
//! global allocator. One relaxed atomic op per alloc/free.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

/// The system allocator plus a live-bytes count.
pub struct Counting;

// Global: a global allocator has no instance state to hold them.

static LIVE: AtomicIsize = AtomicIsize::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards to `System`; only adds a counter.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            LIVE.fetch_add(l.size() as isize, Ordering::Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            LIVE.fetch_add(l.size() as isize, Ordering::Relaxed);
        }
        p
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, size) };
        if !q.is_null() {
            LIVE.fetch_add(size as isize - l.size() as isize, Ordering::Relaxed);
        }
        q
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
    }
}

impl Counting {
    /// Marks the counter as active; called once by the client that installed it.
    pub fn installed() {
        COUNTING.store(true, Ordering::Relaxed);
    }
}

/// Requested bytes currently allocated; None unless [`Counting`] is installed.
pub fn live_bytes() -> Option<i64> {
    COUNTING.load(Ordering::Relaxed).then(|| LIVE.load(Ordering::Relaxed).max(0) as i64)
}
