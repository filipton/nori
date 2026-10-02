//! Every server read against one answer that holds every payload: what page it makes and what it
//! indexes.

use std::sync::Arc;

use nori_core::cache_policy::{Page, Read};
use nori_core::client::{Client, NetProfile};
use nori_core::transport::{block_on, Exchange, Transport, TransportError, TransportResponse};
use nori_core::Core;

struct Answer(String);

#[async_trait::async_trait]
impl Transport for Answer {
    async fn get(&self, _: String, _: u32) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse { status: 200, body: self.0.clone().into_bytes() })
    }
    async fn send(&self, r: Exchange) -> Result<TransportResponse, TransportError> {
        self.get(r.url, 0).await
    }
    fn address_changed(&self) {}
}

const SONG: &str = r#"{"id":"s1","title":"One","artist":"A","album":"Al","albumId":"al1","isDir":false}"#;
const PROVIDER_SONG: &str = r#"{"id":"ext-deezer-song-2","title":"Two","isExternal":true}"#;
const ALBUM: &str = r#"{"id":"al1","name":"Al","artist":"A","year":1999}"#;
const PROVIDER_ALBUM: &str = r#"{"id":"al2","name":"Second","isExternal":true}"#;
const ARTIST: &str = r#"{"id":"ar1","name":"A"}"#;

fn everything() -> String {
    format!(
        r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1","type":"navidrome","serverVersion":"0.53","openSubsonic":true,
        "searchResult3":{{"artist":[{ARTIST}],"album":[{ALBUM},{PROVIDER_ALBUM}],"song":[{SONG},{PROVIDER_SONG}]}},
        "starred2":{{"artist":[{ARTIST}],"album":[{ALBUM}],"song":[{SONG}]}},
        "album":{{"id":"al1","name":"Al","song":[{SONG},{PROVIDER_SONG}],"discTitles":[{{"disc":2,"title":"B side"}}]}},
        "artist":{{"id":"ar1","name":"A","album":[{ALBUM},{PROVIDER_ALBUM}]}},
        "playlist":{{"id":"p1","name":"P","entry":[{SONG}]}},
        "albumList2":{{"album":[{ALBUM},{PROVIDER_ALBUM}]}},
        "artists":{{"index":[{{"name":"A","artist":[{ARTIST}]}},{{"name":"B","artist":[{{"id":"ar2","name":"B"}}]}}]}},
        "playlists":{{"playlist":[{{"id":"p1","name":"P"}}]}},
        "genres":{{"genre":[{{"value":"Jazz","songCount":1}},{{"value":"Rock","songCount":9}}]}},
        "randomSongs":{{"song":[{SONG}]}},
        "song":{PROVIDER_SONG},
        "internetRadioStations":{{"internetRadioStation":[{{"id":"r1","name":"R","streamUrl":"http://r"}}]}},
        "artistInfo2":{{"biography":"  ","lastFmUrl":"ftp://x","musicBrainzId":"mb","mediumImageUrl":"http://m","similarArtist":[{ARTIST}]}},
        "lyricsList":{{"structuredLyrics":[{{"synced":true,"line":[{{"start":1000,"value":"Hello"}}]}}]}},
        "playQueue":{{"entry":[{SONG},{PROVIDER_SONG}],"current":"ext-deezer-song-2","position":1234}},
        "shares":{{"share":[{{"url":"http://share/1"}}]}},
        "musicFolders":{{"musicFolder":[{{"id":1,"name":"Music"}}]}},
        "indexes":{{"index":[{{"name":"A","artist":[{{"id":"d1","name":"Folder"}}]}}]}},
        "directory":{{"id":"d1","name":"Folder","child":[{{"id":"d2","title":"Sub","isDir":true}},{SONG}]}}
        }}}}"#
    )
}

fn ids<T>(v: &[T], id: impl Fn(&T) -> &str) -> String {
    v.iter().map(id).collect::<Vec<_>>().join(",")
}

/// What a page holds, by ids and the fields a read decides.
fn said(p: &Page) -> String {
    fn song(s: &nori_core::Song) -> &str {
        &s.id
    }
    fn album(a: &nori_core::Album) -> &str {
        &a.id
    }
    match p {
        Page::Albums { v } => format!("albums {}", ids(v, album)),
        Page::Artists { v } => format!("artists {}", ids(v, |a| &a.id)),
        Page::AlbumPage { v } => format!("album {} songs {} discs {}", v.album.id, ids(&v.songs, song), ids(&v.disc_titles, |d| &d.title)),
        Page::ArtistPage { v } => format!("artist {} albums {}", v.artist.id, ids(&v.albums, album)),
        Page::About { v } => format!("about {:?} {:?} {:?} {:?} {}", v.biography, v.image_url, v.last_fm_url, v.music_brainz_id, ids(&v.similar, |a| &a.id)),
        Page::Songs { v } => format!("songs {}", ids(v, song)),
        Page::OneSong { v } => format!("song {:?}", v.as_ref().map(|s| &s.id)),
        Page::Playlists { v } => format!("playlists {}", ids(v, |p| &p.id)),
        Page::PlaylistPage { v } => format!("playlist {} songs {}", v.playlist.id, ids(&v.songs, song)),
        Page::StarredPage { v } => format!("starred {} {} {}", ids(&v.artists, |a| &a.id), ids(&v.albums, album), ids(&v.songs, song)),
        Page::Genres { v } => format!("genres {}", ids(v, |g| &g.name)),
        Page::Stations { v } => format!("stations {}", ids(v, |s| &s.id)),
        Page::LyricsPage { v } => format!("lyrics {} {}", v.synced, ids(&v.lines, |l| &l.text)),
        Page::DirectoryPage { v } => format!("folder {} {} folders {} songs {}", v.id, v.name, ids(&v.folders, |a| &a.name), ids(&v.songs, song)),
        Page::Found { v } => format!("found {} {} {}", ids(&v.artists, |a| &a.id), ids(&v.albums, album), ids(&v.songs, song)),
        Page::Folders { v } => format!("folders {}", ids(v, |f| &f.name)),
        Page::Status { v } => format!("status {} {} {} {}", v.version, v.server_type, v.server_version, v.open_subsonic),
        Page::ShareUrl { v } => format!("share {v}"),
        Page::Queue { v } => format!("queue {} at {} {} ms", ids(&v.songs, song), v.index, v.position_ms),
    }
}

#[test]
fn every_read_makes_page_and_index() {
    let id = || "x".to_string();
    let reads = [
        (Read::AlbumList { kind: "newest".into(), size: 5, offset: 0, genre: None }, "albums al1,al2", (0, 1, 0)),
        (Read::FavouriteAlbums { size: 5 }, "albums al1,al2", (0, 1, 0)),
        (Read::ArtistIndex, "artists ar1,ar2", (2, 0, 0)),
        (Read::AlbumById { id: id() }, "album al1 songs s1,ext-deezer-song-2 discs B side", (0, 1, 1)),
        (Read::AlbumSongs { id: id() }, "songs s1,ext-deezer-song-2", (0, 0, 1)),
        (Read::ArtistById { id: id() }, "artist ar1 albums al1,al2", (1, 1, 0)),
        (Read::ArtistAbout { id: id() }, r#"about None Some("http://m") None Some("mb") ar1"#, (0, 0, 0)),
        (Read::RandomSongs { size: 5, genre: None }, "songs s1", (0, 0, 1)),
        // randomSongs comes before a single song.
        (Read::SongById { id: id() }, r#"song Some("s1")"#, (0, 0, 1)),
        (Read::PlaylistList, "playlists p1", (0, 0, 0)),
        (Read::PlaylistById { id: id() }, "playlist p1 songs s1", (0, 0, 1)),
        (Read::PlaylistSongs { id: id() }, "songs s1", (0, 0, 1)),
        (Read::StarredItems, "starred ar1 al1 s1", (1, 1, 1)),
        (Read::GenreList, "genres Rock,Jazz", (0, 0, 0)),
        (Read::RadioList, "stations r1", (0, 0, 0)),
        (Read::LyricsBySong { song_id: id() }, "lyrics true Hello", (0, 0, 0)),
        (Read::FolderIndex, "artists d1", (0, 0, 0)),
        (Read::FolderById { id: id() }, "folder d1 Folder folders Sub songs s1", (0, 0, 0)),
        (Read::MusicFolders, "folders Music", (0, 0, 0)),
        (Read::Ping, "status 1.16.1 navidrome 0.53 true", (0, 0, 0)),
        (Read::Search { query: id(), songs: 5, albums: 5, artists: 5 }, "found ar1 al1,al2 s1,ext-deezer-song-2", (1, 1, 1)),
        (Read::ShareLink { id: id() }, "share http://share/1", (0, 0, 0)),
        (Read::PullQueue, "queue s1,ext-deezer-song-2 at 1 1234 ms", (0, 0, 0)),
    ];
    for (read, want, (artists, albums, songs)) in reads {
        let core = Core::new(String::new(), "reads".into()).unwrap();
        let client = Client::new(core.clone(), Arc::new(Answer(everything())));
        client.set_profile(NetProfile { url: "http://h".into(), ..Default::default() });
        let page = block_on(client.read_now(read.clone())).unwrap();
        assert_eq!(said(&page), want, "{read:?}");
        let n = core.index_size().unwrap();
        assert_eq!((n.artists, n.albums, n.songs), (artists, albums, songs), "indexed by {read:?}");
    }
}

#[test]
fn an_empty_share_is_no_share() {
    let core = Core::new(String::new(), "reads".into()).unwrap();
    let client = Client::new(core, Arc::new(Answer(r#"{"subsonic-response":{"status":"ok","shares":{"share":[{"url":""}]}}}"#.into())));
    client.set_profile(NetProfile { url: "http://h".into(), ..Default::default() });
    assert!(block_on(client.read_now(Read::ShareLink { id: "s".into() })).is_err());
}
