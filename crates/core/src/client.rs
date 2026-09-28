//! The Subsonic client: every request the app makes to its server goes through here. It picks the
//! address, adds the music folder, retries once through the profile's other address, keeps writes that
//! could not be sent and replays them, and walks the library into the index. The platform supplies the
//! GET ([`Transport`]); the reads and their cache are in cache_policy.rs, lyrics from LRCLIB in lrclib.rs,
//! the audio URLs in stream.rs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::transport::{self, FailureKind, NetError, Transport};
use crate::{Core, IngestStats, ServerConfig};

pub use nori_net::requests::{NetProfile, NetResult, Starrable, SyncStep, Write};
pub(crate) use nori_net::requests::{blank, pairs, request, FOLDERED};

/// How long the first address gets to answer before the second one is used.
const ADDRESS_PROBE_MS: u32 = 2_500;

/// The client for one server profile, over that profile's core (its index and caches).
#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct Client {
    pub(crate) core: Arc<Core>,
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) profile: RwLock<NetProfile>,
    /// True while requests go to the profile's second address; stream quality is capped then.
    pub(crate) second: AtomicBool,
    /// What this client's lyrics lookups remember of each other (services failing lately, YouTube matches).
    pub(crate) lyrics: nori_lyrics::services::LyricsMemory,
}

/// The client the app streams through now: for a player that opens its songs in Rust (`stream::resolve_now`).
static ACTIVE_CLIENT: parking_lot::Mutex<std::sync::Weak<Client>> = parking_lot::Mutex::new(std::sync::Weak::new());

pub(crate) fn active_client() -> Option<Arc<Client>> {
    ACTIVE_CLIENT.lock().upgrade()
}

async fn ping(core: &Core, transport: &dyn Transport, timeout_ms: u32) -> NetResult<()> {
    let url = core.server.read().url("ping", &[]);
    core.parse_status(transport::get(transport, url, timeout_ms).await?)?;
    Ok(())
}

impl Client {
    /// The parameters with the music folder added, for the endpoints that take one.
    pub(crate) fn scoped(&self, endpoint: &str, mut params: Vec<(String, String)>) -> Vec<(String, String)> {
        let p = self.profile.read();
        if !p.music_folder_id.is_empty() && FOLDERED.contains(&endpoint) {
            params.push(("musicFolderId".into(), p.music_folder_id.clone()));
        }
        params
    }

    pub(crate) async fn get(&self, endpoint: &str, params: &[(String, String)]) -> NetResult<Vec<u8>> {
        let url = self.core.server.read().url(endpoint, params);
        transport::get(&*self.transport, url, 0).await
    }

    /// One request; if the server is unreachable and the profile has a second address, that is tried
    /// once. Not when the phone is on a metered network and the server is Wi-Fi only: the other address
    /// is the same server and the same rule.
    pub(crate) async fn fetch(&self, endpoint: &str, params: Vec<(String, String)>) -> NetResult<Vec<u8>> {
        let p = self.scoped(endpoint, params);
        match self.get(endpoint, &p).await {
            Err(e) if e.is_io() && !matches!(e, NetError::Transport { kind: FailureKind::Metered, .. }) => {
                if !self.choose_address().await {
                    return Err(e);
                }
                self.get(endpoint, &p).await
            }
            r => r,
        }
    }

    /// A write that cannot reach the server is kept and replayed later, in order, so stars, playlist
    /// edits and plays made offline are not lost. The caller is told it worked either way. Each write
    /// drops the stored reads it makes stale.
    pub(crate) async fn write_raw(&self, endpoint: &str, params: Vec<(String, String)>, stale: &[&str]) -> NetResult<()> {
        match self.fetch(endpoint, params.clone()).await {
            Ok(body) => {
                self.core.parse_status(body)?;
                self.flush_pending().await?;
            }
            Err(e) if e.is_io() => {
                let p = params.into_iter().map(|(key, value)| crate::Param { key, value }).collect();
                self.core.pending_add(endpoint.to_string(), p)?;
            }
            Err(e) => return Err(e),
        }
        for s in stale {
            self.core.cache_evict(s.to_string())?;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new(core: Arc<Core>, transport: Arc<dyn Transport>) -> Arc<Self> {
        let client = Arc::new(Client { core, transport, profile: RwLock::new(NetProfile::default()), second: AtomicBool::new(false), lyrics: Default::default() });
        // The newest client is the one the app streams through, as the newest core is the one it uses.
        *ACTIVE_CLIENT.lock() = Arc::downgrade(&client);
        client
    }

    /// The profile's addresses, folder and bitrate cap; called when the profile is opened or edited.
    pub fn set_profile(&self, profile: NetProfile) {
        if !blank(&profile.alt_url) {
            crate::covers::cover_address_alike(&profile.url, &profile.alt_url);
        }
        *self.profile.write() = profile;
    }

    /// True while requests go to the profile's second address.
    pub fn on_second_address(&self) -> bool {
        self.second.load(Ordering::Relaxed)
    }

    /// A profile with two addresses: ask the first one, briefly; if it does not answer use the second.
    /// Runs when the app comes to the foreground and after a request failed, never on a timer. Returns
    /// true when the address in use changed.
    pub async fn choose_address(&self) -> bool {
        let (url, alt) = {
            let p = self.profile.read();
            (p.url.clone(), p.alt_url.clone())
        };
        if blank(&alt) {
            return false;
        }
        self.core.use_address(url);
        let first_answers = ping(&self.core, &*self.transport, ADDRESS_PROBE_MS).await.is_ok();
        if !first_answers {
            self.core.use_address(alt);
        }
        let changed = self.second.swap(!first_answers, Ordering::Relaxed) == first_answers;
        if changed {
            self.transport.address_changed();
        }
        changed
    }

    /// Checks a profile against the server before it is kept, on a core opened for it. When the first
    /// address does not answer the second is asked. Servers without token auth say so (error 41) and are
    /// asked again with legacy auth; returns true when that is what worked, so the profile remembers it.
    pub async fn login(&self, config: ServerConfig, alt_url: String) -> NetResult<bool> {
        let legacy_possible = !config.legacy_auth && config.api_key.as_deref().unwrap_or("").is_empty();
        match self.attempt(config.clone(), &alt_url).await {
            Ok(()) => Ok(false),
            Err(NetError::Api { code: 41, .. }) if legacy_possible => {
                self.attempt(ServerConfig { legacy_auth: true, ..config }, &alt_url).await?;
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    /// Replays queued writes. Stops at the first network failure; a write the server rejects is dropped,
    /// since replaying it again would not help.
    pub async fn flush_pending(&self) -> NetResult<()> {
        for p in self.core.pending_list()? {
            let params: Vec<(String, String)> = p.params.into_iter().map(|p| (p.key, p.value)).collect();
            match self.get(&p.endpoint, &params).await {
                Err(e) if e.is_io() => return Ok(()),
                Ok(body) => {
                    let _ = self.core.parse_status(body);
                }
                Err(_) => {}
            }
            self.core.pending_done(p.row_id)?;
        }
        Ok(())
    }

    /// One page of the library walk: the page goes from the socket into SQLite here, only the counters
    /// come back. `total` is what the earlier pages brought; the step says where to go on from, or
    /// nothing once a page brought nothing.
    pub async fn sync_page(&self, offset: u32, page: u32, total: IngestStats) -> NetResult<SyncStep> {
        let n = page.to_string();
        let o = offset.to_string();
        let p = pairs(&[
            ("query", String::new()),
            ("songCount", n.clone()),
            ("songOffset", o.clone()),
            ("albumCount", n.clone()),
            ("albumOffset", o.clone()),
            ("artistCount", n),
            ("artistOffset", o),
        ]);
        let seen = self.core.ingest_search(self.fetch("search3", p).await?)?;
        let total = IngestStats { artists: total.artists + seen.artists, albums: total.albums + seen.albums, songs: total.songs + seen.songs };
        let empty = seen.songs == 0 && seen.albums == 0 && seen.artists == 0;
        Ok(SyncStep { total, next_offset: if empty { None } else { Some(offset + page) } })
    }

    /// Throws away the stored answers whose key starts with one of `prefixes`, so the next read of them
    /// has to ask the server. A key is the endpoint followed by its parameters in the order they were
    /// passed, so an endpoint on its own drops every read of it. This is what a manual refresh is for: the
    /// freshness window exists so that browsing costs no requests, and the only way past it is to be told
    /// the stored answer is not wanted.
    pub fn drop_cached(&self, prefixes: Vec<String>) -> NetResult<()> {
        for p in prefixes {
            self.core.cache_evict(p)?;
        }
        Ok(())
    }
}

impl Client {
    async fn attempt(&self, config: ServerConfig, alt_url: &str) -> NetResult<()> {
        self.core.configure(config)?;
        let first = ping(&self.core, &*self.transport, 0).await;
        if first.is_err() && !blank(alt_url) {
            self.core.use_address(alt_url.to_string());
            ping(&self.core, &*self.transport, 0).await
        } else {
            first
        }
    }
}

/// The app's one database file, and dropping a removed profile's rows from it: the database's own.
pub use nori_db::{db_file_name, db_forget_server, DB_FILE};

// ---- writes -------------------------------------------------------------------------------------------

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Sends one change, or keeps it for later when the server cannot be reached.
    pub async fn write(&self, w: Write) -> NetResult<()> {
        let (endpoint, params, stale) = request(w);
        self.write_raw(endpoint, params, stale).await
    }
}

// ---- tests ---------------------------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use parking_lot::Mutex;

    use super::*;
    use crate::transport::{Exchange, TransportError, TransportResponse};

    pub const OK: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;

    /// Answers from a script, in order, and remembers every URL it was asked for (and the headers and
    /// body of a request that had them).
    #[derive(Default)]
    pub struct Fake {
        pub answers: Mutex<VecDeque<Result<(u16, Vec<u8>), FailureKind>>>,
        pub asked: Mutex<Vec<(String, u32)>>,
        pub sent: Mutex<Vec<Exchange>>,
        pub switched: Mutex<u32>,
    }

    impl Fake {
        pub fn answer(&self, body: &str) {
            self.answers.lock().push_back(Ok((200, body.as_bytes().to_vec())));
        }
        pub fn fail(&self, kind: FailureKind) {
            self.answers.lock().push_back(Err(kind));
        }
        pub fn asked(&self) -> Vec<String> {
            self.asked.lock().iter().map(|(u, _)| u.clone()).collect()
        }
    }

    #[async_trait::async_trait]
    impl Transport for Fake {
        async fn get(&self, url: String, timeout_ms: u32) -> Result<TransportResponse, TransportError> {
            self.asked.lock().push((url, timeout_ms));
            match self.answers.lock().pop_front().unwrap_or(Err(FailureKind::Connect)) {
                Ok((status, body)) => Ok(TransportResponse { status, body }),
                Err(kind) => Err(TransportError::Failed { kind, detail: Some(format!("{kind:?}")) }),
            }
        }
        async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError> {
            self.sent.lock().push(request.clone());
            self.get(request.url, request.timeout_ms).await
        }
        fn address_changed(&self) {
            *self.switched.lock() += 1;
        }
    }

    /// The fake answers at once, so a future here never has to wait.
    pub fn block<F: Future>(f: F) -> F::Output {
        let mut f = pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    pub fn client(profile: NetProfile) -> (Arc<Client>, Arc<Fake>) {
        let core = Core::new(String::new(), "t".into()).unwrap();
        core.configure(ServerConfig { url: profile.url.clone(), user: "u".into(), password: "p".into(), ..Default::default() }).unwrap();
        let fake = Arc::new(Fake::default());
        let c = Client::new(core, fake.clone());
        c.set_profile(profile);
        (c, fake)
    }

    pub fn two_addresses() -> NetProfile {
        NetProfile { url: "http://lan:4533".into(), alt_url: "https://wan.example".into(), ..Default::default() }
    }

    #[test]
    fn folder_goes_only_to_the_endpoints_that_take_it() {
        let (c, _) = client(NetProfile { url: "h".into(), music_folder_id: "3".into(), ..Default::default() });
        assert_eq!(c.scoped("search3", vec![]), vec![("musicFolderId".to_string(), "3".to_string())]);
        assert!(c.scoped("getAlbum", vec![]).is_empty());
        let (c, _) = client(NetProfile { url: "h".into(), ..Default::default() });
        assert!(c.scoped("search3", vec![]).is_empty());
    }

    #[test]
    fn unreachable_server_is_asked_again_through_the_other_address() {
        let (c, fake) = client(two_addresses());
        fake.fail(FailureKind::Connect); // the request
        fake.fail(FailureKind::Timeout); // the ping to the first address
        fake.answer(OK); // the request again, through the second
        assert!(block(c.fetch("getGenres", vec![])).is_ok());
        let asked = fake.asked();
        assert!(asked[0].starts_with("http://lan:4533/rest/getGenres"));
        assert!(asked[1].starts_with("http://lan:4533/rest/ping"));
        assert_eq!(fake.asked.lock()[1].1, 2_500);
        assert!(asked[2].starts_with("https://wan.example/rest/getGenres"));
        assert!(c.on_second_address());
        assert_eq!(*fake.switched.lock(), 1);

        // Back home: the first address answers again, so the switch goes back and is reported.
        fake.answer(OK);
        assert!(block(c.choose_address()));
        assert!(!c.on_second_address());
        assert_eq!(*fake.switched.lock(), 2);
        fake.answer(OK);
        assert!(!block(c.choose_address()), "no change, nothing to report");
    }

    #[test]
    fn metered_and_refused_requests_are_not_retried() {
        let (c, fake) = client(two_addresses());
        fake.fail(FailureKind::Metered);
        assert!(matches!(block(c.fetch("ping", vec![])), Err(NetError::Transport { kind: FailureKind::Metered, .. })));
        fake.fail(FailureKind::Other);
        assert!(block(c.fetch("ping", vec![])).is_err());
        assert_eq!(fake.asked().len(), 2);

        // One address only: nothing to switch to.
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.fail(FailureKind::Connect);
        assert!(block(c.fetch("ping", vec![])).is_err());
        assert_eq!(fake.asked().len(), 1);
    }

    #[test]
    fn empty_error_status_is_a_failure_but_an_error_body_is_read() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answers.lock().push_back(Ok((502, vec![])));
        assert!(matches!(block(c.fetch("ping", vec![])), Err(NetError::Http { status: 502 })));
        fake.answers.lock().push_back(Ok((401, br#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"no"}}}"#.to_vec())));
        let body = block(c.fetch("ping", vec![])).unwrap();
        assert!(matches!(c.core.parse_status(body), Err(crate::CoreError::Api { code: 40, .. })));
        // A proxy's own page for a server that is down says its status, not that the answer did not parse.
        fake.answers.lock().push_back(Ok((522, b"<html><body>error code: 522</body></html>".to_vec())));
        assert!(matches!(block(c.fetch("ping", vec![])), Err(NetError::Http { status: 522 })));
    }

    #[test]
    fn offline_writes_are_queued_and_replayed_in_order() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.cache_put("getPlaylist&id=1".into(), b"x".to_vec()).unwrap();
        fake.fail(FailureKind::UnknownHost);
        block(c.write(Write::AddToPlaylist { id: "1".into(), song_ids: vec!["a".into(), "b".into()] })).unwrap();
        fake.fail(FailureKind::UnknownHost);
        block(c.write(Write::Scrobble { id: "s".into(), submission: true, time_ms: Some(5) })).unwrap();
        assert_eq!(c.core.pending_list().unwrap().len(), 2);
        assert_eq!(c.core.cache_get("getPlaylist&id=1".into()).unwrap(), None, "the stale read goes even when queued");

        // Back online: the next write goes, then the queue in order; a rejected one is dropped.
        fake.answer(OK);
        fake.answer(r#"{"subsonic-response":{"status":"failed","error":{"code":70,"message":"gone"}}}"#);
        fake.answer(OK);
        block(c.write(Write::Star { kind: Starrable::Album, id: "al".into(), on: true })).unwrap();
        let asked = fake.asked();
        assert!(asked[2].contains("/rest/star?") && asked[2].ends_with("&albumId=al"));
        assert!(asked[3].contains("/rest/updatePlaylist?") && asked[3].ends_with("&playlistId=1&songIdToAdd=a&songIdToAdd=b"));
        assert!(asked[4].contains("/rest/scrobble?") && asked[4].ends_with("&id=s&submission=true&time=5"));
        assert!(c.core.pending_list().unwrap().is_empty());
    }

    #[test]
    fn replay_stops_at_the_first_network_failure() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.pending_add("star".into(), vec![]).unwrap();
        c.core.pending_add("unstar".into(), vec![]).unwrap();
        fake.fail(FailureKind::Timeout);
        block(c.flush_pending()).unwrap();
        assert_eq!(c.core.pending_list().unwrap().len(), 2);
        assert_eq!(fake.asked().len(), 1);
    }

    #[test]
    fn a_rejected_write_is_an_error_and_is_not_queued() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.cache_put("getStarred2".into(), b"x".to_vec()).unwrap();
        fake.answer(r#"{"subsonic-response":{"status":"failed","error":{"code":50,"message":"no"}}}"#);
        assert!(matches!(block(c.write(Write::Star { kind: Starrable::Song, id: "1".into(), on: false })), Err(NetError::Api { code: 50, .. })));
        assert!(c.core.pending_list().unwrap().is_empty());
        assert!(c.core.cache_get("getStarred2".into()).unwrap().is_some(), "nothing changed, nothing is stale");
    }

    /// What tools/feature-e2e.sh used to check against a real server: a heart, a new playlist and "now
    /// playing" leave as the Subsonic calls the server agrees with.
    #[test]
    fn a_heart_a_new_playlist_and_now_playing_are_asked_as_subsonic_says() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.cache_put("getStarred2".into(), b"x".to_vec()).unwrap();
        c.core.cache_put("getPlaylist&id=9".into(), b"x".to_vec()).unwrap();
        for _ in 0..4 {
            fake.answer(OK);
        }
        block(c.write(Write::Star { kind: Starrable::Song, id: "s1".into(), on: true })).unwrap();
        assert_eq!(c.core.cache_get("getStarred2".into()).unwrap(), None, "the favourites are read again");
        block(c.write(Write::Star { kind: Starrable::Song, id: "s1".into(), on: false })).unwrap();
        block(c.write(Write::CreatePlaylist { name: "nori check".into(), song_ids: vec!["s1".into(), "s2".into()] })).unwrap();
        assert_eq!(c.core.cache_get("getPlaylist&id=9".into()).unwrap(), None, "the playlists are read again");
        block(c.read_now(crate::cache_policy::Read::NowPlaying { id: "s1".into() })).unwrap();
        let asked = fake.asked();
        assert!(asked[0].contains("/rest/star?") && asked[0].ends_with("&id=s1"), "{}", asked[0]);
        assert!(asked[1].contains("/rest/unstar?") && asked[1].ends_with("&id=s1"), "{}", asked[1]);
        assert!(asked[2].contains("/rest/createPlaylist?") && asked[2].contains("&name=nori") && asked[2].ends_with("&songId=s1&songId=s2"), "{}", asked[2]);
        assert!(asked[3].contains("/rest/scrobble?") && asked[3].ends_with("&id=s1&submission=false"), "{}", asked[3]);
        assert!(c.core.pending_list().unwrap().is_empty(), "all of it answered, nothing waits");
    }

    #[test]
    fn login_falls_back_to_the_other_address_and_to_legacy_auth() {
        let (c, fake) = client(NetProfile::default());
        let config = ServerConfig { url: "http://lan".into(), user: "u".into(), password: "p".into(), ..Default::default() };
        fake.fail(FailureKind::Connect);
        fake.answer(OK);
        assert!(!block(c.login(config.clone(), "https://wan".into())).unwrap());
        assert!(fake.asked()[1].starts_with("https://wan/rest/ping"));

        let no_token = r#"{"subsonic-response":{"status":"failed","error":{"code":41,"message":"no token"}}}"#;
        fake.answer(no_token);
        fake.answer(OK);
        assert!(block(c.login(config.clone(), String::new())).unwrap());
        assert!(fake.asked()[3].contains("&p=enc:70&"));

        // An API key cannot fall back to a password.
        fake.answer(no_token);
        let keyed = ServerConfig { api_key: Some("k".into()), ..config };
        assert!(matches!(block(c.login(keyed, String::new())), Err(NetError::Api { code: 41, .. })));
    }

    #[test]
    fn sync_walks_pages_until_one_brings_nothing() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","searchResult3":{"artist":[{"id":"ar1","name":"A"}],
            "album":[{"id":"al1","name":"B"},{"id":"al2","name":"C"}],"song":[{"id":"s1","title":"x"},{"id":"s2","title":"y"},{"id":"s3","title":"z"}]}}}"#);
        let step = block(c.sync_page(0, 500, IngestStats::default())).unwrap();
        assert_eq!((step.total.artists, step.total.albums, step.total.songs, step.next_offset), (1, 2, 3, Some(500)));
        assert!(fake.asked()[0].ends_with("&query=&songCount=500&songOffset=0&albumCount=500&albumOffset=0&artistCount=500&artistOffset=0"));
        fake.answer(r#"{"subsonic-response":{"status":"ok","searchResult3":{}}}"#);
        let step = block(c.sync_page(500, 500, step.total)).unwrap();
        assert_eq!((step.total.songs, step.next_offset), (3, None));
    }
}
