//! Queued songs by id. The platform player holds only ids; everything needing song data (planner window,
//! ReplayGain, the queue view, the saved queue) reads it here.

use std::collections::HashMap;

use nori_db as db;
use nori_model::Song;
use nori_player::gain::{song_gain, stereo_loudness_of_mid, GainMode as PlayerGainMode, GainPrefs, GainTags as PlayerGainTags, SongLoudness};
use nori_player::transitions::{in_album_run, WindowSong};

use crate::Session;

/// How long a song outside the queue is kept after it was last registered.
const KEEP_MS: i64 = 60_000;

/// Songs by id, each with when it was registered.
#[derive(Default)]
pub struct Store {
    pub songs: HashMap<String, (Song, i64)>,
}

pub use nori_model::{analysable, is_radio};

/// The planner's and seek bar's view of `id` (as `song` says, when known) at a place with album run `run`.
pub fn window_song_of(song: Option<&Song>, id: &str, run: u32) -> WindowSong {
    match song {
        Some(song) => WindowSong {
            id: song.id.clone(),
            title: song.title.clone(),
            duration_ms: song.duration as i64 * 1000,
            album_id: song.album_id.clone(),
            disc: song.disc_number as i32,
            track: song.track as i32,
            tag_bpm: song.bpm as f32,
            radio: false,
            album_run: run,
        },
        None => WindowSong { id: id.to_string(), title: id.to_string(), radio: is_radio(id), ..Default::default() },
    }
}

fn window_song(s: &Store, id: &str, run: u32) -> WindowSong {
    window_song_of(s.songs.get(id).map(|(song, _)| song), id, run)
}

/// AutoMix's measured mid-signal loudness of `id` in `db`, if analysed.
fn measured_lufs(db: &nori_db::Profile, id: &str) -> Option<f32> {
    let db = db.get()?;
    let a = nori_automix::store::get(&db.lock(), id).ok().flatten()?;
    Some(a.lufs)
}

impl Session {
    /// Lends the store to `f`.
    pub fn store<R>(&self, f: impl FnOnce(&mut Store) -> R) -> R {
        f(&mut self.store.lock())
    }

    /// Registers songs about to be queued.
    pub fn register(&self, songs: Vec<Song>) {
        let now = db::now_ms();
        self.store(|s| {
            for song in songs {
                s.songs.insert(song.id.clone(), (song, now));
            }
        });
    }

    /// The cover art id of queued song `id`, if known.
    pub fn cover_art(&self, id: &str) -> Option<String> {
        self.store(|s| s.songs.get(id).and_then(|(song, _)| song.cover_art.clone()))
    }

    /// A registered song.
    pub fn song(&self, id: &str) -> Option<Song> {
        self.store(|s| s.songs.get(id).map(|(song, _)| song.clone()))
    }

    /// The songs for `ids` in order (unknown ones as id only). Prunes the store to `ids`, the queue, and
    /// songs registered within [`KEEP_MS`].
    pub fn songs(&self, ids: Vec<String>) -> Vec<Song> {
        self.songs_at(ids, db::now_ms())
    }

    fn songs_at(&self, ids: Vec<String>, now: i64) -> Vec<Song> {
        // The queue's ids are looked at in place (the playlist's lock, then the store's, as elsewhere).
        self.playlist(|p| {
            let kept: std::collections::HashSet<&str> = ids.iter().chain(p.ids()).map(String::as_str).collect();
            self.store(|s| {
                s.songs.retain(|id, (_, at)| kept.contains(id.as_str()) || now - *at < KEEP_MS);
                ids.iter().map(|id| s.songs.get(id).map_or_else(|| Song::only_id(id.clone()), |(song, _)| song.clone())).collect()
            })
        })
    }

    /// Each id with its length in ms (0 if unknown).
    pub(crate) fn durations(&self, ids: &[String]) -> Vec<(String, i64)> {
        self.store(|s| ids.iter().map(|id| (id.clone(), s.songs.get(id).map_or(0, |(song, _)| song.duration as i64 * 1000))).collect())
    }

    /// Hands the planner its window: (id, album run) for the previous, current and next songs in play order.
    pub(crate) fn hand_window(&self, songs: &[(String, u32)], shuffling: bool) {
        let window = self.store(|s| songs.iter().map(|(id, run)| window_song(s, id, *run)).collect());
        self.planner.transition_window(window, shuffling);
    }

    /// The ReplayGain volume for `current` given its neighbours (each with its album run; album gain
    /// applies only inside a run). 1.0 for nothing, radio or bit-perfect output. Untagged songs fall back
    /// to AutoMix's measured loudness.
    pub fn queue_gain(&self, before: Option<(String, u32)>, current: Option<(String, u32)>, after: Option<(String, u32)>, prefs: &GainPrefs, bit_perfect: bool, shuffling: bool) -> f32 {
        let Some((current, current_run)) = current.filter(|(id, _)| !is_radio(id)) else { return 1.0 };
        if bit_perfect {
            return 1.0;
        }
        let (run, mut song, channels) = self.store(|s| {
            let w = |p: &Option<(String, u32)>| p.as_ref().map(|(i, r)| window_song(s, i, *r));
            let (b, c, a) = (w(&before), window_song(s, &current, current_run), w(&after));
            let run = in_album_run(b.as_ref(), &c, a.as_ref(), shuffling);
            let known = s.songs.get(&current).map(|(song, _)| song);
            let rg = known.and_then(|song| song.replay_gain.as_ref());
            let tags = rg.map(|g| PlayerGainTags {
                track_gain: g.track_gain,
                album_gain: g.album_gain,
                track_peak: g.track_peak,
                album_peak: g.album_peak,
            });
            let song = SongLoudness { tags, fallback_db: rg.and_then(|g| g.fallback_gain), measured_lufs: None };
            (run, song, known.map_or(0, |s| s.channel_count))
        });
        let untagged = song.tags.is_none_or(|g| g.track_gain.is_none() && g.album_gain.is_none()) && song.fallback_db.is_none();
        if untagged && prefs.measured && prefs.mode != PlayerGainMode::Off {
            // Outside the store's lock: the database's is never taken inside it.
            song.measured_lufs = measured_lufs(&self.db, &current).map(|mid| stereo_loudness_of_mid(mid, channels));
        }
        song_gain(prefs, &song, run)
    }

    /// Whether registered song `id` is marked explicit; false if unknown.
    pub fn explicit(&self, id: &str) -> bool {
        self.store(|s| s.songs.get(id).is_some_and(|(song, _)| song.explicit_status == "explicit"))
    }

    /// The distinct album ids of `ids`.
    pub fn albums(&self, ids: Vec<String>) -> Vec<String> {
        self.store(|s| {
            let mut seen = std::collections::HashSet::new();
            ids.iter().filter_map(|id| s.songs.get(id)?.0.album_id.clone()).filter(|a| seen.insert(a.clone())).collect()
        })
    }

    /// The ids that may be prefetched: no radio, no provider songs (fetching one makes the server
    /// download it).
    pub fn fetchable(&self, ids: Vec<String>) -> Vec<String> {
        self.store(|s| ids.into_iter().filter(|id| analysable(id) && !s.songs.get(id).is_some_and(|(song, _)| song.is_provider())).collect())
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Session {
    /// [`Session::register`].
    pub fn queue_register(&self, songs: Vec<Song>) {
        self.register(songs)
    }

    /// [`Session::songs`].
    pub fn queue_songs(&self, ids: Vec<String>) -> Vec<Song> {
        self.songs(ids)
    }

    /// Whether registered song `id` is starred, as the server said when it was queued; false if unknown.
    pub fn queue_starred(&self, id: String) -> bool {
        self.store(|s| s.songs.get(&id).is_some_and(|(song, _)| song.starred))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_songs_kept() {
        assert!(analysable("a1b2"));
        assert!(!analysable("ext-deezer-1"));
        assert!(!analysable("pl-7"));
        assert!(!analysable("radio:3"));

        // Songs lookup keeps queued songs.
        let s = crate::playlist::tests::session(&["keep1", "keep2", "keep3"], 0);
        let song = |id: &str| Song { duration: 200, ..Song::only_id(id.to_string()) };
        s.register(vec![song("keep1"), song("keep2"), song("keep3"), song("gone")]);
        // Asking for one song keeps the rest of the queue.
        let later = db::now_ms() + 2 * KEEP_MS;
        assert_eq!(s.songs_at(vec!["keep1".into()], later)[0].duration, 200);
        for id in ["keep2", "keep3"] {
            assert_eq!(s.song(id).map(|s| s.duration), Some(200), "{id}");
        }
        assert!(s.song("gone").is_none());
    }

}
