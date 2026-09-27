//! Everything the screen talks to: the core opened for one server profile, its client over nori-http,
//! the engine playing through cpal, the store, the downloader, the measurer, the cover loader and the
//! desktop's media controls. Every call that may wait on the network runs on a thread of its own and
//! answers with a [`Msg`]; nothing here polls.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use nori_core::bridge::BridgeTake;
use nori_core::cache_policy::{Page, Read};
use nori_core::client::{Client, NetProfile};
use nori_core::covers::set_cover_transport;
use nori_core::library::StarsShown;
use nori_core::race::{LyricsPick, LyricsShown};
use nori_core::playlist::{self, Hand, QueueEdit};
use nori_core::rules::{queue_keep, song_arrived, BridgeStep, QueueMoment};
use nori_core::transport::{Exchange, FailureKind, Transport, TransportError, TransportResponse};
use nori_core::search::{SearchSession, SearchView};
use nori_core::settings::{SavedServer, SettingChange, StoredPrefs};
use crate::settings_view::{Facts, Storage};
use nori_core::settings_store::{self, APPLY_AUDIO, APPLY_GAIN, PLAYER, REPLAN, SOUND};
use nori_core::{AlbumDetail, ArtistDetail, Core, OriginKind, PageOrigin, PlaylistDetail, ServerConfig, Song};
use nori_covers::loader::{Config as CoverConfig, Loader};
use nori_covers::memory::Image;
use nori_engine::core::{settings, CoreApp, CoreLibrary, CoreOrder, CoreQueue, Downloader, Measurer};
use nori_engine::{AudioOutput, Body, ByteSource, Config, Engine, Event, OpenError, State, Status, Store};
use nori_http::Http;
use nori_look::cover::CoverColours;
use nori_output_cpal::{CpalOutput, Volume};
use ratatui::crossterm::event::{KeyEvent, MouseEvent};


/// The core's calls are async over a transport that answers at once: polling them once finishes them.
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

/// Everything that wakes the screen.
pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize,
    /// The terminal (or, under tmux, the pane) was given focus (true) or lost it.
    Focus(bool),
    Engine(Event),
    Data(Req, Result<Data, String>),
    /// A cover by its id, decoded, and the page's colours when they were asked for.
    Cover { art: String, image: Arc<Image>, colours: Option<Box<CoverColours>> },
    /// Lyrics for a song, and where they are from, as the core hands them over (`Client::lyrics_for`).
    Lyrics { song: String, pick: LyricsPick },
    Search(SearchView),
    /// A line for the status bar; `error` shows it as a failure.
    Note { text: String, error: bool },
    /// A server answered the login form: the profile to keep, or why not.
    LoggedIn(Result<SavedServer, String>),
    /// The server's reachability, checked once a session opens.
    Reachable(Result<(), String>),
}

impl Msg {
    /// In a few words, for the debug log (no lyrics, no pixels, no passwords).
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

/// A read a screen asked for.
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
    /// One shelf of the home page: its place, its title and its albums.
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

/// The downloads screen: what is running, waiting, failed and finished, and what is on this computer.
#[derive(Default)]
pub struct Downloads {
    pub active: Vec<Song>,
    pub queued: Vec<Song>,
    pub failed: Vec<Song>,
    pub stored: Vec<Song>,
}

/// The home page's shelves, in order: title and album list kind.
pub const HOME_ROWS: [(&str, &str); 5] =
    [("Recently added", "newest"), ("Recently played", "recent"), ("Most played", "frequent"), ("Favourites", "starred"), ("Something random", "random")];

/// Page sizes: albums are asked for in pages this long, songs come from the index in its own pages.
pub const ALBUM_PAGE: u32 = 500;

/// The audio side of the network: none at all when offline.
struct Audio {
    http: Arc<Http>,
    offline: bool,
}

impl ByteSource for Audio {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        if self.offline {
            return Err("offline".into());
        }
        self.http.open(url, from)
    }

    fn open_live(&self, url: &str) -> Result<(Body, Option<usize>), String> {
        if self.offline {
            return Err("offline".into());
        }
        self.http.open_live(url)
    }
}

/// The desktop's media controls drive the engine, and read what plays from it and from `song`: the
/// song the engine's status names (the core's queue, or the script's own list).
pub struct Desktop {
    pub engine: Arc<Engine>,
    pub song: Box<dyn Fn(&Status) -> Option<Song> + Send + Sync>,
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
        let s = self.engine.status();
        let song = (self.song)(&s).unwrap_or_default();
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

/// Hands the lyrics to the screen as they come.
struct Shown {
    song: String,
    tx: Sender<Msg>,
}

impl LyricsShown for Shown {
    fn show(&self, pick: LyricsPick) {
        let _ = self.tx.send(Msg::Lyrics { song: self.song.clone(), pick });
    }
}

/// The favourites' marks: the terminal reads them from the core as it draws, so nothing is kept here.
struct Marks;

impl StarsShown for Marks {
    fn marks(&self, _: nori_core::stars::StarMarks) {}
}

/// The network for a session opened offline (`--offline`): every request refused as a network failure
/// would be, so the core behaves as it does with the server out of reach (what is stored is shown,
/// writes wait for later) and nothing leaves the computer.
struct Offline;

#[async_trait::async_trait]
impl Transport for Offline {
    async fn get(&self, _: String, _: u32) -> Result<TransportResponse, TransportError> {
        Err(TransportError::Failed { kind: FailureKind::NoRoute, detail: Some("offline".into()) })
    }

    async fn send(&self, _: Exchange) -> Result<TransportResponse, TransportError> {
        Err(TransportError::Failed { kind: FailureKind::NoRoute, detail: Some("offline".into()) })
    }

    fn address_changed(&self) {}
}

/// Keeps the queue for next time a moment after it changed, as the core says (`queue_keep`), on a
/// thread of its own that sleeps until a save is due and never wakes otherwise.
struct Keeper {
    /// When the next save is due, and whether the session closed.
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

    /// A save `ms` from now, in place of one already waiting: a burst of changes saves once.
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

/// The queue and the place in it kept for next time.
fn save(core: &Core, engine: &Engine) {
    let _ = core.playlist_save(engine.status().position_now().max(0) as u64);
}

/// One server profile opened: the core, its client and the player.
pub struct Session {
    pub core: Arc<Core>,
    pub client: Arc<Client>,
    pub engine: Arc<Engine>,
    pub store: Arc<Store>,
    pub downloader: Arc<Downloader>,
    pub covers: Option<Arc<Loader>>,
    pub volume: Volume,
    pub search: Arc<SearchSession>,
    pub offline: bool,
    mpris: Option<nori_mpris::Mpris>,
    keeper: Arc<Keeper>,
    tx: Sender<Msg>,
}

/// The database every profile keeps its rows in, and the settings with it.
pub fn db_path(data: &Path) -> String {
    data.join(nori_core::db::DB_FILE).to_string_lossy().into_owned()
}

fn config(p: &SavedServer) -> ServerConfig {
    ServerConfig { url: p.url.clone(), user: p.user.clone(), password: p.password.clone(), api_key: (!p.api_key.is_empty()).then(|| p.api_key.clone()), legacy_auth: p.legacy_auth }
}

fn net(p: &SavedServer) -> NetProfile {
    NetProfile { url: p.url.clone(), alt_url: p.alt_url.clone(), music_folder_id: p.music_folder_id.clone(), alt_max_bit_rate: p.alt_max_bit_rate.max(0) as u32 }
}

/// Checks `draft` against its server, as Android's login does: the core tries the second address and,
/// for servers without token auth, legacy auth, which is then kept. Waits on the network.
pub fn check_login(data: &Path, http: Arc<Http>, draft: SavedServer) -> Result<SavedServer, String> {
    let probe = Core::new(db_path(data), nori_core::settings::server_db_id(&draft.id)).map_err(|e| e.to_string())?;
    let client = Client::new(probe, http);
    let legacy = block_on(client.login(config(&draft), draft.alt_url.clone())).map_err(|e| login_error(&said(&e)))?;
    Ok(SavedServer { legacy_auth: legacy || draft.legacy_auth, ..draft })
}

/// A failed login in words someone can act on.
fn login_error(e: &str) -> String {
    if e.is_empty() {
        "the server did not answer".into()
    } else {
        e.to_string()
    }
}

/// The client's own settings, kept beside the app's in the database (`app_kv`).
/// How a card's small cover is named among the pictures: `thumb:<cover id>`.
pub const THUMB: &str = "thumb:";

pub mod own {
    pub const MOUSE: &str = "tui.mouse";
    pub const IMAGES: &str = "tui.images";
    /// Small covers on the album cards, besides the large one playing.
    pub const CARD_COVERS: &str = "tui.cardCovers";
    pub const VOLUME: &str = "tui.volume";
    /// The output device opened at start, by name; none or empty for the system's own.
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
    pub mpris: bool,
    pub tx: Sender<Msg>,
}

impl Session {
    /// Opens the profile: nothing here asks the network, so a server that is down still opens, with
    /// what is stored, and says so once [`Session::check`] hears back.
    pub fn open(o: Open) -> Result<Session, String> {
        let db = db_path(o.data);
        let core = Core::new(db, nori_core::settings::server_db_id(&o.profile.id)).map_err(|e| format!("the database: {e}"))?;
        core.configure(config(&o.profile)).map_err(|e| format!("the server: {e}"))?;
        // Offline, the core's requests are refused before they leave: it shows what is stored and keeps
        // the writes for later, as it does with the server out of reach.
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
        // Before the engine starts, so its first chain has the loudness compensation for this volume.
        nori_engine::core::set_output_volume_db(if volume.get() > 0.0 { 20.0 * (volume.get() as f64).log10() } else { -96.0 });
        let output: Box<dyn AudioOutput> = Box::new(output);
        let store = Store::open(o.data.join("music"), prefs.cache_mb.max(0) as u64 * 1024 * 1024, Box::new(CoreOrder)).map_err(|e| format!("the music directory: {e}"))?;
        let audio = Arc::new(Audio { http: o.http.clone(), offline: o.offline });
        let downloader = Downloader::new(core.clone(), client.clone(), audio.clone(), store.clone());
        // A song the network would not bring goes to the offline bridge (`Event::Bridge`, `bridge`).
        let app = CoreApp::new().measuring(Measurer::new(core.clone(), client.clone(), store.clone())).per_device(core.clone()).bridging();
        let library = CoreLibrary { client: client.clone(), bytes: audio, metered: false, store: Some(store.clone()) };
        let tx = o.tx.clone();
        let engine = Engine::start(library, app, CoreQueue, output, None, Config { memory_mb: 256, settings: settings(&prefs), ..Config::default() }, move |e| {
            let _ = tx.send(Msg::Engine(e));
        });
        let engine = Arc::new(engine);
        let covers = o.images.then(|| Arc::new(Loader::new(CoverConfig::new(o.data.join("covers")), o.http.clone())));
        let mpris = if o.mpris {
            let name = format!("nori.instance{}", std::process::id());
            nori_mpris::Mpris::start(&name, Arc::new(Desktop { engine: engine.clone(), song: Box::new(|s| s.id.clone().and_then(nori_core::queue::queue_song)) })).ok()
        } else {
            None
        };
        // The songs the last run left in the queue, picked up where they were.
        let keeper = Keeper::start(core.clone(), engine.clone());
        let s = Session { core, client, engine, store, downloader, covers, volume, search: SearchSession::new(), offline: o.offline, mpris, keeper, tx: o.tx };
        s.restore();
        // Downloads a previous run left unfinished carry on.
        if !s.offline && s.core.download_counts().pending > 0 {
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

    /// Asks the server whether it is there, and whether the index needs filling; says what it heard.
    pub fn check(&self) {
        if self.offline {
            return;
        }
        let (client, core, tx) = (self.client.clone(), self.core.clone(), self.tx.clone());
        spawn("nori-check", move || {
            let r = block_on(client.read_now(Read::Ping)).map(|_| ()).map_err(|e| unreachable(&said(&e)));
            let ok = r.is_ok();
            let _ = tx.send(Msg::Reachable(r));
            // Search and the songs list read the offline index: filled once, in the background.
            if ok && core.index_size().map_or(true, |s| s.songs == 0) {
                sync(&client, &tx);
            }
            let _ = block_on(client.flush_pending());
        });
    }

    /// Fills the offline index from the server, page by page.
    pub fn sync(&self) {
        let (client, tx) = (self.client.clone(), self.tx.clone());
        spawn("nori-sync", move || sync(&client, &tx));
    }

    /// The last queue kept, put back, paused at its place.
    fn restore(&self) {
        let Ok(q) = self.core.load_queue() else { return };
        if q.songs.is_empty() {
            return;
        }
        let index = q.index as usize;
        // With the page it came from, so the page still answers for it.
        playlist::playlist_set(q.songs.iter().map(|s| s.id.clone()).collect(), index as i32, false, q.origin);
        self.engine.queue_changed();
        self.engine.go_to(index, q.position_ms as i64);
    }

    /// The queue kept for next time, and handed to the server, as the core says of `moment`.
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

    /// A read for a screen, answered with what is stored first and the server's answer after it when
    /// that differs (`Client::read_each`).
    pub fn load(&self, req: Req) {
        let (client, core, tx, store) = (self.client.clone(), self.core.clone(), self.tx.clone(), self.store.clone());
        let offline = self.offline;
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
                Req::Facts => send(Ok(Data::Facts(Box::new(facts(&core, &client, &store, covers_bytes, offline))))),
            }
        });
    }

    /// Plays `songs` from `start` (-1 with `shuffle`: wherever shuffle starts). A provider's song
    /// (octo-fiesta's `ext-`) goes in only when it is the one picked: the server downloads whatever is
    /// asked for. `from` is the page they are the songs of (see nori-queue `playlist_set`).
    pub fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        let picked = songs.get(start).map(|s| s.id.clone());
        let songs: Vec<Song> = songs.into_iter().filter(|s| !is_provider(s) || Some(&s.id) == picked.as_ref()).collect();
        if songs.is_empty() {
            return;
        }
        let start = picked.and_then(|id| songs.iter().position(|s| s.id == id)).unwrap_or(0);
        nori_core::queue::queue_register(songs.clone());
        let at = if shuffle { -1 } else { start as i32 };
        let change = playlist::playlist_set(songs.iter().map(|s| s.id.clone()).collect(), at, shuffle, from);
        self.edited();
        self.engine.play_at(change.at.max(0) as usize, 0);
    }

    /// Plays an album, a playlist or an artist whose songs are not loaded yet.
    pub fn play_later(&self, what: Fetch, shuffle: bool) {
        let (client, tx) = (self.client.clone(), self.tx.clone());
        let me = self.handle();
        let origin = what.origin();
        spawn("nori-play", move || match fetch_songs(&client, what) {
            Ok(songs) if !songs.is_empty() => me.play(songs, 0, shuffle, Some(origin)),
            Ok(_) => {
                let _ = tx.send(Msg::Note { text: "Nothing to play".into(), error: false });
            }
            Err(e) => {
                let _ = tx.send(Msg::Note { text: format!("Could not load the songs: {e}"), error: true });
            }
        });
    }

    /// Songs added after the current one (`next`) or at the end of the queue.
    pub fn enqueue(&self, songs: Vec<Song>, next: bool) {
        self.handle().enqueue(songs, next, None);
    }

    /// The same, for songs still to be loaded: a whole album added so stays gapless as its page's Play does.
    pub fn enqueue_later(&self, what: Fetch, next: bool) {
        let client = self.client.clone();
        let me = self.handle();
        let tx = self.tx.clone();
        let from = what.origin();
        spawn("nori-enqueue", move || match fetch_songs(&client, what) {
            Ok(songs) => me.enqueue(songs, next, Some(from)),
            Err(e) => {
                let _ = tx.send(Msg::Note { text: format!("Could not load the songs: {e}"), error: true });
            }
        });
    }

    /// The part of the session a worker thread may use.
    fn handle(&self) -> Handle {
        Handle { engine: self.engine.clone(), keeper: self.keeper.clone(), tx: self.tx.clone() }
    }

    /// The core's queue was edited: the engine told, the queue saved a moment later.
    fn edited(&self) {
        self.handle().edited();
    }

    /// Removes the song at list index `index`; the one playing moves on to the next.
    pub fn remove(&self, index: usize) {
        let current = self.engine.status().index;
        let playing = self.engine.status().state == State::Playing;
        let change = playlist::playlist_remove(index as u32, index as u32 + 1);
        self.edited();
        if current == Some(index) && change.at >= 0 {
            if playing {
                self.engine.play_at(change.at as usize, 0);
            } else {
                self.engine.go_to(change.at as usize, 0);
            }
        }
    }

    /// Puts the song `id` taken out last back where it was; the song playing stays the one playing.
    pub fn put_back(&self, id: &str) {
        let was_empty = playlist::with(|p| p.is_empty());
        if playlist::playlist_restore(id.to_string()).at < 0 {
            self.note("Nothing to put back".to_string(), false);
            return;
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

    /// Queues `songs` for download and starts fetching.
    pub fn download(&self, songs: Vec<Song>) {
        let songs: Vec<Song> = songs.into_iter().filter(|s| !is_provider(s)).collect();
        warm_covers(&self.core, self.covers.as_ref(), &songs);
        match self.core.download_queue(songs) {
            Ok(q) => self.note(format!("Downloading {} songs", q.fresh.len() + q.again.len()), false),
            Err(e) => self.note(format!("Could not download: {e}"), true),
        }
        let n = settings_store::with_prefs(|p| p.parallel_downloads).unwrap_or(2);
        self.downloader.start(n.max(1) as usize);
    }

    pub fn download_later(&self, what: Fetch) {
        let (client, core, downloader, tx) = (self.client.clone(), self.core.clone(), self.downloader.clone(), self.tx.clone());
        let covers = self.covers.clone();
        spawn("nori-download-ask", move || match fetch_songs(&client, what) {
            Ok(songs) => {
                let songs: Vec<Song> = songs.into_iter().filter(|s| !is_provider(s)).collect();
                warm_covers(&core, covers.as_ref(), &songs);
                let n = songs.len();
                let _ = core.download_queue(songs);
                let slots = settings_store::with_prefs(|p| p.parallel_downloads).unwrap_or(2);
                downloader.start(slots.max(1) as usize);
                let _ = tx.send(Msg::Note { text: format!("Downloading {n} songs"), error: false });
            }
            Err(e) => {
                let _ = tx.send(Msg::Note { text: format!("Could not load the songs: {e}"), error: true });
            }
        });
    }

    /// A download removed from this computer.
    pub fn download_remove(&self, id: &str) {
        let _ = self.core.download_remove(id.to_string());
        let _ = std::fs::remove_file(self.store.download_path(id));
    }

    pub fn note(&self, text: String, error: bool) {
        let _ = self.tx.send(Msg::Note { text, error });
    }

    /// A setting changed by name, kept by the core, and whatever it changes applied to the engine, as
    /// Android's player takes a change in.
    pub fn setting(&self, name: &str, value: &str) -> Option<SettingChange> {
        let change = nori_core::settings_model::setting_set(name.to_string(), value.to_string())?;
        self.apply(change.effect, &change.prefs);
        if change.apply_cache_limit {
            self.store.set_limit(change.prefs.cache_mb.max(0) as u64 * 1024 * 1024);
        }
        if change.server {
            // The active server's own settings (its music folder, the second address's bitrate): kept with
            // the profile, and the client told.
            let mut p = change.prefs.clone();
            if let Some(s) = p.servers.iter().find(|s| s.id == p.active_server_id).cloned() {
                self.client.set_profile(net(&s));
                p.servers.iter_mut().filter(|x| x.id == s.id).for_each(|x| *x = s.clone());
                settings_store::settings_put(p);
            }
        }
        Some(change)
    }

    /// This client's volume (0 to 1, a gain on the samples) moved: loudness compensation follows it, as
    /// dB below full, and the chain is set up again when that moves the sound.
    pub fn volume_changed(&self, v: f32) {
        let db = if v > 0.0 { 20.0 * (v as f64).log10() } else { -96.0 };
        if nori_engine::core::set_output_volume_db(db) {
            if let Some(p) = settings_store::settings_current().filter(|p| p.loudness) {
                self.engine.set_settings(settings(&p));
            }
        }
    }

    /// What a change of the kept settings asks of the engine.
    pub fn apply(&self, effect: u32, prefs: &StoredPrefs) {
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

    /// The effect of an edit made in place (an equalizer band, a level), applied.
    pub fn applied(&self, effect: u32) {
        if let Some(p) = settings_store::settings_current() {
            self.apply(effect, &p);
        }
    }

    /// Lyrics for `song`, each better answer as it comes: the server's first, then the lyrics services
    /// the settings switch on, in the core's order (`Client::lyrics_for`).
    pub fn lyrics(&self, song: String) {
        let (client, tx) = (self.client.clone(), self.tx.clone());
        spawn("nori-lyrics", move || {
            let shown = Arc::new(Shown { song: song.clone(), tx });
            let _ = block_on(client.lyrics_for(song, shown));
        });
    }

    /// The cover at `url`, decoded at `size` pixels a side, with the page's colours worked out from it
    /// (on the loader's worker, not the screen's thread) when `colours`.
    pub fn cover(&self, art: String, size: u32, colours: bool) -> Option<nori_covers::loader::Ticket> {
        self.cover_as(art.clone(), art, size, colours)
    }

    /// A small cover for an album card, handed over as `thumb:<id>` so it is kept apart from the large one.
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
                if let Some(v) = search.failed(query, Some(said(&e))) {
                    let _ = tx.send(Msg::Search(v));
                }
            }
        });
    }

    /// A favourite marked or unmarked, sent to the server (kept for later when offline); the mark from
    /// before comes back if the server refuses it (`Client::star`).
    pub fn star(&self, kind: nori_core::client::Starrable, id: String, on: bool) {
        let (client, tx) = (self.client.clone(), self.tx.clone());
        spawn("nori-star", move || {
            let r = block_on(client.star(kind, id, on, Arc::new(Marks)));
            let text = match r {
                Ok(()) => (if on { "Added to favourites" } else { "Removed from favourites" }).to_string(),
                Err(e) => format!("Could not change the favourite: {e}"),
            };
            let _ = tx.send(Msg::Note { text, error: false });
        });
    }

    /// One of the settings screen's actions.
    pub fn action(&self, action: &str) {
        match action {
            "sync-library" => self.sync(),
            "download-library" => {
                match self.core.download_queue_library() {
                    Ok(q) => self.note(format!("Downloading {} songs", q.fresh.len() + q.again.len()), false),
                    Err(e) => self.note(format!("Could not download the library: {e}"), true),
                }
                let n = settings_store::with_prefs(|p| p.parallel_downloads).unwrap_or(2);
                self.downloader.start(n.max(1) as usize);
            }
            "clear-stream" => {
                self.store.clear_cache();
                self.note("Cleared the streamed music".into(), false);
            }
            "clear-lyrics" => {
                self.core.lyrics_cache_clear();
                self.note("Cleared the lyrics found online".into(), false);
            }
            "clear-covers" => {
                if let Some(d) = self.covers.as_ref().and_then(|l| l.disk()) {
                    d.clear();
                }
                self.note("Cleared the covers".into(), false);
            }
            "measure-again" => {
                let n = self.core.analysis_clear().unwrap_or(0);
                nori_core::automix::planner::analyses_changed();
                self.engine.replan();
                self.note(format!("Forgot {n} measured songs"), false);
            }
            "system-effects" => self.note("A desktop has no system audio effects here".into(), false),
            other => self.note(format!("{other}: not in the terminal"), false),
        }
    }

    /// What the engine said, followed where the core keeps track: plays counted (and sent, as the
    /// settings say), the history recorded, the queue refilled at its end.
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
                // Writes: made offline, they wait in the pending queue and keep their time.
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

    /// A new song: what the core says it asks for (`song_arrived`). Fetching ahead is the engine's own
    /// here (`CoreLibrary::ahead`, as the next song is fetched).
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

    /// A song the network would not bring, handed over by the engine: the core's offline bridge takes it
    /// (`Core::bridge_take`), and the engine is told what it did.
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

    /// The bridge has played up to the parked song: back to it if the server answers, more downloads
    /// before it if not. The server is asked off the screen's thread.
    fn bridge_parked(&self) {
        let (client, core, me) = (self.client.clone(), self.core.clone(), self.handle());
        spawn("nori-bridge", move || {
            let up = block_on(client.read_now(Read::Ping)).is_ok();
            if let Some(edit) = core.bridge_parked(up) {
                me.apply(&edit);
            }
        });
    }

    /// Songs for the queue's end, as "Keep playing when the queue ends" says; a next pressed while they
    /// were on the way is taken when they land.
    fn refill(&self) {
        let (client, me) = (self.client.clone(), self.handle());
        spawn("nori-autofill", move || {
            let fresh = block_on(client.autofill());
            if nori_core::autofill::autofill_arrived(fresh.len() as u32) && !fresh.is_empty() {
                let len = playlist::with(|p| p.len());
                let n = fresh.len();
                playlist::playlist_take(len as u32, fresh.iter().map(|s| s.id.clone()).collect(), vec![Hand::No; n], None);
                me.edited();
            }
            if nori_core::autofill::autofill_landed() {
                me.engine.next();
            }
        });
    }

    /// Next, as the button does it: at the queue's end with refilling on, songs are fetched first.
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
    tx: Sender<Msg>,
}

impl Handle {
    /// The core's queue was edited: the engine told, the queue saved a moment later.
    fn edited(&self) {
        self.engine.queue_changed();
        self.keeper.later(queue_keep(QueueMoment::Edited).save_after_ms);
    }

    /// An edit the core made to its queue by itself (the offline bridge): the engine told, and the jump
    /// it makes played.
    fn apply(&self, e: &QueueEdit) {
        self.edited();
        if e.seek >= 0 {
            self.engine.play_at(e.seek as usize, 0);
        }
    }

    fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        let songs: Vec<Song> = songs.into_iter().filter(|s| !is_provider(s)).collect();
        if songs.is_empty() {
            return;
        }
        nori_core::queue::queue_register(songs.clone());
        let change = playlist::playlist_set(songs.iter().map(|s| s.id.clone()).collect(), if shuffle { -1 } else { start as i32 }, shuffle, from);
        self.edited();
        self.engine.play_at(change.at.max(0) as usize, 0);
    }

    /// `from`: the page they are all the songs of (an album added whole is one album run).
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
        let _ = self.tx.send(Msg::Note { text: format!("{words}: {n} song{}", if n == 1 { "" } else { "s" }), error: false });
    }
}

/// Songs a list row stands for, still to be read.
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

fn fetch_songs(client: &Arc<Client>, what: Fetch) -> Result<Vec<Song>, String> {
    let now = |r: Read| block_on(client.read_now(r)).map_err(|e| said(&e));
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

/// The covers of songs being downloaded, fetched onto the disk and not decoded, so they are there
/// offline though never drawn: which ones, at which addresses, is the core's (`download_cover_urls`).
fn warm_covers(core: &Core, covers: Option<&Arc<Loader>>, songs: &[Song]) {
    let Some(loader) = covers else { return };
    for url in core.download_cover_urls(songs.iter().filter_map(|s| s.cover_art.clone()).collect()) {
        loader.warm(&url);
    }
}

/// A provider's item: octo-fiesta downloads it the moment it is asked for.
pub fn is_provider(s: &Song) -> bool {
    s.is_external || s.id.starts_with("ext-")
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(f);
}

/// A failure in this client's words for it (`text::net_error`), as Android says it.
pub fn said(e: &nori_core::transport::NetError) -> String {
    crate::text::net_error(e)
}

/// A server that could not be reached, in words: the status and what the transport said.
fn unreachable(e: &str) -> String {
    if e.is_empty() {
        "The server did not answer".into()
    } else if e.ends_with(['.', '?']) {
        // Already a sentence of the core's.
        e.to_string()
    } else {
        format!("The server did not answer: {e}")
    }
}

/// The core's screen read (`Client::read_each`: what is stored at once, then the server's answer when it
/// differs; an error only when nothing was stored), each answer sent through `make`. Whether anything was.
fn read_into(client: &Arc<Client>, read: Read, send: &impl Fn(Result<Data, String>), make: impl Fn(Page) -> Option<Data>) -> Result<bool, String> {
    let mut any = false;
    block_on(client.read_each(read, |p| {
        if let Some(d) = make(p) {
            any = true;
            send(Ok(d));
        }
    }))
    .map_err(|e| unreachable(&said(&e)))?;
    Ok(any)
}

fn report(client: &Arc<Client>, read: Read, send: impl Fn(Result<Data, String>), make: impl Fn(Page) -> Option<Data>) {
    match read_into(client, read, &send, make) {
        Ok(true) => {}
        Ok(false) => send(Err("The server sent nothing for this page".into())),
        Err(e) => send(Err(e)),
    }
}

fn sync(client: &Arc<Client>, tx: &Sender<Msg>) {
    let _ = tx.send(Msg::Note { text: "Filling the offline index…".into(), error: false });
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
                let _ = tx.send(Msg::Note { text: format!("The offline index stopped: {e}"), error: true });
                return;
            }
        }
    }
    let _ = tx.send(Msg::Note { text: format!("Offline index: {} songs, {} albums, {} artists", total.songs, total.albums, total.artists), error: false });
}

/// What the settings pages show besides the settings, as far as a desktop knows it.
fn facts(core: &Arc<Core>, client: &Arc<Client>, store: &Arc<Store>, cover_bytes: u64, offline: bool) -> Facts {
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
    let db_bytes = std::fs::metadata(PathBuf::from(core_db_path())).map(|m| m.len() as i64).unwrap_or(0);
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

/// A monotonic clock in ms, for the scrobbler's play and pause edges.
fn monotonic_ms() -> i64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as i64
}

/// The local time zone's offset from UTC at wall time `wall_ms`, ms.
fn tz_offset_ms(wall_ms: i64) -> i32 {
    (nori_core::library::local_offset_s(wall_ms / 1000) * 1000) as i32
}

/// The database file, as the runner opened it.
static DB: parking_lot::Mutex<String> = parking_lot::Mutex::new(String::new());

pub fn set_core_db_path(p: String) {
    *DB.lock() = p;
}

fn core_db_path() -> String {
    DB.lock().clone()
}

/// The page's colours from a cover, as Android's `CoverLoader.colours` works them out: the picture's
/// straight RGBA as ARGB, into `nori_look::cover::derive`, for the dark theme.
pub fn derive(image: &Image) -> CoverColours {
    let px: Vec<u32> = image.pixels.chunks_exact(4).map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]])).collect();
    nori_look::cover::derive(&px, image.width as usize, image.height as usize, true, false)
}
