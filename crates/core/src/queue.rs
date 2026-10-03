//! Saving the queue in the core's database. The song store is nori-queue's.

use crate::Core;

pub use nori_queue::queue::*;
pub use nori_queue::Session;

impl Core {
    /// Saves the known songs of `ids` (radio streams dropped) with their album `runs`, the current `index`
    /// remapped to the kept songs, and the page it came from.
    pub(crate) fn queue_save(&self, ids: Vec<String>, runs: Vec<u32>, index: u32, position_ms: u64, origin: Option<crate::PageOrigin>) -> crate::Result<()> {
        let (songs, runs, index) = self.session.store(|s| {
            let mut kept = Vec::with_capacity(ids.len());
            let mut kept_runs = Vec::with_capacity(ids.len());
            let mut at = 0;
            for (i, id) in ids.iter().enumerate() {
                let Some((song, _)) = s.songs.get(id).filter(|_| !is_radio(id)) else { continue };
                if i < index as usize {
                    at += 1;
                }
                kept.push(song.clone());
                kept_runs.push(runs.get(i).copied().unwrap_or(0));
            }
            (kept, kept_runs, at)
        });
        self.save_queue_with_runs(crate::PlayQueue { index: index.min(songs.len().saturating_sub(1) as u32), songs, position_ms, origin }, runs)
    }
}
