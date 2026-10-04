//! Saving the queue in the core's database. The song store is nori-queue's.

use crate::Core;

pub use nori_queue::queue::*;
pub use nori_queue::Session;

impl Core {
    /// Saves the known songs of `ids` (radio streams dropped), the current `index` remapped to the kept
    /// songs, and the page it came from.
    pub(crate) fn queue_save(&self, ids: Vec<String>, index: u32, position_ms: u64, origin: Option<crate::PageOrigin>) -> crate::Result<()> {
        let (songs, index) = self.session.store(|s| {
            let mut kept = Vec::with_capacity(ids.len());
            let mut at = 0;
            for (i, id) in ids.iter().enumerate() {
                let Some((song, _)) = s.songs.get(id).filter(|_| !is_radio(id)) else { continue };
                if i < index as usize {
                    at += 1;
                }
                kept.push(song.clone());
            }
            (kept, at)
        });
        self.save_queue(crate::PlayQueue { index: index.min(songs.len().saturating_sub(1) as u32), songs, position_ms, origin })
    }
}
