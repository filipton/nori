//! What playing something means, where it takes more than handing a list to the player: what a tap on
//! a song does, shuffling, starting a radio or an instant mix from one song, playing a whole artist,
//! and making a playlist out of an M3U file. The requests these need are made here too, so a screen
//! asks once and gets the list to play.

use nori_library::mixes;
use nori_model::Song;
use nori_settings::settings::TapAction;

use crate::autofill::seed_now;

/// Songs the server is asked for when it finds nothing similar to a radio's song, and how many similar
/// ones it is asked for in the first place.
pub const RADIO: i32 = 50;
/// Songs in an instant mix drawn from the index.
pub const INSTANT_MIX: u32 = 50;
/// Songs "shuffle all" plays.
pub const SHUFFLE_ALL: i32 = 200;
/// Random albums "shuffle albums" asks for: the first of the library's own among them plays.
pub const SHUFFLE_ALBUMS: i32 = 10;

/// What a plain tap on a song in a list does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum TapPlan {
    /// Something is selected: the tap adds the song to the selection or takes it out.
    Select,
    /// The whole list, from the song tapped.
    PlayList,
    /// Only the song tapped.
    PlayOne,
    Queue,
    PlayNext,
}

fn tap(selecting: bool, tap_action: TapAction) -> TapPlan {
    match tap_action {
        _ if selecting => TapPlan::Select,
        TapAction::PlayOne => TapPlan::PlayOne,
        TapAction::Queue => TapPlan::Queue,
        TapAction::PlayNext => TapPlan::PlayNext,
        TapAction::PlayList => TapPlan::PlayList,
    }
}

/// What a tap on a song does, as the settings say; while songs are selected, a tap selects.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn tap_plan(selecting: bool) -> TapPlan {
    tap(selecting, nori_settings::settings_store::with_prefs(|p| p.tap_action).unwrap_or(TapAction::PlayList))
}

/// How to shuffle a list.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ShufflePlan {
    /// Nothing to play.
    Empty,
    /// The player's own random order.
    PlayerShuffle,
    /// The list's songs in this order (positions in it), played as it is: artists and albums spread apart.
    /// Positions, not songs: the caller holds the songs already.
    Order { order: Vec<u32> },
}

/// Spreads artists and albums apart, so a shuffle never plays the same artist or record twice in a row
/// when it can help it. It used to be a switch, which read as fighting "Carry on with" the same artist
/// right under it; the two answer different questions (the order of what was shuffled, and what comes
/// after the queue), and nobody wants two songs off one record back to back in a shuffle.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn shuffle_plan(songs: Vec<Song>) -> ShufflePlan {
    plan_shuffle(songs, seed_now())
}

/// Two songs cannot be spread, so they get the plain shuffle.
fn plan_shuffle(songs: Vec<Song>, seed: u64) -> ShufflePlan {
    if songs.is_empty() {
        ShufflePlan::Empty
    } else if songs.len() > 2 {
        ShufflePlan::Order { order: mixes::weighted_shuffle_order(&songs, seed).into_iter().map(|i| i as u32).collect() }
    } else {
        ShufflePlan::PlayerShuffle
    }
}

/// A radio started from `seed`: the song itself, then what the server finds similar to it. None when it
/// finds nothing but the song itself; the radio then goes on with random songs of the same genre
/// ([radio_fallback]).
pub fn radio_queue(seed: Song, similar: Vec<Song>) -> Option<Vec<Song>> {
    let rest: Vec<Song> = similar.into_iter().filter(|s| s.id != seed.id).collect();
    if rest.is_empty() {
        return None;
    }
    Some(std::iter::once(seed).chain(rest).collect())
}

/// The radio when nothing similar was found: the song, then the server's random songs as they came.
pub fn radio_fallback(seed: Song, random: Vec<Song>) -> Vec<Song> {
    std::iter::once(seed).chain(random).collect()
}

/// What the debug test bridge names: `album:<id>`, `song:<id>`, `search:<text>` (the first song found),
/// `downloaded:<n>` (the n-th finished download, newest first, from 0), `downloaded:<song id>` (that song,
/// if it is a finished download), or nothing it knows.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum TestRef {
    Album { id: String },
    Song { id: String },
    Search { text: String },
    Downloaded { index: u32 },
    DownloadedSong { id: String },
    Nothing,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn test_ref(text: String) -> TestRef {
    let Some((kind, arg)) = text.split_once(':') else { return TestRef::Nothing };
    let arg = arg.to_string();
    match kind {
        "album" => TestRef::Album { id: arg },
        "song" => TestRef::Song { id: arg },
        "search" => TestRef::Search { text: arg },
        "downloaded" => match arg.parse() {
            Ok(index) => TestRef::Downloaded { index },
            Err(_) if arg.is_empty() => TestRef::Downloaded { index: 0 },
            Err(_) => TestRef::DownloadedSong { id: arg },
        },
        _ => TestRef::Nothing,
    }
}

/// An M3U file matched against the index: the songs for the new playlist, in the file's order, and how
/// many entries the file had (the client says how many were found, or, with none, that none were).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct M3uImport {
    pub song_ids: Vec<String>,
    pub entries: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A song as nori-library's history tests make them.
    fn song(id: &str, title: &str, artist: &str, album: &str, genre: &str, year: u32) -> Song {
        Song {
            id: id.into(),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            artist_id: (!artist.is_empty()).then(|| format!("ar-{}", artist.to_lowercase())),
            album_id: (!album.is_empty()).then(|| format!("al-{}", album.to_lowercase())),
            cover_art: Some(format!("cv-{id}")),
            genre: (!genre.is_empty()).then(|| genre.to_string()),
            year,
            duration: 200,
            suffix: "flac".into(),
            ..Default::default()
        }
        .dressed()
    }

    #[test]
    fn a_tap_does_what_the_settings_say_unless_songs_are_selected() {
        let all = [TapAction::PlayList, TapAction::PlayOne, TapAction::Queue, TapAction::PlayNext];
        assert_eq!(all.map(|a| tap(false, a)), [TapPlan::PlayList, TapPlan::PlayOne, TapPlan::Queue, TapPlan::PlayNext]);
        assert_eq!(tap(true, TapAction::Queue), TapPlan::Select);
    }

    #[test]
    fn shuffle_spreads_only_what_can_be_spread() {
        let l: Vec<Song> = (0..6).map(|i| song(&i.to_string(), "t", &format!("A{}", i % 2), "b", "", 0)).collect();
        assert_eq!(plan_shuffle(vec![], 1), ShufflePlan::Empty);
        assert_eq!(plan_shuffle(l[..2].to_vec(), 1), ShufflePlan::PlayerShuffle);
        let ShufflePlan::Order { order } = plan_shuffle(l.clone(), 9) else { panic!("spread") };
        assert_eq!(order.iter().map(|&i| l[i as usize].clone()).collect::<Vec<_>>(), mixes::weighted_shuffle(l, 9));
    }

    #[test]
    fn radio_starts_with_its_song_once() {
        let (a, b, c) = (song("a", "t", "x", "y", "", 0), song("b", "t", "x", "y", "", 0), song("c", "t", "x", "y", "", 0));
        assert_eq!(radio_queue(a.clone(), vec![a.clone()]), None);
        assert_eq!(radio_queue(a.clone(), vec![b.clone(), a.clone(), c.clone()]).unwrap(), [a.clone(), b.clone(), c.clone()]);
        assert_eq!(radio_fallback(a.clone(), vec![a.clone(), b.clone()]), [a.clone(), a, b]);
    }

    #[test]
    fn test_refs() {
        assert_eq!(test_ref("album:a:1".into()), TestRef::Album { id: "a:1".into() });
        assert_eq!(test_ref("search:dogs".into()), TestRef::Search { text: "dogs".into() });
        assert_eq!(test_ref("downloaded:x".into()), TestRef::DownloadedSong { id: "x".into() });
        assert_eq!(test_ref("downloaded:".into()), TestRef::Downloaded { index: 0 });
        assert_eq!(test_ref("downloaded:2".into()), TestRef::Downloaded { index: 2 });
        assert_eq!(test_ref("song".into()), TestRef::Nothing);
    }
}
