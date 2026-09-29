//! The transition engine's host backed by the core: plans, the analysis store and the log.

use nori_player::automix::analysis::Analyzer;
use nori_player::engine::{Host, Plan};

pub struct CoreHost<F: FnMut()> {
    /// The clock the engine's call was made at.
    pub now_ms: i64,
    /// Called when the song being heard changes.
    pub heard_changed: F,
}

impl<F: FnMut()> Host for CoreHost<F> {
    fn plan_for(&mut self, outgoing_id: &str) -> Option<Plan> {
        super::planner::plan_for(outgoing_id)
    }

    fn wants_analysis(&mut self, song_id: &str) -> Option<u64> {
        super::planner::wants_analysis(song_id)
    }

    fn analysed(&mut self, song_id: &str, analyzer: Analyzer, _channels: usize, frames: u64, rate: u32) {
        super::planner::analysed(song_id, analyzer, frames, rate);
    }

    fn heard_changed(&mut self) {
        (self.heard_changed)();
    }

    fn log(&mut self, message: &str) {
        nori_model::alog::info(message);
    }

    fn now_ms(&self) -> i64 {
        self.now_ms
    }
}
