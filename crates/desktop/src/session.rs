//! Everything the window talks to, as the terminal client's backend.rs has it: the core opened for one
//! server profile, its client over nori-http, the engine playing through cpal, the store, the downloader,
//! the measurer, the cover loader and the desktop's media controls. Every call that may wait on the
//! network runs on a thread of its own and answers with a [`Msg`], handed to the window's event loop.
//!
//! This is the terminal's backend with only what this window uses; the words are this client's own.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use nori_core::bridge::BridgeTake;
use nori_core::cache_policy::{Page, Read};
use nori_core::client::{Client, NetProfile};
use nori_core::covers::set_cover_transport;
use nori_core::playlist::{self, Hand, QueueEdit};
use nori_core::rules::{queue_keep, song_arrived, BridgeStep, QueueMoment};
use nori_core::race::{LyricsPick, LyricsShown};
use nori_core::search::{SearchSession, SearchView};
use nori_core::settings::{SavedServer, SettingChange, StoredPrefs};
use nori_core::settings_store::{self, APPLY_AUDIO, APPLY_GAIN, PLAYER, REPLAN, SOUND};
use nori_core::{AlbumDetail, ArtistDetail, Core, OriginKind, PageOrigin, PlaylistDetail, ServerConfig, Song};
use nori_covers::loader::{Config as CoverConfig, Loader, Ticket};
use nori_covers::memory::Image;
use nori_engine::core::{settings, CoreApp, CoreLibrary, CoreOrder, CoreQueue, Downloader, Measurer};
use nori_engine::{AudioOutput, Body, ByteSource, Config, Engine, Event, OpenError, State, Status, Store};
use nori_http::Http;
use nori_look::cover::CoverColours;
use nori_output_cpal::{CpalOutput, Volume};

/// The core's calls are async over a transport that answers at once: polling them finishes them.
pub fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

/// Everything that wakes the window from another thread.
pub enum Msg {
    Engine(Event),
    Data(Req, Result<Data, String>),
    /// A cover by the key it was asked under, decoded, and the page's colours when they were asked for.
    Cover { key: String, image: Arc<Image>, colours: Option<Box<CoverColours>> },
    Search(SearchView),
    /// Lyrics for a song, and where they are from, as the core hands them over (`Client::lyrics_for`).
    Lyrics { song: String, pick: LyricsPick },
    /// What the settings pages show besides the settings.
    Facts(Box<crate::settings::Facts>),
    Note { text: String, error: bool },
    LoggedIn(Result<SavedServer, String>),
    Reachable(Result<(), String>),
}

/// Hands a message to the window's thread. Cheap to copy into any worker or the engine's callback.
#[derive(Clone, Copy)]
pub struct Tx;

impl Tx {
    pub fn send(&self, m: Msg) {
        let _ = slint::invoke_from_event_loop(move || crate::app::take(m));
    }
}

/// A read a page asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Req {
    Home,
    Albums,
    Artists,
    Playlists,
    Songs { offset: u32 },
    Album(String),
    Artist(String),
    Playlist(String),
}

pub enum Data {
    HomeRow(usize, Vec<nori_core::Album>),
    Albums(Vec<nori_core::Album>),
    Artists(Vec<nori_core::Artist>),
    Playlists(Vec<nori_core::Playlist>),
    Songs(Vec<Song>, bool),
    Album(Box<AlbumDetail>),
    Artist(Box<ArtistDetail>),
    Playlist(Box<PlaylistDetail>),
}

/// The home page's shelves, in order: title and album list kind (the terminal's).
pub const HOME_ROWS: [(&str, &str); 5] =
    [("Recently added", "newest"), ("Recently played", "recent"), ("Most played", "frequent"), ("Favorites", "starred"), ("Something random", "random")];

const ALBUM_PAGE: i32 = 500;

/// Songs a card stands for, still to be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    Album(String),
    Playlist(String),
    Artist(String),
}

impl Fetch {
    /// The page these songs are the whole of: played, they are that page's queue.
    pub fn origin(&self) -> PageOrigin {
        match self {
            Fetch::Album(id) => PageOrigin::new(OriginKind::Album, id.as_str()),
            Fetch::Playlist(id) => PageOrigin::new(OriginKind::Playlist, id.as_str()),
            Fetch::Artist(id) => PageOrigin::new(OriginKind::Artist, id.as_str()),
        }
    }
}

struct Audio {
    http: Arc<Http>,
}

impl ByteSource for Audio {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        self.http.open(url, from)
    }

    fn open_live(&self, url: &str) -> Result<(Body, Option<usize>), String> {
        self.http.open_live(url)
    }
}

/// Hands the lyrics to the window as they come.
struct Shown {
    song: String,
}

impl LyricsShown for Shown {
    fn show(&self, pick: LyricsPick) {
        Tx.send(Msg::Lyrics { song: self.song.clone(), pick });
    }
}

/// The desktop's media controls (MPRIS on Linux) drive the engine and read what plays from it.
struct Desktop {
    engine: Arc<Engine>,
}

impl nori_mpris::Controls for Desktop {
    fn play(&self) {
        self.engine.play();
    }
    fn pause(&self) {
        self.engine.pause();
    }
    fn toggle(&self) {
        self.engine.toggle();
    }
    fn next(&self) {
        self.engine.next();
    }
    fn previous(&self) {
        self.engine.previous();
    }
    fn seek(&self, ms: i64) {
        self.engine.seek(ms);
    }
    fn now(&self) -> nori_mpris::Now {
        let s: Status = self.engine.status();
        let song = s.id.clone().and_then(nori_core::queue::queue_song).unwrap_or_default();
        nori_mpris::Now {
            playing: s.state == State::Playing,
            loaded: s.state == State::Paused,
            index: s.index,
            title: song.title,
            artist: song.artist,
            album: song.album,
            length_ms: song.duration as i64 * 1000,
            position_ms: s.position_now(),
        }
    }
}

/// Keeps the queue for next time a moment after it changed (`queue_keep`), on a thread that sleeps
/// until a save is due.
struct Keeper {
    due: parking_lot::Mutex<(Option<Instant>, bool)>,
    wake: parking_lot::Condvar,
}

impl Keeper {
    fn start(core: Arc<Core>, engine: Arc<Engine>) -> Arc<Keeper> {
        let k = Arc::new(Keeper { due: parking_lot::Mutex::new((None, false)), wake: parking_lot::Condvar::new() });
        let me = k.clone();
        spawn("nori-keep", move || {
            let mut due = me.due.lock();
            while !due.1 {
                match due.0 {
                    None => me.wake.wait(&mut due),
                    Some(at) if Instant::now() < at => {
                        me.wake.wait_until(&mut due, at);
                    }
                    Some(_) => {
                        due.0 = None;
                        parking_lot::MutexGuard::unlocked(&mut due, || save(&core, &engine));
                    }
                }
            }
        });
        k
    }

    fn later(&self, ms: i64) {
        let mut due = self.due.lock();
        due.0 = Some(Instant::now() + Duration::from_millis(ms.max(0) as u64));
        self.wake.notify_one();
    }

    fn stop(&self) {
        self.due.lock().1 = true;
        self.wake.notify_one();
    }
}

fn save(core: &Core, engine: &Engine) {
    let _ = core.playlist_save(engine.status().position_now().max(0) as u64);
}

/// The client's own settings, kept beside the app's in the database.
pub mod own {
    pub const VOLUME: &str = "desktop.volume";
    /// The output device opened at start, by name; none or empty for the system's own.
    pub const DEVICE: &str = "desktop.device";

    pub fn text(key: &str) -> Option<String> {
        nori_core::settings_store::app_value(key).filter(|v| !v.is_empty())
    }

    pub fn number(key: &str, default: f32) -> f32 {
        nori_core::settings_store::app_value(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn keep(key: &'static str, value: String) {
        nori_core::settings_store::keep_app_value(key, value);
    }
}

pub fn db_path(data: &Path) -> String {
    data.join(nori_core::db::DB_FILE).to_string_lossy().into_owned()
}

fn config(p: &SavedServer) -> ServerConfig {
    ServerConfig { url: p.url.clone(), user: p.user.clone(), password: p.password.clone(), api_key: (!p.api_key.is_empty()).then(|| p.api_key.clone()), legacy_auth: p.legacy_auth }
}

fn net(p: &SavedServer) -> NetProfile {
    NetProfile { url: p.url.clone(), alt_url: p.alt_url.clone(), music_folder_id: p.music_folder_id.clone(), alt_max_bit_rate: p.alt_max_bit_rate.max(0) as u32 }
}

/// Checks `draft` against its server, as the other clients' logins do. Waits on the network.
pub fn check_login(data: &Path, http: Arc<Http>, draft: SavedServer) -> Result<SavedServer, String> {
    let probe = Core::new(db_path(data), nori_core::settings::server_db_id(&draft.id)).map_err(|e| e.to_string())?;
    let client = Client::new(probe, http);
    let legacy = block_on(client.login(config(&draft), draft.alt_url.clone())).map_err(|e| crate::words::net_error(&e))?;
    Ok(SavedServer { legacy_auth: legacy || draft.legacy_auth, ..draft })
}

/// One server profile opened: the core, its client and the player.
pub struct Session {
    pub core: Arc<Core>,
    pub client: Arc<Client>,
    pub engine: Arc<Engine>,
    covers: Arc<Loader>,
    store: Arc<Store>,
    /// The one downloader: a second would fetch the same songs beside it, past "downloads at once".
    downloader: Arc<Downloader>,
    pub volume: Volume,
    search: Arc<SearchSession>,
    mpris: Option<nori_mpris::Mpris>,
    keeper: Arc<Keeper>,
}

impl Session {
    /// Opens the profile: nothing here asks the network, so a server that is down still opens.
    pub fn open(data: &Path, http: Arc<Http>, profile: SavedServer) -> Result<Session, String> {
        let core = Core::new(db_path(data), nori_core::settings::server_db_id(&profile.id)).map_err(|e| format!("The database: {e}"))?;
        core.configure(config(&profile)).map_err(|e| format!("The server: {e}"))?;
        let client = Client::new(core.clone(), http.clone());
        client.set_profile(net(&profile));
        set_cover_transport(http.clone());
        let prefs = settings_store::settings_current().unwrap_or_default();
        let output = match own::text(own::DEVICE) {
            Some(name) => CpalOutput::with_device(&name),
            None => CpalOutput::new(),
        };
        let volume = output.volume();
        volume.set(own::number(own::VOLUME, 1.0));
        nori_engine::core::set_output_volume_db(volume_db(volume.get()));
        let output: Box<dyn AudioOutput> = Box::new(output);
        let store = Store::open(data.join("music"), prefs.cache_mb.max(0) as u64 * 1024 * 1024, Box::new(CoreOrder)).map_err(|e| format!("The music directory: {e}"))?;
        let audio = Arc::new(Audio { http: http.clone() });
        let app = CoreApp::new().measuring(Measurer::new(core.clone(), client.clone(), store.clone())).per_device(core.clone()).bridging();
        let library = CoreLibrary { client: client.clone(), bytes: audio.clone(), metered: false, store: Some(store.clone()) };
        // The engine's events are handed to the window as they come; nothing polls.
        let engine = Arc::new(Engine::start(library, app, CoreQueue, output, None, Config { memory_mb: 256, settings: settings(&prefs), ..Config::default() }, |e| Tx.send(Msg::Engine(e))));
        let covers = Arc::new(Loader::new(CoverConfig::new(data.join("covers")), http));
        let mpris = nori_mpris::Mpris::start(&format!("nori.desktop{}", std::process::id()), Arc::new(Desktop { engine: engine.clone() })).ok();
        let keeper = Keeper::start(core.clone(), engine.clone());
        let downloader = Downloader::new(core.clone(), client.clone(), audio.clone(), store.clone());
        let s = Session { core, client, engine, covers, store, downloader, volume, search: SearchSession::new(), mpris, keeper };
        s.restore();
        if s.core.download_counts().pending > 0 {
            s.downloader.start(prefs.parallel_downloads.max(1) as usize);
        }
        Ok(s)
    }

    /// The desktop's media controls are told the song or the state changed.
    pub fn desktop_changed(&self) {
        if let Some(m) = &self.mpris {
            m.changed();
        }
    }

    /// Asks the server whether it is there, and fills the offline index once if it is empty.
    pub fn check(&self) {
        let (client, core) = (self.client.clone(), self.core.clone());
        spawn("nori-check", move || {
            let r = block_on(client.read_now(Read::Ping)).map(|_| ()).map_err(|e| crate::words::net_error(&e));
            let ok = r.is_ok();
            Tx.send(Msg::Reachable(r));
            if ok && core.index_size().map_or(true, |s| s.songs == 0) {
                sync(&client);
            }
            let _ = block_on(client.flush_pending());
        });
    }

    /// The last queue kept, put back, paused at its place.
    fn restore(&self) {
        let Ok(q) = self.core.load_queue() else { return };
        if q.songs.is_empty() {
            return;
        }
        let index = q.index as usize;
        playlist::playlist_set(q.songs.iter().map(|s| s.id.clone()).collect(), index as i32, false, q.origin);
        self.engine.queue_changed();
        self.engine.go_to(index, q.position_ms as i64);
    }

    pub fn keep(&self, moment: QueueMoment) {
        let k = queue_keep(moment);
        if k.save_after_ms > 0 {
            self.keeper.later(k.save_after_ms);
        } else {
            save(&self.core, &self.engine);
        }
        if k.push {
            let (client, st) = (self.client.clone(), self.engine.status());
            spawn("nori-push", move || {
                let _ = block_on(client.playlist_push(st.id.clone(), st.position_now().max(0)));
            });
        }
    }

    /// A read for a page: what is stored first, the server's answer after it when that differs.
    pub fn load(&self, req: Req) {
        let (client, core) = (self.client.clone(), self.core.clone());
        spawn("nori-read", move || {
            let send = |r: Result<Data, String>| Tx.send(Msg::Data(req.clone(), r));
            match &req {
                Req::Home => {
                    for (i, (_, kind)) in HOME_ROWS.iter().enumerate() {
                        let read = if *kind == "starred" { Read::FavouriteAlbums { size: 40 } } else { Read::AlbumList { kind: kind.to_string(), size: 40, offset: 0, genre: None } };
                        if let Err(e) = read_into(&client, read, &send, |p| if let Page::Albums { v } = p { Some(Data::HomeRow(i, v)) } else { None }) {
                            send(Err(e));
                            return;
                        }
                    }
                }
                Req::Albums => {
                    let read = Read::AlbumList { kind: "alphabeticalByName".into(), size: ALBUM_PAGE, offset: 0, genre: None };
                    report(&client, read, send, |p| if let Page::Albums { v } = p { Some(Data::Albums(v)) } else { None });
                }
                Req::Artists => report(&client, Read::ArtistIndex, send, |p| if let Page::Artists { v } = p { Some(Data::Artists(v)) } else { None }),
                Req::Playlists => report(&client, Read::PlaylistList, send, |p| if let Page::Playlists { v } = p { Some(Data::Playlists(v)) } else { None }),
                Req::Album(id) => report(&client, Read::AlbumById { id: id.clone() }, send, |p| if let Page::AlbumPage { v } = p { Some(Data::Album(Box::new(v))) } else { None }),
                Req::Artist(id) => report(&client, Read::ArtistById { id: id.clone() }, send, |p| if let Page::ArtistPage { v } = p { Some(Data::Artist(Box::new(v))) } else { None }),
                Req::Playlist(id) => report(&client, Read::PlaylistById { id: id.clone() }, send, |p| if let Page::PlaylistPage { v } = p { Some(Data::Playlist(Box::new(v))) } else { None }),
                Req::Songs { offset } => match core.songs_page("title".into(), false, 0, 0, *offset) {
                    Ok(p) => send(Ok(Data::Songs(p.songs, p.exhausted))),
                    Err(e) => send(Err(e.to_string())),
                },
            }
        });
    }

    /// Plays `songs` from `start` (with `shuffle`, wherever shuffle starts). A provider's song
    /// (octo-fiesta's `ext-`) goes in only when it is the one picked: the server downloads whatever is asked for.
    pub fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        self.handle().play(songs, start, shuffle, from);
    }

    /// Plays an album, a playlist or an artist whose songs are not loaded yet.
    pub fn play_later(&self, what: Fetch, shuffle: bool) {
        let (client, me) = (self.client.clone(), self.handle());
        let origin = what.origin();
        spawn("nori-play", move || match fetch_songs(&client, what) {
            Ok(songs) if !songs.is_empty() => me.play(songs, 0, shuffle, Some(origin)),
            Ok(_) => Tx.send(Msg::Note { text: "Nothing to play".into(), error: false }),
            Err(e) => Tx.send(Msg::Note { text: format!("Could not load the songs: {e}"), error: true }),
        });
    }

    /// Songs added after the current one (`next`) or at the end of the queue, all of them the songs of the
    /// page `from` when they are (an album added whole stays gapless, as its page's Play does).
    pub fn enqueue(&self, songs: Vec<Song>, next: bool, from: Option<PageOrigin>) {
        self.handle().enqueue(songs, next, from);
    }

    fn handle(&self) -> Handle {
        Handle { engine: self.engine.clone(), keeper: self.keeper.clone() }
    }

    pub fn shuffle(&self, on: bool) {
        playlist::playlist_show_shuffle(on);
        playlist::playlist_shuffle(on);
        self.handle().edited();
    }

    /// Everything after the song playing taken out of the queue, as Clear does it.
    pub fn clear_upcoming(&self) {
        let mut upcoming: Vec<u32> = playlist::with(|p| p.upcoming().map(|i| i as u32).collect());
        upcoming.sort_unstable_by(|a, b| b.cmp(a));
        for i in upcoming {
            playlist::playlist_remove(i, i + 1);
        }
        self.handle().edited();
    }

    pub fn repeat(&self, mode: u8) {
        playlist::playlist_repeat(mode);
        self.engine.set_repeat(mode);
    }

    /// This client's volume (0 to 1) moved: loudness compensation follows it, and the chain is set up
    /// again when that moves the sound.
    pub fn set_volume(&self, v: f32) {
        self.volume.set(v);
        if nori_engine::core::set_output_volume_db(volume_db(v)) {
            if let Some(p) = settings_store::settings_current().filter(|p| p.loudness) {
                self.engine.set_settings(settings(&p));
            }
        }
    }

    /// The cover `art` at `size` pixels a side, handed back under `key`; with the page's colours (worked
    /// out on the loader's worker) when `colours`.
    pub fn cover(&self, art: &str, key: String, size: u32, colours: bool) -> Ticket {
        let url = self.core.cover_address(art.to_string(), size);
        self.covers.request(&url, size, size, move |r| {
            let Ok(image) = r else { return };
            let colours = colours.then(|| Box::new(derive(&image)));
            Tx.send(Msg::Cover { key, image, colours });
        })
    }

    /// Typed into the search field: the offline index answers at once.
    pub fn search_typed(&self, text: &str) -> SearchView {
        let view = self.search.typed(text.to_string());
        if view.query.is_empty() {
            return view;
        }
        let limit = nori_core::browse::library_sizes().local_search;
        self.search.local(self.core.clone(), view.query.clone(), limit).ok().flatten().unwrap_or(view)
    }

    /// Typing paused: the server is asked too.
    pub fn search_server(&self, query: String) {
        if query.trim().is_empty() {
            return;
        }
        let _ = self.core.search_remember_recent(query.clone());
        let (search, client) = (self.search.clone(), self.client.clone());
        spawn("nori-search", move || match block_on(search.ask(client, query.clone())) {
            Ok(Some(v)) => Tx.send(Msg::Search(v)),
            Ok(None) => {}
            Err(e) => {
                if let Some(v) = search.failed(query, Some(crate::words::net_error(&e))) {
                    Tx.send(Msg::Search(v));
                }
            }
        });
    }

    /// Lyrics for `song`, each better answer as it comes: the server's first, then the lyrics services the
    /// settings switch on, in the core's order (`Client::lyrics_for`).
    pub fn lyrics(&self, song: String) {
        let client = self.client.clone();
        spawn("nori-lyrics", move || {
            let _ = block_on(client.lyrics_for(song.clone(), Arc::new(Shown { song })));
        });
    }

    /// A setting changed by name, kept by the core, and whatever it changes applied to the engine, as the
    /// other clients take a change in.
    pub fn setting(&self, name: &str, value: &str) -> Option<SettingChange> {
        let change = nori_core::settings_model::setting_set(name.to_string(), value.to_string())?;
        self.apply(change.effect, &change.prefs);
        if change.apply_cache_limit {
            self.store.set_limit(change.prefs.cache_mb.max(0) as u64 * 1024 * 1024);
        }
        Some(change)
    }

    /// The effect of an edit made in place (an equalizer band, a level), applied.
    pub fn applied(&self, effect: u32) {
        if let Some(p) = settings_store::settings_current() {
            self.apply(effect, &p);
        }
    }

    /// The engine trades its deep buffer for an instant response while the equalizer is being moved.
    pub fn tuning(&self, on: bool) {
        self.engine.set_tuning(on);
    }

    /// What the settings pages show besides the settings, worked out off the window's thread (the server's
    /// music folders are asked for), and handed back.
    pub fn facts(&self) {
        let (core, client, store, covers) = (self.core.clone(), self.client.clone(), self.store.clone(), self.covers.clone());
        spawn("nori-facts", move || {
            let index = core.index_size().unwrap_or_default();
            let downloads = core.downloads(true).unwrap_or_default();
            let folders = match block_on(client.read_now(Read::MusicFolders)) {
                Ok(Page::Folders { v }) => v.into_iter().map(|f| (f.name, f.id)).collect(),
                _ => Vec::new(),
            };
            let database = std::fs::metadata(core_db()).map(|m| m.len()).unwrap_or(0);
            let f = crate::settings::Facts {
                analysed: core.analysis_count().unwrap_or(0),
                indexed: (index.songs, index.albums, index.artists),
                stream_bytes: store.cache_bytes(),
                cover_bytes: covers.disk().map_or(0, |d| d.bytes()),
                lyrics_bytes: core.lyrics_cache_bytes().max(0) as u64,
                download_bytes: downloads.iter().map(|s| s.size).sum(),
                download_songs: downloads.len() as u32,
                database_bytes: database,
                folders,
                devices: CpalOutput::devices(),
                device: own::text(own::DEVICE).unwrap_or_default(),
                syncing: false,
            };
            Tx.send(Msg::Facts(Box::new(f)));
        });
    }

    fn apply(&self, effect: u32, prefs: &StoredPrefs) {
        if effect & (APPLY_AUDIO | SOUND | PLAYER) != 0 {
            self.engine.set_settings(settings(prefs));
        }
        if effect & APPLY_GAIN != 0 {
            self.engine.gain_changed();
        }
        if effect & REPLAN != 0 {
            self.engine.replan();
        }
    }

    /// One of the settings page's buttons.
    pub fn action(&self, action: &str) {
        match action {
            "sync-library" => {
                let client = self.client.clone();
                spawn("nori-sync", move || sync(&client));
            }
            "download-library" => {
                match self.core.download_queue_library() {
                    Ok(q) => Tx.send(Msg::Note { text: format!("Downloading {} songs", q.fresh.len() + q.again.len()), error: false }),
                    Err(e) => Tx.send(Msg::Note { text: format!("Could not download the library: {e}"), error: true }),
                }
                let n = settings_store::with_prefs(|p| p.parallel_downloads).unwrap_or(2);
                self.downloader.start(n.max(1) as usize);
            }
            "measure-again" => {
                let n = self.core.analysis_clear().unwrap_or(0);
                nori_core::automix::planner::analyses_changed();
                self.engine.replan();
                Tx.send(Msg::Note { text: format!("Forgot {n} measured songs"), error: false });
            }
            "clear-stream" => {
                self.store.clear_cache();
                Tx.send(Msg::Note { text: "Cleared the streamed music".into(), error: false });
            }
            "clear-lyrics" => {
                self.core.lyrics_cache_clear();
                Tx.send(Msg::Note { text: "Cleared the lyrics found online".into(), error: false });
            }
            "clear-covers" => {
                if let Some(d) = self.covers.disk() {
                    d.clear();
                }
                Tx.send(Msg::Note { text: "Cleared the covers".into(), error: false });
            }
            _ => {}
        }
    }

    /// What the engine said, followed where the core keeps track: plays counted and sent, the queue
    /// refilled at its end, the offline bridge.
    pub fn followed(&self, e: &Event) {
        use nori_core::scrobble::{scrobble_playing, scrobble_track, TrackChange};
        let (now, wall) = (monotonic_ms(), nori_core::db::now_ms());
        let playing = self.engine.status_with(|s| s.state == State::Playing);
        let tz = (nori_core::library::local_offset_s(wall / 1000) * 1000) as i32;
        let send = match e {
            Event::Song { id, .. } => Some(scrobble_track(Some(id.clone()), TrackChange::Moved, playing, now, wall, tz)),
            Event::Looped { id, .. } => Some(scrobble_track(Some(id.clone()), TrackChange::Looped, playing, now, wall, tz)),
            Event::State(State::Ended) => Some(scrobble_track(None, TrackChange::Ended, false, now, wall, tz)),
            Event::State(s) => {
                scrobble_playing(*s == State::Playing, now);
                None
            }
            _ => None,
        };
        if let Some(send) = send.filter(|s| s.submit_id.is_some() || s.now_playing_id.is_some()) {
            let client = self.client.clone();
            spawn("nori-scrobble", move || {
                if let Some(id) = send.submit_id {
                    let _ = block_on(client.write(nori_core::client::Write::Scrobble { id, submission: true, time_ms: Some(send.submit_at) }));
                }
                if let Some(id) = send.now_playing_id {
                    let _ = block_on(client.write(nori_core::client::Write::Scrobble { id, submission: false, time_ms: None }));
                }
            });
        }
        match e {
            Event::Song { .. } => self.arrived(),
            Event::State(State::Paused) => self.keep(QueueMoment::Paused),
            Event::Bridge { .. } => self.bridge(),
            _ => {}
        }
    }

    fn arrived(&self) {
        let steps = song_arrived();
        self.keeper.later(steps.save_after_ms);
        if steps.fill {
            self.refill();
        }
        if steps.bridge == BridgeStep::Parked {
            let (client, core, me) = (self.client.clone(), self.core.clone(), self.handle());
            spawn("nori-bridge", move || {
                let up = block_on(client.read_now(Read::Ping)).is_ok();
                if let Some(edit) = core.bridge_parked(up) {
                    me.apply(&edit);
                }
            });
        }
        if steps.pause_at_end {
            self.engine.pause_at_end(true);
        }
    }

    fn bridge(&self) {
        match self.core.bridge_take() {
            BridgeTake::Jump { index } => {
                self.engine.play_at(index as usize, 0);
            }
            BridgeTake::Bridged { edit } => self.handle().apply(&edit),
            BridgeTake::Skip => {
                self.engine.next();
                self.engine.play();
            }
            BridgeTake::Stop => {}
        }
    }

    /// Songs for the queue's end, as "Keep playing when the queue ends" says.
    fn refill(&self) {
        let (client, me) = (self.client.clone(), self.handle());
        spawn("nori-autofill", move || {
            let fresh = block_on(client.autofill());
            if nori_core::autofill::autofill_arrived(fresh.songs.len() as u32) && !fresh.songs.is_empty() {
                let len = playlist::with(|p| p.len());
                let n = fresh.songs.len();
                // An album comes from its page: played as an album, as the one before it.
                playlist::playlist_take(len as u32, fresh.songs.iter().map(|s| s.id.clone()).collect(), vec![Hand::No; n], fresh.from());
                me.edited();
            }
            if nori_core::autofill::autofill_landed() {
                me.engine.next();
            }
        });
    }

    /// Next, as the button does it: at the queue's end with refilling on, songs are fetched first.
    pub fn next(&self) {
        if playlist::with(|p| p.next().is_some()) {
            self.engine.next();
            return;
        }
        match nori_core::autofill::autofill_next() {
            nori_core::autofill::FillNext::Skip => {
                self.engine.next();
            }
            nori_core::autofill::FillNext::Fetch => self.refill(),
            nori_core::autofill::FillNext::Wait => {}
        }
    }

    /// Let everything go: the queue kept, the engine stopped, the output closed.
    pub fn close(&self) {
        self.keep(QueueMoment::Closing);
        self.keeper.stop();
        self.engine.stop();
    }
}

/// What a worker thread may do with the session.
#[derive(Clone)]
struct Handle {
    engine: Arc<Engine>,
    keeper: Arc<Keeper>,
}

impl Handle {
    fn edited(&self) {
        self.engine.queue_changed();
        self.keeper.later(queue_keep(QueueMoment::Edited).save_after_ms);
    }

    fn apply(&self, e: &QueueEdit) {
        self.edited();
        if e.seek >= 0 {
            self.engine.play_at(e.seek as usize, 0);
        }
    }

    fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        let picked = songs.get(start).map(|s| s.id.clone());
        let songs: Vec<Song> = songs.into_iter().filter(|s| !is_provider(s) || Some(&s.id) == picked.as_ref()).collect();
        if songs.is_empty() {
            return;
        }
        let start = picked.and_then(|id| songs.iter().position(|s| s.id == id)).unwrap_or(0);
        nori_core::queue::queue_register(songs.clone());
        let change = playlist::playlist_set(songs.iter().map(|s| s.id.clone()).collect(), if shuffle { -1 } else { start as i32 }, shuffle, from);
        self.edited();
        self.engine.play_at(change.at.max(0) as usize, 0);
    }

    fn enqueue(&self, songs: Vec<Song>, next: bool, from: Option<PageOrigin>) {
        // Only the one song picked may be a provider's; a list of them never goes in whole.
        let songs: Vec<Song> = if songs.len() == 1 { songs } else { songs.into_iter().filter(|s| !is_provider(s)).collect() };
        if songs.is_empty() {
            return;
        }
        let n = songs.len();
        nori_core::queue::queue_register(songs.clone());
        let (len, current) = playlist::with(|p| (p.len(), p.current()));
        let at = if next { current.map_or(len, |c| c + 1) } else { len };
        let hand = if next { Hand::Next } else { Hand::Last };
        playlist::playlist_take(at as u32, songs.iter().map(|s| s.id.clone()).collect(), vec![hand; n], from);
        self.edited();
        if len == 0 {
            self.engine.go_to(0, 0);
        }
        let words = if next { "Playing next" } else { "Added to the queue" };
        Tx.send(Msg::Note { text: format!("{words}: {}", crate::words::songs(n)), error: false });
    }
}

fn fetch_songs(client: &Arc<Client>, what: Fetch) -> Result<Vec<Song>, String> {
    let now = |r: Read| block_on(client.read_now(r)).map_err(|e| crate::words::net_error(&e));
    match what {
        Fetch::Album(id) => match now(Read::AlbumSongs { id })? {
            Page::Songs { v } => Ok(v),
            Page::AlbumPage { v } => Ok(v.songs),
            _ => Ok(Vec::new()),
        },
        Fetch::Playlist(id) => match now(Read::PlaylistSongs { id })? {
            Page::Songs { v } => Ok(v),
            Page::PlaylistPage { v } => Ok(v.songs),
            _ => Ok(Vec::new()),
        },
        Fetch::Artist(id) => match now(Read::ArtistById { id })? {
            Page::ArtistPage { v } => Ok(block_on(client.artist_songs(v.albums))),
            _ => Ok(Vec::new()),
        },
    }
}

/// A provider's item: octo-fiesta downloads it the moment it is asked for.
fn is_provider(s: &Song) -> bool {
    s.is_external || s.id.starts_with("ext-")
}

fn volume_db(v: f32) -> f64 {
    if v > 0.0 {
        20.0 * (v as f64).log10()
    } else {
        -96.0
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(f);
}

fn read_into(client: &Arc<Client>, read: Read, send: &impl Fn(Result<Data, String>), make: impl Fn(Page) -> Option<Data>) -> Result<bool, String> {
    let mut any = false;
    block_on(client.read_each(read, |p| {
        if let Some(d) = make(p) {
            any = true;
            send(Ok(d));
        }
    }))
    .map_err(|e| crate::words::net_error(&e))?;
    Ok(any)
}

fn report(client: &Arc<Client>, read: Read, send: impl Fn(Result<Data, String>), make: impl Fn(Page) -> Option<Data>) {
    match read_into(client, read, &send, make) {
        Ok(true) => {}
        Ok(false) => send(Err("The server sent nothing for this page".into())),
        Err(e) => send(Err(e)),
    }
}

/// Fills the offline index from the server, page by page.
fn sync(client: &Arc<Client>) {
    Tx.send(Msg::Note { text: "Filling the offline index…".into(), error: false });
    let mut total = nori_core::IngestStats::default();
    let mut offset = 0;
    let page = nori_core::browse::library_sizes().sync_page;
    loop {
        match block_on(client.sync_page(offset, page, total.clone())) {
            Ok(step) => {
                total = step.total;
                match step.next_offset {
                    Some(next) => offset = next,
                    None => break,
                }
            }
            Err(e) => {
                Tx.send(Msg::Note { text: format!("The offline index stopped: {e}"), error: true });
                return;
            }
        }
    }
    Tx.send(Msg::Note { text: format!("Offline index: {} songs", total.songs), error: false });
}

fn monotonic_ms() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as i64
}

/// The page's colours from a cover, as Android's `CoverLoader.colours` works them out: the picture's
/// straight RGBA as ARGB, into `nori_look::cover::derive`, for the dark theme.
fn derive(image: &Image) -> CoverColours {
    let px: Vec<u32> = image.pixels.chunks_exact(4).map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]])).collect();
    nori_look::cover::derive(&px, image.width as usize, image.height as usize, true, false)
}

/// The database's file, for its size.
fn core_db() -> std::path::PathBuf {
    DB.lock().clone()
}

static DB: parking_lot::Mutex<std::path::PathBuf> = parking_lot::Mutex::new(std::path::PathBuf::new());

/// Where the database is, as main opened it.
pub fn set_db_path(p: &Path) {
    *DB.lock() = p.to_path_buf();
}
