//! Core and client calls on the queue: saving it locally and pushing it to the server. The queue is nori-queue's.

use crate::client::{Client, NetResult};
use crate::Core;

pub use nori_queue::playlist::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Saves the queue and its origin page for the next start.
    pub fn playlist_save(&self, position_ms: u64) -> crate::Result<()> {
        let (ids, index) = self.session.playlist(|p| (p.ids().to_vec(), p.current().unwrap_or(0) as u32));
        self.queue_save(ids, index, position_ms, self.session.origin())
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Saves the queue as the server's play queue ([`nori_queue::Session::to_push`]), at `current` and `position_ms`.
    pub async fn playlist_push(&self, current: Option<String>, position_ms: i64) -> NetResult<()> {
        match self.core.session.push_write(current, position_ms) {
            Some(w) => self.write(w).await,
            None => Ok(()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use super::*;

    /// A core over its own queue, playing `ids` from `start`.
    pub(crate) fn core(ids: &[&str], start: u32) -> Arc<crate::Core> {
        let core = crate::Core::new(String::new(), "t".into(), Arc::default()).unwrap();
        core.session.set(ids.iter().map(|s| s.to_string()).collect(), Some(start), false, None);
        core
    }

    #[test]
    fn saved_queue() {
        {
            use crate::{OriginKind, PageOrigin, Song};
            let core = core(&["sv0"], 0);
            let s = &core.session;
            let song = |id: &str| Song { id: id.into(), ..Default::default() };
            s.register(vec![song("sv1"), song("sv2"), song("sv3")]);
            let from = PageOrigin::new(OriginKind::Playlist, "pl-7");
            s.set(vec!["sv1".into(), "sv2".into()], Some(1), false, Some(from.clone()));
            // Edits and the offline bridge keep the origin.
            s.take(9, vec!["sv3".into()], vec![Hand::Last]);
            s.edit_splice(|p| p.bridge(vec!["sv3".into()]), vec![]);
            s.unbridge();
            assert_eq!(s.origin(), Some(from.clone()));
            core.playlist_save(1500).unwrap();

            let q = core.load_queue().unwrap();
            assert_eq!((q.songs.len(), q.index, q.origin.as_ref()), (3, 1, Some(&from)));
            s.set(q.songs.iter().map(|s| s.id.clone()).collect(), Some(q.index), false, q.origin);
            assert!(s.from_page(&nori_library::pages::PageQueue::new(from)));

            // No origin, a pre-origin save, or an unknown kind: songs still restore.
            s.set(vec!["sv1".into()], Some(0), false, None);
            core.playlist_save(0).unwrap();
            assert_eq!(core.load_queue().unwrap().origin, None);
            for origin in ["", r#","origin":{"kind":"Nebula","id":"x"}"#] {
                let json = format!(r#"{{"songs":[{{"id":"sv1"}}],"index":0,"position":0{origin}}}"#);
                nori_db::kv_put(&core.db.lock(), "queue", &json).unwrap();
                let q = core.load_queue().unwrap();
                assert_eq!((q.songs.len(), q.origin), (1, None), "{origin}");
            }
        }

        // Save drops unsaveable songs.
        {
            let core = core(&["radio:1", "rk-unknown", "rk1", "rk2"], 2);
            core.session.register(vec![crate::Song { id: "rk1".into(), ..Default::default() }, crate::Song { id: "rk2".into(), ..Default::default() }]);
            core.playlist_save(0).unwrap();
            let q = core.load_queue().unwrap();
            assert_eq!((q.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), q.index), (vec!["rk1", "rk2"], 0), "rk1 stays current");
        }
    }

    #[test]
    fn songs_rewritten_only_when_the_queue_changes() {
        let core = core(&["radio:1", "w1", "w2", "w3"], 1);
        core.session.register(["w1", "w2", "w3"].map(|id| crate::Song { id: id.into(), ..Default::default() }).to_vec());
        let songs = || nori_db::kv_get(&core.db.lock(), "queue").unwrap();
        core.playlist_save(0).unwrap();
        let first = songs();
        core.session.moved_to(3);
        core.playlist_save(4200).unwrap();
        assert_eq!(songs(), first, "a song change keeps the stored songs");
        let q = core.load_queue().unwrap();
        assert_eq!((q.index, q.position_ms, q.songs.len()), (2, 4200, 3));

        core.session.set(vec!["w2".into(), "w1".into()], Some(0), false, None);
        core.playlist_save(10).unwrap();
        let q = core.load_queue().unwrap();
        assert_eq!((q.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), q.index, q.position_ms), (vec!["w2", "w1"], 0, 10));
    }

}
