//! The Subsonic client over the platform's [`Transport`]: address choice and fallback, music folder
//! scoping, offline write queue and replay, library sync. Reads and caching are in cache_policy.rs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::transport::{self, FailureKind, NetError, Transport};
use crate::{api, Core, CoreError, IngestStats, ServerConfig};

pub use nori_net::requests::{NetProfile, NetResult, Starrable, SyncStep, Write};
pub(crate) use nori_net::requests::{blank, pairs, request, FOLDERED};

/// Ping timeout for the first address before falling back to the second.
const ADDRESS_PROBE_MS: u32 = 2_500;

/// The client for one server profile, over its core.
#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct Client {
    pub(crate) core: Arc<Core>,
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) profile: RwLock<NetProfile>,
    /// Requests go to the profile's second address (stream quality is capped then).
    pub(crate) second: AtomicBool,
    /// State shared between lyrics lookups (failing services, YouTube matches).
    pub(crate) lyrics: nori_lyrics::services::LyricsMemory,
    /// Motion cover token and cached videos (motion.rs).
    pub(crate) motion: parking_lot::Mutex<crate::motion::Motion>,
    /// A replay of the queued writes is running; a second one would send them twice.
    replaying: AtomicBool,
    /// The car's folders last listed, for their later pages and the list a picked row plays (car.rs).
    pub(crate) car: parking_lot::Mutex<crate::car::Shown>,
    /// What the last autofill fetch picked, recorded once its songs are appended (autofill.rs).
    pub(crate) autofill_picks: parking_lot::Mutex<Option<(crate::autofill::Picked, Vec<String>)>>,
}

/// The newest client, for code without a handle (`stream::resolve_now`, the beat model download).
// Global: reached from the engine's threads with no client handle.
static ACTIVE_CLIENT: parking_lot::Mutex<std::sync::Weak<Client>> = parking_lot::Mutex::new(std::sync::Weak::new());

pub(crate) fn active_client() -> Option<Arc<Client>> {
    ACTIVE_CLIENT.lock().upgrade()
}

async fn ping(transport: &dyn Transport, server: &api::Server, timeout_ms: u32) -> NetResult<()> {
    crate::parse(&transport::get(transport, server.url("ping", &[]), timeout_ms).await?)?;
    Ok(())
}

/// Verifies a profile before it is kept, opening nothing: pings its address, else `alt_url`. On error 41
/// (no token auth) tries legacy auth and returns true if that worked.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub async fn login_check(transport: Arc<dyn Transport>, config: ServerConfig, alt_url: String) -> NetResult<bool> {
    let legacy_possible = !config.legacy_auth && config.api_key.as_deref().unwrap_or("").is_empty();
    match login_attempt(&*transport, &config, &alt_url).await {
        Ok(()) => Ok(false),
        Err(NetError::Api { code: 41, .. }) if legacy_possible => {
            login_attempt(&*transport, &ServerConfig { legacy_auth: true, ..config }, &alt_url).await?;
            Ok(true)
        }
        Err(e) => Err(e),
    }
}

async fn login_attempt(transport: &dyn Transport, config: &ServerConfig, alt_url: &str) -> NetResult<()> {
    let server = config.server();
    let first = ping(transport, &server, 0).await;
    if first.is_err() && !blank(alt_url) {
        ping(transport, &server.rebased(alt_url), 0).await
    } else {
        first
    }
}

impl Client {
    /// The queue of this client's core.
    pub fn session(&self) -> &Arc<nori_queue::Session> {
        &self.core.session
    }

    /// `params` plus the music folder, for endpoints that take one.
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

    /// One request, retried once through the other address on an I/O failure (not a metered refusal:
    /// the other address is the same server under the same rule).
    pub(crate) async fn fetch(&self, endpoint: &str, params: Vec<(String, String)>) -> NetResult<Vec<u8>> {
        self.send(endpoint, params, true).await
    }

    /// [`Client::fetch`]; one not `repeatable` is retried only when nothing of it was sent.
    async fn send(&self, endpoint: &str, params: Vec<(String, String)>, repeatable: bool) -> NetResult<Vec<u8>> {
        let p = self.scoped(endpoint, params);
        match self.get(endpoint, &p).await {
            Err(e) if e.is_io() && (repeatable || e.nothing_sent()) && !matches!(e, NetError::Transport { kind: FailureKind::Metered, .. }) => {
                if !self.choose_address().await {
                    return Err(e);
                }
                self.get(endpoint, &p).await
            }
            r => r,
        }
    }

}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new(core: Arc<Core>, transport: Arc<dyn Transport>) -> Arc<Self> {
        let client = Arc::new(Client { core, transport, profile: RwLock::new(NetProfile::default()), second: AtomicBool::new(false), lyrics: Default::default(), motion: Default::default(), replaying: AtomicBool::new(false), car: Default::default(), autofill_picks: Default::default() });
        *ACTIVE_CLIENT.lock() = Arc::downgrade(&client);
        client
    }

    /// Sets the profile's addresses, folder and bitrate cap.
    pub fn set_profile(&self, profile: NetProfile) {
        if !blank(&profile.alt_url) {
            crate::covers::cover_address_alike(&profile.url, &profile.alt_url);
        }
        *self.profile.write() = profile;
    }

    /// Whether requests go to the second address.
    pub fn on_second_address(&self) -> bool {
        self.second.load(Ordering::Relaxed)
    }

    /// Pings the first address briefly and uses the second if it does not answer. Called on foreground and
    /// after failures. Returns whether the address changed.
    pub async fn choose_address(&self) -> bool {
        let (url, alt) = {
            let p = self.profile.read();
            (p.url.clone(), p.alt_url.clone())
        };
        if blank(&alt) {
            return false;
        }
        let probe = self.core.server.read().rebased(&url);
        let first_answers = ping(&*self.transport, &probe, ADDRESS_PROBE_MS).await.is_ok();
        self.core.use_address(if first_answers { url } else { alt });
        let changed = self.second.swap(!first_answers, Ordering::Relaxed) == first_answers;
        if changed {
            self.transport.address_changed();
        }
        changed
    }

    /// Replays queued writes in order until none are left, stopping at the first network failure or an
    /// answer that is not the server's (a captive portal's page); writes the server rejects are dropped. While one replay runs, another returns at once: the running one sends what was
    /// queued meanwhile too.
    pub async fn flush_pending(&self) -> NetResult<()> {
        if self.replaying.swap(true, Ordering::Acquire) {
            return Ok(());
        }
        struct Done<'a>(&'a AtomicBool);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _done = Done(&self.replaying);
        loop {
            let queued = self.core.pending_list()?;
            if queued.is_empty() {
                return Ok(());
            }
            for p in queued {
                match self.get(&p.endpoint, &p.params).await.map(|body| self.core.parse_status(body)) {
                    Err(e) if e.is_io() => return Ok(()),
                    Ok(Err(CoreError::Parse { .. })) => return Ok(()),
                    _ => {}
                }
                self.core.pending_done(p.row_id)?;
            }
        }
    }

    /// Indexes one search3 page of the library; returns running totals and the next offset (None after
    /// an empty page).
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

    /// Evicts cached reads whose key (endpoint then params) starts with one of `prefixes`: a manual refresh.
    pub fn drop_cached(&self, prefixes: Vec<String>) -> NetResult<()> {
        for p in prefixes {
            self.core.cache_evict(p)?;
        }
        Ok(())
    }
}

pub use nori_db::{db_file_name, db_forget_server, DB_FILE};

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Sends one change, or queues it (reported as success) while the server is unreachable. Behind
    /// queued changes it is queued too, so the server gets them in order. A change that must not reach
    /// the server twice is queued only when the failure shows nothing was sent. Evicts the change's stale
    /// cache prefixes when sent or queued.
    pub async fn write(&self, w: Write) -> NetResult<()> {
        let repeatable = w.repeatable();
        let (endpoint, params, stale) = request(w);
        if self.core.pending_any()? {
            self.core.pending_add(endpoint, &params)?;
            self.flush_pending().await?;
        } else {
            match self.send(endpoint, params.clone(), repeatable).await {
                Ok(body) => {
                    self.core.parse_status(body)?;
                }
                Err(e) if e.nothing_sent() || (repeatable && e.is_io()) => self.core.pending_add(endpoint, &params)?,
                Err(e) => return Err(e),
            }
        }
        for s in stale {
            self.core.cache_evict(s.to_string())?;
        }
        Ok(())
    }
}

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

    type Answer = Result<(u16, Vec<u8>), FailureKind>;

    /// A scripted transport: answers in order, records every request.
    #[derive(Default)]
    pub struct Fake {
        pub answers: Mutex<VecDeque<Answer>>,
        pub asked: Mutex<Vec<(String, u32)>>,
        pub sent: Mutex<Vec<Exchange>>,
        pub switched: Mutex<u32>,
        /// Each request pends once before answering, as a real one would.
        pub pends: Mutex<bool>,
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
            if *self.pends.lock() {
                let mut pended = false;
                std::future::poll_fn(|cx| {
                    if std::mem::replace(&mut pended, true) {
                        return Poll::Ready(());
                    }
                    cx.waker().wake_by_ref();
                    Poll::Pending
                })
                .await;
            }
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

    /// Polls `f` to completion; the fake transport never pends.
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
        let core = Core::open(String::new(), "t".into(), Arc::default()).unwrap();
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
    fn music_folder_only_on_foldered_endpoints() {
        let (c, _) = client(NetProfile { url: "h".into(), music_folder_id: "3".into(), ..Default::default() });
        assert_eq!(c.scoped("search3", vec![]), vec![("musicFolderId".to_string(), "3".to_string())]);
        assert!(c.scoped("getAlbum", vec![]).is_empty());
        let (c, _) = client(NetProfile { url: "h".into(), ..Default::default() });
        assert!(c.scoped("search3", vec![]).is_empty());
    }

    #[test]
    fn io_failure_retries_through_other_address() {
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

        fake.answer(OK);
        assert!(block(c.choose_address()));
        assert!(!c.on_second_address());
        assert_eq!(*fake.switched.lock(), 2);
        fake.answer(OK);
        assert!(!block(c.choose_address()));
    }

    #[test]
    fn metered_and_other_failures_are_not_retried() {
        let (c, fake) = client(two_addresses());
        fake.fail(FailureKind::Metered);
        assert!(matches!(block(c.fetch("ping", vec![])), Err(NetError::Transport { kind: FailureKind::Metered, .. })));
        fake.fail(FailureKind::Other);
        assert!(block(c.fetch("ping", vec![])).is_err());
        assert_eq!(fake.asked().len(), 2);

        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.fail(FailureKind::Connect);
        assert!(block(c.fetch("ping", vec![])).is_err());
        assert_eq!(fake.asked().len(), 1);
    }

    #[test]
    fn http_error_status_vs_error_body() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answers.lock().push_back(Ok((502, vec![])));
        assert!(matches!(block(c.fetch("ping", vec![])), Err(NetError::Http { status: 502 })));
        fake.answers.lock().push_back(Ok((401, br#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"no"}}}"#.to_vec())));
        let body = block(c.fetch("ping", vec![])).unwrap();
        assert!(matches!(c.core.parse_status(body), Err(crate::CoreError::Api { code: 40, .. })));
        // A proxy's HTML error page reports its status, not a parse error.
        fake.answers.lock().push_back(Ok((522, b"<html><body>error code: 522</body></html>".to_vec())));
        assert!(matches!(block(c.fetch("ping", vec![])), Err(NetError::Http { status: 522 })));
    }

    #[test]
    fn offline_writes_queue_and_replay_in_order() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.cache_put("getPlaylist&id=1".into(), b"x".to_vec()).unwrap();
        fake.fail(FailureKind::UnknownHost);
        block(c.write(Write::AddToPlaylist { id: "1".into(), song_ids: vec!["a".into(), "b".into()] })).unwrap();
        fake.fail(FailureKind::UnknownHost);
        block(c.write(Write::Scrobble { id: "s".into(), submission: true, time_ms: Some(5) })).unwrap();
        assert_eq!(c.core.pending_list().unwrap().len(), 2);
        assert_eq!(c.core.cache_get("getPlaylist&id=1".into()).unwrap(), None, "evicted even when queued");

        // Online: the queue in order, then the new write; the rejected one is dropped.
        fake.answer(r#"{"subsonic-response":{"status":"failed","error":{"code":70,"message":"gone"}}}"#);
        fake.answer(OK);
        fake.answer(OK);
        block(c.write(Write::Star { kind: Starrable::Album, id: "al".into(), on: true })).unwrap();
        let asked = fake.asked();
        assert!(asked[2].contains("/rest/updatePlaylist?") && asked[2].ends_with("&playlistId=1&songIdToAdd=a&songIdToAdd=b"));
        assert!(asked[3].contains("/rest/scrobble?") && asked[3].ends_with("&id=s&submission=true&time=5"));
        assert!(asked[4].contains("/rest/star?") && asked[4].ends_with("&albumId=al"));
        assert!(c.core.pending_list().unwrap().is_empty());
    }

    #[test]
    fn timed_out_edit_not_queued() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.fail(FailureKind::Timeout);
        assert!(block(c.write(Write::AddToPlaylist { id: "1".into(), song_ids: vec!["a".into()] })).is_err());
        fake.fail(FailureKind::Timeout);
        block(c.write(Write::Star { kind: Starrable::Song, id: "s".into(), on: true })).unwrap();
        let queued: Vec<String> = c.core.pending_list().unwrap().into_iter().map(|p| p.endpoint).collect();
        assert_eq!(queued, ["star"]);
    }

    #[test]
    fn unsure_edit_not_sent_again_elsewhere() {
        let (c, fake) = client(two_addresses());
        fake.fail(FailureKind::Timeout);
        fake.fail(FailureKind::Timeout);
        fake.answer(OK);
        assert!(block(c.write(Write::AddToPlaylist { id: "1".into(), song_ids: vec!["a".into()] })).is_err());
        assert_eq!(fake.asked().len(), 1, "{:?}", fake.asked());
    }

    #[test]
    fn a_portal_page_keeps_the_queue() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.pending_add("star", &[]).unwrap();
        fake.answer("<html>sign in</html>");
        block(c.flush_pending()).unwrap();
        assert_eq!(c.core.pending_list().unwrap().len(), 1);
    }

    #[test]
    fn replay_stops_at_network_failure() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.pending_add("star", &[]).unwrap();
        c.core.pending_add("unstar", &[]).unwrap();
        fake.fail(FailureKind::Timeout);
        block(c.flush_pending()).unwrap();
        assert_eq!(c.core.pending_list().unwrap().len(), 2);
        assert_eq!(fake.asked().len(), 1);
    }

    #[test]
    fn replays_at_once_send_each_write_once() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.pending_add("scrobble", &[]).unwrap();
        *fake.pends.lock() = true;
        fake.answer(OK);
        fake.answer(OK);
        let (a, b) = block(futures_util::future::join(c.flush_pending(), c.flush_pending()));
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(fake.asked().len(), 1);
        assert!(c.core.pending_list().unwrap().is_empty());
    }

    #[test]
    fn a_star_leaves_other_lists_cached() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let keys = ["getAlbum&id=1", "getAlbumList2&type=newest&size=20", "getArtistInfo2&id=1&count=10", "getArtists", "getPlaylists", "getPlaylist&id=2"];
        for k in keys {
            c.core.cache_put(k.into(), b"x".to_vec()).unwrap();
        }
        fake.fail(FailureKind::Connect);
        block(c.write(Write::Star { kind: Starrable::Song, id: "s".into(), on: true })).unwrap();
        let kept: Vec<&str> = keys.into_iter().filter(|k| c.core.cache_get(k.to_string()).unwrap().is_some()).collect();
        assert_eq!(kept, ["getAlbumList2&type=newest&size=20", "getArtistInfo2&id=1&count=10", "getArtists", "getPlaylists"]);
    }

    #[test]
    fn rejected_write_errors_and_is_not_queued() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.cache_put("getStarred2".into(), b"x".to_vec()).unwrap();
        fake.answer(r#"{"subsonic-response":{"status":"failed","error":{"code":50,"message":"no"}}}"#);
        assert!(matches!(block(c.write(Write::Star { kind: Starrable::Song, id: "1".into(), on: false })), Err(NetError::Api { code: 50, .. })));
        assert!(c.core.pending_list().unwrap().is_empty());
        assert!(c.core.cache_get("getStarred2".into()).unwrap().is_some());
    }

    #[test]
    fn star_playlist_and_now_playing_requests() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.cache_put("getStarred2".into(), b"x".to_vec()).unwrap();
        c.core.cache_put("getPlaylists".into(), b"x".to_vec()).unwrap();
        for _ in 0..4 {
            fake.answer(OK);
        }
        block(c.write(Write::Star { kind: Starrable::Song, id: "s1".into(), on: true })).unwrap();
        assert_eq!(c.core.cache_get("getStarred2".into()).unwrap(), None);
        block(c.write(Write::Star { kind: Starrable::Song, id: "s1".into(), on: false })).unwrap();
        block(c.write(Write::CreatePlaylist { name: "nori check".into(), song_ids: vec!["s1".into(), "s2".into()] })).unwrap();
        assert_eq!(c.core.cache_get("getPlaylists".into()).unwrap(), None);
        block(c.read_now(crate::cache_policy::Read::NowPlaying { id: "s1".into() })).unwrap();
        let asked = fake.asked();
        assert!(asked[0].contains("/rest/star?") && asked[0].ends_with("&id=s1"), "{}", asked[0]);
        assert!(asked[1].contains("/rest/unstar?") && asked[1].ends_with("&id=s1"), "{}", asked[1]);
        assert!(asked[2].contains("/rest/createPlaylist?") && asked[2].contains("&name=nori") && asked[2].ends_with("&songId=s1&songId=s2"), "{}", asked[2]);
        assert!(asked[3].contains("/rest/scrobble?") && asked[3].ends_with("&id=s1&submission=false"), "{}", asked[3]);
        assert!(c.core.pending_list().unwrap().is_empty());
    }

    #[test]
    fn login_fallbacks() {
        let fake = Arc::new(Fake::default());
        let config = ServerConfig { url: "http://lan".into(), user: "u".into(), password: "p".into(), ..Default::default() };
        fake.fail(FailureKind::Connect);
        fake.answer(OK);
        assert!(!block(login_check(fake.clone(), config.clone(), "https://wan".into())).unwrap());
        assert!(fake.asked()[1].starts_with("https://wan/rest/ping"));

        let no_token = r#"{"subsonic-response":{"status":"failed","error":{"code":41,"message":"no token"}}}"#;
        fake.answer(no_token);
        fake.answer(OK);
        assert!(block(login_check(fake.clone(), config.clone(), String::new())).unwrap());
        assert!(fake.asked()[3].contains("&p=enc:70&"));

        fake.answer(no_token);
        let keyed = ServerConfig { api_key: Some("k".into()), ..config };
        assert!(matches!(block(login_check(fake.clone(), keyed, String::new())), Err(NetError::Api { code: 41, .. })));
    }

    #[test]
    fn probe_keeps_second_address() {
        let (c, fake) = client(two_addresses());
        fake.fail(FailureKind::Timeout);
        assert!(block(c.choose_address()));
        *fake.pends.lock() = true;
        fake.fail(FailureKind::Timeout);
        let (_, during) = block(futures_util::future::join(c.choose_address(), async { c.core.url_prefix("stream".into()) }));
        assert!(during.starts_with("https://wan.example/"), "{during}");
    }

    #[test]
    fn sync_pages_until_empty() {
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
