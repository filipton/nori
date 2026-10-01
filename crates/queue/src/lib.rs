//! The queue the app plays: list and order (playlist.rs), songs by id (queue.rs), transport rules
//! (rules.rs), refilling (autofill.rs), the offline bridge (bridge.rs), the audible song (heard.rs), play
//! counting (scrobble.rs) and play actions (actions.rs).

use std::sync::{Arc, LazyLock};

use nori_automix::planner::Planner;
use nori_settings::settings_store::Settings;
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
/// play counting, with the settings, the open profile's database and the transition planner they use.
/// The platform's client owns one; tests make their own.
pub struct Session {
    pub settings: Arc<Settings>,
    pub db: Arc<nori_db::Profile>,
    pub planner: Arc<Planner>,
    queue: Mutex<playlist::Queue>,
    /// Taken inside the queue's lock, never around it.
    store: Mutex<queue::Store>,
    controls: Mutex<rules::Controls>,
    refill: Mutex<nori_player::queue::Refill>,
    scrobbler: Mutex<scrobble::Scrobbler>,
}

impl Session {
    pub fn new(settings: Arc<Settings>) -> Session {
        let db = Arc::new(nori_db::Profile::default());
        let read = settings.clone();
        let planner = Planner::new(db.clone(), Box::new(move || read.with_prefs(nori_settings::settings::StoredPrefs::transition_prefs)));
        Session { settings, db, planner, queue: Default::default(), store: Default::default(), controls: Default::default(), refill: Default::default(), scrobbler: Default::default() }
    }
}

/// Over settings of its own, never opened (the defaults).
impl Default for Session {
    fn default() -> Session {
        Session::new(Arc::default())
    }
}

/// The session behind the platform's free entry points (uniffi, JNI), over the shared settings. Global:
/// those calls carry no handle.
pub fn shared() -> &'static Arc<Session> {
    static SHARED: LazyLock<Arc<Session>> = LazyLock::new(|| Arc::new(Session::new(nori_settings::settings_store::shared().clone())));
    &SHARED
}
