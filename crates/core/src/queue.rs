//! Saving the queue in the core's database. The song store is nori-queue's.

use crate::Core;

pub use nori_queue::queue::*;
pub use nori_queue::Session;

impl Core {
    /// Saves the known songs of `ids` (radio streams dropped), the current `index` remapped to the kept
    /// songs, and the page it came from. The songs are written only when `ids` or `origin` changed since
    /// the last save; otherwise only the place.
    pub(crate) fn queue_save(&self, ids: Vec<String>, index: u32, position_ms: u64, origin: Option<crate::PageOrigin>) -> crate::Result<()> {
        let mut saved = self.saved_queue.lock();
        if let Some(s) = saved.as_ref().filter(|s| s.ids == ids && s.origin == origin) {
            return self.save_place(s.place(index), position_ms);
        }
        let (songs, kept) = self.session.store(|s| {
            let mut songs = Vec::with_capacity(ids.len());
            let kept = ids.iter().map(|id| s.songs.get(id).filter(|_| !is_radio(id)).map(|(song, _)| songs.push(song.clone())).is_some()).collect();
            (songs, kept)
        });
        let s = SavedQueue { ids, origin, kept };
        self.write_queue(crate::PlayQueue { index: s.place(index), songs, position_ms, origin: s.origin.clone() })?;
        *saved = Some(s);
        Ok(())
    }
}

/// The queue whose songs are stored: its ids and origin, and which ids were kept.
pub(crate) struct SavedQueue {
    ids: Vec<String>,
    origin: Option<crate::PageOrigin>,
    kept: Vec<bool>,
}

impl SavedQueue {
    /// `index` among the kept songs.
    fn place(&self, index: u32) -> u32 {
        let before = self.kept.iter().take(index as usize).filter(|k| **k).count();
        before.min(self.kept.iter().filter(|k| **k).count().saturating_sub(1)) as u32
    }
}
