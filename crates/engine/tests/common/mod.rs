//! A clock the test moves by hand, for the engine's thread and the sound card the test plays it
//! through: time only moves when the engine sleeps (it takes no time to do anything), and the sound card
//! pulls on that time rather than on a thread of its own. What a test sees no longer depends on how busy
//! the machine is, and minutes of music take as long as the engine takes to make them.
//!
//! A test drives it through [`Stepper`]: each step waits until the engine sleeps, moves the time to the
//! next thing due (the engine's own timer, or the card's next pull) and does it. A song's bytes still
//! come on the loader's own thread; while the engine waits for them the time stands still, as though the
//! network were instant (a server made slow on purpose holds it up to [`BYTES_WAIT`]).

#![allow(dead_code)]

pub mod card;

use std::sync::Arc;
use std::thread::Thread;
use std::time::{Duration, Instant};

use nori_engine::Clock;
use parking_lot::{Condvar, Mutex};

/// How long, in real time, the time stands still while the engine waits for a song's bytes, per sleep:
/// longer than any in-memory server takes, however busy the machine.
pub const BYTES_WAIT: Duration = Duration::from_millis(2_000);
/// How long the time stands still instead while a server is itself waiting for the time to move (a
/// transcode coming out at so many bytes a second of the test's clock): only long enough for bytes
/// already on their way to land, since waiting longer only waits for itself.
pub const TIME_WAIT: Duration = Duration::from_millis(20);
/// An engine that has not gone to sleep after this long, in real time, is stuck.
const STUCK: Duration = Duration::from_secs(120);

#[derive(Default)]
struct State {
    now_ns: i64,
    engine: Option<Thread>,
    /// The engine sleeps, until `deadline_ns` if it said, waiting for bytes if `bytes`.
    parked: bool,
    deadline_ns: Option<i64>,
    bytes: bool,
    /// Sleeps so far, and the one the test stopped waiting for bytes in.
    sleeps: u64,
    gave_up: u64,
    /// Servers waiting, in [`Virtual::wait_until`], for the time to move.
    time_waiters: u32,
    /// The engine was woken by the test (a command, its timer, the card's pull) and has not yet taken a
    /// turn since.
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
    fn waits(&self) -> nori_engine::Waits {
        nori_engine::Waits { stall_ms: 600_000, retry_ms: 2 }
    }

    fn now_ms(&self) -> i64 {
        self.0.s.lock().now_ns / 1_000_000
    }

    fn sleep(&self, ms: Option<u64>, waiting: impl FnOnce() -> bool) {
        let bytes = waiting();
        let mut s = self.0.s.lock();
        s.engine.get_or_insert_with(std::thread::current);
        if s.woken {
            // Woken since the turn began: another turn, which takes the unpark that came with it.
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
            // The unpark that came with the wake (made under this lock) is taken now, so the next sleep
            // is not cut short by it once the time has moved on.
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

    /// Waits, in real time, until the engine sleeps with nothing left to do at this time.
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

    /// Blocks a server's thread, in real time, until the test's clock reaches `ns`. While it waits, an
    /// engine waiting for bytes lets the time move after [`TIME_WAIT`] rather than [`BYTES_WAIT`].
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

    /// Blocks a server's thread, in real time, while `on` holds (a request that hangs), for `limit` at
    /// most: whether it still held then. Meanwhile an engine waiting for bytes lets the time move after
    /// [`TIME_WAIT`], as it does for a server waiting on the clock: a request that never answers must not
    /// hold the test's time up.
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

    /// The engine's thread, once it has slept on this clock: to tell its doings from other tests' engines.
    pub fn engine_thread(&self) -> Option<std::thread::ThreadId> {
        self.0.s.lock().engine.as_ref().map(Thread::id)
    }

    /// How many times the engine has gone to sleep so far: each one ends in a wake.
    pub fn sleeps(&self) -> u64 {
        self.0.s.lock().sleeps
    }

    /// When the engine asked to be woken, if it did.
    pub fn deadline_ns(&self) -> Option<i64> {
        self.0.s.lock().deadline_ns
    }

    /// The time is `ns`: the engine is woken if its timer ran out.
    pub fn move_to(&self, ns: i64) {
        let mut s = self.0.s.lock();
        s.now_ns = s.now_ns.max(ns);
        self.0.cv.notify_all();
        if s.deadline_ns.is_some_and(|d| d <= s.now_ns) {
            s.deadline_ns = None;
            Self::poke(&mut s);
        }
    }

    /// Something the test did (a pull that took the ring to its low mark) woke the engine.
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

/// A device on the clock: pulls when due, and says whether that woke the engine.
pub trait Device: Send {
    /// When it next wants to pull, ns.
    fn due_ns(&self) -> i64;
    /// Its pull at `now_ns`: true when it woke the engine.
    fn tick(&mut self, now_ns: i64) -> bool;
}

/// Moves a [`Virtual`] clock through the engine's timers and a device's pulls.
pub struct Stepper<D: Device> {
    pub clock: Virtual,
    pub device: Arc<Mutex<D>>,
}

impl<D: Device> Stepper<D> {
    pub fn new(clock: Virtual, device: Arc<Mutex<D>>) -> Self {
        Stepper { clock, device }
    }

    /// One thing due: the engine's timer or the device's pull, whichever comes first, once the engine
    /// sleeps.
    pub fn step(&self) {
        self.clock.settle();
        let pull = self.device.lock().due_ns();
        let t = self.clock.deadline_ns().map_or(pull, |d| d.min(pull));
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

    /// Runs the time on by `d`.
    pub fn run(&self, d: Duration) {
        let until = self.clock.now_ns() + d.as_nanos() as i64;
        while self.clock.now_ns() < until {
            self.step();
        }
        self.clock.settle();
    }

    /// Runs the time on until `done`, for `limit` at most: whether it came.
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
