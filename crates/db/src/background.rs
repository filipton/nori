//! The core's background thread for database writes, so callers (often the main or audio thread) never
//! wait on the database.

use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

type Job = Box<dyn FnOnce() + Send>;

/// Runs `job` on the background thread, after earlier jobs.
pub fn run(job: impl FnOnce() + Send + 'static) {
    // Global: one writer thread per process, reached from callers with no core handle.
    static TX: OnceLock<Sender<Job>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("nori-core".into())
            .spawn(move || {
                for job in rx {
                    job();
                }
            })
            .expect("spawn the background thread");
        tx
    });
    let _ = tx.send(Box::new(job));
}
