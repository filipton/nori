//! What a song's menu offers and in what order, what the sleep timer offers, and what a page's
//! download entry does: the screens draw these lists as they come, one icon and their own words per
//! action, and do what the action names. Made once each time a menu opens.

use nori_model::{DownloadPhase, Song};
use nori_settings::settings::SwipeAction;

/// Something the song menu can do. The client draws one icon per kind, words it and does what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SongAction {
    /// The heart; `on` is what pressing it sets.
    Favourite { on: bool },
    PlayNext,
    AddToQueue,
    AddToPlaylist,
    RemoveDownload,
    StopDownload,
    Download,
    GoToAlbum { id: String },
    /// `name` is the artist's, for the page to show before it has loaded. `named` is set on a song by
    /// several artists, where each gets a line of its own and the line names it ("Go to A" rather than
    /// "Go to artist").
    GoToArtist { id: String, name: String, named: bool },
    /// A provider's song: octo-fiesta fetches it into the library when it is starred.
    AddToLibrary,
    SleepTimer,
    StartRadio,
    InstantMix,
    ExcludeFromMixes,
    Share,
    Details,
}

/// One line of the song menu.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SongMenuItem {
    pub action: SongAction,
    /// Under "More": what one reaches for perhaps once a month.
    pub more: bool,
}

/// Where a song's download stands, as its menu needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SongDownload {
    None,
    /// Waiting or on its way.
    Pending,
    Done,
}

/// The menu of `song`. `starred` is the heart as the screen shows it (this session's mark included);
/// `player` is true when the player's own ⋯ opened it, which adds the sleep timer.
///
/// What someone opens a menu for comes first: the heart (the one thing here about the song rather than
/// the queue), queueing it, keeping it, going where it came from. A provider's song (not in the library)
/// has no mix to seed or exclude from and no link to share: those need it on the server.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn song_menu(song: Song, starred: bool, download: SongDownload, player: bool) -> Vec<SongMenuItem> {
    let mut out = Vec::with_capacity(16);
    let mut add = |action: SongAction, more: bool| out.push(SongMenuItem { action, more });
    add(SongAction::Favourite { on: !starred }, false);
    add(SongAction::PlayNext, false);
    add(SongAction::AddToQueue, false);
    add(SongAction::AddToPlaylist, false);
    match download {
        SongDownload::Done => add(SongAction::RemoveDownload, false),
        SongDownload::Pending => add(SongAction::StopDownload, false),
        SongDownload::None => add(SongAction::Download, false),
    }
    if let Some(id) = &song.album_id {
        add(SongAction::GoToAlbum { id: id.clone() }, false);
    }
    // A song by several artists names each one it can go to; one by a single artist says "artist".
    if song.artists.len() > 1 {
        for a in song.artists.iter().filter(|a| !a.id.is_empty()) {
            add(SongAction::GoToArtist { id: a.id.clone(), name: a.name.clone(), named: true }, false);
        }
    } else if let Some(id) = &song.artist_id {
        add(SongAction::GoToArtist { id: id.clone(), name: song.artist.clone(), named: false }, false);
    }
    if song.is_external {
        add(SongAction::AddToLibrary, false);
    }
    if player {
        add(SongAction::SleepTimer, false);
    }
    add(SongAction::StartRadio, true);
    if !song.is_external {
        add(SongAction::InstantMix, true);
        add(SongAction::ExcludeFromMixes, true);
        add(SongAction::Share, true);
    }
    add(SongAction::Details, true);
    out
}

/// One choice of the sleep timer; the client words it from its numbers ("30 minutes", "End of track",
/// "After 3 songs"; "Off" is all zeros).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SleepChoice {
    /// Minutes from now; 0 for the choices that are not a time.
    pub minutes: u32,
    pub end_of_track: bool,
    /// After this many songs; 0 for the rest.
    pub songs: u32,
}

/// The sleep timer's choices, "Off" first while one is running (it is a choice of all zeros).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn sleep_choices(running: bool) -> Vec<SleepChoice> {
    let c = |minutes, end_of_track, songs| SleepChoice { minutes, end_of_track, songs };
    let mut out = Vec::with_capacity(10);
    if running {
        out.push(c(0, false, 0));
    }
    out.extend([15, 30, 45, 60].map(|m| c(m, false, 0)));
    out.push(c(0, true, 0));
    out.extend([2, 3, 5, 10].map(|n| c(0, false, n)));
    out
}

/// What a sideways swipe on a song row does, and what the client says under the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum RowSwipeAct {
    Queue,
    PlayNext,
    /// The heart; `on` is what letting go sets.
    Favourite { on: bool },
    Download,
}

/// What the swipe set in the settings does on a song whose heart is `starred`; None when that side does
/// nothing.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn row_swipe(setting: SwipeAction, starred: bool) -> Option<RowSwipeAct> {
    match setting {
        SwipeAction::None => None,
        SwipeAction::Queue => Some(RowSwipeAct::Queue),
        SwipeAction::PlayNext => Some(RowSwipeAct::PlayNext),
        SwipeAction::Favourite => Some(RowSwipeAct::Favourite { on: !starred }),
        SwipeAction::Download => Some(RowSwipeAct::Download),
    }
}

/// The mark a song row shows for its download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum DownloadGlyph {
    None,
    /// Waiting or arriving: waiting and downloading share the ring, so one flows into the other.
    Ring,
    Done,
    Failed,
}

/// A row's download mark: this session's phase for the song when it has one ([`DownloadPhase`]), else
/// whether it is downloaded or in the queue.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_glyph(phase: Option<DownloadPhase>, downloaded: bool, pending: bool) -> DownloadGlyph {
    match phase {
        Some(DownloadPhase::Done | DownloadPhase::FindingLyrics | DownloadPhase::Analysing | DownloadPhase::DetectingBeats) => DownloadGlyph::Done,
        Some(DownloadPhase::Failed) => DownloadGlyph::Failed,
        Some(DownloadPhase::Queued | DownloadPhase::Downloading) => DownloadGlyph::Ring,
        None if downloaded => DownloadGlyph::Done,
        None if pending => DownloadGlyph::Ring,
        None => DownloadGlyph::None,
    }
}

/// What a page's download entry does; the client words it ("Download", "Remove downloads", "Download
/// the other 3" with the count of songs missing it already has).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum DownloadAct {
    /// Download every song of the page.
    All,
    /// Download the songs that are not here yet.
    Missing,
    /// Everything is here: give the space back.
    Remove,
}

/// A page's download entry, for `songs` songs of which `missing` are not downloaded: "Download" is the
/// wrong word once they are all here, and so is offering all of them when only a few are missing.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_entry(songs: u32, missing: u32) -> DownloadAct {
    match (songs, missing) {
        (0, _) => DownloadAct::All,
        (_, 0) => DownloadAct::Remove,
        (s, m) if s == m => DownloadAct::All,
        _ => DownloadAct::Missing,
    }
}

/// The songs of a page not downloaded yet, as positions in `songs` in order: what a [`download_entry`]
/// of [`DownloadAct::Missing`] fetches. `done` answers for one song id.
///
/// Twin of the `missing` list in `downloadEntry` (app/.../ui/DetailScreens.kt), which Android keeps.
pub fn download_missing<'a>(songs: impl IntoIterator<Item = &'a str>, done: impl Fn(&str) -> bool) -> Vec<usize> {
    songs.into_iter().enumerate().filter(|(_, id)| !done(id)).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_model::model::ArtistRef;

    fn actions(m: &[SongMenuItem]) -> Vec<(SongAction, bool)> {
        m.iter().map(|i| (i.action.clone(), i.more)).collect()
    }

    #[test]
    fn a_library_song_offers_everything() {
        let s = Song { id: "1".into(), album_id: Some("al".into()), artist_id: Some("ar".into()), artist: "Björk".into(), ..Default::default() };
        let m = song_menu(s, false, SongDownload::None, false);
        use SongAction::*;
        assert_eq!(
            actions(&m),
            [
                (Favourite { on: true }, false), (PlayNext, false), (AddToQueue, false), (AddToPlaylist, false), (Download, false),
                (GoToAlbum { id: "al".into() }, false), (GoToArtist { id: "ar".into(), name: "Björk".into(), named: false }, false),
                (StartRadio, true), (InstantMix, true), (ExcludeFromMixes, true), (Share, true), (Details, true),
            ]
        );
    }

    #[test]
    fn a_providers_song_is_offered_to_the_library_and_nothing_that_needs_it_there() {
        let s = Song {
            id: "ext-deezer-song-9".into(),
            is_external: true,
            artists: vec![ArtistRef { id: "a".into(), name: "A".into() }, ArtistRef { id: String::new(), name: "Nobody".into() }, ArtistRef { id: "b".into(), name: "B".into() }],
            ..Default::default()
        };
        let m = song_menu(s, true, SongDownload::Pending, true);
        use SongAction::*;
        assert_eq!(
            actions(&m),
            [
                (Favourite { on: false }, false), (PlayNext, false), (AddToQueue, false), (AddToPlaylist, false), (StopDownload, false),
                (GoToArtist { id: "a".into(), name: "A".into(), named: true }, false), (GoToArtist { id: "b".into(), name: "B".into(), named: true }, false),
                (AddToLibrary, false), (SleepTimer, false),
                (StartRadio, true), (Details, true),
            ]
        );
        let done = song_menu(Song::default(), false, SongDownload::Done, false);
        assert_eq!(done[4].action, SongAction::RemoveDownload);
    }

    #[test]
    fn sleep_offers_off_only_while_running() {
        let idle = sleep_choices(false);
        let c = |minutes, end_of_track, songs| SleepChoice { minutes, end_of_track, songs };
        assert_eq!(idle, [c(15, false, 0), c(30, false, 0), c(45, false, 0), c(60, false, 0), c(0, true, 0), c(0, false, 2), c(0, false, 3), c(0, false, 5), c(0, false, 10)]);
        assert_eq!(sleep_choices(true)[0], c(0, false, 0));
    }

    #[test]
    fn a_row_swipe_says_what_letting_go_does() {
        assert_eq!(row_swipe(SwipeAction::None, false), None);
        assert_eq!(row_swipe(SwipeAction::Queue, false), Some(RowSwipeAct::Queue));
        assert_eq!(row_swipe(SwipeAction::PlayNext, true), Some(RowSwipeAct::PlayNext));
        assert_eq!(row_swipe(SwipeAction::Favourite, true), Some(RowSwipeAct::Favourite { on: false }));
        assert_eq!(row_swipe(SwipeAction::Favourite, false), Some(RowSwipeAct::Favourite { on: true }));
        assert_eq!(row_swipe(SwipeAction::Download, false), Some(RowSwipeAct::Download));
    }

    #[test]
    fn a_rows_download_mark() {
        use DownloadPhase as P;
        assert_eq!(download_glyph(Some(P::Done), false, false), DownloadGlyph::Done);
        assert_eq!(download_glyph(Some(P::Failed), true, false), DownloadGlyph::Failed, "this session's phase wins");
        assert_eq!((download_glyph(Some(P::Queued), false, false), download_glyph(Some(P::Downloading), false, false)), (DownloadGlyph::Ring, DownloadGlyph::Ring));
        // Saved and still processing, whichever step: downloaded already.
        for p in [P::FindingLyrics, P::Analysing, P::DetectingBeats] {
            assert_eq!(download_glyph(Some(p), false, false), DownloadGlyph::Done, "{p:?}");
        }
        assert_eq!(download_glyph(None, true, true), DownloadGlyph::Done);
        assert_eq!(download_glyph(None, false, true), DownloadGlyph::Ring);
        assert_eq!(download_glyph(None, false, false), DownloadGlyph::None);
    }

    #[test]
    fn the_download_entry_says_what_is_left() {
        assert_eq!(download_entry(0, 0), DownloadAct::All);
        assert_eq!(download_entry(10, 0), DownloadAct::Remove);
        assert_eq!(download_entry(10, 10), DownloadAct::All);
        assert_eq!(download_entry(10, 3), DownloadAct::Missing);
    }
}
