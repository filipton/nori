//! Every read the app makes of its server, and which of them are answered from the stored response.
//! A screen opens with the stored answer at once; the network answer replaces it only when it differs.
//! An answer younger than its freshness window is not asked again: opening the same screens within a
//! couple of minutes then costs no request at all, which on mobile data means no radio wake-up. Writes
//! evict what they change (client.rs), so the user's own actions are never hidden by this.
//!
//! The platform asks in two steps - [`Client::read_stored`] paints, [`Client::read_fetch`] refreshes - so
//! the stored answer is on screen while the request is out. Keys, windows, parameters and parsing are all
//! here; the platform only emits what comes back.

use std::hash::{Hash, Hasher};

use rusqlite::OptionalExtension;

use crate::client::{pairs, Client, NetResult};
use crate::{
    Album, AlbumDetail, Artist, ArtistDetail, ArtistInfo, Directory, Genre, Lyrics, MusicFolder, PlayQueue, Playlist, PlaylistDetail, RadioStation, SearchResult,
    ServerInfo, Song, Starred,
};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
/// Most reads: long enough for going back and forth between screens, short enough that the server's
/// own changes show up soon.
const BROWSE: i64 = 2 * MINUTE;

/// A read of the server. The cached ones come first; the rest always ask.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum Read {
    /// getAlbumList2 of `kind` (newest, recent, frequent, random, alphabeticalByName, ...). "byYear" means
    /// this year's albums; "random" is never stored, or it would be the same shuffle every time.
    AlbumList { kind: String, size: i32, offset: i32, genre: Option<String> },
    AlbumsByYear { from: i32, to: i32, size: i32, offset: i32 },
    /// The first `size` starred albums, the same request and stored answer as that `AlbumList`, with this
    /// session's marks laid over it: the home page's favourites shelf, which follows the hearts.
    FavouriteAlbums { size: i32 },
    ArtistIndex,
    AlbumById { id: String },
    ArtistById { id: String },
    ArtistAbout { id: String },
    TopSongs { artist: String },
    PlaylistList,
    PlaylistById { id: String },
    StarredItems,
    GenreList,
    RadioList,
    /// `enhanced` asks OpenSubsonic servers for word cues and translation layers (songLyrics v2).
    LyricsBySong { song_id: String },
    /// The server's folder tree, for libraries organised by directory rather than by tags.
    FolderIndex,
    FolderById { id: String },
    // ---- never stored ----
    MusicFolders,
    Ping,
    /// Always asks the server, so provider results from octo-fiesta show up. One big page: the proxy
    /// repeats its external results on every offset.
    Search { query: String, songs: i32, albums: i32, artists: i32 },
    RandomSongs { size: i32, genre: Option<String> },
    SongsByGenre { genre: String, count: i32 },
    SimilarSongs { id: String, count: i32 },
    SongById { id: String },
    AlbumSongs { id: String },
    PlaylistSongs { id: String },
    /// Not queued like a play: by the time it could be replayed it is no longer true.
    NowPlaying { id: String },
    /// A public link to a song or album; the server must have sharing enabled.
    ShareLink { id: String },
    PullQueue,
}

/// A parsed answer. The variant follows the read.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum Page {
    Albums { v: Vec<Album> },
    Artists { v: Vec<Artist> },
    AlbumPage { v: AlbumDetail },
    ArtistPage { v: ArtistDetail },
    About { v: ArtistInfo },
    Songs { v: Vec<Song> },
    OneSong { v: Option<Song> },
    Playlists { v: Vec<Playlist> },
    PlaylistPage { v: PlaylistDetail },
    StarredPage { v: Starred },
    Genres { v: Vec<Genre> },
    Stations { v: Vec<RadioStation> },
    LyricsPage { v: Lyrics },
    DirectoryPage { v: Directory },
    Found { v: SearchResult },
    Folders { v: Vec<MusicFolder> },
    Status { v: ServerInfo },
    ShareUrl { v: String },
    Queue { v: PlayQueue },
}

/// What is stored for a read. `digest` identifies the stored bytes (None: nothing stored) and goes back
/// into [`Client::read_fetch`], which only returns a page when the server's answer differs from it.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Stored {
    pub page: Option<Page>,
    pub digest: Option<u64>,
    /// The stored answer was shown and is young enough: the server is not asked.
    pub fresh: bool,
}

#[derive(Clone, Copy)]
enum Parser {
    AlbumList,
    FavouriteAlbums,
    Artists,
    Album,
    AlbumSongs,
    Artist,
    ArtistInfo,
    Songs,
    OneSong,
    Playlists,
    Playlist,
    PlaylistSongs,
    Starred,
    Genres,
    Radio,
    Lyrics,
    Indexes,
    Directory,
    Search,
    Folders,
    Status,
    Share,
    Queue,
}

struct Spec {
    endpoint: &'static str,
    params: Vec<(String, String)>,
    /// None: never stored.
    fresh_ms: Option<i64>,
    parser: Parser,
}

/// The year it is where the phone is.
fn this_year() -> i32 {
    #[cfg(unix)]
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if !libc::localtime_r(&now, &mut tm).is_null() {
            return tm.tm_year + 1900;
        }
    }
    // Days since 1970 over the mean Gregorian year: exact except within hours of New Year.
    (1970.0 + (crate::db::now_ms() as f64 / 86_400_000.0) / 365.2425) as i32
}

fn by_year(from: i32, to: i32, size: i32, offset: i32) -> Spec {
    let p = pairs(&[("type", "byYear".into()), ("fromYear", from.to_string()), ("toYear", to.to_string()), ("size", size.to_string()), ("offset", offset.to_string())]);
    Spec { endpoint: "getAlbumList2", params: p, fresh_ms: Some(BROWSE), parser: Parser::AlbumList }
}

fn spec(read: Read) -> Spec {
    let s = |endpoint, params, fresh_ms, parser| Spec { endpoint, params, fresh_ms, parser };
    let id = |id: String| pairs(&[("id", id)]);
    match read {
        Read::AlbumList { kind, size, offset, genre } => {
            if kind == "byYear" {
                return by_year(this_year(), 0, size, offset);
            }
            let fresh = if kind == "random" { None } else { Some(BROWSE) };
            let mut p = pairs(&[("type", kind), ("size", size.to_string()), ("offset", offset.to_string())]);
            if let Some(g) = genre {
                p.push(("genre".into(), g));
            }
            s("getAlbumList2", p, fresh, Parser::AlbumList)
        }
        Read::AlbumsByYear { from, to, size, offset } => by_year(from, to, size, offset),
        Read::FavouriteAlbums { size } => {
            let p = pairs(&[("type", "starred".into()), ("size", size.to_string()), ("offset", "0".into())]);
            s("getAlbumList2", p, Some(BROWSE), Parser::FavouriteAlbums)
        }
        Read::ArtistIndex => s("getArtists", vec![], Some(BROWSE), Parser::Artists),
        Read::AlbumById { id: i } => s("getAlbum", id(i), Some(BROWSE), Parser::Album),
        Read::ArtistById { id: i } => s("getArtist", id(i), Some(BROWSE), Parser::Artist),
        Read::ArtistAbout { id: i } => s("getArtistInfo2", pairs(&[("id", i), ("count", "10".into())]), Some(DAY), Parser::ArtistInfo),
        Read::TopSongs { artist } => s("getTopSongs", pairs(&[("artist", artist), ("count", "20".into())]), Some(DAY), Parser::Songs),
        Read::PlaylistList => s("getPlaylists", vec![], Some(BROWSE), Parser::Playlists),
        Read::PlaylistById { id: i } => s("getPlaylist", id(i), Some(BROWSE), Parser::Playlist),
        Read::StarredItems => s("getStarred2", vec![], Some(BROWSE), Parser::Starred),
        Read::GenreList => s("getGenres", vec![], Some(HOUR), Parser::Genres),
        Read::RadioList => s("getInternetRadioStations", vec![], Some(HOUR), Parser::Radio),
        Read::LyricsBySong { song_id } => s("getLyricsBySongId", pairs(&[("id", song_id), ("enhanced", "true".into())]), Some(HOUR), Parser::Lyrics),
        Read::FolderIndex => s("getIndexes", vec![], Some(HOUR), Parser::Indexes),
        Read::FolderById { id: i } => s("getMusicDirectory", id(i), Some(BROWSE), Parser::Directory),
        Read::MusicFolders => s("getMusicFolders", vec![], None, Parser::Folders),
        Read::Ping => s("ping", vec![], None, Parser::Status),
        Read::Search { query, songs, albums, artists } => s(
            "search3",
            pairs(&[("query", query), ("songCount", songs.to_string()), ("albumCount", albums.to_string()), ("artistCount", artists.to_string())]),
            None,
            Parser::Search,
        ),
        Read::RandomSongs { size, genre } => {
            let mut p = pairs(&[("size", size.to_string())]);
            if let Some(g) = genre {
                p.push(("genre".into(), g));
            }
            s("getRandomSongs", p, None, Parser::Songs)
        }
        Read::SongsByGenre { genre, count } => s("getSongsByGenre", pairs(&[("genre", genre), ("count", count.to_string())]), None, Parser::Songs),
        Read::SimilarSongs { id: i, count } => s("getSimilarSongs2", pairs(&[("id", i), ("count", count.to_string())]), None, Parser::Songs),
        Read::SongById { id: i } => s("getSong", id(i), None, Parser::OneSong),
        Read::AlbumSongs { id: i } => s("getAlbum", id(i), None, Parser::AlbumSongs),
        Read::PlaylistSongs { id: i } => s("getPlaylist", id(i), None, Parser::PlaylistSongs),
        Read::NowPlaying { id: i } => s("scrobble", pairs(&[("id", i), ("submission", "false".into())]), None, Parser::Status),
        Read::ShareLink { id: i } => s("createShare", id(i), None, Parser::Share),
        Read::PullQueue => s("getPlayQueue", vec![], None, Parser::Queue),
    }
}

/// The stored answer's key: the endpoint and its parameters (the music folder included) as they are
/// sent, in order and unencoded. Eviction prefixes depend on this shape ("getAlbumList2&type=starred").
fn key(endpoint: &str, params: &[(String, String)]) -> String {
    let mut k = String::with_capacity(endpoint.len() + params.iter().map(|(a, b)| a.len() + b.len() + 2).sum::<usize>());
    k.push_str(endpoint);
    for (a, b) in params {
        k.push('&');
        k.push_str(a);
        k.push('=');
        k.push_str(b);
    }
    k
}

fn digest(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Whether `page` lists a provider's item (octo-fiesta's `isExternal`, `ext-` and `pl-` ids). The server's
/// answer for it moves as the provider's songs are downloaded (a song played is the library's afterwards,
/// its cloud gone), so a stored one is shown but never young enough to skip asking again.
fn lists_provider_items(page: &Page) -> bool {
    let song = |s: &Song| s.is_external || crate::db::external(&s.id);
    let album = |a: &Album| a.is_external || crate::db::external(&a.id);
    match page {
        Page::AlbumPage { v } => album(&v.album) || v.songs.iter().any(song),
        Page::ArtistPage { v } => v.albums.iter().any(album),
        Page::PlaylistPage { v } => v.songs.iter().any(song),
        Page::Songs { v } => v.iter().any(song),
        Page::Albums { v } => v.iter().any(album),
        _ => false,
    }
}

impl Client {
    fn parse(&self, parser: Parser, body: Vec<u8>) -> NetResult<Page> {
        let c = &self.core;
        Ok(match parser {
            Parser::AlbumList => Page::Albums { v: c.parse_album_list(body)? },
            Parser::FavouriteAlbums => Page::Albums { v: crate::stars::star_overlay_albums(c.parse_album_list(body)?) },
            Parser::Artists => Page::Artists { v: c.parse_artists(body)? },
            Parser::Album => Page::AlbumPage { v: c.parse_album(body)? },
            Parser::AlbumSongs => Page::Songs { v: c.parse_album(body)?.songs },
            Parser::Artist => Page::ArtistPage { v: c.parse_artist(body)? },
            Parser::ArtistInfo => Page::About { v: c.parse_artist_info(body)? },
            Parser::Songs => Page::Songs { v: c.parse_songs(body)? },
            Parser::OneSong => Page::OneSong { v: c.parse_songs(body)?.into_iter().next() },
            Parser::Playlists => Page::Playlists { v: c.parse_playlists(body)? },
            Parser::Playlist => Page::PlaylistPage { v: c.parse_playlist(body)? },
            Parser::PlaylistSongs => Page::Songs { v: c.parse_playlist(body)?.songs },
            // This session's marks are laid over the favourites wherever they are read, stored or fresh,
            // so an unstarred item leaves them at once rather than when the server's new answer comes.
            Parser::Starred => Page::StarredPage { v: crate::stars::star_overlay(c.parse_starred(body)?) },
            Parser::Genres => Page::Genres { v: c.parse_genres(body)? },
            Parser::Radio => Page::Stations { v: c.parse_radio(body)? },
            Parser::Lyrics => {
                let mut v = c.parse_lyrics(body)?;
                crate::look::keep(&mut v);
                Page::LyricsPage { v }
            }
            Parser::Indexes => Page::Artists { v: c.parse_indexes(body)? },
            Parser::Directory => Page::DirectoryPage { v: c.parse_directory(body)? },
            Parser::Search => Page::Found { v: c.parse_search(body)? },
            Parser::Folders => Page::Folders { v: c.parse_music_folders(body)? },
            Parser::Status => Page::Status { v: c.parse_status(body)? },
            Parser::Share => Page::ShareUrl { v: c.parse_share(body)? },
            Parser::Queue => Page::Queue { v: c.parse_play_queue(body)? },
        })
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Asks the server, nothing stored: the reads that must be current.
    pub async fn read_now(&self, read: Read) -> NetResult<Page> {
        let sp = spec(read);
        let body = self.fetch(sp.endpoint, sp.params).await?;
        self.parse(sp.parser, body)
    }
}

/// Asked only in Rust, so not exported to Kotlin.
impl Client {
    /// The stored answer for `read`, parsed, and whether it is young enough to skip the server. A stored
    /// answer that no longer parses is not shown, and the server is asked. Reads that are never stored
    /// come back empty and not fresh.
    pub fn read_stored(&self, read: Read) -> NetResult<Stored> {
        let fresh_ms = spec(read.clone()).fresh_ms;
        self.stored_for(read, fresh_ms)
    }

    /// [`Client::read_stored`] with its own measure of young enough: fresh when stored less than
    /// `fresh_ms` ago, whatever the read's usual time is (a downloaded song's lyrics stand for longer).
    pub fn read_stored_within(&self, read: Read, fresh_ms: i64) -> NetResult<Stored> {
        let stored = spec(read.clone()).fresh_ms.is_some();
        self.stored_for(read, stored.then_some(fresh_ms))
    }

    fn stored_for(&self, read: Read, fresh_ms: Option<i64>) -> NetResult<Stored> {
        let sp = spec(read);
        let Some(fresh_ms) = fresh_ms else { return Ok(Stored { page: None, digest: None, fresh: false }) };
        let k = key(sp.endpoint, &self.scoped(sp.endpoint, sp.params));
        let row: Option<(Vec<u8>, i64)> = {
            let c = self.core.db.lock();
            let mut st = c.prepare_cached("SELECT body, ts FROM cache WHERE server=sid() AND key=?1")?;
            st.query_row([k], |r| Ok((r.get(0)?, r.get(1)?))).optional()?
        };
        let Some((body, ts)) = row else { return Ok(Stored { page: None, digest: None, fresh: false }) };
        let digest = Some(digest(&body));
        let page = self.parse(sp.parser, body).ok();
        let fresh = page.as_ref().is_some_and(|p| !lists_provider_items(p)) && crate::db::now_ms() - ts < fresh_ms;
        Ok(Stored { page, digest, fresh })
    }

    /// Asks the server. The answer is returned when it differs from what was stored (`stored_digest`,
    /// None when nothing was) and stored again either way: an unchanged answer restarts the freshness
    /// window. A read that is never stored always returns its page.
    pub async fn read_fetch(&self, read: Read, stored_digest: Option<u64>) -> NetResult<Option<Page>> {
        let sp = spec(read);
        if sp.fresh_ms.is_none() {
            let body = self.fetch(sp.endpoint, sp.params).await?;
            return Ok(Some(self.parse(sp.parser, body)?));
        }
        let k = key(sp.endpoint, &self.scoped(sp.endpoint, sp.params.clone()));
        let body = self.fetch(sp.endpoint, sp.params).await?;
        let page = if stored_digest != Some(digest(&body)) { Some(self.parse(sp.parser, body.clone())?) } else { None };
        self.core.cache_put(k, body)?;
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::{block, client, Fake};
    use crate::client::NetProfile;
    use crate::transport::FailureKind;
    use std::sync::Arc;

    const GENRES: &str = r#"{"subsonic-response":{"status":"ok","genres":{"genre":[{"value":"Rock","songCount":3}]}}}"#;
    const GENRES2: &str = r#"{"subsonic-response":{"status":"ok","genres":{"genre":[{"value":"Jazz","songCount":1}]}}}"#;

    fn genres(p: &Option<Page>) -> String {
        match p {
            Some(Page::Genres { v }) => v.iter().map(|g| g.name.clone()).collect::<Vec<_>>().join(","),
            other => panic!("{other:?}"),
        }
    }

    fn setup() -> (Arc<Client>, Arc<Fake>) {
        client(NetProfile { url: "h".into(), music_folder_id: "7".into(), ..Default::default() })
    }

    #[test]
    fn stored_answer_paints_and_a_fresh_one_skips_the_server() {
        let (c, fake) = setup();
        let s = c.read_stored(Read::GenreList).unwrap();
        assert!(s.page.is_none() && s.digest.is_none() && !s.fresh);
        fake.answer(GENRES);
        assert_eq!(genres(&block(c.read_fetch(Read::GenreList, s.digest)).unwrap()), "Rock");

        let s = c.read_stored(Read::GenreList).unwrap();
        assert_eq!(genres(&s.page), "Rock");
        assert!(s.fresh, "an hour's window");
        assert_eq!(fake.asked().len(), 1);
    }

    #[test]
    fn unchanged_answer_is_not_emitted_again_but_restarts_the_window() {
        let (c, fake) = setup();
        c.core.cache_put("getGenres".into(), GENRES.into()).unwrap();
        c.core.db.lock().execute("UPDATE cache SET ts = 0", []).unwrap();
        let s = c.read_stored(Read::GenreList).unwrap();
        assert!(s.page.is_some() && !s.fresh);
        fake.answer(GENRES);
        assert!(block(c.read_fetch(Read::GenreList, s.digest)).unwrap().is_none());
        assert!(c.read_stored(Read::GenreList).unwrap().fresh);

        c.core.db.lock().execute("UPDATE cache SET ts = 0", []).unwrap();
        fake.answer(GENRES2);
        assert_eq!(genres(&block(c.read_fetch(Read::GenreList, s.digest)).unwrap()), "Jazz");
    }

    #[test]
    fn a_page_with_a_providers_songs_is_shown_stored_and_always_asked_again() {
        let (c, fake) = setup();
        let album = |ext: bool| {
            format!(
                r#"{{"subsonic-response":{{"status":"ok","album":{{"id":"ext-deezer-album-1","name":"A","song":[{{"id":"ext-deezer-song-1","title":"t","isExternal":{ext}}},{{"id":"s2","title":"u"}}]}}}}}}"#
            )
        };
        let read = || Read::AlbumById { id: "ext-deezer-album-1".into() };
        fake.answer(&album(true));
        block(c.read_fetch(read(), None)).unwrap();
        let s = c.read_stored(read()).unwrap();
        assert!(s.page.is_some() && !s.fresh, "the provider's song may be the library's by now");

        let library = r#"{"subsonic-response":{"status":"ok","album":{"id":"al-1","name":"A","song":[{"id":"s1","title":"t"}]}}}"#;
        fake.answer(library);
        block(c.read_fetch(Read::AlbumById { id: "al-1".into() }, None)).unwrap();
        assert!(c.read_stored(Read::AlbumById { id: "al-1".into() }).unwrap().fresh, "the library's own keeps its window");
    }

    #[test]
    fn unparseable_stored_answer_is_not_shown_and_not_fresh() {
        let (c, _) = setup();
        c.core.cache_put("getGenres".into(), b"garbage".to_vec()).unwrap();
        let s = c.read_stored(Read::GenreList).unwrap();
        assert!(s.page.is_none() && s.digest.is_some() && !s.fresh);
    }

    #[test]
    fn keys_carry_the_folder_and_the_parameters_in_order() {
        let (c, fake) = setup();
        fake.answer(r#"{"subsonic-response":{"status":"ok","albumList2":{"album":[]}}}"#);
        block(c.read_fetch(Read::AlbumList { kind: "starred".into(), size: 20, offset: 0, genre: None }, None)).unwrap();
        assert!(c.core.cache_get("getAlbumList2&type=starred&size=20&offset=0&musicFolderId=7".into()).unwrap().is_some());
        assert!(fake.asked()[0].ends_with("&type=starred&size=20&offset=0&musicFolderId=7"));
        fake.answer(r#"{"subsonic-response":{"status":"ok","lyricsList":{}}}"#);
        block(c.read_fetch(Read::LyricsBySong { song_id: "s 1".into() }, None)).unwrap();
        assert!(c.core.cache_get("getLyricsBySongId&id=s 1&enhanced=true".into()).unwrap().is_some());
    }

    #[test]
    fn favourite_albums_share_the_starred_list_and_wear_this_sessions_marks() {
        let (c, fake) = setup();
        fake.answer(r#"{"subsonic-response":{"status":"ok","albumList2":{"album":[{"id":"fa-1","name":"A"},{"id":"fa-2","name":"B"}]}}}"#);
        block(c.read_fetch(Read::AlbumList { kind: "starred".into(), size: 20, offset: 0, genre: None }, None)).unwrap();
        crate::stars::star_mark(crate::client::Starrable::Album, "fa-2".into(), false);
        match c.read_stored(Read::FavouriteAlbums { size: 20 }).unwrap().page {
            Some(Page::Albums { v }) => assert_eq!(v.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["fa-1"]),
            other => panic!("{other:?}"),
        }
        match c.read_stored(Read::AlbumList { kind: "starred".into(), size: 20, offset: 0, genre: None }).unwrap().page {
            Some(Page::Albums { v }) => assert_eq!(v.len(), 2, "the album grid is not a favourites answer"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn random_is_never_stored_and_by_year_means_this_year() {
        let (c, fake) = setup();
        let random = Read::AlbumList { kind: "random".into(), size: 5, offset: 0, genre: Some("Rock".into()) };
        assert!(c.read_stored(random.clone()).unwrap().digest.is_none());
        let list = r#"{"subsonic-response":{"status":"ok","albumList2":{"album":[{"id":"a","name":"A"}]}}}"#;
        fake.answer(list);
        assert!(block(c.read_fetch(random.clone(), None)).unwrap().is_some());
        fake.answer(list);
        assert!(block(c.read_fetch(random, None)).unwrap().is_some(), "every time");
        assert_eq!(c.core.db.lock().query_row("SELECT count(*) FROM cache", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert!(fake.asked()[0].ends_with("&type=random&size=5&offset=0&genre=Rock&musicFolderId=7"));

        fake.answer(list);
        block(c.read_fetch(Read::AlbumList { kind: "byYear".into(), size: 50, offset: 0, genre: None }, None)).unwrap();
        let year = this_year();
        assert!(year >= 2024);
        assert!(fake.asked()[2].ends_with(&format!("&type=byYear&fromYear={year}&toYear=0&size=50&offset=0&musicFolderId=7")));
    }

    #[test]
    fn failure_reaches_the_caller() {
        let (c, fake) = setup();
        fake.fail(FailureKind::Connect);
        assert!(block(c.read_fetch(Read::GenreList, None)).is_err());
        fake.answer(r#"{"subsonic-response":{"status":"ok","song":{"id":"1","title":"t"}}}"#);
        match block(c.read_now(Read::SongById { id: "1".into() })).unwrap() {
            Page::OneSong { v } => assert_eq!(v.unwrap().title, "t"),
            other => panic!("{other:?}"),
        }
    }
}
