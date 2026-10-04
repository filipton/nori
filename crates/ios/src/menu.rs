//! The song menu (`menus::song_menu`), what its lines do, playlists and the sleep timer. A song is named
//! by the list token and index a page kept it under; the playing song is kept as a one-song list first
//! (`nori_ios_keep_now`). Doors that reach the network block: the app calls them off the main thread.

use std::ffi::{c_char, CString};

use nori_core::cache_policy::{Page, Read};
use nori_core::client::{Starrable, Write};
use nori_core::menus::{row_swipe, sleep_choices, song_menu, RowSwipeAct, SongAction, SongDownload, SongMenuItem};
use nori_core::Song;
use nori_host::session::Session;
use serde_json::{json, Value};

use crate::pages::{keep_list, list, owned};
use crate::session::{c_text, with_session};

fn action_code(a: &SongAction) -> i32 {
    match a {
        SongAction::Favourite { .. } => 0,
        SongAction::PlayNext => 1,
        SongAction::AddToQueue => 2,
        SongAction::AddToPlaylist => 3,
        SongAction::RemoveDownload => 4,
        SongAction::StopDownload => 5,
        SongAction::Download => 6,
        SongAction::GoToAlbum { .. } => 7,
        SongAction::GoToArtist { .. } => 8,
        SongAction::AddToLibrary => 9,
        SongAction::SleepTimer => 10,
        SongAction::StartRadio => 11,
        SongAction::InstantMix => 12,
        SongAction::ExcludeFromMixes => 13,
        SongAction::Share => 14,
        SongAction::Details => 15,
    }
}

fn item_json(m: &SongMenuItem) -> Value {
    let mut v = json!({ "a": action_code(&m.action), "more": m.more });
    match &m.action {
        SongAction::Favourite { on } => v["on"] = json!(on),
        SongAction::GoToAlbum { id } => v["id"] = json!(id),
        SongAction::GoToArtist { id, name, named } => {
            v["id"] = json!(id);
            v["name"] = json!(name);
            v["named"] = json!(named);
        }
        _ => {}
    }
    v
}

/// What the details sheet shows, as Android's `Say.trackInfo` reads it; empty and zero fields are the
/// client's to leave out.
fn details_json(s: &Song) -> Value {
    let artists: Vec<&str> = s.artists.iter().map(|a| a.name.as_str()).collect();
    let gain = s.replay_gain.as_ref();
    json!({
        "title": s.title, "artist": s.artist, "artists": artists, "album": s.album, "year": s.year,
        "track": s.track, "disc": s.disc_number, "seconds": s.duration, "genre": s.genre,
        "suffix": s.suffix, "type": s.content_type, "kbps": s.bit_rate, "hz": s.sampling_rate,
        "bits": s.bit_depth, "bytes": s.size, "bpm": s.bpm, "plays": s.play_count,
        "trackGain": gain.and_then(|g| g.track_gain), "albumGain": gain.and_then(|g| g.album_gain),
        "peak": gain.and_then(|g| g.track_peak), "played": s.played, "added": s.created, "path": s.path,
        "mbid": s.music_brainz_id, "comment": s.comment, "id": s.id,
    })
}

fn menu_json(song: &Song, starred: bool, download: SongDownload, player: bool) -> Value {
    let items: Vec<Value> = song_menu(song.clone(), starred, download, player)
        .iter()
        .map(item_json)
        .collect();
    json!({ "items": items, "details": details_json(song) })
}

fn song_at(token: u64, index: i32) -> Option<Song> {
    let i = usize::try_from(index).ok()?;
    list(token)?.0.into_iter().nth(i)
}

fn download_of(s: &Session, id: &str) -> SongDownload {
    if s.core.download_song(id).is_some() {
        return SongDownload::Done;
    }
    let pending = s.core.download_sections().is_ok_and(|d| {
        d.active.iter().chain(d.queued.iter()).any(|song| song.id == id)
    });
    if pending {
        SongDownload::Pending
    } else {
        SongDownload::None
    }
}

/// Plays a radio (`radio`) or an instant mix seeded by `song`: the songs played, or -1 when the
/// server could not be asked.
fn play_mix(s: &Session, song: &Song, radio: bool) -> i32 {
    let id = song.id.clone();
    let asked = if radio {
        nori_core::transport::block_on(s.client.radio(id))
    } else {
        nori_core::transport::block_on(s.client.instant_mix(id))
    };
    match asked {
        Ok(songs) if songs.is_empty() => 0,
        Ok(songs) => {
            let n = i32::try_from(songs.len()).unwrap_or(i32::MAX);
            s.play(songs, 0, false, None);
            n
        }
        Err(_) => -1,
    }
}

fn wrote(s: &Session, w: Write) -> i32 {
    i32::from(nori_core::transport::block_on(s.client.write(w)).is_ok())
}

/// Keeps the playing song as a one-song list under `token`, for the doors below. 1 when one plays.
#[no_mangle]
pub extern "C" fn nori_ios_keep_now(token: u64) -> i32 {
    with_session(|s| {
        let id = s.engine.status().id?;
        let song = s.core.session.song(&id)?;
        keep_list(token, vec![song], None);
        Some(1)
    })
    .flatten()
    .unwrap_or(0)
}

/// Song `index` of list `token`'s menu as JSON to free: `{items: [{a, more, on?, id?, name?, named?}],
/// details}`. `starred` as shown; `player`: opened from the player (adds the sleep timer).
#[no_mangle]
pub extern "C" fn nori_ios_song_menu(token: u64, index: i32, starred: i32, player: i32) -> *mut c_char {
    let Some(song) = song_at(token, index) else {
        return std::ptr::null_mut();
    };
    with_session(|s| owned(&menu_json(&song, starred != 0, download_of(s, &song.id), player != 0)))
        .unwrap_or(std::ptr::null_mut())
}

/// Does menu line `action` (its `a`) for song `index` of list `token`; `on` for the heart. Lines the
/// client draws itself (album, artist, playlist, sleep timer, share, details) do nothing here. Radio
/// and instant mix block: the songs played, -1 when the server could not be asked. Otherwise 1 done.
#[no_mangle]
pub extern "C" fn nori_ios_song_act(token: u64, index: i32, action: i32, on: i32) -> i32 {
    let Some(song) = song_at(token, index) else { return 0 };
    with_session(|s| match action {
        0 => {
            s.star(Starrable::Song, song.id.clone(), on != 0);
            1
        }
        1 | 2 => {
            s.enqueue(vec![song.clone()], action == 1, None);
            1
        }
        4 | 5 => {
            s.download_remove(&song.id);
            1
        }
        6 => {
            s.download(vec![song.clone()]);
            1
        }
        9 => {
            s.star(Starrable::Song, song.id.clone(), true);
            1
        }
        11 | 12 => play_mix(s, &song, action == 11),
        13 => i32::from(s.core.mix_excluded_set(song.id.clone(), true).is_ok()),
        _ => 0,
    })
    .unwrap_or(0)
}

/// A link to song `index` of list `token` from the server, to free; NULL when it could not be made.
/// Blocks.
#[no_mangle]
pub extern "C" fn nori_ios_song_share(token: u64, index: i32) -> *mut c_char {
    let Some(song) = song_at(token, index) else {
        return std::ptr::null_mut();
    };
    with_session(|s| match nori_core::transport::block_on(s.client.read_now(Read::ShareLink { id: song.id })) {
        Ok(Page::ShareUrl { v }) => CString::new(v).map_or(std::ptr::null_mut(), CString::into_raw),
        _ => std::ptr::null_mut(),
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Adds song `index` of list `token` to playlist `playlist`. Blocks; 1 when the server took it (or it
/// waits for the server, offline).
///
/// # Safety
/// `playlist` is NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_playlist_add(token: u64, index: i32, playlist: *const c_char) -> i32 {
    let Some(song) = song_at(token, index) else { return 0 };
    let id = c_text(playlist);
    with_session(|s| wrote(s, Write::AddToPlaylist { id, song_ids: vec![song.id] })).unwrap_or(0)
}

/// Makes playlist `name` holding song `index` of list `token`. Blocks; 1 when done.
///
/// # Safety
/// `name` is NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_playlist_create(token: u64, index: i32, name: *const c_char) -> i32 {
    let Some(song) = song_at(token, index) else { return 0 };
    let name = c_text(name);
    if name.trim().is_empty() {
        return 0;
    }
    with_session(|s| wrote(s, Write::CreatePlaylist { name: name.trim().to_string(), song_ids: vec![song.id] }))
        .unwrap_or(0)
}

/// What swiping a song row does, to the left when `left` and else to the right, on a song whose heart is
/// `starred`: -1 nothing, else a `NORI_SWIPE_*` code.
#[no_mangle]
pub extern "C" fn nori_ios_row_swipe(left: i32, starred: i32) -> i32 {
    let setting = with_session(|s| s.core.session.settings.prefs(|p| if left != 0 { p.swipe_left } else { p.swipe_right }));
    match setting.and_then(|s| row_swipe(s, starred != 0)) {
        None => -1,
        Some(RowSwipeAct::Queue) => 0,
        Some(RowSwipeAct::PlayNext) => 1,
        Some(RowSwipeAct::Favourite { on: true }) => 2,
        Some(RowSwipeAct::Favourite { on: false }) => 3,
        Some(RowSwipeAct::Download) => 4,
    }
}

/// Takes entry `index` out of playlist `playlist`. Blocks; 1 when done.
///
/// # Safety
/// `playlist` is NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_playlist_remove(playlist: *const c_char, index: i32) -> i32 {
    if index < 0 {
        return 0;
    }
    let id = c_text(playlist);
    with_session(|s| wrote(s, Write::RemoveFromPlaylist { id, index })).unwrap_or(0)
}

/// Deletes playlist `playlist`. Blocks; 1 when done.
///
/// # Safety
/// `playlist` is NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_playlist_delete(playlist: *const c_char) -> i32 {
    let id = c_text(playlist);
    with_session(|s| wrote(s, Write::DeletePlaylist { id })).unwrap_or(0)
}

/// The sleep timer's choices as JSON to free: `[{minutes, end, songs}]`, all zeros for "Off" (first,
/// while one is `running`).
#[no_mangle]
pub extern "C" fn nori_ios_sleep_choices(running: i32) -> *mut c_char {
    let rows: Vec<Value> = sleep_choices(running != 0)
        .iter()
        .map(|c| json!({ "minutes": c.minutes, "end": c.end_of_track, "songs": c.songs }))
        .collect();
    owned(&Value::Array(rows))
}

/// Pauses after `songs` songs or at the end of this one; 0 and 0 cancel that kind of timer.
#[no_mangle]
pub extern "C" fn nori_ios_sleep_set(songs: u32, end_of_track: i32) {
    with_session(|s| {
        let pause = s.core.session.sleep_set(songs, end_of_track != 0);
        s.engine.pause_at_end(pause);
    });
}

/// A timer of `minutes`: `[delay ms, slack ms]` as JSON to free, for the client's one-shot timer.
#[no_mangle]
pub extern "C" fn nori_ios_sleep_delay(minutes: u32) -> *mut c_char {
    owned(&json!(nori_core::rules::sleep_delay(minutes)))
}

/// The client's sleep timer ran out: playback pauses.
#[no_mangle]
pub extern "C" fn nori_ios_sleep_now() {
    with_session(|s| s.engine.pause());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> Song {
        Song {
            id: "s1".into(),
            title: "Blue".into(),
            album_id: Some("al".into()),
            artist_id: Some("ar".into()),
            artist: "Joni".into(),
            ..Song::default()
        }
    }

    fn codes(v: &Value) -> Vec<i64> {
        v["items"].as_array().unwrap().iter().map(|i| i["a"].as_i64().unwrap()).collect()
    }

    #[test]
    fn the_menu_carries_each_line_by_code_with_what_it_needs() {
        let v = menu_json(&song(), true, SongDownload::Done, false);
        let items = v["items"].as_array().unwrap();
        assert_eq!((items[0]["a"].as_i64(), items[0]["on"].as_bool()), (Some(0), Some(false)));
        assert!(codes(&v).contains(&4), "a downloaded song offers removing it: {v}");
        let album = items.iter().find(|i| i["a"] == 7).unwrap();
        assert_eq!(album["id"], "al");
        let artist = items.iter().find(|i| i["a"] == 8).unwrap();
        assert_eq!((artist["id"].as_str(), artist["named"].as_bool()), (Some("ar"), Some(false)));
        assert!(!codes(&v).contains(&10), "the sleep timer is the player's");
        assert!(codes(&menu_json(&song(), false, SongDownload::Pending, true)).contains(&10));
        assert!(codes(&menu_json(&song(), false, SongDownload::Pending, true)).contains(&5));
        assert_eq!(v["details"]["title"], "Blue");
    }

    #[test]
    fn every_action_has_its_own_code() {
        let actions = [
            SongAction::Favourite { on: true },
            SongAction::PlayNext,
            SongAction::AddToQueue,
            SongAction::AddToPlaylist,
            SongAction::RemoveDownload,
            SongAction::StopDownload,
            SongAction::Download,
            SongAction::GoToAlbum { id: String::new() },
            SongAction::GoToArtist { id: String::new(), name: String::new(), named: false },
            SongAction::AddToLibrary,
            SongAction::SleepTimer,
            SongAction::StartRadio,
            SongAction::InstantMix,
            SongAction::ExcludeFromMixes,
            SongAction::Share,
            SongAction::Details,
        ];
        let mut seen: Vec<i32> = actions.iter().map(action_code).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), actions.len());
    }
}
