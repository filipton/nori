//! One opened server profile: core, client, engine (cpal), store, downloader, cover loader and MPRIS.
//! A trimmed copy of the terminal's backend.rs. Network calls run on their own threads and answer with a
//! [`Msg`] through [`Tx`].

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
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
use nori_core::settings_store::{self, APPLY_AUDIO, APPLY_GAIN, CACHE_LIMIT, PLAYER, REPLAN, SOUND};
use nori_core::{AlbumDetail, ArtistDetail, Core, OriginKind, PageOrigin, PlaylistDetail, ServerConfig, Song};
use nori_covers::loader::{Config as CoverConfig, Loader, Ticket};
use nori_covers::memory::Image;
use nori_engine::core::{settings, CoreApp, CoreLibrary, CoreOrder, CoreQueue, Downloader, Measurer, OutputVolume};
use nori_engine::{AudioOutput, Body, ByteSource, Config, Engine, Event, OpenError, State, Status, Store};
use nori_http::Http;
use nori_look::cover::CoverColours;
use nori_output_cpal::{CpalOutput, Volume};

use crate::settings::Chore;
use crate::AppWindow;

pub use nori_core::transport::block_on;

/// Cover draw size. Large and hero covers also get their page colours derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoverSize {
    Card,
    Large,
    Hero,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoverKey {
    pub id: String,
    pub size: CoverSize,
}

/// Messages from other threads to the UI thread.
pub enum Msg {
    Engine(Event),
    Data(Req, Result<Data, String>),
    /// A decoded cover, with page colours for large sizes.
    Cover { key: CoverKey, image: Arc<Image>, colours: Option<Box<CoverColours>> },
    Search(SearchView),
    Lyrics { song: String, pick: LyricsPick },
    Facts(Box<crate::settings::Facts>),
    Note { text: String, error: bool },
    LoggedIn(Result<SavedServer, String>),
    Reachable(Result<(), String>),
}

/// Sends [`Msg`]s to the UI thread: queued in the app's inbox, then the window is woken to drain it.
#[derive(Clone)]
pub struct Tx {
    inbox: mpsc::Sender<Msg>,
    ui: slint::Weak<AppWindow>,
}

impl Tx {
    pub fn new(ui: slint::Weak<AppWindow>) -> (Tx, mpsc::Receiver<Msg>) {
        let (inbox, rx) = mpsc::channel();
        (Tx { inbox, ui }, rx)
    }

    pub fn send(&self, m: Msg) {
        if self.inbox.send(m).is_ok() {
            let _ = self.ui.upgrade_in_event_loop(|ui| ui.invoke_messages_arrived());
        }
    }

    fn note(&self, text: impl Into<String>, error: bool) {
        self.send(Msg::Note { text: text.into(), error });
    }
}

/// A page read.
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

/// Home shelves: title and album list kind.
pub const HOME_ROWS: [(&str, &str); 5] =
    [("Recently added", "newest"), ("Recently played", "recent"), ("Most played", "frequent"), ("Favorites", "starred"), ("Something random", "random")];

const ALBUM_PAGE: i32 = 500;

/// A collection whose songs are fetched on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    Album(String),
    Playlist(String),
    Artist(String),
}

impl Fetch {
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

/// Forwards each lyrics answer to the UI.
struct Shown {
    song: String,
    tx: Tx,
}

impl LyricsShown for Shown {
    fn show(&self, pick: LyricsPick) {
        self.tx.send(Msg::Lyrics { song: self.song.clone(), pick });
    }
}

/// MPRIS controls over the engine.
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

/// Saves the queue after a delay (`queue_keep`) on a thread that sleeps until a save is due.
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

/// Desktop-only settings, stored as app values in the database.
pub mod own {
    pub const VOLUME: &str = "desktop.volume";
    /// Output device name; empty for the system default.
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

/// Logs `draft` in against its server (blocking).
pub fn check_login(http: Arc<Http>, draft: SavedServer) -> Result<SavedServer, String> {
    let legacy = block_on(nori_core::client::login_check(http, config(&draft), draft.alt_url.clone())).map_err(|e| crate::words::net_error(&e))?;
    Ok(SavedServer { legacy_auth: legacy || draft.legacy_auth, ..draft })
}

pub struct Session {
    pub core: Arc<Core>,
    pub client: Arc<Client>,
    pub engine: Arc<Engine>,
    covers: Arc<Loader>,
    store: Arc<Store>,
    /// Single instance so the "downloads at once" limit holds.
    downloader: Arc<Downloader>,
    pub volume: Volume,
    /// [`Session::volume`] in dB, for loudness compensation.
    loudness: Arc<OutputVolume>,
    search: Arc<SearchSession>,
    mpris: Option<Arc<nori_mpris::Mpris>>,
    keeper: Arc<Keeper>,
    db: PathBuf,
    tx: Tx,
    /// Monotonic clock origin for scrobbling; shared by every session of the process.
    epoch: Instant,
}

impl Session {
    /// Opens the profile without touching the network, so an unreachable server still opens.
    /// `mpris`: the process's media controls, driven by this session while it is open.
    pub fn open(data: &Path, http: Arc<Http>, profile: SavedServer, tx: Tx, epoch: Instant, mpris: Option<Arc<nori_mpris::Mpris>>) -> Result<Session, String> {
        let db = db_path(data);
        let core = Core::new(db.clone(), nori_core::settings::server_db_id(&profile.id)).map_err(|e| format!("The database: {e}"))?;
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
        let loudness = Arc::new(OutputVolume::default());
        loudness.set(volume_db(volume.get()));
        let output: Box<dyn AudioOutput> = Box::new(output);
        let store = Store::open(data.join("music"), prefs.cache_mb.max(0) as u64 * 1024 * 1024, Box::new(CoreOrder)).map_err(|e| format!("The music directory: {e}"))?;
        let audio = Arc::new(Audio { http: http.clone() });
        let app = CoreApp::new().measuring(Measurer::new(core.clone(), client.clone(), store.clone())).per_device(core.clone()).bridging().volume(loudness.clone());
        let library = CoreLibrary { client: client.clone(), bytes: audio.clone(), metered: false, store: Some(store.clone()) };
        let events = tx.clone();
        let engine = Arc::new(Engine::start(library, app, CoreQueue, output, None, Config { memory_mb: 256, settings: settings(&prefs, loudness.db()), ..Config::default() }, move |e| events.send(Msg::Engine(e))));
        let covers = Arc::new(Loader::new(CoverConfig::new(data.join("covers")), http));
        if let Some(m) = &mpris {
            m.serve(Some(Arc::new(Desktop { engine: engine.clone() })));
        }
        let keeper = Keeper::start(core.clone(), engine.clone());
        let downloader = Downloader::new(core.clone(), client.clone(), audio.clone(), store.clone());
        let s = Session { core, client, engine, covers, store, downloader, volume, loudness, search: SearchSession::new(), mpris, keeper, db: PathBuf::from(db), tx, epoch };
        s.restore();
        if s.core.download_counts().pending > 0 {
            s.downloader.start(prefs.parallel_downloads.max(1) as usize);
        }
        Ok(s)
    }

    pub fn mpris_changed(&self) {
        if let Some(m) = &self.mpris {
            m.changed();
        }
    }

    /// Pings the server, and fills the offline index if it is empty.
    pub fn check(&self) {
        let (client, core, tx) = (self.client.clone(), self.core.clone(), self.tx.clone());
        spawn("nori-check", move || {
            let r = block_on(client.read_now(Read::Ping)).map(|_| ()).map_err(|e| crate::words::net_error(&e));
            let ok = r.is_ok();
            tx.send(Msg::Reachable(r));
            if ok && core.index_size().map_or(true, |s| s.songs == 0) {
                sync(&client, &tx);
            }
            let _ = block_on(client.flush_pending());
        });
    }

    /// Restores the saved queue, paused at its position.
    fn restore(&self) {
        let Ok(q) = self.core.load_queue() else { return };
        if q.songs.is_empty() {
            return;
        }
        let index = q.index as usize;
        playlist::playlist_set(q.songs.iter().map(|s| s.id.clone()).collect(), Some(index as u32), false, q.origin);
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

    /// Reads a page: the cached answer first, then the server's if it differs.
    pub fn load(&self, req: Req) {
        let (client, core, tx) = (self.client.clone(), self.core.clone(), self.tx.clone());
        spawn("nori-read", move || {
            let send = |r: Result<Data, String>| tx.send(Msg::Data(req.clone(), r));
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

    /// Plays `songs` from `start`. Provider songs (octo-fiesta `ext-`) are kept only if picked, since the
    /// server downloads whatever is requested.
    pub fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        self.handle().play(songs, start, shuffle, from);
    }

    pub fn play_later(&self, what: Fetch, shuffle: bool) {
        let (client, me) = (self.client.clone(), self.handle());
        let origin = what.origin();
        spawn("nori-play", move || match fetch_songs(&client, what) {
            Ok(songs) if !songs.is_empty() => me.play(songs, 0, shuffle, Some(origin)),
            Ok(_) => me.tx.note("Nothing to play", false),
            Err(e) => me.tx.note(format!("Could not load the songs: {e}"), true),
        });
    }

    /// Adds songs after the current one (`next`) or at the end. `from` marks a whole page (keeps an album
    /// gapless).
    pub fn enqueue(&self, songs: Vec<Song>, next: bool, from: Option<PageOrigin>) {
        self.handle().enqueue(songs, next, from);
    }

    fn handle(&self) -> Handle {
        Handle { engine: self.engine.clone(), keeper: self.keeper.clone(), tx: self.tx.clone() }
    }

    pub fn shuffle(&self, on: bool) {
        playlist::playlist_show_shuffle(on);
        playlist::playlist_shuffle(on);
        self.handle().edited();
    }

    /// Removes everything after the current song.
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

    /// Sets the volume (0..1); rebuilds the chain when loudness compensation changes with it.
    pub fn set_volume(&self, v: f32) {
        self.volume.set(v);
        if self.loudness.set(volume_db(v)) {
            if let Some(p) = settings_store::settings_current().filter(|p| p.loudness) {
                self.engine.set_settings(settings(&p, self.loudness.db()));
            }
        }
    }

    /// Requests a cover at `px` square; large ones also get page colours, derived on the loader thread.
    pub fn cover(&self, key: CoverKey, px: u32) -> Ticket {
        let url = self.core.cover_address(key.id.clone(), px);
        let colours = key.size != CoverSize::Card;
        let tx = self.tx.clone();
        self.covers.request(&url, px, px, move |r| {
            let Ok(image) = r else { return };
            let colours = colours.then(|| Box::new(derive(&image)));
            tx.send(Msg::Cover { key, image, colours });
        })
    }

    /// Searches the offline index at once.
    pub fn search_typed(&self, text: &str) -> SearchView {
        let view = self.search.typed(text.to_string());
        if view.query.is_empty() {
            return view;
        }
        let limit = nori_core::browse::library_sizes().local_search;
        self.search.local(self.core.clone(), view.query.clone(), limit).ok().flatten().unwrap_or(view)
    }

    /// Searches the server (after a typing pause).
    pub fn search_server(&self, query: String) {
        if query.trim().is_empty() {
            return;
        }
        let _ = self.core.search_remember_recent(query.clone());
        let (search, client, tx) = (self.search.clone(), self.client.clone(), self.tx.clone());
        spawn("nori-search", move || match block_on(search.ask(client, query.clone())) {
            Ok(Some(v)) => tx.send(Msg::Search(v)),
            Ok(None) => {}
            Err(e) => {
                if let Some(v) = search.failed(query, Some(crate::words::net_error(&e))) {
                    tx.send(Msg::Search(v));
                }
            }
        });
    }

    /// Fetches lyrics for `song`; each better answer is sent as it arrives (`Client::lyrics_for`).
    pub fn lyrics(&self, song: String) {
        let (client, tx) = (self.client.clone(), self.tx.clone());
        spawn("nori-lyrics", move || {
            let _ = block_on(client.lyrics_for(song.clone(), Arc::new(Shown { song, tx })));
        });
    }

    /// Sets a setting by name and applies its effect to the engine. None if no such setting.
    pub fn setting(&self, name: &str, value: &str) -> Option<SettingChange> {
        let change = nori_core::settings_model::setting_set(name.to_string(), value.to_string())?;
        self.apply(change.effect, &change.prefs);
        if change.effect & CACHE_LIMIT != 0 {
            self.store.set_limit(change.prefs.cache_mb.max(0) as u64 * 1024 * 1024);
        }
        Some(change)
    }

    /// Applies the effect of an in-place edit (an EQ band, a level).
    pub fn applied(&self, effect: u32) {
        if let Some(p) = settings_store::settings_current() {
            self.apply(effect, &p);
        }
    }

    /// Shallow engine buffer while the equalizer is being edited, so changes are heard at once.
    pub fn tuning(&self, on: bool) {
        self.engine.set_tuning(on);
    }

    /// Gathers [`Facts`](crate::settings::Facts) on a worker (asks the server for music folders).
    pub fn facts(&self) {
        let (core, client, store, covers, db, tx) = (self.core.clone(), self.client.clone(), self.store.clone(), self.covers.clone(), self.db.clone(), self.tx.clone());
        spawn("nori-facts", move || {
            let index = core.index_size().unwrap_or_default();
            let downloads = core.downloads(true).unwrap_or_default();
            let folders = match block_on(client.read_now(Read::MusicFolders)) {
                Ok(Page::Folders { v }) => v.into_iter().map(|f| (f.name, f.id)).collect(),
                _ => Vec::new(),
            };
            let database = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
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
            tx.send(Msg::Facts(Box::new(f)));
        });
    }

    fn apply(&self, effect: u32, prefs: &StoredPrefs) {
        if effect & (APPLY_AUDIO | SOUND | PLAYER) != 0 {
            self.engine.set_settings(settings(prefs, self.loudness.db()));
        }
        if effect & APPLY_GAIN != 0 {
            self.engine.gain_changed();
        }
        if effect & REPLAN != 0 {
            self.engine.replan();
        }
    }

    pub fn action(&self, chore: Chore) {
        match chore {
            Chore::SyncLibrary => {
                let (client, tx) = (self.client.clone(), self.tx.clone());
                spawn("nori-sync", move || sync(&client, &tx));
            }
            Chore::DownloadLibrary => {
                match self.core.download_queue_library() {
                    Ok(q) => self.tx.note(format!("Downloading {} songs", q.fresh.len() + q.again.len()), false),
                    Err(e) => self.tx.note(format!("Could not download the library: {e}"), true),
                }
                let n = settings_store::with_prefs(|p| p.parallel_downloads).unwrap_or(2);
                self.downloader.start(n.max(1) as usize);
            }
            Chore::MeasureAgain => {
                let n = self.core.analysis_clear().unwrap_or(0);
                nori_core::automix::planner::analyses_changed();
                self.engine.replan();
                self.tx.note(format!("Forgot {n} measured songs"), false);
            }
            Chore::ClearStream => {
                self.store.clear_cache();
                self.tx.note("Cleared the streamed music", false);
            }
            Chore::ClearLyrics => {
                self.core.lyrics_cache_clear();
                self.tx.note("Cleared the lyrics found online", false);
            }
            Chore::ClearCovers => {
                if let Some(d) = self.covers.disk() {
                    d.clear();
                }
                self.tx.note("Cleared the covers", false);
            }
        }
    }

    /// Feeds an engine event to the core: scrobbling, queue refill, the offline bridge.
    pub fn on_engine_event(&self, e: &Event) {
        use nori_core::scrobble::{scrobble_playing, scrobble_track, TrackChange};
        let (now, wall) = (self.epoch.elapsed().as_millis() as i64, nori_core::db::now_ms());
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

    /// Appends autofill songs ("Keep playing when the queue ends").
    fn refill(&self) {
        let (client, me) = (self.client.clone(), self.handle());
        spawn("nori-autofill", move || {
            let fresh = block_on(client.autofill());
            if nori_core::autofill::autofill_arrived(fresh.songs.len() as u32) && !fresh.songs.is_empty() {
                let len = playlist::with(|p| p.len());
                let n = fresh.songs.len();
                playlist::playlist_take(len as u32, fresh.songs.iter().map(|s| s.id.clone()).collect(), vec![Hand::No; n], fresh.from);
                me.edited();
            }
            if nori_core::autofill::autofill_landed() {
                me.engine.next();
            }
        });
    }

    /// Next; at the queue's end with autofill on, fetches songs first.
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

    /// Saves the queue and stops the engine.
    pub fn close(&self) {
        if let Some(m) = &self.mpris {
            m.serve(None);
        }
        self.keep(QueueMoment::Closing);
        self.keeper.stop();
        self.engine.stop();
    }
}

/// The parts of a session worker threads use.
#[derive(Clone)]
struct Handle {
    engine: Arc<Engine>,
    keeper: Arc<Keeper>,
    tx: Tx,
}

impl Handle {
    fn edited(&self) {
        self.engine.queue_changed();
        self.keeper.later(queue_keep(QueueMoment::Edited).save_after_ms);
    }

    fn apply(&self, e: &QueueEdit) {
        self.edited();
        if let Some(seek) = e.seek {
            self.engine.play_at(seek as usize, 0);
        }
    }

    fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        let picked = songs.get(start).map(|s| s.id.clone());
        let songs: Vec<Song> = songs.into_iter().filter(|s| !s.is_provider() || Some(&s.id) == picked.as_ref()).collect();
        if songs.is_empty() {
            return;
        }
        let start = picked.and_then(|id| songs.iter().position(|s| s.id == id)).unwrap_or(0);
        nori_core::queue::queue_register(songs.clone());
        let change = playlist::playlist_set(songs.iter().map(|s| s.id.clone()).collect(), (!shuffle).then_some(start as u32), shuffle, from);
        self.edited();
        self.engine.play_at(change.at.unwrap_or(0) as usize, 0);
    }

    fn enqueue(&self, songs: Vec<Song>, next: bool, from: Option<PageOrigin>) {
        // A provider song goes in only when picked alone.
        let songs: Vec<Song> = if songs.len() == 1 { songs } else { songs.into_iter().filter(|s| !s.is_provider()).collect() };
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
        self.tx.note(format!("{words}: {}", crate::words::songs(n)), false);
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

/// An octo-fiesta provider item; the server downloads it when requested.
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
fn sync(client: &Arc<Client>, tx: &Tx) {
    tx.note("Filling the offline index…", false);
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
                tx.note(format!("The offline index stopped: {e}"), true);
                return;
            }
        }
    }
    tx.note(format!("Offline index: {} songs", total.songs), false);
}

/// Page colours from a cover (dark theme), as Android's `CoverLoader.colours`: RGBA converted to ARGB.
fn derive(image: &Image) -> CoverColours {
    let px: Vec<u32> = image.pixels.as_chunks::<4>().0.iter().map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]])).collect();
    nori_look::cover::derive(&px, image.width as usize, image.height as usize, true, false)
}
