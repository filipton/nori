//! Offline bridge calls over the downloads in the core's database. The policy is nori-queue's.

use crate::playlist::{self, QueueEdit};
use crate::{db, queue, Core, Result};

pub use nori_queue::bridge::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Handles a network failure routed to the bridge (`OnError::Bridge`): jump to a queued download,
    /// else start a bridge, else skip or stop like any failure.
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

    /// The parked song is next (`BridgeStep::Parked`): resume the queue if `network_up`, else add more
    /// downloads before it. None when nothing changes.
    pub fn bridge_parked(&self, network_up: bool) -> Option<QueueEdit> {
        if network_up {
            playlist::playlist_unbridge()
        } else {
            self.bridge_start().ok().flatten()
        }
    }
}

impl Core {
    /// Inserts downloads most similar to the current song (none already queued), parking the queue
    /// behind them. None when no download qualifies.
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

    /// The first downloaded song after the current one in play order.
    pub(crate) fn bridge_next_downloaded(&self) -> Result<Option<u32>> {
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

/// What [`Core::bridge_take`] did, for the platform to mirror.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum BridgeTake {
    /// Play the queued download at `index`.
    Jump { index: u32 },
    /// A bridge started: apply `edit` and play, then watch the network.
    Bridged { edit: QueueEdit },
    Skip,
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
    fn bridge_starts_and_unbridges() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        for id in ["dl1", "dl2"] {
            core.download_queue(vec![song(id, "Muse", "x", false)]).unwrap();
            core.download_done(id.into()).unwrap();
        }
        queue::queue_register(vec![song("on1", "Muse", "y", false), song("on2", "Muse", "y", false)]);
        let _g = crate::playlist::tests::hold(&["on1", "on2"], 0);
        assert_eq!(core.bridge_next_downloaded().unwrap(), None);
        let e = core.bridge_start().unwrap().unwrap();
        assert_eq!((e.at, e.seek, e.songs.len(), e.remove.len()), (0, Some(0), 2, 0));
        assert!(crate::playlist::playlist_bridge_state().bridging);
        let back = crate::playlist::playlist_unbridge().unwrap();
        assert_eq!((back.remove, back.seek), (vec![0, 2], Some(0)));
        assert_eq!(crate::playlist::playlist_upcoming(5), vec!["on1".to_string(), "on2".to_string()]);
    }

    #[test]
    fn bridge_take_prefers_jump_then_bridge() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        queue::queue_register(vec![song("tk1", "Muse", "y", false), song("tk2", "Muse", "y", false), song("tk3", "Muse", "y", false)]);
        let _g = crate::playlist::tests::hold(&["tk1", "tk2", "tk3"], 0);
        crate::rules::queue_playing();
        assert!(matches!(core.bridge_take(), BridgeTake::Skip), "nothing downloaded");
        core.download_queue(vec![song("tk3", "Muse", "y", false)]).unwrap();
        core.download_done("tk3".into()).unwrap();
        assert!(matches!(core.bridge_take(), BridgeTake::Jump { index: 2 }));
        crate::playlist::playlist_moved_to(2);
        core.download_queue(vec![song("tk9", "Muse", "x", false)]).unwrap();
        core.download_done("tk9".into()).unwrap();
        match core.bridge_take() {
            BridgeTake::Bridged { edit } => assert_eq!(edit.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["tk9"]),
            t => panic!("{t:?}"),
        }
        assert!(crate::playlist::playlist_bridge_state().bridging);
        assert!(core.bridge_parked(true).is_some());
        assert!(!crate::playlist::playlist_bridge_state().bridging);
        crate::rules::queue_playing();
    }
}
