//! Analysis store calls on the core's database. AutoMix itself is nori-automix's.

use nori_automix::beats::{EndGrid, MixEnd};
use nori_automix::analysis::Analyzer;
use nori_automix::store::{get, get_voice, missing, neural_missing, put, put_finished, AnalysisStream};

use crate::{Core, Result, TrackAnalysis};

pub use nori_automix::*;

#[cfg(test)]
mod tests;

/// What "Measure again" did: the songs forgotten, and what it asks of the player, as a settings change's
/// effect bits (`settings_store::REPLAN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct MeasuredAgain {
    pub forgot: u32,
    pub effect: u32,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    pub fn analysis_get(&self, song_id: String) -> Result<Option<TrackAnalysis>> {
        Ok(get(&self.db.lock(), &song_id)?)
    }

    /// "Measure again": deletes every analysis and vocal curve, so songs are measured anew, and has the
    /// transition coming up planned again without them.
    pub fn measure_again(&self) -> Result<MeasuredAgain> {
        let forgot = {
            let c = self.db.lock();
            let n = c.execute("DELETE FROM track_analysis WHERE server=sid()", [])? as u32;
            c.execute("DELETE FROM vocal_curve WHERE server=sid()", [])?;
            n
        };
        self.session.planner.analyses_changed();
        Ok(MeasuredAgain { forgot, effect: crate::settings_store::REPLAN })
    }

    /// Number of stored analyses.
    pub fn analysis_count(&self) -> Result<u32> {
        Ok(self.db.lock().query_row("SELECT count(*) FROM track_analysis WHERE server=sid()", [], |r| r.get(0))?)
    }
}

impl Core {
    /// The analysable ids of `song_ids` lacking a current analysis.
    pub fn analysis_missing(&self, song_ids: Vec<String>) -> Result<Vec<String>> {
        let song_ids: Vec<String> = song_ids.into_iter().filter(|id| crate::queue::analysable(id)).collect();
        Ok(missing(&self.db.lock(), &song_ids)?)
    }

    /// Finishes `stream` (one whole song from its start) into the store; dropped unless its length matches
    /// `expected_ms` (0: unknown).
    pub fn analysis_finish_whole(&self, song_id: &str, stream: AnalysisStream, expected_ms: i64) -> Result<Option<TrackAnalysis>> {
        let a = stream.into_analyzer();
        let heard_ms = (a.samples() as f64 * 1000.0 / a.rate()) as i64;
        if !nori_player::transitions::whole_song(heard_ms, expected_ms) {
            return Ok(None);
        }
        self.analysis_finish(song_id, a)
    }

    /// Finishes an analyser fed one whole song and stores the result; None under 30 s.
    pub fn analysis_finish(&self, song_id: &str, mut a: Analyzer) -> Result<Option<TrackAnalysis>> {
        if a.samples() < (a.rate() * 30.0) as u64 {
            return Ok(None);
        }
        let stored = put_finished(&self.db.lock(), song_id, &a.take_features())?;
        self.session.planner.analyses_changed();
        Ok(Some(stored))
    }

    /// The song's vocal activity curve (checked against synced lyrics).
    pub(crate) fn analysis_voice(&self, song_id: &str) -> Result<Option<nori_player::automix::vocal::VocalCurve>> {
        Ok(get_voice(&self.db.lock(), song_id)?)
    }

    /// The ids of `song_ids` with a current analysis that has an end the beat model has not checked.
    pub fn analysis_neural_missing(&self, song_ids: Vec<String>) -> Result<Vec<String>> {
        let song_ids: Vec<String> = song_ids.into_iter().filter(|id| crate::queue::analysable(id)).collect();
        Ok(neural_missing(&self.db.lock(), &song_ids)?)
    }

    /// Stores the beat model's `grid` for `end` (adopted if confident) and marks the end checked. Returns
    /// whether it was adopted; false without an analysis.
    pub fn analysis_neural_store(&self, song_id: &str, end: MixEnd, grid: Option<EndGrid>) -> Result<bool> {
        let c = self.db.lock();
        let Some(mut row) = get(&c, song_id)? else { return Ok(false) };
        let adopted = nori_automix::beats::merge(&mut row, end, grid);
        put(&c, &row)?;
        if adopted {
            self.session.planner.analyses_changed();
        }
        Ok(adopted)
    }
}
