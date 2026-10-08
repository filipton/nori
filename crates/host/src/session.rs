//! One open server profile: core, client, the engine on the client's output, store, downloader, cover
//! loader and search. Work that waits on the network runs on its own thread and reports through the
//! client's [`Out`], as facts the client words.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use nori_core::bridge::BridgeTake;
use nori_core::cache_policy::{Page, Read};
use nori_core::client::{Client, Starrable};
use nori_core::covers::CoverNet;
use nori_core::library::StarsShown;
use nori_core::playlist::{Hand, QueueEdit};
use nori_core::race::{LyricsPick, LyricsShown};
use nori_core::rules::{queue_keep, BridgeStep, QueueMoment};
use nori_core::search::{SearchSession, SearchView};
use nori_core::settings::{SavedServer, SettingChange, StoredPrefs};
use nori_core::settings_store::{APPLY_AUDIO, APPLY_GAIN, CACHE_LIMIT, PLAYER, REPLAN, SOUND};
use nori_core::transport::{block_on, Exchange, FailureKind, NetError, Transport, TransportError, TransportResponse};
use nori_core::{Core, CoreError, IngestStats, PageOrigin, Song};
use nori_covers::loader::{Config as CoverConfig, Loader, Ticket};
use nori_covers::memory::Image;
use nori_engine::core::{settings, Analyses, CoreApp, CoreLibrary, CoreQueue, Downloader, Measurer};
use nori_engine::{AudioOutput, Body, ByteSource, Cancel, Config, Engine, Event, OpenError, State, Store};
use nori_http::Http;
use nori_look::cover::CoverColours;

#[cfg(feature = "desktop")]
use crate::Controls;
use crate::remote::Press;
use crate::{config, db_path, derive, net, save, spawn, Fetch, Keeper, Level};
use nori_remote::wire::Op;

/// What a session reports from any thread.
pub enum Said {
    Engine(Event),
    /// One of possibly several lyrics answers for `song`, each better than the last.
    Lyrics { song: String, pick: LyricsPick },
    Search(SearchView),
    Note(Note),
    /// The reachability check made when a session opens.
    Reachable(Result<(), NetError>),
    /// The other devices or the jam changed (remote control): read them again.
    Remote,
    /// Another device set the volume (0 to 1).
    Volume(f32),
    /// A heart changed (pressed here, or by another device): hearts are read again ([`Session::starred`]), or drawn from the core's marks
    /// carried here.
    Starred(nori_core::stars::StarMarks),
}

/// Something done or failed, for the status line.
pub enum Note {
    Queued { next: bool, songs: usize },
    NothingToPlay,
    SongsFailed(NetError),
    NothingToPutBack,
    Downloading(usize),
    DownloadFailed(CoreError),
    Starred(bool),
    StarFailed(NetError),
    Indexing,
    Indexed(IngestStats),
    IndexStopped(NetError),
    Done(Chore),
    /// Forgot this many songs' measurements.
    Forgot(u32),
}

/// Maintenance a settings page runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chore {
    SyncLibrary,
    DownloadLibrary,
    MeasureAgain,
    ClearStream,
    ClearCovers,
    ClearLyrics,
}

/// Where a session's reports go.
pub type Out = Arc<dyn Fn(Said) + Send + Sync>;

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
        self.open_cancellable(url, None, from, &Cancel::new())
    }

    fn open_cancellable(&self, url: &str, key: Option<&str>, from: u64, cancel: &Cancel) -> Result<Body, OpenError> {
        if self.offline {
            return Err("offline".into());
        }
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.http.open_cancellable(url, key, from, cancel)
    }

    fn open_live(&self, url: &str) -> Result<(Body, Option<usize>), String> {
        if self.offline {
            return Err("offline".into());
        }
        self.http.open_live(url)
    }
}

/// The offline transport: every request fails as unreachable, so the core shows what is stored and
/// queues writes for later.
pub(crate) struct Offline;

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

    fn network(&self) -> nori_core::transport::Network {
        nori_core::transport::Network::Unmetered
    }
}

struct Shown {
    song: String,
    out: Out,
}

impl LyricsShown for Shown {
    fn show(&self, pick: LyricsPick) {
        (self.out)(Said::Lyrics { song: self.song.clone(), pick });
    }
}

/// The core's star marks moved: the client reads its hearts again, and other devices see the queue's.
struct Hearts {
    out: Out,
    remotes: Arc<crate::remote::Remotes>,
    engine: Arc<Engine>,
}

impl StarsShown for Hearts {
    fn marks(&self, marks: nori_core::stars::StarMarks) {
        self.remotes.played(&self.engine);
        (self.out)(Said::Starred(marks));
    }
}

pub struct Open<'a> {
    /// The app's queue session, over its settings.
    pub queue: Arc<nori_core::queue::Session>,
    pub data: &'a Path,
    pub http: Arc<Http>,
    pub profile: SavedServer,
    /// The sound card. The client built it and owns its device volume.
    pub output: Box<dyn AudioOutput>,
    /// The volume, set through [`Session::set_volume`] (or followed with [`Session::volume_followed`] where
    /// the system keeps it).
    pub volume: Arc<Level>,
    /// Bytes the engine may hold for songs ahead. 256 on the desktop, less on a phone.
    pub memory_mb: u32,
    pub covers: bool,
    pub offline: bool,
    /// The process's media controls, driven by this session while it is open. Always a field, so a
    /// client built without `desktop` (where nothing can fill it) compiles either way.
    pub mpris: Option<MediaControls>,
    /// This device as the account's other devices list it (remote control).
    pub device: nori_core::remote::RemoteMe,
    /// The platform's mDNS for remote control (Bonjour on iOS). None: with `desktop`, the session's own
    /// (mdns-sd); without, no nearby devices.
    pub discovery: Option<Arc<dyn nori_core::remote::Discovery>>,
    pub out: Out,
}

#[cfg(feature = "desktop")]
pub type MediaControls = Arc<nori_mpris::Mpris>;
/// No media controls without `desktop`: only `None` fits.
#[cfg(not(feature = "desktop"))]
pub enum MediaControls {}

pub struct Session {
    pub core: Arc<Core>,
    pub client: Arc<Client>,
    pub engine: Arc<Engine>,
    pub store: Arc<Store>,
    downloader: Arc<Downloader>,
    pub covers: Option<Arc<Loader>>,
    level: Arc<Level>,
    search: Arc<SearchSession>,
    pub offline: bool,
    #[cfg(feature = "desktop")]
    mpris: Option<Arc<nori_mpris::Mpris>>,
    keeper: Arc<Keeper>,
    /// The database file, for its size.
    pub db: PathBuf,
    /// Remote control and jams, while switched on.
    remotes: Arc<crate::remote::Remotes>,
    device: nori_core::remote::RemoteMe,
    /// The profile is a jam guest's: songs picked are asked of the jam's host, and nothing else plays.
    pub guest: bool,
    discovery: Option<Arc<dyn nori_core::remote::Discovery>>,
    out: Out,
}

impl Session {
    /// Opens the profile without touching the network, so an unreachable server still opens with what
    /// is stored; [`Session::check`] reports reachability.
    pub fn open(o: Open) -> Result<Session, String> {
        let db = db_path(o.data);
        let core = Core::new(db.clone(), nori_core::settings::server_db_id(&o.profile.id), o.queue.clone()).map_err(|e| format!("the database: {e}"))?;
        core.configure(config(&o.profile)).map_err(|e| format!("the server: {e}"))?;
        let transport: Arc<dyn Transport> = if o.offline { Arc::new(Offline) } else { o.http.clone() };
        let cover_net = CoverNet::over(o.http.clone());
        let client = Client::new(core.clone(), transport, cover_net.clone());
        client.set_profile(net(&o.profile));
        let prefs = core.session.settings.current().unwrap_or_default();
        // Set by the client before the engine starts, so the first chain has the right compensation.
        let level = o.volume;
        let loudness = level.loudness.clone();
        let output = o.output;
        let store = Store::open(o.data.join("music"), prefs.cache_mb.max(0) as u64 * 1024 * 1024).map_err(|e| format!("the music directory: {e}"))?;
        let audio = Arc::new(Audio::new(o.http.clone(), o.offline));
        let analyses = Analyses::of(client.clone());
        let downloader = Downloader::new(client.clone(), audio.clone(), store.clone(), analyses.clone());
        // `bridging`: a song the network cannot bring raises `Event::Bridge` (see `Session::bridge`).
        let app = CoreApp::new(core.session.clone()).measuring(Measurer::new(analyses.clone(), store.clone())).per_device(core.clone()).bridging().volume(loudness.clone());
        let library = CoreLibrary { client: client.clone(), bytes: audio, store: Some(store.clone()), analyses };
        let events = o.out.clone();
        let config = Config { memory_mb: o.memory_mb, settings: settings(&prefs, loudness.db()), ..Config::default() };
        let engine = Arc::new(Engine::start(library, app, CoreQueue(core.session.clone()), output, None, config, move |e| events(Said::Engine(e))));
        let covers = o.covers.then(|| Arc::new(Loader::new(CoverConfig::new(o.data.join("covers")), cover_net)));
        let remotes = Arc::new(crate::remote::Remotes::new(level.clone()));
        let keeper = Keeper::start(core.clone(), engine.clone());
        let guest = nori_remote::is_guest_key(&o.profile.api_key);
        let s = Session { core, client, engine, store, downloader, covers, level, search: SearchSession::new(), offline: o.offline, #[cfg(feature = "desktop")] mpris: o.mpris, keeper, db: PathBuf::from(db), remotes, device: o.device, guest, discovery: o.discovery, out: o.out };
        #[cfg(feature = "desktop")]
        if let Some(m) = &s.mpris {
            let cover = crate::remote::NowCover::new(s.covers.clone(), s.core.clone(), Arc::downgrade(m));
            m.serve(Some(Arc::new(crate::remote::Keys { here: Controls::over_queue(s.engine.clone(), s.core.session.clone()), remotes: s.remotes.clone(), hearts: s.handle(), cover })));
        }
        s.restore();
        s.follow_remote();
        if !s.offline && s.core.download_counts().pending > 0 {
            s.start_downloads();
        }
        Ok(s)
    }

    fn note(&self, n: Note) {
        (self.out)(Said::Note(n));
    }

    /// Tells the media controls the song or state changed.
    #[cfg(feature = "desktop")]
    pub fn mpris_changed(&self) {
        if let Some(m) = &self.mpris {
            m.changed();
        }
    }

    /// Pings the server in the background, fills an empty offline index and flushes pending writes.
    pub fn check(&self) {
        if self.offline {
            return;
        }
        let (client, core, out, guest) = (self.client.clone(), self.core.clone(), self.out.clone(), self.guest);
        spawn("nori-check", move || {
            let r = block_on(client.read_now(Read::Ping)).map(|_| ());
            let ok = r.is_ok();
            out(Said::Reachable(r));
            // Search and the songs list read the offline index; a guest has no library to index.
            if ok && !guest && core.index_size().map_or(true, |s| s.songs == 0) {
                sync(&client, &out);
            }
            let _ = block_on(client.flush_pending());
        });
    }

    /// Fills the offline index from the server in the background.
    pub fn sync(&self) {
        let (client, out) = (self.client.clone(), self.out.clone());
        spawn("nori-sync", move || sync(&client, &out));
    }

    /// Restores the saved queue, paused at its position.
    fn restore(&self) {
        let Ok(q) = self.core.load_queue() else { return };
        if q.songs.is_empty() {
            return;
        }
        let index = q.index as usize;
        self.core.session.set(q.songs.iter().map(|s| s.id.clone()).collect(), Some(index as u32), false, q.origin);
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

    /// Plays `songs` from `start` (with `shuffle`, from wherever shuffle starts), on the device playing.
    /// Provider songs are dropped unless picked: the server downloads whatever is requested. `from` is the
    /// page the songs are the list of (`playlist_set`).
    pub fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        self.handle().play(songs, start, shuffle, from);
    }

    /// `songs` as the queue around the song playing, `songs[at]`: it goes on where it is, playing or
    /// paused, rather than starting again (a tap on it in a list). Any other song plays as [`Session::play`]
    /// would. True when it went on.
    pub fn keep_playing(&self, songs: Vec<Song>, at: usize, from: Option<PageOrigin>) -> bool {
        let playing = self.engine.status_with(|s| s.id.clone());
        let keep = playing.is_some() && songs.get(at).map(|s| &s.id) == playing.as_ref() && self.elsewhere().is_none();
        self.handle().play_kept(songs, at, false, from, keep);
        keep
    }

    /// Plays an album, playlist or artist once its songs are fetched.
    pub fn play_later(&self, what: Fetch, shuffle: bool) {
        let (client, me) = (self.client.clone(), self.handle());
        let origin = what.origin();
        spawn("nori-play", move || match what.songs(&client) {
            Ok(songs) if !songs.is_empty() => me.play(songs, 0, shuffle, Some(origin)),
            Ok(_) => me.note(Note::NothingToPlay),
            Err(e) => me.note(Note::SongsFailed(e)),
        });
    }

    /// Adds songs after the current one (`next`) or at the end, on the device playing.
    pub fn enqueue(&self, songs: Vec<Song>, next: bool) {
        self.handle().add(songs, next);
    }

    /// Enqueues an album, playlist or artist once fetched.
    pub fn enqueue_later(&self, what: Fetch, next: bool) {
        let (client, me) = (self.client.clone(), self.handle());
        spawn("nori-enqueue", move || match what.songs(&client) {
            Ok(songs) => me.add(songs, next),
            Err(e) => me.note(Note::SongsFailed(e)),
        });
    }

    /// The account's active device while it is another one, which the player shows and every control
    /// here acts on.
    pub fn elsewhere(&self) -> Option<crate::remote::Elsewhere> {
        self.remotes.elsewhere()
    }

    /// Plays or pauses; a queue restored but never started starts where it was.
    pub fn toggle(&self) {
        if self.there(Press::Toggle) {
            return;
        }
        let st = self.engine.status();
        let restored = self.core.session.playlist(|p| p.current());
        match restored.filter(|_| st.state == State::Idle) {
            Some(at) => {
                self.engine.play_at(at, st.position_now());
            }
            None => self.engine.toggle(),
        }
    }

    pub fn previous(&self) {
        if !self.there(Press::Previous) {
            self.engine.previous();
        }
    }

    pub fn seek(&self, ms: i64) {
        if !self.there(Press::Seek(ms)) {
            self.engine.seek(ms);
        }
    }

    /// Plays the song at list index `index`.
    pub fn jump(&self, index: usize) {
        if !self.there(Press::Jump(index as u32)) {
            self.engine.play_at(index, 0);
        }
    }

    /// Sends `press` to the active device while it is another one; false while this one plays. A jam
    /// guest's presses go nowhere but its own volume.
    fn there(&self, press: Press) -> bool {
        if self.guest && !matches!(press, Press::Volume(_)) {
            return true;
        }
        self.elsewhere().map(|e| e.press(press)).is_some()
    }

    fn handle(&self) -> Handle {
        Handle { engine: self.engine.clone(), queue: self.core.session.clone(), client: self.client.clone(), keeper: self.keeper.clone(), remotes: self.remotes.clone(), level: self.level.clone(), guest: self.guest, out: self.out.clone() }
    }

    /// The remote control and jams, while switched on: other devices to control, the jam hosted.
    pub fn remote(&self) -> Option<Arc<nori_core::remote::Remote>> {
        self.remotes.get()
    }

    /// Makes or drops the remote as the settings say, and serves while remote control is on.
    fn follow_remote(&self) {
        let prefs = self.core.session.settings.current().unwrap_or_default();
        if self.offline || !(prefs.remote_control || prefs.jam || self.guest) {
            return self.remotes.set(None);
        }
        let remote = self.remotes.get().unwrap_or_else(|| {
            #[cfg(feature = "desktop")]
            let mdns = self.discovery.is_none().then(crate::remote::Mdns::start).flatten();
            #[cfg(feature = "desktop")]
            let discovery = self.discovery.clone().or_else(|| mdns.clone().map(|m| m as Arc<dyn nori_core::remote::Discovery>));
            #[cfg(not(feature = "desktop"))]
            let discovery = self.discovery.clone();
            let player = Arc::new(crate::remote::HostPlayer(self.handle()));
            let shown = crate::remote::Shown { out: self.out.clone(), remotes: self.remotes.clone(), engine: self.engine.clone() };
            let r = nori_core::remote::Remote::new(self.client.clone(), self.device.clone(), player, Arc::new(shown), discovery);
            #[cfg(feature = "desktop")]
            if let Some(m) = &mdns {
                m.serve(&r);
            }
            self.remotes.set(Some(r.clone()));
            r
        });
        // A guest is no device of the account's; it follows its jam.
        remote.clone().serve(prefs.remote_control && !self.guest);
        if self.guest {
            remote.clone().watch(true);
        }
        if !prefs.jam {
            // Jams switched off while remote control stays on: the one hosted ends.
            remote.jam_close();
        }
        self.remotes.played(&self.engine);
    }

    fn edited(&self) {
        self.handle().edited();
    }

    /// Removes the song at list index `index` on the device playing; if it was playing, the next one
    /// takes its place.
    pub fn remove(&self, index: usize) {
        if !self.there(Press::Remove(index as u32)) {
            self.handle().remove(index);
        }
    }

    /// Removes everything after the current song, on the device playing.
    pub fn clear_upcoming(&self) {
        if self.there(Press::Clear) {
            return;
        }
        for i in self.core.session.playlist(|p| p.after_current()) {
            self.core.session.remove(i as u32, i as u32 + 1);
        }
        self.edited();
    }

    /// Undoes the removal of `id` where it was removed (here, or on the device playing); the playing
    /// song is unchanged.
    pub fn put_back(&self, id: &str) {
        let back = match self.elsewhere() {
            Some(e) => e.put_back(id),
            None => self.handle().restore(id),
        };
        if !back {
            self.note(Note::NothingToPutBack);
        }
    }

    /// Moves the song at list index `from` to `to`, on the device playing.
    pub fn move_song(&self, from: usize, to: usize) {
        if !self.there(Press::Move(from as u32, to as u32)) {
            self.handle().move_song(from, to);
        }
    }

    pub fn shuffle(&self, on: bool) {
        if !self.there(Press::Shuffle(on)) {
            self.handle().shuffle(on);
        }
    }

    pub fn repeat(&self, mode: u8) {
        if !self.there(Press::Repeat(mode)) {
            self.handle().repeat(mode);
        }
    }

    fn start_downloads(&self) {
        self.downloader.start(self.core.session.settings.prefs(|p| p.parallel_downloads).max(1) as usize);
    }

    /// Queues `songs` for download (their covers fetched to disk too) and starts the downloader.
    pub fn download(&self, songs: Vec<Song>) {
        self.note(queue_downloads(&self.core, self.covers.as_ref(), songs));
        self.start_downloads();
    }

    /// [`Session::download`] of what `what` names, asked of the server on a thread of its own.
    pub fn download_later(&self, what: Fetch) {
        let (client, core, downloader, out, covers) = (self.client.clone(), self.core.clone(), self.downloader.clone(), self.out.clone(), self.covers.clone());
        spawn("nori-download-ask", move || match what.songs(&client) {
            Ok(songs) => {
                out(Said::Note(queue_downloads(&core, covers.as_ref(), songs)));
                downloader.start(core.session.settings.prefs(|p| p.parallel_downloads).max(1) as usize);
            }
            Err(e) => out(Said::Note(Note::SongsFailed(e))),
        });
    }

    /// Deletes a download from disk.
    pub fn download_remove(&self, id: &str) {
        let _ = self.core.download_remove(id.to_string());
        let _ = std::fs::remove_file(self.store.download_path(id));
    }

    /// Sets a setting by name and applies its effects, as Android's player does. None if no such setting.
    pub fn setting(&self, name: &str, value: &str) -> Option<SettingChange> {
        let change = self.core.session.settings.edit_by_name(name, value)?;
        self.apply(change.effect, &change.prefs);
        if matches!(name, "remoteControl" | "jam") {
            self.follow_remote();
        }
        if change.effect & CACHE_LIMIT != 0 {
            self.store.set_limit(change.prefs.cache_mb.max(0) as u64 * 1024 * 1024);
        }
        if change.server {
            // Per-server settings (music folder, second address bitrate) live on the profile.
            if let Some(s) = change.prefs.servers.iter().find(|s| s.id == change.prefs.active_server_id) {
                self.client.set_profile(net(s));
            }
        }
        Some(change)
    }

    /// The system moved the volume by itself (its keys, where it keeps the volume), 0 to 1: loudness
    /// compensation follows, and the account's other devices see it.
    pub fn volume_followed(&self, v: f32) {
        self.handle().set_volume(v);
    }

    /// The volume, 0 to 1.
    pub fn volume(&self) -> f32 {
        self.level.get()
    }

    /// Sets the volume of the device playing, as the client's own control does; the account's other
    /// devices see it.
    pub fn set_volume(&self, v: f32) {
        if !self.there(Press::Volume(v)) {
            self.handle().set_volume(v);
        }
    }

    /// Applies a settings change's `effect` bits to the engine.
    pub fn apply(&self, effect: u32, prefs: &StoredPrefs) {
        if effect & (APPLY_AUDIO | SOUND | PLAYER) != 0 {
            self.engine.set_settings(settings(prefs, self.level.loudness.db()));
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
        if let Some(p) = self.core.session.settings.current() {
            self.apply(effect, &p);
        }
    }

    /// Fetches lyrics for `song`; each better answer arrives as [`Said::Lyrics`].
    pub fn lyrics(&self, song: String) {
        let (client, out) = (self.client.clone(), self.out.clone());
        spawn("nori-lyrics", move || {
            let _ = block_on(client.lyrics_for(song.clone(), Arc::new(Shown { song, out })));
        });
    }

    /// Requests cover `id` at `px` square: the core's rendition for that size is fetched
    /// ([`nori_core::covers::cover_rendition`]) and decoded to `px`. `done` gets the image and, when
    /// `colours`, its page colours (derived on the loader thread). None without a cover loader.
    pub fn cover(&self, id: String, px: u32, colours: bool, done: impl FnOnce(Arc<Image>, Option<Box<CoverColours>>) + Send + 'static) -> Option<Ticket> {
        let url = self.core.cover_address(id, nori_core::covers::cover_rendition(px));
        Some(self.covers.as_ref()?.request(&url, px, px, move |r| {
            if let Ok(image) = r {
                let colours = colours.then(|| Box::new(derive(&image)));
                done(image, colours);
            }
        }))
    }

    /// Searches the offline index for the typed text.
    pub fn search_typed(&self, text: &str) -> SearchView {
        let view = self.search.typed(text.to_string());
        if view.query.is_empty() {
            return view;
        }
        let limit = nori_core::browse::library_sizes().local_search;
        self.search.local(self.core.clone(), view.query.clone(), limit).ok().flatten().unwrap_or(view)
    }

    /// Searches the server in the background. `error` words a failure for the results.
    pub fn search_server(&self, query: String, error: fn(&NetError) -> String) {
        if self.offline || query.trim().is_empty() {
            return;
        }
        let _ = self.core.search_remember_recent(query.clone());
        let (search, client, out) = (self.search.clone(), self.client.clone(), self.out.clone());
        spawn("nori-search", move || match block_on(search.ask(client, query.clone())) {
            Ok(Some(v)) => out(Said::Search(v)),
            Ok(None) => {}
            Err(e) => {
                if let Some(v) = search.failed(query, Some(error(&e))) {
                    out(Said::Search(v));
                }
            }
        });
    }

    /// Stars or unstars on the server (queued when offline); says so if "Confirm favorites" is on, and
    /// always when it fails.
    pub fn star(&self, kind: Starrable, id: String, on: bool) {
        // A jam guest's key cannot star.
        if self.guest {
            return;
        }
        let (client, out, notice, hearts) = (self.client.clone(), self.out.clone(), self.core.favourite_notice(), self.handle().hearts());
        spawn("nori-star", move || {
            let said = match block_on(client.star(kind, id, on, hearts)) {
                Ok(()) if !notice => return,
                Ok(()) => Note::Starred(on),
                Err(e) => Note::StarFailed(e),
            };
            out(Said::Note(said));
        });
    }

    /// Whether `song` shows starred: as `there` (the device playing, while it is another one) shows it
    /// when the song is in its queue, else this session's mark over the song's record.
    pub fn starred(&self, song: &Song, there: Option<&crate::remote::Elsewhere>) -> bool {
        there.and_then(|e| e.starred(&song.id)).unwrap_or_else(|| self.core.starred(Starrable::Song, &song.id, song.starred))
    }

    /// Stars or unstars a song once: through `there` while the song is in its queue (that device tells
    /// the server and shows it), else from here.
    pub fn star_song(&self, id: String, on: bool, there: Option<&crate::remote::Elsewhere>) {
        match there.filter(|e| e.starred(&id).is_some()) {
            Some(e) => e.send(Op::Star { id, on }),
            None => self.star(Starrable::Song, id, on),
        }
    }

    /// Runs a settings page's maintenance button.
    pub fn action(&self, chore: Chore) {
        match chore {
            Chore::SyncLibrary => return self.sync(),
            Chore::DownloadLibrary => {
                let r = self.core.download_queue_library();
                self.start_downloads();
                return match r {
                    Ok(q) => self.note(Note::Downloading(q.fresh.len() + q.again.len())),
                    Err(e) => self.note(Note::DownloadFailed(e)),
                };
            }
            Chore::ClearStream => self.store.clear_cache(),
            Chore::ClearLyrics => self.core.lyrics_cache_clear(),
            Chore::ClearCovers => {
                if let Some(d) = self.covers.as_ref().and_then(|l| l.disk()) {
                    d.clear();
                }
            }
            Chore::MeasureAgain => {
                let n = self.core.analysis_clear().unwrap_or(0);
                self.core.session.planner.analyses_changed();
                self.engine.replan();
                return self.note(Note::Forgot(n));
            }
        }
        self.note(Note::Done(chore));
    }

    /// Feeds an engine event to the core: scrobbling, queue saves and refills, the offline bridge.
    pub fn followed(&self, e: &Event) {
        use nori_core::scrobble::TrackChange;
        let q = &self.core.session;
        let (now, wall) = (monotonic_ms(), nori_core::db::now_ms());
        let tz = (nori_core::library::local_offset_s(wall / 1000) * 1000) as i32;
        let playing = self.engine.status_with(|s| s.state == State::Playing);
        let send = match e {
            Event::Song { id, .. } => Some(q.scrobble_track(Some(id.clone()), TrackChange::Moved, playing, now, wall, tz)),
            Event::Looped { id, .. } => Some(q.scrobble_track(Some(id.clone()), TrackChange::Looped, playing, now, wall, tz)),
            Event::State(State::Ended) => Some(q.scrobble_track(None, TrackChange::Ended, false, now, wall, tz)),
            Event::State(s) => {
                q.scrobble_playing(*s == State::Playing, now);
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
        if let Event::Buffering(on) = e {
            self.remotes.buffering(*on);
        }
        if matches!(e, Event::Song { .. } | Event::Looped { .. } | Event::State(_) | Event::Position { .. } | Event::Placed { .. } | Event::Buffering(_)) {
            self.remotes.played(&self.engine);
        }
    }

    /// Runs the core's steps for a new song (`nori_queue::Session::song_arrived`).
    fn arrived(&self) {
        let steps = self.core.session.song_arrived();
        self.keeper.later(steps.save_after_ms);
        if steps.fill {
            self.refill();
        }
        if steps.bridge == BridgeStep::Parked {
            // Back to the parked song if the server answers, else bridged further.
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

    /// Appends autofill songs; a next pressed while fetching is applied when they land.
    fn refill(&self) {
        let (client, me) = (self.client.clone(), self.handle());
        spawn("nori-autofill", move || {
            let fresh = block_on(client.autofill());
            let ids: Vec<String> = fresh.songs.iter().map(|s| s.id.clone()).collect();
            if client.autofill_arrived(fresh) && !ids.is_empty() {
                let len = me.queue.playlist(|p| p.len());
                let n = ids.len();
                me.queue.take(len as u32, ids, vec![Hand::No; n]);
                me.edited();
            }
            if me.queue.autofill_landed() {
                me.engine.next();
            }
        });
    }

    /// Next; at the queue's end with autofill on, fetches songs first.
    pub fn next(&self) {
        if self.there(Press::Next) {
            return;
        }
        if self.core.session.playlist(|p| p.next().is_some()) {
            self.engine.next();
            return;
        }
        match self.core.session.autofill_next() {
            nori_core::autofill::FillNext::Skip => {
                self.engine.next();
            }
            nori_core::autofill::FillNext::Fetch => self.refill(),
            nori_core::autofill::FillNext::Wait => {}
        }
    }

    /// Saves the queue and stops the engine.
    pub fn close(&self) {
        #[cfg(feature = "desktop")]
        if let Some(m) = &self.mpris {
            m.serve(None);
        }
        self.keep(QueueMoment::Closing);
        self.remotes.set(None);
        self.keeper.stop();
        self.engine.stop();
    }
}

/// The parts of a session worker threads (and the remote control) use.
pub(crate) struct Handle {
    pub(crate) engine: Arc<Engine>,
    queue: Arc<nori_core::queue::Session>,
    client: Arc<Client>,
    keeper: Arc<Keeper>,
    pub(crate) remotes: Arc<crate::remote::Remotes>,
    level: Arc<Level>,
    guest: bool,
    out: Out,
}

impl Handle {
    fn note(&self, n: Note) {
        (self.out)(Said::Note(n));
    }

    /// Sets the volume; the account's other devices see it.
    pub(crate) fn set_volume(&self, v: f32) {
        if self.level.set(v) {
            self.loudness_moved();
        }
        self.remotes.played(&self.engine);
    }

    /// Another device set the volume: as [`Handle::set_volume`], and the client is told.
    pub(crate) fn volume_from_afar(&self, v: f32) {
        self.set_volume(v);
        (self.out)(Said::Volume(self.level.get()));
    }

    /// Rebuilds the chain for the volume's loudness compensation, when that is on.
    fn loudness_moved(&self) {
        if let Some(p) = self.queue.settings.current().filter(|p| p.loudness) {
            self.engine.set_settings(settings(&p, self.level.loudness.db()));
        }
    }

    /// Whether `song` shows starred here: its record, or a heart pressed since.
    pub(crate) fn starred(&self, song: &Song) -> bool {
        self.client.core().starred(Starrable::Song, &song.id, song.starred)
    }

    /// The marks moving, as this session shows them.
    fn hearts(&self) -> Arc<Hearts> {
        Arc::new(Hearts { out: self.out.clone(), remotes: self.remotes.clone(), engine: self.engine.clone() })
    }

    /// Another device favourited a song (or not): the server is told, and the core's marks follow; the
    /// devices see it once marked.
    pub(crate) fn star(&self, id: String, on: bool) {
        let (client, hearts) = (self.client.clone(), self.hearts());
        spawn("nori-star", move || {
            if let Err(e) = block_on(client.star(Starrable::Song, id, on, hearts)) {
                nori_core::alog::info(&format!("remote: star not kept: {e}"));
            }
        });
    }

    /// After a queue edit: tells the engine and schedules a save.
    fn edited(&self) {
        self.engine.queue_changed();
        self.keeper.later(queue_keep(QueueMoment::Edited).save_after_ms);
        self.remotes.played(&self.engine);
    }

    /// Removes the song at list index `index`; if it was playing, the next one takes its place.
    pub(crate) fn remove(&self, index: usize) {
        let (current, playing) = self.engine.status_with(|s| (s.index, s.state == State::Playing));
        let change = self.queue.remove(index as u32, index as u32 + 1);
        self.edited();
        if let (true, Some(at)) = (current == Some(index), change.at) {
            if playing {
                self.engine.play_at(at as usize, 0);
            } else {
                self.engine.go_to(at as usize, 0);
            }
        }
    }

    /// Puts back `id` where it was taken out; false when it was not the last song removed.
    fn restore(&self, id: &str) -> bool {
        let was_empty = self.queue.playlist(|p| p.is_empty());
        if self.queue.restore(id.to_string()).at.is_none() {
            return false;
        }
        self.edited();
        if was_empty {
            self.engine.go_to(0, 0);
        }
        true
    }

    /// Another device's undo: `song` back where it was taken out, else at list index `index`.
    pub(crate) fn put_back(&self, song: Song, index: usize) {
        if self.restore(&song.id) {
            return;
        }
        let (len, id) = (self.queue.playlist(|p| p.len()), song.id.clone());
        self.queue.register(vec![song]);
        self.queue.take(index.min(len) as u32, vec![id], vec![Hand::No]);
        self.edited();
        if len == 0 {
            self.engine.go_to(0, 0);
        }
    }

    pub(crate) fn move_song(&self, from: usize, to: usize) {
        self.queue.move_range(from as u32, from as u32 + 1, to as u32);
        self.edited();
    }

    pub(crate) fn shuffle(&self, on: bool) {
        self.queue.show_shuffle(on);
        self.queue.shuffle(on);
        self.edited();
    }

    pub(crate) fn repeat(&self, mode: u8) {
        // Set on the queue first so the screen shows it at once.
        self.queue.repeat(mode);
        self.engine.set_repeat(mode);
    }

    /// A queue handed over from another device: `songs` from `index` at `position_ms`, playing or not,
    /// shuffled into `order` under `shuffle`, repeating as `repeat` says.
    #[allow(clippy::too_many_arguments, reason = "the transfer's own fields, as they came")]
    pub(crate) fn replace(&self, songs: Vec<Song>, index: usize, position_ms: i64, play: bool, order: Option<Vec<u32>>, shuffle: bool, repeat: u8) {
        if songs.is_empty() {
            return;
        }
        self.queue.register(songs.clone());
        let change = self.queue.handed(songs.iter().map(|s| s.id.clone()).collect(), index.min(songs.len() - 1) as u32, order, shuffle, repeat);
        self.engine.set_repeat(repeat);
        self.edited();
        let at = change.at.unwrap_or(0) as usize;
        if play {
            self.engine.play_at(at, position_ms);
        } else {
            self.engine.go_to(at, position_ms);
        }
    }

    /// Applies a queue edit the core made itself (the offline bridge).
    fn apply(&self, e: &QueueEdit) {
        self.edited();
        if let Some(seek) = e.seek {
            self.engine.play_at(seek as usize, 0);
        }
    }

    fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        self.play_kept(songs, start, shuffle, from, false);
    }

    /// [`Handle::play`]; `keep`: the start is the song playing, which goes on with no jump (the queue's
    /// `set` keeps its entry). Sent to the active device while it is another one.
    fn play_kept(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>, keep: bool) {
        let picked = songs.get(start).map(|s| s.id.clone());
        let songs: Vec<Song> = songs.into_iter().filter(|s| !s.is_provider() || Some(&s.id) == picked.as_ref()).collect();
        if songs.is_empty() {
            return;
        }
        let start = picked.and_then(|id| songs.iter().position(|s| s.id == id)).unwrap_or(0);
        if self.guest {
            return self.ask(songs.into_iter().skip(start).take(1).collect());
        }
        if let Some(e) = self.remotes.elsewhere() {
            return e.play(songs, start, shuffle);
        }
        self.queue.register(songs.clone());
        let change = self.queue.set(songs.iter().map(|s| s.id.clone()).collect(), (!shuffle).then_some(start as u32), shuffle, from);
        self.edited();
        if !keep {
            self.engine.play_at(change.at.unwrap_or(0) as usize, 0);
        }
    }

    /// [`Handle::enqueue`] on the device playing: the active device while it is another one.
    fn add(&self, songs: Vec<Song>, next: bool) {
        if self.guest {
            return self.ask(queueable(songs));
        }
        let Some(e) = self.remotes.elsewhere() else { return self.enqueue(songs, next) };
        let songs = queueable(songs);
        if !songs.is_empty() {
            e.send(Op::Add { songs, next });
        }
    }

    /// A jam guest asks the host for `songs`, each a request of its own.
    fn ask(&self, songs: Vec<Song>) {
        let Some(r) = self.remotes.get() else { return };
        for song in songs {
            r.clone().jam_act(Op::Request { song });
        }
    }

    pub(crate) fn enqueue(&self, songs: Vec<Song>, next: bool) {
        let songs = queueable(songs);
        if songs.is_empty() {
            return;
        }
        let n = songs.len();
        self.queue.register(songs.clone());
        let (len, current) = self.queue.playlist(|p| (p.len(), p.current()));
        let at = if next { current.map_or(len, |c| c + 1) } else { len };
        let hand = if next { Hand::Next } else { Hand::Last };
        self.queue.take(at as u32, songs.iter().map(|s| s.id.clone()).collect(), vec![hand; n]);
        self.edited();
        if len == 0 {
            self.engine.go_to(0, 0);
        }
        self.note(Note::Queued { next, songs: n });
    }
}

/// `songs` as they may be added to a queue: a single picked song may be a provider's; lists never include
/// them (the server downloads whatever is requested).
fn queueable(songs: Vec<Song>) -> Vec<Song> {
    if songs.len() == 1 { songs } else { songs.into_iter().filter(|s| !s.is_provider()).collect() }
}

/// Queues `songs` but providers' for download, their covers fetched to disk too; what to say of it.
fn queue_downloads(core: &Core, covers: Option<&Arc<Loader>>, songs: Vec<Song>) -> Note {
    let songs: Vec<Song> = songs.into_iter().filter(|s| !s.is_provider()).collect();
    warm_covers(core, covers, &songs);
    match core.download_queue(songs) {
        Ok(q) => Note::Downloading(q.fresh.len() + q.again.len()),
        Err(e) => Note::DownloadFailed(e),
    }
}

fn warm_covers(core: &Core, covers: Option<&Arc<Loader>>, songs: &[Song]) {
    let Some(loader) = covers else { return };
    for url in core.download_cover_urls(songs.iter().filter_map(|s| s.cover_art.clone()).collect()) {
        loader.warm(&url);
    }
}

fn sync(client: &Client, out: &Out) {
    out(Said::Note(Note::Indexing));
    out(Said::Note(match crate::sync(client) {
        Ok(total) => Note::Indexed(total),
        Err(e) => Note::IndexStopped(e),
    }));
}

/// Runs `Client::read_each` (the stored copy, then the server's if different; an error only when
/// nothing is stored), handing each page to `each`.
pub fn read_pages(client: &Client, read: Read, each: impl FnMut(Page)) -> Result<(), NetError> {
    block_on(client.read_each(read, each))
}

/// Monotonic ms for the scrobbler. A process-wide epoch: the core's scrobble state outlives sessions.
fn monotonic_ms() -> i64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as i64
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;
    use std::net::TcpListener;
    use std::time::Duration;

    use nori_engine::Loader;

    use super::*;

    #[test]
    fn downloads_said_are_the_ones_queued() {
        let core = Core::new(String::new(), "t".into(), Default::default()).unwrap();
        let song = |id: &str| Song { id: id.into(), ..Default::default() };
        core.download_queue(vec![song("done")]).unwrap();
        core.download_settle(vec!["done".into()], vec![true]).unwrap();
        let said = queue_downloads(&core, None, vec![song("done"), song("new"), song("ext-1")]);
        assert!(matches!(said, Note::Downloading(1)), "the finished and the provider's song are not downloaded");
    }

    /// A song's request the server never answers is dropped with its song: the connection closes.
    #[test]
    fn a_song_let_go_hangs_up_its_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/song", listener.local_addr().unwrap());
        let (asked, closed) = (std::sync::mpsc::channel(), std::sync::mpsc::channel());
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 1024];
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            asked.0.send(()).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            let _ = closed.0.send(s.read(&mut buf).ok());
        });
        let audio = Arc::new(Audio::new(Http::new(), false));
        let loader = Loader::start(audio, url, [1_000, 4_000, 0, 0, 1 << 30], None, None);
        asked.1.recv_timeout(Duration::from_secs(10)).expect("the request came");
        drop(loader);
        assert_eq!(closed.1.recv_timeout(Duration::from_secs(5)), Ok(Some(0)), "the client hung up");
    }
}
