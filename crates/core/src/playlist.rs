//! Core and client calls on the queue: saving it locally and pushing it to the server. The queue is nori-queue's.

use crate::client::{Client, NetResult};
use crate::Core;

pub use nori_queue::playlist::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Saves the queue and its origin page for the next start.
    pub fn playlist_save(&self, position_ms: u64) -> crate::Result<()> {
        let (ids, runs, index) = with(|p| (p.ids().to_vec(), p.album_runs().to_vec(), p.current().unwrap_or(0) as u32));
        self.queue_save(ids, runs, index, position_ms, playlist_origin())
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Saves the queue as the server's play queue ([`playlist_to_push`]), at `current` and `position_ms`.
    pub async fn playlist_push(&self, current: Option<String>, position_ms: i64) -> NetResult<()> {
        match push_write(current, position_ms) {
            Some(w) => self.write(w).await,
            None => Ok(()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use parking_lot::Mutex;

    use super::*;

    /// The queue is process-wide: tests that use it take turns.
    static TURN: Mutex<()> = Mutex::new(());

    pub(crate) fn hold(ids: &[&str], start: u32) -> parking_lot::MutexGuard<'static, ()> {
        let g = TURN.lock();
        playlist_set(ids.iter().map(|s| s.to_string()).collect(), Some(start), false, None);
        playlist_repeat(0);
        g
    }

    #[test]
    fn saved_queue_restores_origin_and_album_runs() {
        use crate::{OriginKind, PageOrigin, Song};
        let core = crate::Core::new(String::new(), "t".into()).unwrap();
        let song = |id: &str| Song { id: id.into(), ..Default::default() };
        crate::queue::queue_register(vec![song("sv1"), song("sv2"), song("sv3")]);
        let _g = hold(&["sv0"], 0);
        let from = PageOrigin::new(OriginKind::Playlist, "pl-7");
        playlist_set(vec!["sv1".into(), "sv2".into()], Some(1), false, Some(from.clone()));
        // Edits and the offline bridge keep the origin.
        playlist_take(9, vec!["sv3".into()], vec![Hand::Last], None);
        edit_splice(|p| p.bridge(vec!["sv3".into()]), vec![]);
        playlist_unbridge();
        assert_eq!(playlist_origin(), Some(from.clone()));
        core.playlist_save(1500).unwrap();

        let q = core.load_queue().unwrap();
        assert_eq!((q.songs.len(), q.index, q.origin.as_ref()), (3, 1, Some(&from)));
        playlist_set(q.songs.iter().map(|s| s.id.clone()).collect(), Some(q.index), false, q.origin);
        assert!(playlist_from(nori_library::pages::PageQueue::new(from)));

        crate::queue::queue_register(vec![song("sv4"), song("sv5")]);
        playlist_set(vec!["sv4".into(), "sv5".into()], Some(0), false, Some(PageOrigin::new(OriginKind::Album, "al-1")));
        playlist_take(9, vec!["sv1".into()], vec![Hand::Last], None);
        let runs = with(|p| p.album_runs().to_vec());
        assert_eq!(runs[1], 0);
        core.playlist_save(0).unwrap();
        playlist_set(vec!["sv0".into()], Some(0), false, None);
        let q = core.load_queue().unwrap();
        playlist_set(q.songs.iter().map(|s| s.id.clone()).collect(), Some(q.index), false, q.origin);
        assert_eq!(with(|p| p.album_runs().to_vec()), runs);

        // No origin, a pre-origin save, or an unknown kind: songs still restore.
        playlist_set(vec!["sv1".into()], Some(0), false, None);
        core.playlist_save(0).unwrap();
        assert_eq!(core.load_queue().unwrap().origin, None);
        for origin in ["", r#","origin":{"kind":"Nebula","id":"x"}"#] {
            let json = format!(r#"{{"songs":[{{"id":"sv1"}}],"index":0,"position":0{origin}}}"#);
            nori_db::kv_put(&core.db.lock(), "queue", &json).unwrap();
            let q = core.load_queue().unwrap();
            assert_eq!((q.songs.len(), q.origin), (1, None), "{origin}");
        }
    }

    #[test]
    fn save_drops_radio_and_unknown_songs_and_remaps_index() {
        let core = crate::Core::new(String::new(), "t".into()).unwrap();
        crate::queue::queue_register(vec![crate::Song { id: "rk1".into(), ..Default::default() }, crate::Song { id: "rk2".into(), ..Default::default() }]);
        let _g = hold(&["radio:1", "rk-unknown", "rk1", "rk2"], 2);
        core.playlist_save(0).unwrap();
        let q = core.load_queue().unwrap();
        assert_eq!((q.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), q.index), (vec!["rk1", "rk2"], 0), "rk1 stays current");
    }
}
