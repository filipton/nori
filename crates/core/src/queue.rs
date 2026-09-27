//! The queue's songs as the core's calls: the queue saved in the core's database and read back. The songs by
//! id are nori-queue's.

use crate::Core;

pub use nori_queue::queue::*;

impl Core {
    /// Saves the queue for next time from its ids (radio streams are left out: they do not come back),
    /// with the page it was started from.
    /// `runs` is each song's album run (`Playlist::album_runs`), saved with the songs kept.
    pub(crate) fn queue_save(&self, ids: Vec<String>, runs: Vec<u32>, index: u32, position_ms: u64, origin: Option<crate::PageOrigin>) -> crate::Result<()> {
        let (songs, runs, index) = with(|s| {
            let mut kept = Vec::with_capacity(ids.len());
            let mut kept_runs = Vec::with_capacity(ids.len());
            let mut at = 0;
            for (i, id) in ids.iter().enumerate() {
                if id.starts_with(RADIO_PREFIX) {
                    continue;
                }
                if i < index as usize {
                    at += 1;
                }
                if let Some((song, _)) = s.songs.get(id) {
                    kept.push(song.clone());
                    kept_runs.push(runs.get(i).copied().unwrap_or(0));
                }
            }
            (kept, kept_runs, at.min(index))
        });
        self.save_queue_with_runs(crate::PlayQueue { index: index.min(songs.len().saturating_sub(1) as u32), songs, position_ms, origin }, runs)
    }
}
