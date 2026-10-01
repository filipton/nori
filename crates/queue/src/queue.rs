//! Queued songs by id. The platform player holds only ids; everything needing song data (planner window,
//! ReplayGain, the queue view, the saved queue) reads it here.

use std::collections::HashMap;

use nori_db as db;
use nori_model::Song;
use nori_player::gain::{song_gain, stereo_loudness_of_mid, GainMode as PlayerGainMode, GainPrefs, GainTags as PlayerGainTags, SongLoudness};
use nori_player::transitions::{in_album_run, WindowSong};
use parking_lot::Mutex;

/// How long a song outside the queue is kept after it was last registered.
const KEEP_MS: i64 = 60_000;

/// Songs by id, each with when it was registered.
pub struct Store {
    pub songs: HashMap<String, (Song, i64)>,
}

// Global: the uniffi entry points have no handle. Taken inside the queue's lock, never around it.
static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// Lends the store to `f`.
pub fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    f(STORE.lock().get_or_insert_with(|| Store { songs: HashMap::new() }))
}

/// Id prefix of a radio stream.
pub const RADIO_PREFIX: &str = "radio:";

/// Registers songs about to be queued.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_register(songs: Vec<Song>) {
    let now = db::now_ms();
    with(|s| {
        for song in songs {
            s.songs.insert(song.id.clone(), (song, now));
        }
    });
}

/// The cover art id of queued song `id`, if known.
pub fn cover_art(id: &str) -> Option<String> {
    with(|s| s.songs.get(id).and_then(|(song, _)| song.cover_art.clone()))
}

/// A registered song.
pub fn queue_song(id: String) -> Option<Song> {
    with(|s| s.songs.get(&id).map(|(song, _)| song.clone()))
}

/// The songs for `ids` in order (unknown ones as id only). Prunes the store to `ids`, the queue, and
/// songs registered within [`KEEP_MS`].
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_songs(ids: Vec<String>) -> Vec<Song> {
    songs_at(ids, db::now_ms())
}

fn songs_at(ids: Vec<String>, now: i64) -> Vec<Song> {
    // The queue's ids are looked at in place (the playlist's lock, then the store's, as elsewhere).
    crate::playlist::with(|p| {
        let kept: std::collections::HashSet<&str> = ids.iter().chain(p.ids()).map(String::as_str).collect();
        with(|s| {
            s.songs.retain(|id, (_, at)| kept.contains(id.as_str()) || now - *at < KEEP_MS);
            ids.iter().map(|id| s.songs.get(id).map_or_else(|| Song::only_id(id.clone()), |(song, _)| song.clone())).collect()
        })
    })
}

/// The planner's view of `id` at a place with album run `run`.
fn window_song(s: &Store, id: &str, run: u32) -> WindowSong {
    match s.songs.get(id) {
        Some((song, _)) => WindowSong {
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
        None => WindowSong { id: id.to_string(), radio: id.starts_with(RADIO_PREFIX), ..Default::default() },
    }
}

/// Each id with its length in ms (0 if unknown).
pub(crate) fn durations(ids: &[String]) -> Vec<(String, i64)> {
    with(|s| ids.iter().map(|id| (id.clone(), s.songs.get(id).map_or(0, |(song, _)| song.duration as i64 * 1000))).collect())
}

/// Hands the planner its window: (id, album run) for the previous, current and next songs in play order.
pub(crate) fn queue_window(songs: &[(String, u32)], shuffling: bool) {
    let window = with(|s| songs.iter().map(|(id, run)| window_song(s, id, *run)).collect());
    nori_automix::planner::transition_window(window, shuffling);
}

/// The ReplayGain volume for `current` given its neighbours (each with its album run; album gain applies
/// only inside a run). 1.0 for nothing, radio or bit-perfect output. Untagged songs fall back to
/// AutoMix's measured loudness.
pub(crate) fn queue_gain(before: Option<(String, u32)>, current: Option<(String, u32)>, after: Option<(String, u32)>, prefs: &GainPrefs, bit_perfect: bool, shuffling: bool) -> f32 {
    let Some((current, current_run)) = current.filter(|(id, _)| !id.starts_with(RADIO_PREFIX)) else { return 1.0 };
    if bit_perfect {
        return 1.0;
    }
    let (run, mut song, channels) = with(|s| {
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
        song.measured_lufs = measured_lufs(&current).map(|mid| stereo_loudness_of_mid(mid, channels));
    }
    song_gain(prefs, &song, run)
}

/// AutoMix's measured mid-signal loudness of `id`, if analysed.
fn measured_lufs(id: &str) -> Option<f32> {
    let db = db::active()?;
    let a = nori_automix::store::get(&db.lock(), id).ok().flatten()?;
    Some(a.lufs)
}

/// [`queue_flags`] bits.
pub(crate) const EXPLICIT: u32 = 1;
pub(crate) const STARRED: u32 = 2;
pub(crate) const EXTERNAL: u32 = 4;

/// A registered song's flags ([`EXPLICIT`], [`STARRED`], [`EXTERNAL`]); 0 if unknown.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_flags(id: String) -> u32 {
    with(|s| {
        s.songs.get(&id).map_or(0, |(song, _)| {
            (if song.explicit_status == "explicit" { EXPLICIT } else { 0 })
                | (if song.starred { STARRED } else { 0 })
                | (if song.is_external { EXTERNAL } else { 0 })
        })
    })
}

/// The distinct album ids of `ids`.
pub fn queue_albums(ids: Vec<String>) -> Vec<String> {
    with(|s| {
        let mut seen = std::collections::HashSet::new();
        ids.iter().filter_map(|id| s.songs.get(id)?.0.album_id.clone()).filter(|a| seen.insert(a.clone())).collect()
    })
}

/// Whether AutoMix can analyse `id`: not a provider song or radio stream.
pub fn analysable(id: &str) -> bool {
    !nori_model::is_provider_id(id) && !id.starts_with(RADIO_PREFIX)
}

/// The ids that may be prefetched: no radio, no provider songs (fetching one makes the server download it).
pub fn queue_fetchable(ids: Vec<String>) -> Vec<String> {
    with(|s| {
        ids.into_iter()
            .filter(|id| analysable(id) && !s.songs.get(id).is_some_and(|(song, _)| song.is_provider()))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysable_excludes_providers_playlists_radio() {
        assert!(analysable("a1b2"));
        assert!(!analysable("ext-deezer-1"));
        assert!(!analysable("pl-7"));
        assert!(!analysable("radio:3"));
    }

    #[test]
    fn songs_lookup_keeps_queued_songs() {
        let _g = crate::playlist::tests::hold(&["keep1", "keep2", "keep3"], 0);
        let song = |id: &str| Song { duration: 200, ..Song::only_id(id.to_string()) };
        queue_register(vec![song("keep1"), song("keep2"), song("keep3"), song("gone")]);
        // Asking for one song keeps the rest of the queue.
        let later = db::now_ms() + 2 * KEEP_MS;
        assert_eq!(songs_at(vec!["keep1".into()], later)[0].duration, 200);
        for id in ["keep2", "keep3"] {
            assert_eq!(queue_song(id.into()).map(|s| s.duration), Some(200), "{id}");
        }
        assert!(queue_song("gone".into()).is_none());
    }
}
