//! The queue as the core's and the client's calls: a song's credits read from the server, the queue saved
//! there. The queue itself is nori-queue's.

use crate::client::{Client, NetResult};
use crate::Core;

pub use nori_queue::playlist::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Saves the queue for next time (radio streams are left out: they do not come back), and the page
    /// it was started from, so the queue put back still lights that page.
    pub fn playlist_save(&self, position_ms: u64) -> crate::Result<()> {
        let (ids, runs, index) = with(|p| (p.ids().to_vec(), p.album_runs().to_vec(), p.current().unwrap_or(0) as u32));
        self.queue_save(ids, runs, index, position_ms, playlist_origin())
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Hands the server the queue as the core keeps it ([`playlist_to_push`]: radio left out, only while
    /// plays may be sent at all), `current` playing `position_ms` in. The ids stay in the core: this used
    /// to be two calls, the whole list crossing out and back.
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

    /// The one queue is the process's: tests that use it take turns (as nori-queue's own tests do).
    static TURN: Mutex<()> = Mutex::new(());

    pub(crate) fn hold(ids: &[&str], start: i32) -> parking_lot::MutexGuard<'static, ()> {
        let g = TURN.lock();
        playlist_set(ids.iter().map(|s| s.to_string()).collect(), start, false, None);
        playlist_repeat(0);
        g
    }

    #[test]
    fn the_queue_is_saved_and_put_back_with_the_page_it_came_from() {
        use crate::{OriginKind, PageOrigin, Song};
        let core = crate::Core::new(String::new(), "t".into()).unwrap();
        let song = |id: &str| Song { id: id.into(), ..Default::default() };
        crate::queue::queue_register(vec![song("sv1"), song("sv2"), song("sv3")]);
        let _g = hold(&["sv0"], 0);
        let from = PageOrigin::new(OriginKind::Playlist, "pl-7");
        playlist_set(vec!["sv1".into(), "sv2".into()], 1, false, Some(from.clone()));
        // An edit, and the offline bridge coming and going, keep it.
        playlist_take(9, vec!["sv3".into()], vec![Hand::Last], None);
        edit_splice(|p| p.bridge(vec!["sv3".into()]), vec![]);
        playlist_unbridge();
        assert_eq!(playlist_origin(), Some(from.clone()));
        core.playlist_save(1500).unwrap();

        let q = core.load_queue().unwrap();
        assert_eq!((q.songs.len(), q.index, q.origin.as_ref()), (3, 1, Some(&from)));
        // Put back the way a client does, the page lights again.
        playlist_set(q.songs.iter().map(|s| s.id.clone()).collect(), q.index as i32, false, q.origin);
        assert!(playlist_from(nori_library::pages::PageQueue::new(from)));

        // Which songs are an album played as one is saved with them: the album's, not the song added on
        // its own after it, whatever the page.
        crate::queue::queue_register(vec![song("sv4"), song("sv5")]);
        playlist_set(vec!["sv4".into(), "sv5".into()], 0, false, Some(PageOrigin::new(OriginKind::Album, "al-1")));
        playlist_take(9, vec!["sv1".into()], vec![Hand::Last], None);
        let runs = with(|p| p.album_runs().to_vec());
        assert_eq!(runs[1], 0);
        core.playlist_save(0).unwrap();
        playlist_set(vec!["sv0".into()], 0, false, None);
        let q = core.load_queue().unwrap();
        playlist_set(q.songs.iter().map(|s| s.id.clone()).collect(), q.index as i32, false, q.origin);
        assert_eq!(with(|p| p.album_runs().to_vec()), runs);

        // A queue from no page is saved as none; a save from before origins, or with a kind this version
        // does not know, still puts the songs back.
        playlist_set(vec!["sv1".into()], 0, false, None);
        core.playlist_save(0).unwrap();
        assert_eq!(core.load_queue().unwrap().origin, None);
        for origin in ["", r#","origin":{"kind":"Nebula","id":"x"}"#] {
            let json = format!(r#"{{"songs":[{{"id":"sv1"}}],"index":0,"position":0{origin}}}"#);
            nori_db::kv_put(&core.db.lock(), "queue", &json).unwrap();
            let q = core.load_queue().unwrap();
            assert_eq!((q.songs.len(), q.origin), (1, None), "{origin}");
        }
    }
}
