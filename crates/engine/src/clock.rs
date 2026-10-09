//! The engine thread's clock. Clients use [`Monotonic`] ([`crate::Engine::start`]); tests pass a clock
//! they move by hand ([`crate::Engine::start_on`]). Generic, so the real one is just `Instant` and
//! `park_timeout`.

use std::thread::Thread;
use std::time::{Duration, Instant};

/// Time as the engine's thread sees it. Every call but [`Clock::wake`] is made on that thread.
pub trait Clock: Clone + Send + 'static {
    /// Monotonic milliseconds since creation.
    fn now_ms(&self) -> i64;
    /// [`Clock::now_ms`] in µs, from any thread: a followed device's place is timed on it.
    fn now_us(&self) -> i64 {
        self.now_ms() * 1000
    }
    /// Sleeps until unparked or `ms` passed (`None`: until unparked). `waiting` says whether the engine
    /// awaits a song's bytes; a test clock holds time still then, as if the network were instant.
    fn sleep(&self, ms: Option<u64>, waiting: impl FnOnce() -> bool);
    /// The thread woke and is about to take its commands.
    fn woke(&self) {}
    /// Wakes `engine` for a command; called on the sender's thread.
    fn wake(&self, engine: &Thread) {
        engine.unpark();
    }
    /// Real-time limits of the songs' fetches; a test clock cannot see real time and may change them.
    fn waits(&self) -> crate::source::Waits {
        crate::source::Waits::default()
    }
}

/// The machine's monotonic clock and thread parking.
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

    fn now_us(&self) -> i64 {
        self.0.elapsed().as_micros() as i64
    }

    #[inline]
    fn sleep(&self, ms: Option<u64>, _waiting: impl FnOnce() -> bool) {
        match ms {
            Some(ms) => std::thread::park_timeout(Duration::from_millis(ms)),
            None => std::thread::park(),
        }
    }
}
