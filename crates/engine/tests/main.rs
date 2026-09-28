//! The engine's tests on the virtual clock, in one test binary so that they run side by side instead of
//! one binary after another (Cargo.toml lists the test targets). Each file stays a test file of its own
//! with its own helpers, and `cargo test -p nori-engine --test engine <name>` picks any of them.

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

/// The core keeps one queue, planner, database and set of settings per process: the tests here that play
/// through it take turns.
#[cfg(feature = "core")]
fn core_turn() -> parking_lot::MutexGuard<'static, ()> {
    static CORE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    CORE.lock()
}
