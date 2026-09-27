//! The songs of the queue, kept by id. The platform's player holds only ids (and what its system
//! notification shows); everything that needs to know about a queued song - the transition planner's
//! window, ReplayGain, the queue as the app lists it, the queue saved for next time - reads it here,
//! instead of rebuilding a song from the player's metadata on every event.

use std::collections::HashMap;

use nori_db as db;
use nori_model::Song;
use nori_player::gain::{song_gain, stereo_loudness_of_mid, GainMode as PlayerGainMode, GainPrefs, GainTags as PlayerGainTags, SongLoudness};
use nori_player::transitions::{in_album_run, WindowSong};
use parking_lot::Mutex;

/// A song not asked for again within this long, and no longer in the queue, may be let go.
const KEEP_MS: i64 = 60_000;

/// The queued songs by id, each with when it was last asked for.
pub struct Store {
    pub songs: HashMap<String, (Song, i64)>,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// The queued songs, lent to `f`.
pub fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    f(STORE.lock().get_or_insert_with(|| Store { songs: HashMap::new() }))
}

/// The id prefix of a radio stream in the queue.
pub const RADIO_PREFIX: &str = "radio:";

/// Songs about to be queued. One call per list.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_register(songs: Vec<Song>) {
    let now = db::now_ms();
    with(|s| {
        for song in songs {
            s.songs.insert(song.id.clone(), (song, now));
        }
    });
}

/// Each of `ids`' cover id, where the song is known and has one.
pub fn cover_arts(ids: &[String]) -> Vec<Option<String>> {
    with(|s| ids.iter().map(|id| s.songs.get(id).and_then(|(song, _)| song.cover_art.clone())).collect())
}

/// A queued song, if it is known.
pub fn queue_song(id: String) -> Option<Song> {
    with(|s| s.songs.get(&id).map(|(song, _)| song.clone()))
}

/// `ids`, in that order, as the store knows them. A song the store does not know (an item a system
/// controller added from outside) comes back with its id only. The store keeps these, the songs of the
/// queue and what was registered in the last minute, and lets the rest go.
///
/// The queue is kept whatever `ids` holds: a caller asking for a few songs (the perf build's timeline asks
/// for the one playing) once let every other queued song go a minute after it was queued, and the
/// planner's window then knew none of them - no length, so AutoMix and crossfades planned nothing until
/// the queue was loaded again.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_songs(ids: Vec<String>) -> Vec<Song> {
    songs_at(ids, db::now_ms())
}

fn songs_at(ids: Vec<String>, now: i64) -> Vec<Song> {
    // Read before the store is locked: the queue's lock is never taken inside the store's.
    let queued: std::collections::HashSet<String> = crate::playlist::with(|p| p.ids().iter().cloned().collect());
    with(|s| {
        let listed: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
        s.songs.retain(|id, (_, at)| listed.contains(id.as_str()) || queued.contains(id) || now - *at < KEEP_MS);
        ids.iter().map(|id| s.songs.get(id).map_or_else(|| Song::only_id(id.clone()), |(song, _)| song.clone())).collect()
    })
}

/// What the store knows of `id`, at a place in the queue whose album run is `run`
/// (`Playlist::album_run`).
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

/// Each of `ids` with its length in ms (0 when the song is not known), for the heard tracker.
pub(crate) fn durations(ids: &[String]) -> Vec<(String, i64)> {
    with(|s| ids.iter().map(|id| (id.clone(), s.songs.get(id).map_or(0, |(song, _)| song.duration as i64 * 1000))).collect())
}

/// The transition planner's window by id, each with its place's album run: the song before the current
/// one, then the current one and those after it, in play order.
pub fn queue_window(songs: &[(String, u32)], shuffling: bool) {
    let window = with(|s| songs.iter().map(|(id, run)| window_song(s, id, *run)).collect());
    nori_automix::planner::transition_window(window, shuffling);
}

/// The gain `current` plays at, between the songs before and after it in play order (album mode keeps
/// an album played in order at its own levels): over 1 it is turned up. See `nori_player::gain`.
/// Nothing playing, or a radio stream, plays at full volume. A song without a gain of its own, the
/// server's fallback included, plays at its measured loudness when AutoMix's analysis has one.
///
/// Each song comes with its place's album run (`Playlist::album_run`): album gain in auto mode is for an
/// album played as an album, as "keep albums gapless" is.
pub fn queue_gain(before: Option<(String, u32)>, current: Option<(String, u32)>, after: Option<(String, u32)>, prefs: &GainPrefs, bit_perfect: bool, shuffling: bool) -> f32 {
    let (current, current_run) = current.unwrap_or_else(|| (RADIO_PREFIX.to_string(), 0));
    let radio = current.starts_with(RADIO_PREFIX);
    let (run, mut song, channels) = with(|s| {
        let w = |p: &Option<(String, u32)>| p.as_ref().map(|(i, r)| window_song(s, i, *r));
        let (b, c, a) = (w(&before), window_song(s, &current, current_run), w(&after));
        let run = !radio && in_album_run(b.as_ref(), &c, a.as_ref(), shuffling);
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
    if untagged && prefs.measured && prefs.mode != PlayerGainMode::Off && !radio && !bit_perfect {
        // Read with the queue's lock let go: the database's is never taken inside it.
        song.measured_lufs = measured_lufs(&current).map(|mid| stereo_loudness_of_mid(mid, channels));
    }
    song_gain(prefs, &song, run, radio, bit_perfect)
}

/// The loudness AutoMix's analysis measured of `id` (of its mid signal, `TrackAnalysis::lufs`), if it did.
fn measured_lufs(id: &str) -> Option<f32> {
    let db = db::active()?;
    let a = nori_automix::store::get(&db.lock(), id).ok().flatten()?;
    Some(a.lufs)
}

/// [`queue_flags`]: the song is marked explicit.
pub const EXPLICIT: u32 = 1;
/// The song is starred (as the server said when it was queued).
pub const STARRED: u32 = 2;
/// The song is a provider's, not the library's (octo-fiesta).
pub const EXTERNAL: u32 = 4;

/// A queued song's flags ([`EXPLICIT`], [`STARRED`], [`EXTERNAL`]); 0 when it is not known.
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

/// The albums the queued songs `ids` come from (each once).
pub fn queue_albums(ids: Vec<String>) -> Vec<String> {
    with(|s| {
        let mut seen = std::collections::HashSet::new();
        ids.iter().filter_map(|id| s.songs.get(id)?.0.album_id.clone()).filter(|a| seen.insert(a.clone())).collect()
    })
}

/// Whether a queued id is a song AutoMix can measure: not a provider's song (octo-fiesta's `ext-`), not a
/// playlist's own entry (`pl-`) and not a radio stream. Measuring reads only what is already on the
/// device, but those never are and never will be.
pub fn analysable(id: &str) -> bool {
    !id.starts_with("ext-") && !id.starts_with("pl-") && !id.starts_with(RADIO_PREFIX)
}

/// Of the queued songs `ids`, those that can be fetched ahead: not a radio stream, not a provider's
/// song (fetching one makes the provider download it for the server).
pub fn queue_fetchable(ids: Vec<String>) -> Vec<String> {
    with(|s| {
        ids.into_iter()
            .filter(|id| !id.starts_with(RADIO_PREFIX) && !id.starts_with("ext-") && !s.songs.get(id).is_some_and(|(song, _)| song.is_external))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_songs_that_can_be_on_the_device_are_measured() {
        assert!(analysable("a1b2"));
        assert!(!analysable("ext-deezer-1"), "a provider's song");
        assert!(!analysable("pl-7"));
        assert!(!analysable("radio:3"));
    }

    #[test]
    fn asking_for_one_song_long_after_the_queue_was_made_keeps_the_rest_of_the_queue() {
        let _g = crate::playlist::tests::hold(&["keep1", "keep2", "keep3"], 0);
        let song = |id: &str| Song { duration: 200, ..Song::only_id(id.to_string()) };
        queue_register(vec![song("keep1"), song("keep2"), song("keep3"), song("gone")]);
        // Two minutes on, the perf build's timeline asks for the song playing alone.
        let later = db::now_ms() + 2 * KEEP_MS;
        assert_eq!(songs_at(vec!["keep1".into()], later)[0].duration, 200);
        for id in ["keep2", "keep3"] {
            assert_eq!(queue_song(id.into()).map(|s| s.duration), Some(200), "{id} is still queued and still known");
        }
        assert!(queue_song("gone".into()).is_none(), "a song no longer queued is let go");
    }
}
