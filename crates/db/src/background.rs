//! The core's background thread for database writes, so callers (often the main or audio thread) never
//! wait on the database.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

type Job = Box<dyn FnOnce() + Send>;

/// Runs `job` on the background thread, after earlier jobs. A job that panics is logged; the jobs after
/// it still run.
pub fn run(job: impl FnOnce() + Send + 'static) {
    // Global: one writer thread per process, reached from callers with no core handle.
    static TX: OnceLock<Sender<Job>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("nori-core".into())
            .spawn(move || {
                for job in rx {
                    if catch_unwind(AssertUnwindSafe(job)).is_err() {
                        nori_model::alog::info("background: a write panicked");
                    }
                }
            })
            .expect("spawn the background thread");
        tx
    });
    let _ = tx.send(Box::new(job));
}

/// Waits until the jobs sent so far have run: a client quitting, so its last writes are kept.
pub fn flush() {
    let (tx, rx) = channel();
    run(move || {
        let _ = tx.send(());
    });
    let _ = rx.recv();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_write_stops_no_later_one() {
        run(|| panic!("a broken write"));
        let (tx, rx) = channel();
        run(move || tx.send(()).unwrap());
        flush();
        assert!(rx.try_recv().is_ok());
    }
}
