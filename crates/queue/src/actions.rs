//! Play actions beyond handing a list to the player: taps, shuffles, radio, test refs and M3U import.

use nori_library::mixes;
use nori_model::Song;
use nori_settings::settings::TapAction;

use crate::autofill::seed_now;

/// Similar (or, as fallback, random) songs requested for a radio.
pub const RADIO: i32 = 50;
/// Songs in an instant mix.
pub const INSTANT_MIX: u32 = 50;
/// Songs "shuffle all" plays.
pub const SHUFFLE_ALL: i32 = 200;
/// Random albums requested by "shuffle albums"; the first library album plays.
pub const SHUFFLE_ALBUMS: i32 = 10;

/// What a tap on a song row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum TapPlan {
    /// Toggle selection (a selection is active).
    Select,
    /// The whole list from the tapped song.
    PlayList,
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

/// The tap action per settings; selecting while a selection is active.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn tap_plan(selecting: bool) -> TapPlan {
    tap(selecting, nori_settings::settings_store::with_prefs(|p| p.tap_action).unwrap_or(TapAction::PlayList))
}

/// How to shuffle a list.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ShufflePlan {
    Empty,
    /// The player's own random order.
    PlayerShuffle,
    /// Play in this order (positions in the list), artists and albums spread apart.
    Order { order: Vec<u32> },
}

/// Shuffles `songs` with artists and albums spread apart (`mixes::weighted_shuffle_order`).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn shuffle_plan(songs: Vec<Song>) -> ShufflePlan {
    plan_shuffle(songs, seed_now())
}

/// Two songs or fewer cannot be spread: the player's shuffle.
fn plan_shuffle(songs: Vec<Song>, seed: u64) -> ShufflePlan {
    if songs.is_empty() {
        ShufflePlan::Empty
    } else if songs.len() > 2 {
        ShufflePlan::Order { order: mixes::weighted_shuffle_order(&songs, seed).into_iter().map(|i| i as u32).collect() }
    } else {
        ShufflePlan::PlayerShuffle
    }
}

/// A radio: `seed`, then the library songs of `more` besides it. None when there are none.
pub fn radio_queue(seed: Song, more: Vec<Song>) -> Option<Vec<Song>> {
    let rest: Vec<Song> = more.into_iter().filter(|s| s.id != seed.id && !s.is_provider()).collect();
    if rest.is_empty() {
        return None;
    }
    Some(std::iter::once(seed).chain(rest).collect())
}

/// A debug test-bridge reference: `album:<id>`, `song:<id>`, `search:<text>`, `downloaded:<n>` (n-th newest
/// finished download) or `downloaded:<song id>`.
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

/// An M3U file matched against the index: found song ids in file order, and the file's entry count.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct M3uImport {
    pub song_ids: Vec<String>,
    pub entries: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn tap_follows_setting_unless_selecting() {
        let all = [TapAction::PlayList, TapAction::PlayOne, TapAction::Queue, TapAction::PlayNext];
        assert_eq!(all.map(|a| tap(false, a)), [TapPlan::PlayList, TapPlan::PlayOne, TapPlan::Queue, TapPlan::PlayNext]);
        assert_eq!(tap(true, TapAction::Queue), TapPlan::Select);
    }

    #[test]
    fn shuffle_plan_by_size() {
        let l: Vec<Song> = (0..6).map(|i| song(&i.to_string(), "t", &format!("A{}", i % 2), "b", "", 0)).collect();
        assert_eq!(plan_shuffle(vec![], 1), ShufflePlan::Empty);
        assert_eq!(plan_shuffle(l[..2].to_vec(), 1), ShufflePlan::PlayerShuffle);
        let ShufflePlan::Order { order } = plan_shuffle(l.clone(), 9) else { panic!("spread") };
        assert_eq!(order.iter().map(|&i| l[i as usize].clone()).collect::<Vec<_>>(), mixes::weighted_shuffle(l, 9));
    }

    #[test]
    fn radio_starts_with_seed_once() {
        let (a, b, c) = (song("a", "t", "x", "y", "", 0), song("b", "t", "x", "y", "", 0), song("c", "t", "x", "y", "", 0));
        let provider = song("ext-deezer-song-1", "t", "x", "y", "", 0);
        assert_eq!(radio_queue(a.clone(), vec![a.clone(), provider.clone()]), None);
        assert_eq!(radio_queue(a.clone(), vec![b.clone(), a.clone(), provider, c.clone()]).unwrap(), [a, b, c]);
    }

    #[test]
    fn test_ref_parses() {
        assert_eq!(test_ref("album:a:1".into()), TestRef::Album { id: "a:1".into() });
        assert_eq!(test_ref("search:dogs".into()), TestRef::Search { text: "dogs".into() });
        assert_eq!(test_ref("downloaded:x".into()), TestRef::DownloadedSong { id: "x".into() });
        assert_eq!(test_ref("downloaded:".into()), TestRef::Downloaded { index: 0 });
        assert_eq!(test_ref("downloaded:2".into()), TestRef::Downloaded { index: 2 });
        assert_eq!(test_ref("song".into()), TestRef::Nothing);
    }
}
