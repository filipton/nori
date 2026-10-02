//! Whole flows through the core's public calls against a small Subsonic server kept in memory: log in,
//! fill the offline index, search it, change things while the server is unreachable and see them reach
//! it in order once it is back. The server answers as Navidrome does, from its own library.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use nori_core::client::{login_check, Client, NetProfile, Starrable, Write};
use nori_core::library::StarsShown;
use nori_core::stars::StarMarks;
use nori_core::transport::{Exchange, FailureKind, Transport, TransportError, TransportResponse};
use nori_core::{Core, IngestStats, ServerConfig};
use parking_lot::Mutex;

/// A song of the server's library: id, title, artist, album id, genre.
type Track = (&'static str, &'static str, &'static str, &'static str, &'static str);

const LIBRARY: [Track; 5] = [
    ("s1", "Dogs", "Pink Floyd", "al1", "Rock"),
    ("s2", "Pigs", "Pink Floyd", "al1", "Rock"),
    ("s3", "Sheep", "Pink Floyd", "al1", "Rock"),
    ("s4", "So What", "Miles Davis", "al2", "Jazz"),
    ("s5", "Blue in Green", "Miles Davis", "al2", "Jazz"),
];

/// Songs octo-fiesta offers from a provider: never in the library, downloaded when streamed.
const PROVIDER: [Track; 1] = [("ext-deezer-song-9", "Wish You Were Here", "Pink Floyd", "ext-deezer-album-1", "Rock")];

struct Server {
    reachable: AtomicBool,
    starred: Mutex<BTreeSet<String>>,
    /// Changes the server took, as "endpoint id".
    took: Mutex<Vec<String>>,
}

fn song_json(t: &Track, starred: bool) -> String {
    let (id, title, artist, album, genre) = t;
    let provider = if id.starts_with("ext-") { r#","isExternal":true"# } else { "" };
    let star = if starred { r#","starred":"2026-01-01T00:00:00Z""# } else { "" };
    format!(r#"{{"id":"{id}","title":"{title}","artist":"{artist}","album":"{album}","albumId":"{album}","genre":"{genre}","duration":300,"isDir":false{provider}{star}}}"#)
}

fn ok(body: &str) -> Vec<u8> {
    format!(r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1"{}{body}}}}}"#, if body.is_empty() { "" } else { "," }).into_bytes()
}

/// `%XX` escapes decoded.
fn decode(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())) {
            (b'%', Some(x)) => {
                out.push(x);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

impl Server {
    fn new() -> Arc<Server> {
        Arc::new(Server { reachable: AtomicBool::new(true), starred: Mutex::new(BTreeSet::new()), took: Mutex::new(Vec::new()) })
    }

    fn reachable(&self, on: bool) {
        self.reachable.store(on, Ordering::SeqCst);
    }

    fn took(&self) -> Vec<String> {
        self.took.lock().clone()
    }

    fn songs(&self, of: impl Fn(&Track) -> bool) -> String {
        let starred = self.starred.lock();
        LIBRARY.iter().chain(&PROVIDER).filter(|t| of(t)).map(|t| song_json(t, starred.contains(t.0))).collect::<Vec<_>>().join(",")
    }

    fn answer(&self, endpoint: &str, p: &HashMap<String, String>) -> Vec<u8> {
        let num = |k: &str| p.get(k).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        let id = p.get("id").cloned().unwrap_or_default();
        match endpoint {
            "ping" => ok(""),
            "search3" => {
                let query = p.get("query").cloned().unwrap_or_default().to_lowercase();
                let hits: Vec<&Track> = LIBRARY.iter().filter(|t| t.1.to_lowercase().contains(&query)).collect();
                let page: Vec<String> = hits.iter().skip(num("songOffset")).take(num("songCount")).map(|t| song_json(t, false)).collect();
                ok(&format!(r#""searchResult3":{{"song":[{}]}}"#, page.join(",")))
            }
            "getSong" => ok(&format!(r#""song":{}"#, self.songs(|t| t.0 == id))),
            "getSimilarSongs2" => {
                let genre = LIBRARY.iter().chain(&PROVIDER).find(|t| t.0 == id).map_or("", |t| t.4);
                ok(&format!(r#""similarSongs2":{{"song":[{}]}}"#, self.songs(|t| t.4 == genre && t.0 != id)))
            }
            "star" | "unstar" | "scrobble" => {
                self.took.lock().push(format!("{endpoint} {id}"));
                match endpoint {
                    "star" => self.starred.lock().insert(id),
                    "unstar" => self.starred.lock().remove(&id),
                    _ => true,
                };
                ok("")
            }
            "getStarred2" => ok(&format!(r#""starred2":{{"song":[{}]}}"#, self.songs(|t| self.starred.lock().contains(t.0)))),
            _ => format!(r#"{{"subsonic-response":{{"status":"failed","error":{{"code":0,"message":"no {endpoint}"}}}}}}"#).into_bytes(),
        }
    }
}

#[async_trait::async_trait]
impl Transport for Server {
    async fn get(&self, url: String, _timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        if !self.reachable.load(Ordering::SeqCst) {
            return Err(TransportError::Failed { kind: FailureKind::Connect, detail: Some("unreachable".into()) });
        }
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        let endpoint = path.rsplit('/').next().unwrap_or_default();
        let params = query.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), decode(v))).collect();
        Ok(TransportResponse { status: 200, body: self.answer(endpoint, &params) })
    }

    async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError> {
        self.get(request.url, request.timeout_ms).await
    }

    fn address_changed(&self) {}

    fn network(&self) -> nori_core::transport::Network {
        nori_core::transport::Network::Unmetered
    }
}

/// Polls `f` to completion; the server answers at once.
fn block<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

struct Unseen;

impl StarsShown for Unseen {
    fn marks(&self, _: StarMarks) {}
}

fn logged_in(server: &Arc<Server>) -> (Arc<Core>, Arc<Client>) {
    let config = ServerConfig { url: "http://nas:4533".into(), user: "u".into(), password: "p".into(), ..Default::default() };
    assert!(!block(login_check(server.clone(), config.clone(), String::new())).unwrap(), "token auth");
    let core = Core::new(String::new(), "scenario".into()).unwrap();
    core.configure(config).unwrap();
    let client = Client::new(core.clone(), server.clone(), Default::default());
    client.set_profile(NetProfile { url: "http://nas:4533".into(), ..Default::default() });
    (core, client)
}

#[test]
fn offline_changes_reach_server() {
    let server = Server::new();
    let (core, client) = logged_in(&server);
    let (mut total, mut offset) = (IngestStats::default(), 0);
    while let Some(next) = {
        let step = block(client.sync_page(offset, 2, total)).unwrap();
        total = step.total;
        step.next_offset
    } {
        offset = next;
    }
    assert_eq!(core.index_size().unwrap().songs, 5);
    assert_eq!(core.local_search("blue gre".into(), 5).unwrap().songs[0].id, "s5");

    server.reachable(false);
    block(client.star(Starrable::Song, "s1".into(), true, Arc::new(Unseen))).unwrap();
    block(client.write(Write::Scrobble { id: "s2".into(), submission: true, time_ms: Some(1) })).unwrap();
    block(client.star(Starrable::Song, "s1".into(), false, Arc::new(Unseen))).unwrap();
    assert!(server.took().is_empty());

    server.reachable(true);
    block(client.star(Starrable::Song, "s3".into(), true, Arc::new(Unseen))).unwrap();
    block(client.flush_pending()).unwrap();
    assert_eq!(server.took(), ["star s1", "scrobble s2", "unstar s1", "star s3"], "in order, each once");
    assert_eq!(server.starred.lock().iter().collect::<Vec<_>>(), ["s3"]);
}

#[test]
fn a_radio_plays_the_library_only() {
    let server = Server::new();
    let (_, client) = logged_in(&server);
    let radio: Vec<String> = block(client.radio("s1".into())).unwrap().into_iter().map(|s| s.id).collect();
    assert_eq!(radio, ["s1", "s2", "s3"], "the seed, then its like, no provider song");
}
