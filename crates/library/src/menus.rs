//! What a song's menu, its artist line's links, the sleep timer, a row swipe and a page's download entry
//! offer; the client draws and words each action.

use nori_model::model::ArtistRef;
use nori_model::{DownloadPhase, Song};
use nori_settings::settings::SwipeAction;

/// Something the song menu can do.
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
    /// `name` shows before the page loads; `named`: one of several artists, so the line names it.
    GoToArtist { id: String, name: String, named: bool },
    /// A provider's song: starring it has octo-fiesta fetch it into the library.
    AddToLibrary,
    SleepTimer,
    StartRadio,
    InstantMix,
    ExcludeFromMixes,
    Share,
    Details,
    /// Plays the song and starts a jam around it.
    StartJam,
}

/// What a jam adds to the song menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum JamOffer {
    /// Jams are on, the server relays them and none is hosted: the song can start one.
    Start,
    /// A jam guest: Play next and Add to queue ask the host for the song, and only the pages a guest may
    /// open are offered.
    Guest,
}

/// One line of the song menu.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SongMenuItem {
    pub action: SongAction,
    /// Under "More".
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

/// The menu of `song`, most used first. `starred` as shown; `player`: opened from the player, which adds
/// the sleep timer. A provider's song has no mix or share actions: those need it on the server. `jam`:
/// what a jam adds.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn song_menu(song: Song, starred: bool, download: SongDownload, player: bool, jam: Option<JamOffer>) -> Vec<SongMenuItem> {
    let mut out = Vec::with_capacity(16);
    let mut add = |action: SongAction, more: bool| out.push(SongMenuItem { action, more });
    let guest = jam == Some(JamOffer::Guest);
    if !guest {
        add(SongAction::Favourite { on: !starred }, false);
    }
    add(SongAction::PlayNext, false);
    add(SongAction::AddToQueue, false);
    if !guest {
        add(SongAction::AddToPlaylist, false);
        match download {
            SongDownload::Done => add(SongAction::RemoveDownload, false),
            SongDownload::Pending => add(SongAction::StopDownload, false),
            SongDownload::None => add(SongAction::Download, false),
        }
    }
    if let Some(id) = &song.album_id {
        add(SongAction::GoToAlbum { id: id.clone() }, false);
    }
    if song.artists.len() > 1 {
        for a in song.artists.iter().filter(|a| !a.id.is_empty()) {
            add(SongAction::GoToArtist { id: a.id.clone(), name: a.name.clone(), named: true }, false);
        }
    } else if let Some(id) = &song.artist_id {
        add(SongAction::GoToArtist { id: id.clone(), name: song.artist.clone(), named: false }, false);
    }
    if guest {
        add(SongAction::Details, false);
        return out;
    }
    if song.is_provider() {
        add(SongAction::AddToLibrary, false);
    }
    if jam == Some(JamOffer::Start) {
        add(SongAction::StartJam, false);
    }
    if player {
        add(SongAction::SleepTimer, false);
    }
    add(SongAction::StartRadio, true);
    if !song.is_provider() {
        add(SongAction::InstantMix, true);
        add(SongAction::ExcludeFromMixes, true);
        add(SongAction::Share, true);
    }
    add(SongAction::Details, true);
    out
}

/// A song's artist line as names that open their artists' pages.
#[derive(Debug, Clone, PartialEq)]
pub enum ArtistLine {
    /// One artist, or none known: the whole line opens `artist_id`.
    One,
    /// The line read as the credited artists in order, each with its id, and what stands between them
    /// without one.
    Split(Vec<ArtistPiece>),
    /// Several artists the line does not name one by one: a click on it lists them.
    Several(Vec<ArtistRef>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistPiece {
    pub text: String,
    pub id: Option<String>,
}

/// Words that may stand between two artists' names on a line.
const JOINERS: [&str; 10] = ["feat.", "feat", "ft.", "ft", "featuring", "with", "and", "x", "vs.", "vs"];

/// How `song`'s artist line links its credited artists (OpenSubsonic `artists`, those with an id).
pub fn artist_line(song: &Song) -> ArtistLine {
    let artists: Vec<&ArtistRef> = song.artists.iter().filter(|a| !a.id.is_empty() && !a.name.is_empty()).collect();
    if artists.len() < 2 {
        return ArtistLine::One;
    }
    match split_line(&song.artist, &artists) {
        Some(pieces) => ArtistLine::Split(pieces),
        None => ArtistLine::Several(artists.into_iter().cloned().collect()),
    }
}

/// `line` as `artists`' names in order, with only joiners between them and nothing around them.
fn split_line(line: &str, artists: &[&ArtistRef]) -> Option<Vec<ArtistPiece>> {
    let mut pieces = Vec::with_capacity(artists.len() * 2);
    let mut rest = line;
    for (k, a) in artists.iter().enumerate() {
        let at = rest.find(a.name.as_str())?;
        let between = &rest[..at];
        if k == 0 && !between.trim().is_empty() || k > 0 && !joiner(between) {
            return None;
        }
        if k > 0 {
            pieces.push(ArtistPiece { text: between.to_string(), id: None });
        }
        pieces.push(ArtistPiece { text: a.name.clone(), id: Some(a.id.clone()) });
        rest = &rest[at + a.name.len()..];
    }
    rest.trim().is_empty().then_some(pieces)
}

/// Punctuation such as ", " or " & ", or a joining word such as " feat. ".
fn joiner(between: &str) -> bool {
    let word = between.trim_matches(|c: char| c.is_whitespace() || ",&/;+•·×-".contains(c));
    !between.trim().is_empty() && (word.is_empty() || JOINERS.contains(&word.to_lowercase().as_str()))
}

/// One sleep timer choice; all zeros is "Off".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SleepChoice {
    /// Minutes from now; 0 for the choices that are not a time.
    pub minutes: u32,
    pub end_of_track: bool,
    /// After this many songs; 0 for the rest.
    pub songs: u32,
}

/// The sleep timer's choices, "Off" first while one is running.
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

/// What a sideways swipe on a song row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum RowSwipeAct {
    Queue,
    PlayNext,
    /// The heart; `on` is what letting go sets.
    Favourite { on: bool },
    Download,
}

/// The swipe `setting` on a song whose heart is `starred`; none when it does nothing. Without the
/// `account`'s things (a jam guest's profile, `ProfileRules::account`) a song is not hearted or kept.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn row_swipe(setting: SwipeAction, starred: bool, account: bool) -> Option<RowSwipeAct> {
    match setting {
        SwipeAction::None => None,
        SwipeAction::Queue => Some(RowSwipeAct::Queue),
        SwipeAction::PlayNext => Some(RowSwipeAct::PlayNext),
        SwipeAction::Favourite => account.then_some(RowSwipeAct::Favourite { on: !starred }),
        SwipeAction::Download => account.then_some(RowSwipeAct::Download),
    }
}

/// The mark a song row shows for its download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum DownloadGlyph {
    None,
    /// Waiting or downloading.
    Ring,
    Done,
    Failed,
}

/// A row's download mark: this session's phase when there is one, else downloaded or queued.
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

/// What a page's download entry does.
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

/// A page's download entry for `songs` songs of which `missing` are not downloaded.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_entry(songs: u32, missing: u32) -> DownloadAct {
    match (songs, missing) {
        (0, _) => DownloadAct::All,
        (_, 0) => DownloadAct::Remove,
        (s, m) if s == m => DownloadAct::All,
        _ => DownloadAct::Missing,
    }
}

/// Every download entry a page's menu offers for `songs` songs of which `missing` are not downloaded:
/// a partly downloaded page can fetch the rest or give back what it holds. [`DownloadAct::Remove`] then
/// removes only the songs that are downloaded.
pub fn download_entries(songs: u32, missing: u32) -> Vec<DownloadAct> {
    match download_entry(songs, missing) {
        DownloadAct::Missing => vec![DownloadAct::Missing, DownloadAct::Remove],
        one => vec![one],
    }
}

/// The positions of `songs` not yet `done`: what [`DownloadAct::Missing`] fetches. Twin of Android's
/// `downloadEntry` (DetailScreens.kt).
pub fn download_missing<'a>(songs: impl IntoIterator<Item = &'a str>, done: impl Fn(&str) -> bool) -> Vec<usize> {
    songs.into_iter().enumerate().filter(|(_, id)| !done(id)).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partly_downloaded_page_offers_the_rest_and_removal() {
        use DownloadAct::*;
        for (songs, missing, want) in [(12, 12, vec![All]), (12, 0, vec![Remove]), (12, 5, vec![Missing, Remove]), (0, 0, vec![All])] {
            assert_eq!(download_entries(songs, missing), want, "{songs} songs, {missing} missing");
        }
    }

    fn actions(m: &[SongMenuItem]) -> Vec<(SongAction, bool)> {
        m.iter().map(|i| (i.action.clone(), i.more)).collect()
    }

    #[test]
    fn song_menus() {
        let s = Song { id: "1".into(), album_id: Some("al".into()), artist_id: Some("ar".into()), artist: "Björk".into(), ..Default::default() };
        let m = song_menu(s.clone(), false, SongDownload::None, false, None);
        assert_eq!(
            actions(&song_menu(s.clone(), false, SongDownload::None, true, Some(JamOffer::Guest))),
            [(PlayNext, false), (AddToQueue, false), (GoToAlbum { id: "al".into() }, false), (GoToArtist { id: "ar".into(), name: "Björk".into(), named: false }, false), (Details, false)],
            "a jam guest asks for songs and opens what it may read"
        );
        let starts = song_menu(s, false, SongDownload::None, false, Some(JamOffer::Start));
        assert_eq!(actions(&starts).into_iter().filter(|(a, _)| *a == StartJam).collect::<Vec<_>>(), [(StartJam, false)], "a jam starts from the menu's first part");
        use SongAction::*;
        assert_eq!(
            actions(&m),
            [
                (Favourite { on: true }, false), (PlayNext, false), (AddToQueue, false), (AddToPlaylist, false), (Download, false),
                (GoToAlbum { id: "al".into() }, false), (GoToArtist { id: "ar".into(), name: "Björk".into(), named: false }, false),
                (StartRadio, true), (InstantMix, true), (ExcludeFromMixes, true), (Share, true), (Details, true),
            ]
        );

        // Provider song menu.
        let s = Song {
            id: "ext-deezer-song-9".into(),
            is_external: true,
            artists: vec![ArtistRef { id: "a".into(), name: "A".into() }, ArtistRef { id: String::new(), name: "Nobody".into() }, ArtistRef { id: "b".into(), name: "B".into() }],
            ..Default::default()
        };
        let m = song_menu(s, true, SongDownload::Pending, true, None);
        assert_eq!(
            actions(&m),
            [
                (Favourite { on: false }, false), (PlayNext, false), (AddToQueue, false), (AddToPlaylist, false), (StopDownload, false),
                (GoToArtist { id: "a".into(), name: "A".into(), named: true }, false), (GoToArtist { id: "b".into(), name: "B".into(), named: true }, false),
                (AddToLibrary, false), (SleepTimer, false),
                (StartRadio, true), (Details, true),
            ]
        );
        let done = song_menu(Song::default(), false, SongDownload::Done, false, None);
        assert_eq!(done[4].action, SongAction::RemoveDownload);
    }

    #[test]
    fn artist_lines_link_each_artist_they_name() {
        let r = |id: &str, name: &str| ArtistRef { id: id.into(), name: name.into() };
        let piece = |text: &str, id: Option<&str>| ArtistPiece { text: text.into(), id: id.map(str::to_string) };
        let two = vec![r("a", "Alpha Waves"), r("b", "Beta Band")];
        let split = |joiner: &str| ArtistLine::Split(vec![piece("Alpha Waves", Some("a")), piece(joiner, None), piece("Beta Band", Some("b"))]);
        let cases = [
            ("Alpha Waves feat. Beta Band", two.clone(), split(" feat. ")),
            ("Alpha Waves • Beta Band", two.clone(), split(" • ")),
            ("Alpha Waves, Beta Band", two.clone(), split(", ")),
            ("Alpha Waves X Beta Band", two.clone(), split(" X ")),
            ("Alpha Waves Beta Band", two.clone(), ArtistLine::Several(two.clone())),
            ("Beta Band & Alpha Waves", two.clone(), ArtistLine::Several(two.clone())),
            ("The Alpha Waves & Beta Band Show", two.clone(), ArtistLine::Several(two.clone())),
            ("Alpha Waves and friends with Beta Band", two.clone(), ArtistLine::Several(two.clone())),
            ("Alpha Waves", vec![r("a", "Alpha Waves")], ArtistLine::One),
            ("Alpha Waves feat. Nobody", vec![r("a", "Alpha Waves"), r("", "Nobody")], ArtistLine::One),
        ];
        for (line, artists, want) in cases {
            let song = Song { artist: line.into(), artists, ..Default::default() };
            assert_eq!(artist_line(&song), want, "{line}");
        }
    }

    #[test]
    fn sleep_offers_off_only_while_running() {
        let off = SleepChoice { minutes: 0, end_of_track: false, songs: 0 };
        let (running, idle) = (sleep_choices(true), sleep_choices(false));
        assert_eq!(running[0], off);
        assert_eq!(running[1..], idle);
        assert!(!idle.contains(&off));
    }

    #[test]
    fn marks() {
        assert_eq!(row_swipe(SwipeAction::None, false, true), None);
        assert_eq!(row_swipe(SwipeAction::Favourite, true, true), Some(RowSwipeAct::Favourite { on: false }));
        assert_eq!(row_swipe(SwipeAction::Favourite, false, true), Some(RowSwipeAct::Favourite { on: true }));
        // A jam guest's swipes ask for songs; hearts and downloads are the account's.
        let guest: Vec<_> = [SwipeAction::Queue, SwipeAction::PlayNext, SwipeAction::Favourite, SwipeAction::Download].map(|s| row_swipe(s, false, false)).into();
        assert_eq!(guest, [Some(RowSwipeAct::Queue), Some(RowSwipeAct::PlayNext), None, None]);

        // A rows download mark.
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

        // The download entry says what is left.
        assert_eq!(download_entry(0, 0), DownloadAct::All);
        assert_eq!(download_entry(10, 0), DownloadAct::Remove);
        assert_eq!(download_entry(10, 10), DownloadAct::All);
        assert_eq!(download_entry(10, 3), DownloadAct::Missing);
    }

}
