//! The engine's virtual-clock tests as one binary, so they run side by side. Each file keeps its own
//! helpers; `cargo test -p nori-engine --test engine <name>` picks any test.

mod common;

#[path = "perf_alloc.rs"]
mod perf_alloc;
#[path = "engine.rs"]
mod engine;
#[path = "paths.rs"]
mod paths;
#[path = "radio.rs"]
mod radio;
#[path = "tempo.rs"]
mod tempo;
#[path = "estimated.rs"]
mod estimated;
#[path = "hung.rs"]
mod hung;
#[path = "stretch.rs"]
mod stretch;
#[path = "silent.rs"]
mod silent;
#[cfg(feature = "core")]
#[path = "replan.rs"]
mod replan;
#[cfg(feature = "core")]
#[path = "album.rs"]
mod album;

/// The core's queue, planner, database and settings are per process: tests using it take turns.
#[cfg(feature = "core")]
fn core_turn() -> parking_lot::MutexGuard<'static, ()> {
    static CORE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    CORE.lock()
}
