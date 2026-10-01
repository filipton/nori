//! The queue the app plays: list and order (playlist.rs), songs by id (queue.rs), transport rules
//! (rules.rs), refilling (autofill.rs), the offline bridge (bridge.rs), the audible song (heard.rs), play
//! counting (scrobble.rs) and play actions (actions.rs).

use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;

pub mod actions;
pub mod autofill;
pub mod bridge;
pub mod heard;
pub mod playlist;
pub mod queue;
pub mod rules;
pub mod scrobble;

/// One app's queue: the list, the songs by id, what the transport rules remember, the refill and the
/// play counting. The platform's client owns one; tests make their own.
#[derive(Default)]
pub struct Session {
    queue: Mutex<playlist::Queue>,
    /// Taken inside the queue's lock, never around it.
    store: Mutex<queue::Store>,
    controls: Mutex<rules::Controls>,
    refill: Mutex<nori_player::queue::Refill>,
    scrobbler: Mutex<scrobble::Scrobbler>,
}

/// The session behind the platform's free entry points (uniffi, JNI). Global: those calls carry no handle.
pub fn shared() -> &'static Arc<Session> {
    static SHARED: LazyLock<Arc<Session>> = LazyLock::new(Arc::default);
    &SHARED
}
