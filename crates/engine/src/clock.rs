//! The time the engine's thread keeps and sleeps by. A client uses [`Monotonic`], the machine's own
//! clock, through [`crate::Engine::start`]; a test can give the engine a clock it moves by hand
//! ([`crate::Engine::start_on`]) and step it through minutes of music without waiting for them.
//!
//! The engine is generic over its clock, so the real one costs what the code it replaces cost: an
//! `Instant` read, and `park`/`park_timeout`.

use std::thread::Thread;
use std::time::{Duration, Instant};

/// Time as the engine's thread sees it. Every call but [`Clock::wake`] is made on that thread.
pub trait Clock: Clone + Send + 'static {
    /// Milliseconds since the clock was made, never going back.
    fn now_ms(&self) -> i64;
    /// Sleeps until the thread is unparked (a command, the output's pull, a song's bytes arriving), or
    /// `ms` have passed on this clock; `None` sleeps until unparked. `waiting` says whether the engine
    /// is waiting for a song's bytes: a clock that is moved by hand holds its time still for those,
    /// as though the network were instant. It is only asked by such a clock.
    fn sleep(&self, ms: Option<u64>, waiting: impl FnOnce() -> bool);
    /// The engine's thread woke, and is about to take its commands.
    fn woke(&self) {}
    /// A command was sent to the engine: its thread, `engine`, is woken to take it. Called on the
    /// sender's thread.
    fn wake(&self, engine: &Thread) {
        engine.unpark();
    }
    /// The real-time limits of the songs' fetches. A clock moved by hand cannot see real time, so it
    /// may lift the stall and shorten the retries.
    fn waits(&self) -> crate::source::Waits {
        crate::source::Waits::default()
    }
}

/// The machine's monotonic clock, and the thread's own parking.
#[derive(Clone, Copy)]
pub struct Monotonic(Instant);

impl Monotonic {
    pub fn new() -> Monotonic {
        Monotonic(Instant::now())
    }
}

impl Default for Monotonic {
    fn default() -> Self {
        Monotonic::new()
    }
}

impl Clock for Monotonic {
    #[inline]
    fn now_ms(&self) -> i64 {
        self.0.elapsed().as_millis() as i64
    }

    #[inline]
    fn sleep(&self, ms: Option<u64>, _waiting: impl FnOnce() -> bool) {
        match ms {
            Some(ms) => std::thread::park_timeout(Duration::from_millis(ms)),
            None => std::thread::park(),
        }
    }
}
