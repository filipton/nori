//! The session behind the screen: core and client for one server profile, the engine on cpal, store,
//! downloader, cover loader and MPRIS. Network calls run on their own threads and answer with a [`Msg`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use nori_core::bridge::BridgeTake;
use nori_core::cache_policy::{Page, Read};
use nori_core::client::Client;
use nori_core::covers::set_cover_transport;
use nori_core::library::StarsShown;
use nori_core::race::{LyricsPick, LyricsShown};
use nori_core::playlist::{self, Hand, QueueEdit};
use nori_core::rules::{queue_keep, song_arrived, BridgeStep, QueueMoment};
use nori_core::transport::{Exchange, FailureKind, Transport, TransportError, TransportResponse};
use nori_core::search::{SearchSession, SearchView};
use nori_core::settings::{SavedServer, SettingChange, StoredPrefs};
use crate::settings_view::{Chore, Facts, Storage};
use crate::text::net_error;
use nori_core::settings_store::{self, APPLY_AUDIO, APPLY_GAIN, CACHE_LIMIT, PLAYER, REPLAN, SOUND};
use nori_core::{AlbumDetail, ArtistDetail, Core, PageOrigin, PlaylistDetail, Song};
pub use nori_host::{db_path, derive, Controls, Fetch};
use nori_host::{config, net, save, spawn, volume_db, Keeper};
use nori_covers::loader::{Config as CoverConfig, Loader};
use nori_covers::memory::Image;
use nori_engine::core::{settings, CoreApp, CoreLibrary, CoreOrder, CoreQueue, Downloader, Measurer, OutputVolume};
use nori_engine::{AudioOutput, Body, ByteSource, Config, Engine, Event, OpenError, State, Store};
use nori_http::Http;
use nori_look::cover::CoverColours;
use nori_output_cpal::{CpalOutput, Volume};
use ratatui::crossterm::event::{KeyEvent, MouseEvent};

pub use nori_core::transport::block_on;

/// Everything that wakes the event loop.
pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize,
    /// Terminal (or tmux pane) focus gained or lost.
    Focus(bool),
    Engine(Event),
    Data(Req, Result<Data, String>),
    /// A decoded cover by key, with derived colours when asked for.
    Cover { art: String, image: Arc<Image>, colours: Option<Box<CoverColours>> },
    /// One of possibly several lyrics answers for `song`, each better than the last.
    Lyrics { song: String, pick: LyricsPick },
    Search(SearchView),
    /// A status bar message.
    Note { text: String, error: bool },
    /// The login form's result: the profile to keep, or the error.
    LoggedIn(Result<SavedServer, String>),
    /// Result of the reachability check made when a session opens.
    Reachable(Result<(), String>),
}

impl Msg {
    /// A short description for the debug log, without lyrics, pixels or passwords.
    pub fn brief(&self) -> String {
        match self {
            Msg::Key(k) => format!("key {:?} {:?} {:?}", k.code, k.modifiers, k.kind),
            Msg::Mouse(m) => format!("mouse {:?} at {},{}", m.kind, m.column, m.row),
            Msg::Paste(p) => format!("paste of {} bytes", p.len()),
            Msg::Resize => "resize".into(),
            Msg::Focus(on) => format!("focus {}", if *on { "gained" } else { "lost" }),
            Msg::Engine(e) => format!("engine {e:?}"),
            Msg::Data(req, r) => format!("data {req:?} {}", if r.is_ok() { "ok" } else { "failed" }),
            Msg::Cover { art, image, colours } => format!("cover {art} {}x{}{}", image.width, image.height, if colours.is_some() { " with colours" } else { "" }),
            Msg::Lyrics { song, .. } => format!("lyrics for {song}"),
            Msg::Search(v) => format!("search results for {:?}", v.query),
            Msg::Note { text, error } => format!("note {text:?} error={error}"),
            Msg::LoggedIn(r) => format!("logged in: {}", r.is_ok()),
            Msg::Reachable(r) => format!("reachable: {}", r.is_ok()),
        }
    }
}

/// A screen's data request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Req {
    Home,
    Albums { offset: u32 },
    Artists,
    Playlists,
    Songs { offset: u32 },
    Album(String),
    Artist(String),
    Playlist(String),
    Downloads,
    Facts,
}

pub enum Data {
    /// Home shelf: index into `HOME_ROWS`, title, albums.
    HomeRow(usize, &'static str, Vec<nori_core::Album>),
    Albums(Vec<nori_core::Album>),
    Artists(Vec<nori_core::Artist>),
    Playlists(Vec<nori_core::Playlist>),
    Songs(Vec<Song>, bool),
    Album(Box<AlbumDetail>),
    Artist(Box<ArtistDetail>),
    Playlist(Box<PlaylistDetail>),
    Downloads(Box<Downloads>),
    Facts(Box<Facts>),
}

/// The downloads screen's sections.
#[derive(Default)]
pub struct Downloads {
    pub active: Vec<Song>,
    pub queued: Vec<Song>,
    pub failed: Vec<Song>,
    pub stored: Vec<Song>,
}

/// Home shelves: title and Subsonic album list type.
pub const HOME_ROWS: [(&str, &str); 5] =
    [("Recently added", "newest"), ("Recently played", "recent"), ("Most played", "frequent"), ("Favorites", "starred"), ("Something random", "random")];

/// Albums requested per page.
pub const ALBUM_PAGE: u32 = 500;

/// Audio bytes over HTTP: refused when offline, requests counted.
pub struct Audio {
    http: Arc<Http>,
    offline: bool,
    pub requests: AtomicU64,
}

impl Audio {
    pub fn new(http: Arc<Http>, offline: bool) -> Audio {
        Audio { http, offline, requests: AtomicU64::new(0) }
    }
}

impl ByteSource for Audio {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        if self.offline {
            return Err("offline".into());
        }
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.http.open(url, from)
    }

    fn open_live(&self, url: &str) -> Result<(Body, Option<usize>), String> {
        if self.offline {
            return Err("offline".into());
        }
        self.http.open_live(url)
    }
}

/// Forwards each lyrics answer to the event loop.
struct LyricsSink {
    song: String,
    tx: Sender<Msg>,
}

impl LyricsShown for LyricsSink {
    fn show(&self, pick: LyricsPick) {
        let _ = self.tx.send(Msg::Lyrics { song: self.song.clone(), pick });
    }
}

/// The terminal reads star marks from the core when drawing, so updates are ignored.
struct NoMarks;

impl StarsShown for NoMarks {
    fn marks(&self, _: nori_core::stars::StarMarks) {}
}

/// The `--offline` transport: every request fails as unreachable, so the core shows what is stored and
/// queues writes for later.
struct Offline;

fn offline() -> TransportError {
    TransportError::Failed { kind: FailureKind::NoRoute, detail: Some("offline".into()) }
}

#[async_trait::async_trait]
impl Transport for Offline {
    async fn get(&self, _: String, _: u32) -> Result<TransportResponse, TransportError> {
        Err(offline())
    }

    async fn send(&self, _: Exchange) -> Result<TransportResponse, TransportError> {
        Err(offline())
    }

    fn address_changed(&self) {}
}

/// One open server profile: core, client and player.
pub struct Session {
    pub core: Arc<Core>,
    pub client: Arc<Client>,
    pub engine: Arc<Engine>,
    pub store: Arc<Store>,
    pub downloader: Arc<Downloader>,
    pub covers: Option<Arc<Loader>>,
    pub volume: Volume,
    /// `volume` in dB, for loudness compensation.
    loudness: Arc<OutputVolume>,
    pub search: Arc<SearchSession>,
    pub offline: bool,
    mpris: Option<Arc<nori_mpris::Mpris>>,
    keeper: Arc<Keeper>,
    tx: Sender<Msg>,
    /// Database file, for its size on the settings page.
    db: PathBuf,
}

/// Logs `draft` in (blocking). The core tries the second address and falls back to legacy auth, which
/// is then kept on the profile.
pub fn check_login(http: Arc<Http>, draft: SavedServer) -> Result<SavedServer, String> {
    let legacy = block_on(nori_core::client::login_check(http, config(&draft), draft.alt_url.clone())).map_err(|e| match net_error(&e) {
        e if e.is_empty() => "the server did not answer".to_string(),
        e => e,
    })?;
    Ok(SavedServer { legacy_auth: legacy || draft.legacy_auth, ..draft })
}

/// Key prefix of album card covers in [`crate::art::Art`]: `thumb:<cover id>`.
pub const THUMB: &str = "thumb:";

/// The terminal client's own settings, stored in the core's `app_kv`.
pub mod own {
    pub const MOUSE: &str = "tui.mouse";
    pub const IMAGES: &str = "tui.images";
    pub const CARD_COVERS: &str = "tui.cardCovers";
    pub const VOLUME: &str = "tui.volume";
    /// Output device name opened at start; empty for the system default.
    pub const DEVICE: &str = "tui.device";

    pub fn text(key: &str) -> Option<String> {
        nori_core::settings_store::app_value(key).filter(|v| !v.is_empty())
    }

    pub fn flag(key: &str, default: bool) -> bool {
        nori_core::settings_store::app_value(key).map_or(default, |v| v == "true")
    }

    pub fn number(key: &str, default: f32) -> f32 {
        nori_core::settings_store::app_value(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn keep(key: &'static str, value: String) {
        nori_core::settings_store::keep_app_value(key, value);
    }
}

pub struct Open<'a> {
    pub data: &'a Path,
    pub http: Arc<Http>,
    pub profile: SavedServer,
    pub device: Option<String>,
    pub images: bool,
    pub offline: bool,
    /// The process's media controls, driven by this session while it is open.
    pub mpris: Option<Arc<nori_mpris::Mpris>>,
    pub tx: Sender<Msg>,
}

impl Session {
    /// Opens the profile without touching the network, so an unreachable server still opens with what
    /// is stored; [`Session::check`] reports reachability.
    pub fn open(o: Open) -> Result<Session, String> {
        let db = db_path(o.data);
        let core = Core::new(db.clone(), nori_core::settings::server_db_id(&o.profile.id)).map_err(|e| format!("the database: {e}"))?;
        core.configure(config(&o.profile)).map_err(|e| format!("the server: {e}"))?;
        let transport: Arc<dyn Transport> = if o.offline { Arc::new(Offline) } else { o.http.clone() };
        let client = Client::new(core.clone(), transport);
        client.set_profile(net(&o.profile));
        set_cover_transport(o.http.clone());
        let prefs = settings_store::settings_current().unwrap_or_default();
        let output = match o.device.clone().or_else(|| own::text(own::DEVICE)).as_ref() {
            Some(name) => CpalOutput::with_device(name),
            None => CpalOutput::new(),
        };
        let volume = output.volume();
        volume.set(own::number(own::VOLUME, 1.0));
        // Set before the engine starts so its first chain has the right loudness compensation.
        let loudness = Arc::new(OutputVolume::default());
        loudness.set(volume_db(volume.get()));
        let output: Box<dyn AudioOutput> = Box::new(output);
        let store = Store::open(o.data.join("music"), prefs.cache_mb.max(0) as u64 * 1024 * 1024, Box::new(CoreOrder)).map_err(|e| format!("the music directory: {e}"))?;
        let audio = Arc::new(Audio::new(o.http.clone(), o.offline));
        let downloader = Downloader::new(core.clone(), client.clone(), audio.clone(), store.clone());
        // `bridging`: a song the network cannot bring raises `Event::Bridge` (see `Session::bridge`).
        let app = CoreApp::new().measuring(Measurer::new(core.clone(), client.clone(), store.clone())).per_device(core.clone()).bridging().volume(loudness.clone());
        let library = CoreLibrary { client: client.clone(), bytes: audio, metered: false, store: Some(store.clone()) };
        let tx = o.tx.clone();
        let engine = Engine::start(library, app, CoreQueue, output, None, Config { memory_mb: 256, settings: settings(&prefs, loudness.db()), ..Config::default() }, move |e| {
            let _ = tx.send(Msg::Engine(e));
        });
        let engine = Arc::new(engine);
        let covers = o.images.then(|| Arc::new(Loader::new(CoverConfig::new(o.data.join("covers")), o.http.clone())));
        let mpris = o.mpris;
        if let Some(m) = &mpris {
            m.serve(Some(Arc::new(Controls::over_queue(engine.clone()))));
        }
        let keeper = Keeper::start(core.clone(), engine.clone());
        let s = Session { core, client, engine, store, downloader, covers, volume, loudness, search: SearchSession::new(), offline: o.offline, mpris, keeper, tx: o.tx, db: PathBuf::from(db) };
        s.restore();
        if !s.offline && s.core.download_counts().pending > 0 {
            start_downloads(&s.downloader);
        }
        Ok(s)
    }

    /// Tells MPRIS the song or state changed.
    pub fn desktop_changed(&self) {
        if let Some(m) = &self.mpris {
            m.changed();
        }
    }

    /// Pings the server in the background, fills an empty offline index and flushes pending writes.
    pub fn check(&self) {
        if self.offline {
            return;
        }
        let (client, core, tx) = (self.client.clone(), self.core.clone(), self.tx.clone());
        spawn("nori-check", move || {
            let r = block_on(client.read_now(Read::Ping)).map(|_| ()).map_err(|e| unreachable(&net_error(&e)));
            let ok = r.is_ok();
            let _ = tx.send(Msg::Reachable(r));
            // Search and the songs list read the offline index.
            if ok && core.index_size().map_or(true, |s| s.songs == 0) {
                sync(&client, &tx);
            }
            let _ = block_on(client.flush_pending());
        });
    }

    /// Fills the offline index from the server in the background.
    pub fn sync(&self) {
        let (client, tx) = (self.client.clone(), self.tx.clone());
        spawn("nori-sync", move || sync(&client, &tx));
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

    /// Saves the queue and pushes it to the server as `rules::queue_keep` says for `moment`.
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

    /// Loads a screen's data in the background: the stored copy first, then the server's if different.
    pub fn load(&self, req: Req) {
        let (client, core, tx, store) = (self.client.clone(), self.core.clone(), self.tx.clone(), self.store.clone());
        let (offline, db) = (self.offline, self.db.clone());
        let covers_bytes = self.covers.as_ref().and_then(|l| l.disk().map(|d| d.bytes())).unwrap_or(0);
        spawn("nori-read", move || {
            let send = |r: Result<Data, String>| {
                let _ = tx.send(Msg::Data(req.clone(), r));
            };
            match &req {
                Req::Home => {
                    for (i, (title, kind)) in HOME_ROWS.iter().enumerate() {
                        let read = if *kind == "starred" { Read::FavouriteAlbums { size: 40 } } else { Read::AlbumList { kind: kind.to_string(), size: 40, offset: 0, genre: None } };
                        let r = read_into(&client, read, &send, |p| match p {
                            Page::Albums { v } => Some(Data::HomeRow(i, title, v)),
                            _ => None,
                        });
                        if let Err(e) = r {
                            send(Err(e));
                            return;
                        }
                    }
                }
                Req::Albums { offset } => {
                    let read = Read::AlbumList { kind: "alphabeticalByName".into(), size: ALBUM_PAGE as i32, offset: *offset as i32, genre: None };
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
                Req::Downloads => {
                    let sections = core.download_sections().map_err(|e| e.to_string());
                    let stored = core.downloads(true).unwrap_or_default();
                    match sections {
                        Ok(s) => send(Ok(Data::Downloads(Box::new(Downloads { active: s.active, queued: s.queued, failed: s.failed, stored })))),
                        Err(e) => send(Err(e)),
                    }
                }
                Req::Facts => send(Ok(Data::Facts(Box::new(facts(&core, &client, &store, &db, covers_bytes, offline))))),
            }
        });
    }

    /// Plays `songs` from `start` (with `shuffle`, from wherever shuffle starts). Provider songs are
    /// dropped unless picked: the server downloads whatever is requested. `from` is the page the songs
    /// are the list of (`playlist_set`).
    pub fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        let picked = songs.get(start).map(|s| s.id.clone());
        let songs: Vec<Song> = songs.into_iter().filter(|s| !s.is_provider() || Some(&s.id) == picked.as_ref()).collect();
        let start = picked.and_then(|id| songs.iter().position(|s| s.id == id)).unwrap_or(0);
        self.handle().play(songs, start, shuffle, from);
    }

    /// Plays an album, playlist or artist once its songs are fetched.
    pub fn play_later(&self, what: Fetch, shuffle: bool) {
        let (client, me) = (self.client.clone(), self.handle());
        let origin = what.origin();
        spawn("nori-play", move || match what.songs(&client).map_err(|e| net_error(&e)) {
            Ok(songs) if !songs.is_empty() => me.play(songs.into_iter().filter(|s| !s.is_provider()).collect(), 0, shuffle, Some(origin)),
            Ok(_) => note(&me.tx, "Nothing to play".into(), false),
            Err(e) => note(&me.tx, format!("Could not load the songs: {e}"), true),
        });
    }

    /// Adds songs after the current one (`next`) or at the end.
    pub fn enqueue(&self, songs: Vec<Song>, next: bool) {
        self.handle().enqueue(songs, next, None);
    }

    /// Enqueues an album, playlist or artist once fetched, as one run from its page (gapless).
    pub fn enqueue_later(&self, what: Fetch, next: bool) {
        let (client, me) = (self.client.clone(), self.handle());
        let from = what.origin();
        spawn("nori-enqueue", move || match what.songs(&client).map_err(|e| net_error(&e)) {
            Ok(songs) => me.enqueue(songs, next, Some(from)),
            Err(e) => note(&me.tx, format!("Could not load the songs: {e}"), true),
        });
    }

    /// The parts of the session a worker thread uses.
    fn handle(&self) -> Handle {
        Handle { engine: self.engine.clone(), keeper: self.keeper.clone(), tx: self.tx.clone() }
    }

    fn edited(&self) {
        self.handle().edited();
    }

    /// Removes the song at list index `index`; if it was playing, the next one takes its place.
    pub fn remove(&self, index: usize) {
        let current = self.engine.status().index;
        let playing = self.engine.status().state == State::Playing;
        let change = playlist::playlist_remove(index as u32, index as u32 + 1);
        self.edited();
        if let (true, Some(at)) = (current == Some(index), change.at) {
            if playing {
                self.engine.play_at(at as usize, 0);
            } else {
                self.engine.go_to(at as usize, 0);
            }
        }
    }

    /// Undoes the removal of `id`; the playing song is unchanged.
    pub fn put_back(&self, id: &str) {
        let was_empty = playlist::with(|p| p.is_empty());
        if playlist::playlist_restore(id.to_string()).at.is_none() {
            return note(&self.tx, "Nothing to put back".into(), false);
        }
        self.edited();
        if was_empty {
            self.engine.go_to(0, 0);
        }
    }

    /// Moves the song at list index `from` to `to`.
    pub fn move_song(&self, from: usize, to: usize) {
        playlist::playlist_move(from as u32, from as u32 + 1, to as u32);
        self.edited();
    }

    pub fn shuffle(&self, on: bool) {
        playlist::playlist_show_shuffle(on);
        playlist::playlist_shuffle(on);
        self.edited();
    }

    pub fn repeat(&self, mode: u8) {
        self.engine.set_repeat(mode);
    }

    /// Queues `songs` for download and starts the downloader.
    pub fn download(&self, songs: Vec<Song>) {
        let songs: Vec<Song> = songs.into_iter().filter(|s| !s.is_provider()).collect();
        warm_covers(&self.core, self.covers.as_ref(), &songs);
        match self.core.download_queue(songs) {
            Ok(q) => note(&self.tx, format!("Downloading {} songs", q.fresh.len() + q.again.len()), false),
            Err(e) => note(&self.tx, format!("Could not download: {e}"), true),
        }
        start_downloads(&self.downloader);
    }

    pub fn download_later(&self, what: Fetch) {
        let (client, core, downloader, tx) = (self.client.clone(), self.core.clone(), self.downloader.clone(), self.tx.clone());
        let covers = self.covers.clone();
        spawn("nori-download-ask", move || match what.songs(&client).map_err(|e| net_error(&e)) {
            Ok(songs) => {
                let songs: Vec<Song> = songs.into_iter().filter(|s| !s.is_provider()).collect();
                warm_covers(&core, covers.as_ref(), &songs);
                let n = songs.len();
                let _ = core.download_queue(songs);
                start_downloads(&downloader);
                note(&tx, format!("Downloading {n} songs"), false);
            }
            Err(e) => note(&tx, format!("Could not load the songs: {e}"), true),
        });
    }

    /// Deletes a download from disk.
    pub fn download_remove(&self, id: &str) {
        let _ = self.core.download_remove(id.to_string());
        let _ = std::fs::remove_file(self.store.download_path(id));
    }

    /// Sets a setting by name and applies its effects, as Android's player does.
    pub fn setting(&self, name: &str, value: &str) -> Option<SettingChange> {
        let change = nori_core::settings_model::setting_set(name.to_string(), value.to_string())?;
        self.apply(change.effect, &change.prefs);
        if change.effect & CACHE_LIMIT != 0 {
            self.store.set_limit(change.prefs.cache_mb.max(0) as u64 * 1024 * 1024);
        }
        if change.server {
            // Per-server settings (music folder, second address bitrate) live on the profile.
            let mut p = change.prefs.clone();
            if let Some(s) = p.servers.iter().find(|s| s.id == p.active_server_id).cloned() {
                self.client.set_profile(net(&s));
                p.servers.iter_mut().filter(|x| x.id == s.id).for_each(|x| *x = s.clone());
                settings_store::settings_put(p);
            }
        }
        Some(change)
    }

    /// Updates loudness compensation for the volume (0 to 1), rebuilding the chain if that changes the sound.
    pub fn volume_changed(&self, v: f32) {
        if self.loudness.set(volume_db(v)) {
            if let Some(p) = settings_store::settings_current().filter(|p| p.loudness) {
                self.engine.set_settings(settings(&p, self.loudness.db()));
            }
        }
    }

    /// Applies a settings change's `effect` bits to the engine.
    pub fn apply(&self, effect: u32, prefs: &StoredPrefs) {
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

    /// Applies `effect` with the stored settings (after an in-place edit such as an equalizer band).
    pub fn applied(&self, effect: u32) {
        if let Some(p) = settings_store::settings_current() {
            self.apply(effect, &p);
        }
    }

    /// Fetches lyrics for `song`; each better answer arrives as a [`Msg::Lyrics`].
    pub fn lyrics(&self, song: String) {
        let (client, tx) = (self.client.clone(), self.tx.clone());
        spawn("nori-lyrics", move || {
            let shown = Arc::new(LyricsSink { song: song.clone(), tx });
            let _ = block_on(client.lyrics_for(song, shown));
        });
    }

    /// Requests cover `art` at `size` px, deriving colours on the loader thread when `colours`.
    pub fn cover(&self, art: String, size: u32, colours: bool) -> Option<nori_covers::loader::Ticket> {
        self.cover_as(art.clone(), art, size, colours)
    }

    /// Requests an album card cover, delivered under the key `thumb:<art>`.
    pub fn thumb(&self, art: String, size: u32) -> Option<nori_covers::loader::Ticket> {
        self.cover_as(art.clone(), format!("{THUMB}{art}"), size, false)
    }

    fn cover_as(&self, id: String, art: String, size: u32, colours: bool) -> Option<nori_covers::loader::Ticket> {
        let loader = self.covers.as_ref()?;
        let tx = self.tx.clone();
        let url = self.core.cover_address(id, size);
        Some(loader.request(&url, size, size, move |r| {
            let Ok(image) = r else { return };
            let colours = colours.then(|| Box::new(derive(&image)));
            let _ = tx.send(Msg::Cover { art, image, colours });
        }))
    }

    /// Searches the offline index for the typed text (synchronous).
    pub fn search_typed(&self, text: &str) -> SearchView {
        let view = self.search.typed(text.to_string());
        if view.query.is_empty() {
            return view;
        }
        let limit = nori_core::browse::library_sizes().local_search;
        self.search.local(self.core.clone(), view.query.clone(), limit).ok().flatten().unwrap_or(view)
    }

    /// Searches the server in the background.
    pub fn search_server(&self, query: String) {
        if self.offline || query.trim().is_empty() {
            return;
        }
        let (search, client, tx) = (self.search.clone(), self.client.clone(), self.tx.clone());
        spawn("nori-search", move || match block_on(search.ask(client, query.clone())) {
            Ok(Some(v)) => {
                let _ = tx.send(Msg::Search(v));
            }
            Ok(None) => {}
            Err(e) => {
                if let Some(v) = search.failed(query, Some(net_error(&e))) {
                    let _ = tx.send(Msg::Search(v));
                }
            }
        });
    }

    /// Stars or unstars on the server (queued when offline).
    pub fn star(&self, kind: nori_core::client::Starrable, id: String, on: bool) {
        let (client, tx) = (self.client.clone(), self.tx.clone());
        spawn("nori-star", move || {
            let r = block_on(client.star(kind, id, on, Arc::new(NoMarks)));
            let text = match r {
                Ok(()) => (if on { "Added to favorites" } else { "Removed from favorites" }).to_string(),
                Err(e) => format!("Could not change the favorite: {e}"),
            };
            note(&tx, text, false);
        });
    }

    /// Runs a settings page button.
    pub fn action(&self, chore: Chore) {
        let said = match chore {
            Chore::SyncLibrary => return self.sync(),
            Chore::DownloadLibrary => {
                let r = self.core.download_queue_library();
                start_downloads(&self.downloader);
                match r {
                    Ok(q) => format!("Downloading {} songs", q.fresh.len() + q.again.len()),
                    Err(e) => return note(&self.tx, format!("Could not download the library: {e}"), true),
                }
            }
            Chore::ClearStream => {
                self.store.clear_cache();
                "Cleared the streamed music".into()
            }
            Chore::ClearLyrics => {
                self.core.lyrics_cache_clear();
                "Cleared the lyrics found online".into()
            }
            Chore::ClearCovers => {
                if let Some(d) = self.covers.as_ref().and_then(|l| l.disk()) {
                    d.clear();
                }
                "Cleared the covers".into()
            }
            Chore::MeasureAgain => {
                let n = self.core.analysis_clear().unwrap_or(0);
                nori_core::automix::planner::analyses_changed();
                self.engine.replan();
                format!("Forgot {n} measured songs")
            }
        };
        note(&self.tx, said, false);
    }

    /// Feeds an engine event to the core: scrobbling, history, queue saves and refills, the offline bridge.
    pub fn followed(&self, e: &Event) {
        use nori_core::scrobble::{scrobble_playing, scrobble_track, TrackChange};
        let (now, wall) = (monotonic_ms(), nori_core::db::now_ms());
        let playing = self.engine.status_with(|s| s.state == State::Playing);
        let send = match e {
            Event::Song { id, .. } => Some(scrobble_track(Some(id.clone()), TrackChange::Moved, playing, now, wall, tz_offset_ms(wall))),
            Event::Looped { id, .. } => Some(scrobble_track(Some(id.clone()), TrackChange::Looped, playing, now, wall, tz_offset_ms(wall))),
            Event::State(State::Ended) => Some(scrobble_track(None, TrackChange::Ended, false, now, wall, tz_offset_ms(wall))),
            Event::State(s) => {
                scrobble_playing(*s == State::Playing, now);
                None
            }
            _ => None,
        };
        if let Some(send) = send.filter(|s| s.submit_id.is_some() || s.now_playing_id.is_some()) {
            let client = self.client.clone();
            spawn("nori-scrobble", move || {
                // Offline, writes wait in the pending queue with their original time.
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

    /// Runs the core's steps for a new song (`rules::song_arrived`).
    fn arrived(&self) {
        let steps = song_arrived();
        self.keeper.later(steps.save_after_ms);
        if steps.fill {
            self.refill();
        }
        if steps.bridge == BridgeStep::Parked {
            self.bridge_parked();
        }
        if steps.pause_at_end {
            self.engine.pause_at_end(true);
        }
    }

    /// Hands an unreachable song to the core's offline bridge and applies its decision.
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

    /// The bridge reached the parked song: returns to it if the server answers, else bridges further.
    fn bridge_parked(&self) {
        let (client, core, me) = (self.client.clone(), self.core.clone(), self.handle());
        spawn("nori-bridge", move || {
            let up = block_on(client.read_now(Read::Ping)).is_ok();
            if let Some(edit) = core.bridge_parked(up) {
                me.apply(&edit);
            }
        });
    }

    /// Appends autoplay songs; a next pressed while fetching is applied when they land.
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

    /// Next; at the queue's end with autoplay on, fetches songs first.
    pub fn next(&self) {
        let has_next = playlist::with(|p| p.next().is_some());
        if has_next {
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

struct Handle {
    engine: Arc<Engine>,
    keeper: Arc<Keeper>,
    tx: Sender<Msg>,
}

impl Handle {
    /// After a queue edit: tells the engine and schedules a save.
    fn edited(&self) {
        self.engine.queue_changed();
        self.keeper.later(queue_keep(QueueMoment::Edited).save_after_ms);
    }

    /// Applies a queue edit the core made itself (the offline bridge).
    fn apply(&self, e: &QueueEdit) {
        self.edited();
        if let Some(seek) = e.seek {
            self.engine.play_at(seek as usize, 0);
        }
    }

    fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        if songs.is_empty() {
            return;
        }
        nori_core::queue::queue_register(songs.clone());
        let change = playlist::playlist_set(songs.iter().map(|s| s.id.clone()).collect(), (!shuffle).then_some(start as u32), shuffle, from);
        self.edited();
        self.engine.play_at(change.at.unwrap_or(0) as usize, 0);
    }

    /// `from`: the page these are all the songs of.
    fn enqueue(&self, songs: Vec<Song>, next: bool, from: Option<PageOrigin>) {
        // A single picked song may be a provider's; lists never include them.
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
        note(&self.tx, format!("{words}: {}", crate::text::count(n as u64, "song", "songs")), false);
    }
}

/// Fetches covers of downloaded songs to disk (not decoded) so they exist offline.
fn warm_covers(core: &Core, covers: Option<&Arc<Loader>>, songs: &[Song]) {
    let Some(loader) = covers else { return };
    for url in core.download_cover_urls(songs.iter().filter_map(|s| s.cover_art.clone()).collect()) {
        loader.warm(&url);
    }
}

fn note(tx: &Sender<Msg>, text: String, error: bool) {
    let _ = tx.send(Msg::Note { text, error });
}

fn start_downloads(downloader: &Arc<Downloader>) {
    let n = settings_store::prefs(|p| p.parallel_downloads);
    downloader.start(n.max(1) as usize);
}

/// An unreachable server in words.
fn unreachable(e: &str) -> String {
    if e.is_empty() {
        "The server did not answer".into()
    } else if e.ends_with(['.', '?']) {
        // Already a full sentence.
        e.to_string()
    } else {
        format!("The server did not answer: {e}")
    }
}

/// Runs `Client::read_each` (stored copy, then the server's if different; errors only when nothing is
/// stored), sending each page `make` converts. Returns whether anything was sent.
fn read_into(client: &Arc<Client>, read: Read, send: &impl Fn(Result<Data, String>), make: impl Fn(Page) -> Option<Data>) -> Result<bool, String> {
    let mut any = false;
    block_on(client.read_each(read, |p| {
        if let Some(d) = make(p) {
            any = true;
            send(Ok(d));
        }
    }))
    .map_err(|e| unreachable(&net_error(&e)))?;
    Ok(any)
}

fn report(client: &Arc<Client>, read: Read, send: impl Fn(Result<Data, String>), make: impl Fn(Page) -> Option<Data>) {
    match read_into(client, read, &send, make) {
        Ok(true) => {}
        Ok(false) => send(Err("The server sent nothing for this page".into())),
        Err(e) => send(Err(e)),
    }
}

fn sync(client: &Client, tx: &Sender<Msg>) {
    note(tx, "Filling the offline index…".into(), false);
    match nori_host::sync(client) {
        Ok(total) => note(tx, format!("Offline index: {} songs, {} albums, {} artists", total.songs, total.albums, total.artists), false),
        Err(e) => note(tx, format!("The offline index stopped: {e}"), true),
    }
}

/// Library, storage and device facts for the settings page.
fn facts(core: &Arc<Core>, client: &Arc<Client>, store: &Arc<Store>, db: &Path, cover_bytes: u64, offline: bool) -> Facts {
    let index = core.index_size().unwrap_or_default();
    let downloads = core.downloads(true).unwrap_or_default();
    let folders = if offline {
        Vec::new()
    } else {
        match block_on(client.read_now(Read::MusicFolders)) {
            Ok(Page::Folders { v }) => v,
            _ => Vec::new(),
        }
    };
    let db_bytes = std::fs::metadata(db).map(|m| m.len() as i64).unwrap_or(0);
    Facts {
        analysed: core.analysis_count().unwrap_or(0),
        indexed: (index.songs, index.albums, index.artists),
        storage: Storage {
            stream: store.cache_bytes() as i64,
            covers: cover_bytes as i64,
            lyrics: core.lyrics_cache_bytes(),
            downloads: downloads.iter().map(|s| s.size as i64).sum(),
            download_songs: downloads.len() as u32,
            database: db_bytes,
        },
        folders,
        devices: CpalOutput::devices(),
    }
}

/// Monotonic ms for the scrobbler. A process-wide epoch: the core's scrobble state outlives sessions.
fn monotonic_ms() -> i64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as i64
}

/// Local UTC offset in ms at `wall_ms`.
fn tz_offset_ms(wall_ms: i64) -> i32 {
    (nori_core::library::local_offset_s(wall_ms / 1000) * 1000) as i32
}
