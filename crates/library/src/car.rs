//! The browse tree of a car (Android Auto) or other remote browser: its folders, how their rows read and
//! page, what a picked row plays and what a spoken request asks for.

use nori_model::{Album, Artist, Genre, OriginKind, PageOrigin, Playlist, SearchResult, Song};

use crate::mixes::board::{MixName, MixTile};

/// One of the tree's own folders, which the client names. The library's folders carry their name as
/// data, a mix its [`MixName`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[repr(u8)]
pub enum CarFolder {
    /// The tree's root (the app itself).
    Root,
    RecentlyPlayed,
    RecentlyAdded,
    MostPlayed,
    Playlists,
    Favourites,
    Random,
    Downloads,
    /// The first tab: the mixes, then what was played and added lately.
    Home,
    /// The library's lists: albums, artists, playlists, genres.
    Library,
    /// Every album, A to Z.
    Albums,
    Artists,
    Genres,
}

/// A heading a folder's rows are listed under, which the client words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum CarGroup {
    Mixes,
    RecentlyPlayed,
    RecentlyAdded,
    Artists,
    Albums,
    Playlists,
    Songs,
}

/// How a folder's own folders are drawn: rows, or a grid of their covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum CarStyle {
    List,
    Grid,
}

/// A row at the top of a folder that plays the whole of it, as a page's Play and Shuffle do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum CarAction {
    Play,
    Shuffle,
}

/// A folder in the tree: `id` is what is asked for next (the client's `browse_children`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct BrowseFolder {
    pub id: String,
    /// Which of the tree's own folders this is; none for the library's things, named by `title` or `mix`.
    pub kind: Option<CarFolder>,
    /// A mix's name, for the client to word.
    pub mix: Option<MixName>,
    /// The album's, artist's, playlist's or genre's name; empty for the tree's own folders and the mixes.
    pub title: String,
    /// An album's artist.
    pub subtitle: Option<String>,
    /// A playlist's or a genre's number of songs, for the client to say.
    pub songs: Option<u32>,
    /// The ids of its cover: one, or four drawn as a square of four (a mix's, as its tile on Home).
    pub art: Vec<String>,
    /// The heading it is listed under in the folder that holds it.
    pub group: Option<CarGroup>,
    /// How its own folders are drawn.
    pub style: CarStyle,
    /// Whether it can be played whole from where it is listed (an album, a playlist, a mix...).
    pub playable: bool,
}

/// What a folder holds: the rows that play it whole, more folders, and songs.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct BrowsePage {
    pub folders: Vec<BrowseFolder>,
    /// Play and Shuffle, above the songs, for a folder that plays whole.
    pub actions: Vec<CarAction>,
    pub songs: Vec<Song>,
    /// Which of `songs` are downloaded, place by place.
    pub downloaded: Vec<bool>,
    /// The heading over the songs, where the folder has other things too.
    pub songs_group: Option<CarGroup>,
    /// The folder could not be read (the server out of reach, nothing stored): the car says so rather
    /// than showing an empty folder.
    pub failed: bool,
}

/// What a picked row plays: songs from `index`, the page they are the queue of, and in shuffle or not.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct CarQueue {
    pub songs: Vec<Song>,
    pub index: u32,
    pub origin: Option<PageOrigin>,
    pub shuffle: bool,
}

/// The tree's root, as the platform names it.
pub const ROOT: &str = "root";
/// How large a cover a car draws a folder with.
pub const ART: u32 = 300;
/// How many albums a shelf of Home lists.
pub const SHELF: usize = 12;

/// One of the tree's own folders: no subtitle and no cover.
pub fn folder(id: &str, kind: CarFolder) -> BrowseFolder {
    BrowseFolder {
        id: id.into(),
        kind: Some(kind),
        mix: None,
        title: String::new(),
        subtitle: None,
        songs: None,
        art: Vec::new(),
        group: None,
        style: CarStyle::List,
        playable: false,
    }
}

fn styled(mut f: BrowseFolder, style: CarStyle) -> BrowseFolder {
    f.style = style;
    f
}

fn cover(id: &Option<String>) -> Vec<String> {
    id.iter().cloned().collect()
}

/// The tabs at the tree's root, `limit` at most (the car's own limit; none under one): Home, the library,
/// the favourites and the downloads, the downloads first while the phone is `offline`, as they are then
/// all that plays.
pub fn root(limit: u32, offline: bool) -> Vec<BrowseFolder> {
    let mut tabs = vec![
        styled(folder("home", CarFolder::Home), CarStyle::Grid),
        folder("library", CarFolder::Library),
        styled(folder("starred", CarFolder::Favourites), CarStyle::Grid),
        folder("downloads", CarFolder::Downloads),
    ];
    if offline {
        tabs.rotate_right(1);
    }
    tabs.truncate(limit.max(1) as usize);
    tabs
}

/// The library tab: the album lists, the artists, the playlists, the genres and random songs.
pub fn library() -> Vec<BrowseFolder> {
    vec![
        styled(folder("albums:recent", CarFolder::RecentlyPlayed), CarStyle::Grid),
        styled(folder("albums:newest", CarFolder::RecentlyAdded), CarStyle::Grid),
        styled(folder("albums:frequent", CarFolder::MostPlayed), CarStyle::Grid),
        styled(folder("albums:alphabeticalByName", CarFolder::Albums), CarStyle::Grid),
        styled(folder("artists", CarFolder::Artists), CarStyle::Grid),
        folder("playlists", CarFolder::Playlists),
        folder("genres", CarFolder::Genres),
        folder("random", CarFolder::Random),
    ]
}

pub fn album_folder(a: &Album, group: Option<CarGroup>) -> BrowseFolder {
    BrowseFolder {
        id: format!("album:{}", a.id),
        kind: None,
        mix: None,
        title: a.name.clone(),
        subtitle: Some(a.artist.clone()),
        songs: None,
        art: cover(&a.cover_art),
        group,
        style: CarStyle::List,
        playable: true,
    }
}

pub fn artist_folder(a: &Artist, group: Option<CarGroup>) -> BrowseFolder {
    BrowseFolder {
        id: format!("artist:{}", a.id),
        kind: None,
        mix: None,
        title: a.name.clone(),
        subtitle: None,
        songs: None,
        art: cover(&a.cover_art),
        group,
        style: CarStyle::Grid,
        playable: true,
    }
}

pub fn playlist_folder(p: &Playlist) -> BrowseFolder {
    BrowseFolder {
        id: format!("playlist:{}", p.id),
        kind: None,
        mix: None,
        title: p.name.clone(),
        subtitle: None,
        songs: Some(p.song_count),
        art: cover(&p.cover_art),
        group: None,
        style: CarStyle::List,
        playable: true,
    }
}

pub fn genre_folder(g: &Genre) -> BrowseFolder {
    BrowseFolder {
        id: format!("genre:{}", g.name),
        kind: None,
        mix: None,
        title: g.name.clone(),
        subtitle: None,
        songs: Some(g.song_count),
        art: Vec::new(),
        group: None,
        style: CarStyle::Grid,
        playable: true,
    }
}

/// A "For you" tile: its four covers as a square of four when it has them, as on Home, else its first.
pub fn mix_folder(t: &MixTile) -> BrowseFolder {
    let art = if t.covers.len() >= 4 { t.covers[..4].to_vec() } else { t.covers.iter().take(1).cloned().collect() };
    BrowseFolder {
        id: format!("mix:{}", t.id),
        kind: None,
        mix: Some(t.name),
        title: String::new(),
        subtitle: None,
        songs: None,
        art,
        group: Some(CarGroup::Mixes),
        style: CarStyle::List,
        playable: true,
    }
}

/// Albums for the car: a provider's (octo-fiesta's, not in the library) left out, as everywhere a list
/// could start them.
pub fn album_folders(albums: &[Album], group: Option<CarGroup>, max: usize) -> Vec<BrowseFolder> {
    albums.iter().filter(|a| !a.is_external).take(max).map(|a| album_folder(a, group)).collect()
}

/// Songs for the car, a provider's left out: playing one makes the server fetch its whole album.
pub fn library_songs(songs: Vec<Song>) -> Vec<Song> {
    songs.into_iter().filter(|s| !s.is_external).collect()
}

/// The page a queue started from folder `parent` belongs to, so the phone's page reads Pause while it plays.
pub fn origin_of(parent: &str) -> Option<PageOrigin> {
    let (kind, id) = parent.split_once(':')?;
    let kind = match kind {
        "album" => OriginKind::Album,
        "artist" => OriginKind::Artist,
        "playlist" => OriginKind::Playlist,
        "mix" => OriginKind::Mix,
        "genre" => OriginKind::Genre,
        _ => return None,
    };
    Some(PageOrigin { kind, id: id.into() })
}

/// Whether folder `id`, picked as it is listed, plays whole: an album, an artist, a playlist, a mix, a genre.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn car_plays_whole(id: String) -> bool {
    origin_of(&id).is_some()
}

/// A picked row, as its id says: song `song` of folder `parent`, or `action` on the whole of it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct CarRow {
    pub parent: String,
    pub song: Option<String>,
    pub action: Option<CarAction>,
}

/// The id of song `song` listed in folder `parent`: picked, it plays the folder from there, not the song alone.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn car_song_row(parent: String, song: String) -> String {
    format!("in|{parent}|{song}")
}

/// The id of the row that does `action` on folder `parent`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn car_action_row(parent: String, action: CarAction) -> String {
    match action {
        CarAction::Play => format!("play|{parent}"),
        CarAction::Shuffle => format!("shuffle|{parent}"),
    }
}

/// What a row id is, or none for an id that is not one of the tree's rows (a bare song id, a folder).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn car_row(id: String) -> Option<CarRow> {
    let (kind, rest) = id.split_once('|')?;
    match kind {
        // The song's id is the part after the last bar; the folder's (a search's query) may hold bars.
        "in" => rest.rsplit_once('|').map(|(parent, song)| CarRow { parent: parent.into(), song: Some(song.into()), action: None }),
        "play" => Some(CarRow { parent: rest.into(), song: None, action: Some(CarAction::Play) }),
        "shuffle" => Some(CarRow { parent: rest.into(), song: None, action: Some(CarAction::Shuffle) }),
        _ => None,
    }
}

/// The queue a picked `row` plays out of `songs`, the folder's: from the song picked, or from the start,
/// shuffled for Shuffle.
pub fn queue_for(row: &CarRow, songs: Vec<Song>) -> CarQueue {
    let index = row.song.as_ref().and_then(|id| songs.iter().position(|s| &s.id == id)).unwrap_or(0) as u32;
    CarQueue { songs, index, origin: origin_of(&row.parent), shuffle: row.action == Some(CarAction::Shuffle) }
}

/// Page `page` of `size` rows of `all`, in the order a car lists them: Play and Shuffle, folders, songs.
pub fn page_of(all: &BrowsePage, page: u32, size: u32) -> BrowsePage {
    let start = (page as usize).saturating_mul(size as usize);
    let end = start.saturating_add(size as usize);
    // The rows of a list of `len` that starts `skip` rows in.
    let part = |len: usize, skip: usize| start.saturating_sub(skip).min(len)..end.saturating_sub(skip).min(len);
    let (actions, folders) = (all.actions.len(), all.folders.len());
    let songs = part(all.songs.len(), actions + folders);
    BrowsePage {
        actions: all.actions[part(actions, 0)].to_vec(),
        folders: all.folders[part(folders, actions)].to_vec(),
        downloaded: all.downloaded[songs.clone()].to_vec(),
        songs: all.songs[songs].to_vec(),
        songs_group: all.songs_group,
        failed: all.failed,
    }
}

/// What a spoken request is about, as the car's assistant heard it ("play the album ...").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum VoiceFocus {
    /// Nothing said about it: whatever the words name best.
    #[default]
    Any,
    Artist,
    Album,
    Playlist,
    Genre,
    Song,
}

/// A spoken request: its words, what it is about, and the parts the assistant picked out, when it did.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct VoiceAsk {
    pub query: String,
    pub focus: VoiceFocus,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub genre: Option<String>,
    pub playlist: Option<String>,
}

/// What a spoken request plays.
#[derive(Debug, Clone, PartialEq)]
pub enum VoicePick {
    /// All of an artist's songs, shuffled.
    Artist(String),
    Album(String),
    Playlist(String),
    /// Songs of a genre, shuffled.
    Genre(String),
    /// The songs found, from the first.
    Songs(Vec<Song>),
    Nothing,
}

/// Names compared as said: case, the spaces around them and a leading "the" do not count.
fn same(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.trim().to_lowercase();
        s.strip_prefix("the ").map(str::to_string).unwrap_or(s)
    }
    norm(a) == norm(b)
}

/// The words to ask the server for `ask` with: the part picked out for its focus, else all of it.
pub fn voice_term(ask: &VoiceAsk) -> String {
    let part = match ask.focus {
        VoiceFocus::Artist => ask.artist.as_ref(),
        VoiceFocus::Album => ask.album.as_ref(),
        VoiceFocus::Song => ask.title.as_ref(),
        VoiceFocus::Playlist => ask.playlist.as_ref(),
        VoiceFocus::Genre => ask.genre.as_ref(),
        VoiceFocus::Any => None,
    };
    part.filter(|p| !p.trim().is_empty()).cloned().unwrap_or_else(|| ask.query.clone())
}

/// What `ask` plays, out of what the server `found` for [`voice_term`] and the `playlists`. With a focus
/// it plays that kind, the one named exactly first; with none, a playlist, an artist or an album named
/// exactly, else the songs found, else the first artist or album found.
pub fn voice_pick(ask: &VoiceAsk, found: &SearchResult, playlists: &[Playlist]) -> VoicePick {
    let term = voice_term(ask);
    let artists: Vec<&Artist> = found.artists.iter().filter(|a| !a.is_external).collect();
    let albums: Vec<&Album> = found.albums.iter().filter(|a| !a.is_external).collect();
    let songs = library_songs(found.songs.clone());
    let artist_named = || artists.iter().find(|a| same(&a.name, &term)).map(|a| a.id.clone());
    let album_named = || {
        albums.iter().find(|a| same(&a.name, &term) && ask.artist.as_ref().is_none_or(|ar| same(&a.artist, ar))).map(|a| a.id.clone())
    };
    let playlist_named = || playlists.iter().find(|p| same(&p.name, &term)).map(|p| p.id.clone());
    let playlist_like = || {
        let t = term.trim().to_lowercase();
        playlists.iter().find(|p| !t.is_empty() && p.name.to_lowercase().contains(&t)).map(|p| p.id.clone())
    };
    let songs_found = |songs: Vec<Song>| if songs.is_empty() { VoicePick::Nothing } else { VoicePick::Songs(songs) };
    match ask.focus {
        VoiceFocus::Genre => VoicePick::Genre(term),
        VoiceFocus::Playlist => playlist_named().or_else(playlist_like).map(VoicePick::Playlist).unwrap_or_else(|| songs_found(songs)),
        VoiceFocus::Artist => artist_named().or_else(|| artists.first().map(|a| a.id.clone())).map(VoicePick::Artist).unwrap_or(VoicePick::Nothing),
        VoiceFocus::Album => album_named().or_else(|| albums.first().map(|a| a.id.clone())).map(VoicePick::Album).unwrap_or(VoicePick::Nothing),
        VoiceFocus::Song => {
            // The song named exactly first, the others found after it.
            let (named, rest): (Vec<Song>, Vec<Song>) = songs.into_iter().partition(|s| same(&s.title, &term));
            songs_found(named.into_iter().chain(rest).collect())
        }
        VoiceFocus::Any => {
            if let Some(p) = playlist_named() {
                VoicePick::Playlist(p)
            } else if let Some(a) = artist_named() {
                VoicePick::Artist(a)
            } else if let Some(a) = album_named() {
                VoicePick::Album(a)
            } else if !songs.is_empty() {
                VoicePick::Songs(songs)
            } else if let Some(a) = artists.first() {
                VoicePick::Artist(a.id.clone())
            } else if let Some(a) = albums.first() {
                VoicePick::Album(a.id.clone())
            } else {
                VoicePick::Nothing
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, title: &str) -> Song {
        Song { id: id.into(), title: title.into(), ..Default::default() }
    }

    #[test]
    fn the_root_is_four_tabs_with_the_downloads_first_offline_and_the_cars_limit_kept() {
        let ids = |f: Vec<BrowseFolder>| f.into_iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(ids(root(4, false)), ["home", "library", "starred", "downloads"]);
        assert_eq!(ids(root(4, true)), ["downloads", "home", "library", "starred"]);
        assert_eq!(ids(root(2, false)), ["home", "library"]);
        assert_eq!(ids(root(0, false)), ["home"], "a car that says none still gets one");
    }

    #[test]
    fn car_pages() {
        let all = BrowsePage {
            actions: vec![CarAction::Play, CarAction::Shuffle],
            folders: vec![folder("f", CarFolder::Albums)],
            songs: vec![song("a", "A"), song("b", "B")],
            downloaded: vec![false, true],
            ..Default::default()
        };
        let rows = |p: BrowsePage| (p.actions.len(), p.folders.len(), p.songs.iter().map(|s| s.id.clone()).collect::<Vec<_>>(), p.downloaded);
        for (page, size, want) in [
            (0, 2, (2, 0, vec![], vec![])),
            (1, 2, (0, 1, vec!["a".to_string()], vec![false])),
            (2, 2, (0, 0, vec!["b".to_string()], vec![true])),
            (3, 2, (0, 0, vec![], vec![])),
            (0, 10, (2, 1, vec!["a".to_string(), "b".to_string()], vec![false, true])),
        ] {
            assert_eq!(rows(page_of(&all, page, size)), want, "page {page} of {size}");
        }

        // A rows id says its folder and song even when the folder holds bars.
        let row = car_row(car_song_row("search:a|b".into(), "s1".into())).unwrap();
        assert_eq!(row, CarRow { parent: "search:a|b".into(), song: Some("s1".into()), action: None });
        assert_eq!(car_row(car_action_row("album:x".into(), CarAction::Shuffle)).unwrap().action, Some(CarAction::Shuffle));
        assert_eq!(car_row("s1".into()), None, "a bare song id is not one of the tree's rows");
        assert_eq!(car_row("album:x".into()), None);

        // A mix shows four covers as a square or its first.
        let tile = |n: usize| MixTile { id: "discover".into(), name: MixName::Discover, covers: (0..n).map(|i| i.to_string()).collect(), favourites: false };
        assert_eq!(mix_folder(&tile(6)).art, ["0", "1", "2", "3"]);
        assert_eq!(mix_folder(&tile(2)).art, ["0"]);
        assert!(mix_folder(&tile(0)).art.is_empty());
        assert_eq!(mix_folder(&tile(1)).id, "mix:discover");
    }

    #[test]
    fn car_plays() {
        let songs = vec![song("a", "A"), song("b", "B"), song("c", "C")];
        let q = queue_for(&car_row("in|album:x|b".into()).unwrap(), songs.clone());
        assert_eq!((q.index, q.shuffle), (1, false));
        assert_eq!(q.origin, Some(PageOrigin { kind: OriginKind::Album, id: "x".into() }));
        let q = queue_for(&car_row("shuffle|mix:discover".into()).unwrap(), songs.clone());
        assert_eq!((q.index, q.shuffle), (0, true));
        assert_eq!(q.origin.unwrap().kind, OriginKind::Mix);
        assert_eq!(queue_for(&car_row("in|starred|zz".into()).unwrap(), songs).origin, None, "the favourites are no page's queue");
        assert!(car_plays_whole("album:x".into()) && car_plays_whole("genre:Jazz".into()));
        assert!(!car_plays_whole("albums:newest".into()) && !car_plays_whole("starred".into()), "a list of folders is not played whole");

        // A spoken request plays what it names best.
        let ask = |query: &str, focus: VoiceFocus| VoiceAsk { query: query.into(), focus, ..Default::default() };
        let lists = [Playlist { id: "p1".into(), name: "Road trip".into(), ..Default::default() }];
        assert_eq!(voice_pick(&ask("future", VoiceFocus::Any), &found(), &lists), VoicePick::Artist("ar1".into()), "an artist named exactly");
        assert_eq!(voice_pick(&ask("road trip", VoiceFocus::Any), &found(), &lists), VoicePick::Playlist("p1".into()));
        assert_eq!(voice_pick(&ask("the monster", VoiceFocus::Any), &found(), &lists), VoicePick::Album("al1".into()), "a leading 'the' does not count");
        assert!(matches!(voice_pick(&ask("after", VoiceFocus::Any), &found(), &lists), VoicePick::Songs(s) if s.len() == 2));
        assert_eq!(voice_pick(&ask("future", VoiceFocus::Album), &found(), &lists), VoicePick::Album("al2".into()), "with a focus, that kind");
        assert_eq!(voice_pick(&ask("road", VoiceFocus::Playlist), &found(), &lists), VoicePick::Playlist("p1".into()), "a playlist by part of its name");
        assert_eq!(voice_pick(&ask("jazz", VoiceFocus::Genre), &found(), &lists), VoicePick::Genre("jazz".into()));
        let named = voice_pick(&VoiceAsk { query: "monster by future".into(), focus: VoiceFocus::Song, title: Some("Monster".into()), ..Default::default() }, &found(), &lists);
        assert!(matches!(named, VoicePick::Songs(s) if s[0].id == "s2"), "the song named exactly first");
        assert_eq!(voice_pick(&ask("x", VoiceFocus::Any), &SearchResult::default(), &[]), VoicePick::Nothing);

        // A providers songs and albums are left out.
        let ext = Song { is_external: true, ..song("e", "E") };
        assert_eq!(library_songs(vec![song("a", "A"), ext]).len(), 1);
        let albums = [Album { id: "1".into(), ..Default::default() }, Album { id: "2".into(), is_external: true, ..Default::default() }];
        assert_eq!(album_folders(&albums, None, 10).len(), 1);
    }

    fn found() -> SearchResult {
        SearchResult {
            artists: vec![Artist { id: "ar1".into(), name: "Future".into(), ..Default::default() }],
            albums: vec![
                Album { id: "al1".into(), name: "Monster".into(), artist: "Future".into(), ..Default::default() },
                Album { id: "al2".into(), name: "Future".into(), artist: "Future".into(), ..Default::default() },
            ],
            songs: vec![song("s1", "After That"), song("s2", "Monster")],
        }
    }

}
