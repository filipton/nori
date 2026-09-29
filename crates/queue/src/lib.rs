//! The queue the app plays: list and order (playlist.rs), songs by id (queue.rs), transport rules
//! (rules.rs), refilling (autofill.rs), the offline bridge (bridge.rs), the audible song (heard.rs), play
//! counting (scrobble.rs) and play actions (actions.rs).

pub mod actions;
pub mod autofill;
pub mod bridge;
pub mod heard;
pub mod playlist;
pub mod queue;
pub mod rules;
pub mod scrobble;
