//! A per-wake snapshot of the engine for a client's watchdog (the Android perf build's invariants),
//! taken after the report, with no timer or thread of its own. Built only when a [`Watch`] is set
//! (`Config::watch`) and wants it.

use std::sync::Arc;

/// The engine at the end of one wake.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Seen {
    /// Engine clock, ms (only differences mean anything).
    pub now_ms: i64,
    /// Music should be moving: playing, no dip, not starved for bytes.
    pub playing: bool,
    /// Offloaded.
    pub offloaded: bool,
    /// The audible song (queue index, id) and position, ms.
    pub index: Option<usize>,
    pub id: Option<String>,
    pub position_ms: i64,
    /// Music written to the output and not yet played, ms.
    pub in_output_ms: i64,
    /// How long the position stood still while music should move, ms (0 while it moves or bytes are coming).
    pub quiet_ms: i64,
    /// The awaited song's bytes are still being fetched.
    pub bytes_coming: bool,
    /// An output (CPU device or offload track) is open.
    pub output_open: bool,
    /// The engine's state in words, for a stall report.
    pub state: String,
}

/// A client's watcher (`Config::watch`).
pub trait Watch: Send + Sync {
    /// Whether to report now; asked every wake, so it must be cheap.
    fn wanted(&self) -> bool;

    fn seen(&self, seen: &Seen);
}

/// A [`Watch`] as `Config` holds it.
#[derive(Clone)]
pub struct Watcher(pub Arc<dyn Watch>);

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Watcher")
    }
}

/// Reports `make()` to `watch` if set and it wants it.
#[inline]
pub(crate) fn look(watch: Option<&Watcher>, make: impl FnOnce() -> Seen) {
    if let Some(w) = watch {
        if w.0.wanted() {
            w.0.seen(&make());
        }
    }
}
