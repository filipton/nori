//! Differential harness: every read against a corpus of answers, through `read_now` and the cache, with
//! what got indexed, written to `$NORI_DIFF_OUT/parse.txt`.

use std::fmt::Write as _;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use nori_core::cache_policy::Read;
use nori_core::client::{Client, NetProfile};
use nori_core::transport::{Exchange, Transport, TransportError, TransportResponse};
use nori_core::Core;
use parking_lot::Mutex;

struct Fixed(Mutex<Vec<u8>>);

#[async_trait::async_trait]
impl Transport for Fixed {
    async fn get(&self, _url: String, _timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse { status: 200, body: self.0.lock().clone() })
    }
    async fn send(&self, r: Exchange) -> Result<TransportResponse, TransportError> {
        self.get(r.url, 0).await
    }
    fn address_changed(&self) {}
}

fn block<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

const SONG: &str = r#"{"id":"s1","title":"One","artist":"A","album":"Al","albumId":"al1","duration":200,"track":1,"discNumber":2,"isDir":false}"#;
const SONG2: &str = r#"{"id":"ext-deezer-song-2","title":"Two","artist":"B","isExternal":true}"#;
const ALBUM: &str = r#"{"id":"al1","name":"Al","artist":"A","year":1999,"songCount":2}"#;
const ALBUM2: &str = r#"{"id":"al2","title":"Second","artist":"B","isExternal":true}"#;
const ARTIST: &str = r#"{"id":"ar1","name":"A","albumCount":3}"#;

fn everything() -> String {
    format!(
        r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1","type":"navidrome","serverVersion":"0.53","openSubsonic":true,
        "searchResult3":{{"artist":[{ARTIST}],"album":[{ALBUM},{ALBUM2}],"song":[{SONG},{SONG2}]}},
        "starred2":{{"artist":[{ARTIST}],"album":[{ALBUM}],"song":[{SONG}]}},
        "album":{{"id":"al1","name":"Al","artist":"A","song":[{SONG},{SONG2}],"discTitles":[{{"disc":2,"title":"B side"}}]}},
        "artist":{{"id":"ar1","name":"A","album":[{ALBUM},{ALBUM2}]}},
        "playlist":{{"id":"p1","name":"P","owner":"me","entry":[{SONG}]}},
        "albumList2":{{"album":[{ALBUM},{ALBUM2}]}},
        "artists":{{"ignoredArticles":"The","index":[{{"name":"A","artist":[{ARTIST}]}},{{"name":"B","artist":[{{"id":"ar2","name":"B"}}]}}]}},
        "playlists":{{"playlist":[{{"id":"p1","name":"P"}}]}},
        "genres":{{"genre":[{{"value":"Jazz","songCount":1}},{{"value":"Rock","songCount":9}}]}},
        "randomSongs":{{"song":[{SONG}]}},
        "similarSongs2":{{"song":[{SONG2}]}},
        "song":{SONG},
        "internetRadioStations":{{"internetRadioStation":[{{"id":"r1","name":"R","streamUrl":"http://r"}}]}},
        "artistInfo2":{{"biography":"  ","lastFmUrl":"ftp://x","musicBrainzId":"mb","mediumImageUrl":"http://m","similarArtist":[{ARTIST}]}},
        "lyricsList":{{"structuredLyrics":[{{"lang":"en","synced":true,"line":[{{"start":1000,"value":"Hello"}},{{"start":3000,"value":"World"}}]}}]}},
        "playQueue":{{"entry":[{SONG},{SONG2}],"current":"ext-deezer-song-2","position":1234}},
        "shares":{{"share":[{{"url":"http://share/1"}}]}},
        "musicFolders":{{"musicFolder":[{{"id":1,"name":"Music"}}]}},
        "indexes":{{"index":[{{"name":"A","artist":[{{"id":"d1","name":"Folder"}}]}}]}},
        "directory":{{"id":"d1","name":"Folder","child":[{{"id":"d2","title":"Sub","isDir":true}},{{"id":"d3","name":"Named","isDir":true}},{SONG}]}}
        }}}}"#
    )
}

fn corpus() -> Vec<(&'static str, String)> {
    let ok = |body: &str| format!(r#"{{"subsonic-response":{{"status":"ok"{body}}}}}"#);
    vec![
        ("everything", everything()),
        ("empty ok", ok("")),
        ("nulls", ok(r#","album":null,"artist":null,"playlist":null,"albumList2":null,"song":null,"shares":null,"playQueue":null,"directory":null"#)),
        ("empty lists", ok(r#","albumList2":{},"artists":{},"playlists":{},"genres":{},"shares":{"share":[{"url":""}]},"playQueue":{"current":7},"searchResult3":{},"topSongs":{"song":[]},"songsByGenre":{"song":[]}"#)),
        ("top songs", ok(&format!(r#","topSongs":{{"song":[{SONG}]}},"songsByGenre":{{"song":[{SONG2}]}}"#))),
        ("by genre", ok(&format!(r#","songsByGenre":{{"song":[{SONG2}]}},"song":{SONG}"#))),
        ("failed", r#"{"subsonic-response":{"status":"failed","error":{"code":70,"message":"not found"}}}"#.into()),
        ("failed bare", r#"{"subsonic-response":{"status":"failed"}}"#.into()),
        ("not json", "<html>".into()),
        ("wrong shape", ok(r#","albumList2":{"album":"x"}"#)),
        ("queue numeric", ok(&format!(r#","playQueue":{{"entry":[{SONG}],"current":1,"position":5}}"#))),
    ]
}

fn reads() -> Vec<Read> {
    vec![
        Read::AlbumList { kind: "newest".into(), size: 5, offset: 0, genre: Some("Rock".into()) },
        Read::AlbumList { kind: "random".into(), size: 5, offset: 0, genre: None },
        Read::AlbumsByYear { from: 1990, to: 2000, size: 5, offset: 10 },
        Read::FavouriteAlbums { size: 20 },
        Read::ArtistIndex,
        Read::AlbumById { id: "al1".into() },
        Read::ArtistById { id: "ar1".into() },
        Read::ArtistAbout { id: "ar1".into() },
        Read::TopSongs { artist: "A".into() },
        Read::PlaylistList,
        Read::PlaylistById { id: "p1".into() },
        Read::StarredItems,
        Read::GenreList,
        Read::RadioList,
        Read::LyricsBySong { song_id: "s1".into() },
        Read::FolderIndex,
        Read::FolderById { id: "d1".into() },
        Read::MusicFolders,
        Read::Ping,
        Read::Search { query: "a".into(), songs: 5, albums: 5, artists: 5 },
        Read::RandomSongs { size: 5, genre: None },
        Read::SongsByGenre { genre: "Rock".into(), count: 5 },
        Read::SimilarSongs { id: "s1".into(), count: 5 },
        Read::SongById { id: "s1".into() },
        Read::AlbumSongs { id: "al1".into() },
        Read::PlaylistSongs { id: "p1".into() },
        Read::NowPlaying { id: "s1".into() },
        Read::ShareLink { id: "s1".into() },
        Read::PullQueue,
    ]
}

#[test]
fn transcribe() {
    let Some(dir) = std::env::var_os("NORI_DIFF_OUT") else { return };
    let mut log = String::new();
    for (name, body) in corpus() {
        for read in reads() {
            let core = Core::new(String::new(), "diff".into()).unwrap();
            let t = Arc::new(Fixed(Mutex::new(body.clone().into_bytes())));
            let client = Client::new(core.clone(), t);
            client.set_profile(NetProfile { url: "http://h".into(), ..Default::default() });
            let now = block(client.read_now(read.clone()));
            let _ = writeln!(log, "{name} {read:?}\n  now {now:?}\n  indexed {:?}", core.index_size().unwrap());
            let mut seen = Vec::new();
            let each = block(client.read_each(read.clone(), |p| seen.push(format!("{p:?}"))));
            let _ = writeln!(log, "  each {each:?} {seen:?}");
            let mut seen = Vec::new();
            let again = block(client.read_each(read.clone(), |p| seen.push(format!("{p:?}"))));
            let _ = writeln!(log, "  again {again:?} {seen:?}");
        }
    }
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(std::path::Path::new(&dir).join("parse.txt"), log).unwrap();
}
