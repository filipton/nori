//! Offline bridge calls over the downloads in the core's database. The policy is nori-queue's.

use crate::cache_policy::Read;
use crate::client::Client;
use crate::playlist::QueueEdit;
use crate::{db, Core, Result};

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
            self.session.bridged();
            return BridgeTake::Jump { index };
        }
        if let Some(edit) = self.bridge_start().ok().flatten() {
            nori_model::alog::info(&format!("bridge: bridging with {} downloads", edit.songs.len()));
            self.session.bridged();
            return BridgeTake::Bridged { edit };
        }
        if self.session.bridge_failed() { BridgeTake::Skip } else { BridgeTake::Stop }
    }
}

/// Whether the server is back is the server's answer to a ping, asked when the parked song comes up and
/// when the platform reports another network; never on a timer.
#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// The parked song is next (`BridgeStep::Parked`): the parked queue comes back if the server answers,
    /// else more downloads go in before it. None when nothing changes.
    pub async fn bridge_parked(&self) -> Option<QueueEdit> {
        if self.server_answers().await {
            self.core.session.unbridge()
        } else {
            self.core.bridge_start().ok().flatten()
        }
    }

    /// The platform moved to another network while bridging: the parked queue comes back if the server
    /// answers. None when nothing is bridged or the server is still gone.
    pub async fn bridge_network_changed(&self) -> Option<QueueEdit> {
        if !self.core.session.bridge_state().bridging || !self.server_answers().await {
            return None;
        }
        self.core.session.unbridge()
    }
}

impl Client {
    async fn server_answers(&self) -> bool {
        self.read_now(Read::Ping).await.is_ok()
    }
}

impl Core {
    /// Inserts downloads most similar to the current song (none already queued), parking the queue
    /// behind them. None when no download qualifies.
    pub fn bridge_start(&self) -> Result<Option<QueueEdit>> {
        let (current, queued) = self.session.snapshot();
        let pool = self.downloads(true)?;
        let seed = current.and_then(|id| self.session.song(&id));
        let picks = pick(seed.as_ref(), &pool, &queued, BATCH as usize, db::now_ms() as u64);
        if picks.is_empty() {
            return Ok(None);
        }
        let ids = picks.iter().map(|s| s.id.clone()).collect();
        self.session.register(picks.clone());
        Ok(self.session.edit_splice(|p| p.bridge(ids), picks))
    }

    /// The first downloaded song after the current one in play order.
    pub(crate) fn bridge_next_downloaded(&self) -> Result<Option<u32>> {
        let after: Vec<(usize, String)> = self.session.playlist(|p| p.upcoming().skip(1).map(|i| (i, p.ids()[i].clone())).collect());
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
    fn bridge_cycle() {
        let core = crate::playlist::tests::core(&["on1", "on2"], 0);
        for id in ["dl1", "dl2"] {
            core.download_queue(vec![song(id, "Muse", "x", false)]).unwrap();
            core.download_done(id.into()).unwrap();
        }
        core.session.register(vec![song("on1", "Muse", "y", false), song("on2", "Muse", "y", false)]);
        assert_eq!(core.bridge_next_downloaded().unwrap(), None);
        let e = core.bridge_start().unwrap().unwrap();
        assert_eq!((e.at, e.seek, e.songs.len(), e.remove.len()), (0, Some(0), 2, 0));
        assert!(core.session.bridge_state().bridging);
        let back = core.session.unbridge().unwrap();
        assert_eq!((back.remove, back.seek), (vec![0, 2], Some(0)));
        assert_eq!(core.session.upcoming(5), vec!["on1".to_string(), "on2".to_string()]);

        // Bridge take prefers jump then bridge.
        let core = crate::playlist::tests::core(&["tk1", "tk2", "tk3"], 0);
        core.session.register(vec![song("tk1", "Muse", "y", false), song("tk2", "Muse", "y", false), song("tk3", "Muse", "y", false)]);
        assert!(matches!(core.bridge_take(), BridgeTake::Skip), "nothing downloaded");
        core.download_queue(vec![song("tk3", "Muse", "y", false)]).unwrap();
        core.download_done("tk3".into()).unwrap();
        assert!(matches!(core.bridge_take(), BridgeTake::Jump { index: 2 }));
        core.session.moved_to(2);
        core.download_queue(vec![song("tk9", "Muse", "x", false)]).unwrap();
        core.download_done("tk9".into()).unwrap();
        match core.bridge_take() {
            BridgeTake::Bridged { edit } => assert_eq!(edit.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["tk9"]),
            t => panic!("{t:?}"),
        }
        assert!(core.session.bridge_state().bridging);
    }

    /// The parked queue comes back when the server answers a ping, on the parked song or another network.
    #[test]
    fn back_when_the_server_answers() {
        use crate::client::tests::{block, client, Fake, OK};
        use crate::transport::FailureKind;
        let profile = || crate::client::NetProfile { url: "h".into(), ..Default::default() };
        let bridged = || {
            let (c, fake) = client(profile());
            c.core.session.set(vec!["on1".into(), "on2".into()], Some(0), false, None);
            c.core.session.register(vec![song("on1", "Muse", "y", false), song("on2", "Muse", "y", false)]);
            // Enough for a second batch.
            for k in 0..BATCH * 2 {
                let id = format!("dl{k}");
                c.core.download_queue(vec![song(&id, "Muse", "x", false)]).unwrap();
                c.core.download_done(id).unwrap();
            }
            assert!(c.core.bridge_start().unwrap().is_some());
            (c, fake)
        };
        let ping = |fake: &Fake, up: bool| if up { fake.answer(OK) } else { fake.fail(FailureKind::Connect) };
        let pings = |fake: &Fake| fake.asked().iter().filter(|u| u.contains("/rest/ping")).count();

        for up in [false, true] {
            let (c, fake) = bridged();
            ping(&fake, up);
            let edit = block(c.bridge_network_changed());
            assert_eq!((edit.is_some(), c.core.session.bridge_state().bridging), (up, !up), "network changed, server up {up}");
            assert_eq!(pings(&fake), 1);

            let (c, fake) = bridged();
            ping(&fake, up);
            let edit = block(c.bridge_parked()).expect("the queue changes either way");
            assert_eq!(c.core.session.bridge_state().bridging, !up, "parked, server up {up}");
            assert_eq!(edit.remove.is_empty(), !up, "back: the bridge's songs go; still gone: more go in");
        }

        let (c, fake) = client(profile());
        assert!(block(c.bridge_network_changed()).is_none());
        assert_eq!(pings(&fake), 0, "nothing bridged, nothing asked");
    }

}
