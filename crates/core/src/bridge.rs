//! The offline bridge as the core's calls, over the downloads in its database. When to bridge and back is
//! nori-queue's.

use crate::playlist::{self, QueueEdit};
use crate::{db, queue, Core, Result};

pub use nori_queue::bridge::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// A song the network would not bring, the core having said it is the bridge's (`queue_error`
    /// answered `Bridge`, or nori-engine's `Event::Bridge`): a download still queued after it plays, or
    /// else a bridge starts, and failing both the song is skipped or the music stops, as any failure
    /// would be. The run of failures is counted here either way.
    pub fn bridge_take(&self) -> BridgeTake {
        let next = self.bridge_next_downloaded().unwrap_or_else(|e| {
            nori_model::alog::info(&format!("bridge: the downloads could not be read: {e}"));
            None
        });
        if let Some(index) = next {
            crate::rules::queue_bridged();
            return BridgeTake::Jump { index };
        }
        if let Some(edit) = self.bridge_start().ok().flatten() {
            nori_model::alog::info(&format!("bridge: bridging with {} downloads", edit.songs.len()));
            crate::rules::queue_bridged();
            return BridgeTake::Bridged { edit };
        }
        if crate::rules::queue_bridge_failed() { BridgeTake::Skip } else { BridgeTake::Stop }
    }

    /// The bridge has played up to the parked song (`song_arrived` said `BridgeStep::Parked`): with the
    /// network back (`network_up`) the bridge's songs go and the parked song plays, else more downloads go
    /// in before it. None when there is nothing to change.
    pub fn bridge_parked(&self, network_up: bool) -> Option<QueueEdit> {
        if network_up {
            playlist::playlist_unbridge()
        } else {
            self.bridge_start().ok().flatten()
        }
    }
}

/// Asked only in Rust, so not exported to Kotlin.
impl Core {
    /// The server cannot be reached for the song playing: downloads to play instead, the closest to it
    /// first, none already queued. The first time the song and what follows are parked behind them;
    /// while bridging, more go in before the parked song. None when there is nothing downloaded to play.
    pub fn bridge_start(&self) -> Result<Option<QueueEdit>> {
        let (current, queued) = playlist::snapshot();
        let pool = self.downloads(true)?;
        let seed = current.and_then(queue::queue_song);
        let picks = pick(seed.as_ref(), &pool, &queued, BATCH as usize, db::now_ms() as u64);
        if picks.is_empty() {
            return Ok(None);
        }
        let ids = picks.iter().map(|s| s.id.clone()).collect();
        queue::queue_register(picks.clone());
        Ok(playlist::edit_splice(|p| p.bridge(ids), picks))
    }

    /// The first song after the playing one, in play order, that is on the phone: where to skip to when
    /// the server is out of reach before bridging at all. None when there is none.
    pub fn bridge_next_downloaded(&self) -> Result<Option<u32>> {
        let after: Vec<(usize, String)> = playlist::with(|p| p.upcoming().skip(1).map(|i| (i, p.ids()[i].clone())).collect());
        let c = self.db.lock();
        let mut st = c.prepare_cached("SELECT 1 FROM downloads WHERE server=sid() AND id=?1 AND done=1")?;
        for (i, id) in after {
            if st.exists([id])? {
                return Ok(Some(i as u32));
            }
        }
        Ok(None)
    }
}

/// What [`Core::bridge_take`] did with a song the network would not bring, for the platform to do the
/// same to its player.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum BridgeTake {
    /// A download queued after the song: play from queue position `index`.
    Jump { index: u32 },
    /// A bridge started: the edit made to the queue (it jumps to the first download), to apply and play;
    /// then watch for the network to come back (`playlist_unbridge`).
    Bridged { edit: QueueEdit },
    /// Nothing to bridge with: skip the song, as any failure.
    Skip,
    /// Nothing to bridge with, and the music stops here.
    Stop,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::Song;

    fn song(id: &str, artist: &str, album: &str, starred: bool) -> Song {
        Song { id: id.into(), artist: artist.into(), album_id: Some(album.into()), starred, ..Default::default() }
    }

    #[test]
    fn a_bridge_is_started_and_undone_over_the_core_queue() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        for id in ["dl1", "dl2"] {
            core.download_queue(vec![song(id, "Muse", "x", false)]).unwrap();
            core.download_done(id.into()).unwrap();
        }
        queue::queue_register(vec![song("on1", "Muse", "y", false), song("on2", "Muse", "y", false)]);
        let _g = crate::playlist::tests::hold(&["on1", "on2"], 0);
        assert_eq!(core.bridge_next_downloaded().unwrap(), None, "nothing queued after it is on the phone");
        let e = core.bridge_start().unwrap().unwrap();
        assert_eq!((e.at, e.seek, e.songs.len(), e.remove.len()), (0, 0, 2, 0));
        assert!(crate::playlist::playlist_bridge_state().bridging);
        let back = crate::playlist::playlist_unbridge().unwrap();
        assert_eq!((back.remove, back.seek), (vec![0, 2], 0));
        assert_eq!(crate::playlist::playlist_upcoming(5), vec!["on1".to_string(), "on2".to_string()]);
    }

    #[test]
    fn a_song_the_network_would_not_bring_is_taken_in_order() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        queue::queue_register(vec![song("tk1", "Muse", "y", false), song("tk2", "Muse", "y", false), song("tk3", "Muse", "y", false)]);
        let _g = crate::playlist::tests::hold(&["tk1", "tk2", "tk3"], 0);
        crate::rules::queue_playing();
        assert!(matches!(core.bridge_take(), BridgeTake::Skip), "nothing downloaded: skipped, as any failure is by default");
        core.download_queue(vec![song("tk3", "Muse", "y", false)]).unwrap();
        core.download_done("tk3".into()).unwrap();
        assert!(matches!(core.bridge_take(), BridgeTake::Jump { index: 2 }), "a download still queued plays first");
        crate::playlist::playlist_moved_to(2);
        core.download_queue(vec![song("tk9", "Muse", "x", false)]).unwrap();
        core.download_done("tk9".into()).unwrap();
        match core.bridge_take() {
            BridgeTake::Bridged { edit } => assert_eq!(edit.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["tk9"]),
            t => panic!("{t:?}"),
        }
        assert!(crate::playlist::playlist_bridge_state().bridging);
        assert!(core.bridge_parked(true).is_some(), "the network back: the parked queue returns");
        assert!(!crate::playlist::playlist_bridge_state().bridging);
        crate::rules::queue_playing();
    }
}
