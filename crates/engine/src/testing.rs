//! A virtual clock for tests: time moves only while the engine sleeps, and the test's device pulls on that
//! time, so results do not depend on machine load and minutes of music run in moments. [`Stepper`]
//! waits for the engine to sleep, then advances to the next engine timer or device pull. While the engine
//! waits for a song's bytes, time stands still (up to [`BYTES_WAIT`] of real time).

use std::sync::Arc;
use std::thread::Thread;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::Clock;

/// Real time the clock waits per sleep for bytes the engine awaits; longer than any in-memory server takes.
pub const BYTES_WAIT: Duration = Duration::from_millis(2_000);
/// The shorter wait while a server itself waits for the clock (a paced transcode).
pub const TIME_WAIT: Duration = Duration::from_millis(20);
/// Real time after which an engine that never sleeps is taken as stuck.
const STUCK: Duration = Duration::from_secs(120);

#[derive(Default)]
struct State {
    now_ns: i64,
    engine: Option<Thread>,
    /// The engine sleeps (until `deadline_ns`, if any), awaiting bytes if `bytes`.
    parked: bool,
    deadline_ns: Option<i64>,
    bytes: bool,
    /// Sleeps so far, and the one whose byte wait was given up.
    sleeps: u64,
    gave_up: u64,
    /// Servers waiting for the clock ([`Virtual::wait_until`]).
    time_waiters: u32,
    /// Woken by the test and no turn taken since.
    woken: bool,
}

#[derive(Default)]
struct Shared {
    s: Mutex<State>,
    cv: Condvar,
}

#[derive(Clone, Default)]
pub struct Virtual(Arc<Shared>);

impl Clock for Virtual {
    /// Retries take milliseconds, and a request never stalls: this clock cannot see real time, and a
    /// server paced on it takes far longer in real time than any stall on a phone.
    fn waits(&self) -> crate::Waits {
        crate::Waits { stall_ms: 600_000, retry_ms: 2 }
    }

    fn now_ms(&self) -> i64 {
        self.0.s.lock().now_ns / 1_000_000
    }

    fn sleep(&self, ms: Option<u64>, waiting: impl FnOnce() -> bool) {
        let bytes = waiting();
        let mut s = self.0.s.lock();
        s.engine.get_or_insert_with(std::thread::current);
        if s.woken {
            // Woken during the turn: turn again.
            return;
        }
        s.parked = true;
        s.deadline_ns = ms.map(|ms| s.now_ns + ms as i64 * 1_000_000);
        s.bytes = bytes;
        s.sleeps += 1;
        self.0.cv.notify_all();
        drop(s);
        std::thread::park();
        let mut s = self.0.s.lock();
        s.parked = false;
        s.deadline_ns = None;
    }

    fn woke(&self) {
        let mut s = self.0.s.lock();
        if std::mem::take(&mut s.woken) {
            // Consume the wake's unpark so it cannot cut the next sleep short.
            std::thread::park_timeout(Duration::ZERO);
        }
    }

    fn wake(&self, engine: &Thread) {
        let mut s = self.0.s.lock();
        s.woken = true;
        engine.unpark();
    }
}

impl Virtual {
    pub fn now_ns(&self) -> i64 {
        self.0.s.lock().now_ns
    }

    /// Blocks until the engine sleeps with nothing due now.
    pub fn settle(&self) {
        let started = Instant::now();
        let mut s = self.0.s.lock();
        let mut bytes_since: Option<(u64, Instant)> = None;
        loop {
            if s.parked && !s.woken {
                if !s.bytes || s.gave_up == s.sleeps {
                    return;
                }
                let hold = if s.time_waiters > 0 { TIME_WAIT } else { BYTES_WAIT };
                match bytes_since {
                    Some((n, t)) if n == s.sleeps => {
                        if t.elapsed() >= hold {
                            s.gave_up = s.sleeps;
                            return;
                        }
                    }
                    _ => bytes_since = Some((s.sleeps, Instant::now())),
                }
            }
            assert!(started.elapsed() < STUCK, "the engine did not go to sleep");
            self.0.cv.wait_for(&mut s, Duration::from_millis(1));
        }
    }

    /// Blocks a server thread until the clock reaches `ns`; meanwhile byte waits are [`TIME_WAIT`].
    pub fn wait_until(&self, ns: i64) {
        let started = Instant::now();
        let mut s = self.0.s.lock();
        s.time_waiters += 1;
        while s.now_ns < ns {
            assert!(started.elapsed() < STUCK, "the test's clock never reached {ns} ns (it stays at {} ns)", s.now_ns);
            self.0.cv.wait_for(&mut s, Duration::from_millis(1));
        }
        s.time_waiters -= 1;
    }

    /// Blocks a server thread while `on` holds (a hung request), at most `limit`; returns whether it
    /// still held. Meanwhile byte waits are [`TIME_WAIT`].
    pub fn hang_while(&self, mut on: impl FnMut() -> bool, limit: Duration) -> bool {
        let started = Instant::now();
        let mut s = self.0.s.lock();
        s.time_waiters += 1;
        let held = loop {
            if !on() {
                break false;
            }
            if started.elapsed() >= limit {
                break true;
            }
            self.0.cv.wait_for(&mut s, Duration::from_millis(1));
        };
        s.time_waiters -= 1;
        held
    }

    /// The engine's thread, once it slept on this clock.
    pub fn engine_thread(&self) -> Option<std::thread::ThreadId> {
        self.0.s.lock().engine.as_ref().map(Thread::id)
    }

    /// Sleeps so far.
    pub fn sleeps(&self) -> u64 {
        self.0.s.lock().sleeps
    }

    /// When the engine asked to be woken.
    pub fn deadline_ns(&self) -> Option<i64> {
        self.0.s.lock().deadline_ns
    }

    /// Advances to `ns`, waking the engine if its timer ran out.
    pub fn move_to(&self, ns: i64) {
        let mut s = self.0.s.lock();
        s.now_ns = s.now_ns.max(ns);
        self.0.cv.notify_all();
        if s.deadline_ns.is_some_and(|d| d <= s.now_ns) {
            s.deadline_ns = None;
            Self::poke(&mut s);
        }
    }

    /// The test woke the engine (e.g. a pull reached the ring's low mark).
    pub fn woke_engine(&self) {
        Self::poke(&mut self.0.s.lock());
    }

    fn poke(s: &mut State) {
        if let Some(t) = &s.engine {
            s.woken = true;
            t.unpark();
        }
    }
}

/// A device on the clock.
pub trait Device: Send {
    /// When it next pulls, ns.
    fn due_ns(&self) -> i64;
    /// Pulls at `now_ns`; true when that woke the engine.
    fn tick(&mut self, now_ns: i64) -> bool;
}

/// Drives a [`Virtual`] clock through engine timers and device pulls.
pub struct Stepper<D: Device> {
    pub clock: Virtual,
    pub device: Arc<Mutex<D>>,
}

impl<D: Device> Stepper<D> {
    pub fn new(clock: Virtual, device: Arc<Mutex<D>>) -> Self {
        Stepper { clock, device }
    }

    /// Runs the next due event: the engine's timer or the device's pull.
    pub fn step(&self) {
        self.step_by(i64::MAX);
    }

    /// [`Stepper::step`], the clock going no further than `limit_ns`.
    fn step_by(&self, limit_ns: i64) {
        self.clock.settle();
        let pull = self.device.lock().due_ns();
        let t = self.clock.deadline_ns().map_or(pull, |d| d.min(pull)).min(limit_ns);
        self.clock.move_to(t);
        self.clock.settle();
        if t >= pull {
            let woke = self.device.lock().tick(t);
            if woke {
                self.clock.woke_engine();
                self.clock.settle();
            }
        }
    }

    /// Advances by `d`.
    pub fn run(&self, d: Duration) {
        let until = self.clock.now_ns() + d.as_nanos() as i64;
        while self.clock.now_ns() < until {
            self.step_by(until);
        }
        self.clock.settle();
    }

    /// Advances until `done`, at most `limit`; returns whether it came.
    pub fn until(&self, limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        let until = self.clock.now_ns() + limit.as_nanos() as i64;
        self.clock.settle();
        loop {
            if done() {
                return true;
            }
            if self.clock.now_ns() >= until {
                return false;
            }
            self.step();
        }
    }
}
