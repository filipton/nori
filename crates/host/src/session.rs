//! One open server profile: core, client, the engine on cpal, store, downloader, cover loader, search
//! and media controls. Work that waits on the network runs on its own thread and reports through the
//! client's [`Out`], as facts the client words.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use nori_core::bridge::BridgeTake;
use nori_core::cache_policy::{Page, Read};
use nori_core::client::{Client, Starrable};
use nori_core::library::StarsShown;
use nori_core::playlist::{Hand, QueueEdit};
use nori_core::race::{LyricsPick, LyricsShown};
use nori_core::rules::{queue_keep, BridgeStep, QueueMoment};
use nori_core::search::{SearchSession, SearchView};
use nori_core::settings::{SavedServer, SettingChange, StoredPrefs};
use nori_core::settings_store::{self, APPLY_AUDIO, APPLY_GAIN, CACHE_LIMIT, PLAYER, REPLAN, SOUND};
use nori_core::transport::{block_on, Exchange, FailureKind, NetError, Transport, TransportError, TransportResponse};
use nori_core::{Core, CoreError, IngestStats, PageOrigin, Song};
use nori_covers::loader::{Config as CoverConfig, Loader, Ticket};
use nori_covers::memory::Image;
use nori_engine::core::{settings, Analyses, CoreApp, CoreLibrary, CoreQueue, Downloader, Measurer, OutputVolume};
use nori_engine::{AudioOutput, Body, ByteSource, Config, Engine, Event, OpenError, State, Recent, Store};
use nori_http::Http;
use nori_look::cover::CoverColours;
use nori_output_cpal::{CpalOutput, Volume};

use crate::{config, db_path, derive, net, save, spawn, volume_db, Controls, Fetch, Keeper};

/// What a session reports from any thread.
pub enum Said {
    Engine(Event),
    /// One of possibly several lyrics answers for `song`, each better than the last.
    Lyrics { song: String, pick: LyricsPick },
    Search(SearchView),
    Note(Note),
    /// The reachability check made when a session opens.
    Reachable(Result<(), NetError>),
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

/// The offline transport: every request fails as unreachable, so the core shows what is stored and
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

struct Shown {
    song: String,
    out: Out,
}

impl LyricsShown for Shown {
    fn show(&self, pick: LyricsPick) {
        (self.out)(Said::Lyrics { song: self.song.clone(), pick });
    }
}

/// The clients read star marks from the core when drawing.
struct NoMarks;

impl StarsShown for NoMarks {
    fn marks(&self, _: nori_core::stars::StarMarks) {}
}

pub struct Open<'a> {
    pub data: &'a Path,
    pub http: Arc<Http>,
    pub profile: SavedServer,
    /// Output device name; None for the system default.
    pub device: Option<String>,
    /// Starting volume, 0 to 1.
    pub volume: f32,
    pub covers: bool,
    pub offline: bool,
    /// The process's media controls, driven by this session while it is open.
    pub mpris: Option<Arc<nori_mpris::Mpris>>,
    pub out: Out,
}

pub struct Session {
    pub core: Arc<Core>,
    pub client: Arc<Client>,
    pub engine: Arc<Engine>,
    pub store: Arc<Store>,
    downloader: Arc<Downloader>,
    pub covers: Option<Arc<Loader>>,
    pub volume: Volume,
    /// `volume` in dB, for loudness compensation.
    loudness: Arc<OutputVolume>,
    search: Arc<SearchSession>,
    pub offline: bool,
    mpris: Option<Arc<nori_mpris::Mpris>>,
    keeper: Arc<Keeper>,
    /// The database file, for its size.
    pub db: PathBuf,
    out: Out,
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
        nori_core::covers::set_cover_transport(o.http.clone());
        let prefs = settings_store::shared().current().unwrap_or_default();
        let output = match &o.device {
            Some(name) => CpalOutput::with_device(name),
            None => CpalOutput::new(),
        };
        let volume = output.volume();
        volume.set(o.volume);
        // Set before the engine starts so its first chain has the right loudness compensation.
        let loudness = Arc::new(OutputVolume::default());
        loudness.set(volume_db(o.volume));
        let output: Box<dyn AudioOutput> = Box::new(output);
        let store = Store::open(o.data.join("music"), prefs.cache_mb.max(0) as u64 * 1024 * 1024, Box::new(Recent::default())).map_err(|e| format!("the music directory: {e}"))?;
        let audio = Arc::new(Audio::new(o.http.clone(), o.offline));
        let analyses = Analyses::of(client.clone());
        let downloader = Downloader::new(client.clone(), audio.clone(), store.clone(), analyses.clone());
        // `bridging`: a song the network cannot bring raises `Event::Bridge` (see `Session::bridge`).
        let app = CoreApp::new(core.session.clone()).measuring(Measurer::new(analyses.clone(), store.clone())).per_device(core.clone()).bridging().volume(loudness.clone());
        let library = CoreLibrary { client: client.clone(), bytes: audio, metered: false, store: Some(store.clone()), analyses };
        let events = o.out.clone();
        let config = Config { memory_mb: 256, settings: settings(&prefs, loudness.db()), ..Config::default() };
        let engine = Arc::new(Engine::start(library, app, CoreQueue(core.session.clone()), output, None, config, move |e| events(Said::Engine(e))));
        let covers = o.covers.then(|| Arc::new(Loader::new(CoverConfig::new(o.data.join("covers")), o.http.clone())));
        if let Some(m) = &o.mpris {
            m.serve(Some(Arc::new(Controls::over_queue(engine.clone(), core.session.clone()))));
        }
        let keeper = Keeper::start(core.clone(), engine.clone());
        let s = Session { core, client, engine, store, downloader, covers, volume, loudness, search: SearchSession::new(), offline: o.offline, mpris: o.mpris, keeper, db: PathBuf::from(db), out: o.out };
        s.restore();
        if !s.offline && s.core.download_counts().pending > 0 {
            s.start_downloads();
        }
        Ok(s)
    }

    fn note(&self, n: Note) {
        (self.out)(Said::Note(n));
    }

    /// Tells the media controls the song or state changed.
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
        let (client, core, out) = (self.client.clone(), self.core.clone(), self.out.clone());
        spawn("nori-check", move || {
            let r = block_on(client.read_now(Read::Ping)).map(|_| ());
            let ok = r.is_ok();
            out(Said::Reachable(r));
            // Search and the songs list read the offline index.
            if ok && core.index_size().map_or(true, |s| s.songs == 0) {
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

    /// Plays `songs` from `start` (with `shuffle`, from wherever shuffle starts). Provider songs are
    /// dropped unless picked: the server downloads whatever is requested. `from` is the page the songs
    /// are the list of (`playlist_set`).
    pub fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin>) {
        self.handle().play(songs, start, shuffle, from);
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

    /// Adds songs after the current one (`next`) or at the end. `from`: the page these are all the
    /// songs of (keeps an album gapless).
    pub fn enqueue(&self, songs: Vec<Song>, next: bool, from: Option<PageOrigin>) {
        self.handle().enqueue(songs, next, from);
    }

    /// Enqueues an album, playlist or artist once fetched, as one run from its page.
    pub fn enqueue_later(&self, what: Fetch, next: bool) {
        let (client, me) = (self.client.clone(), self.handle());
        let from = what.origin();
        spawn("nori-enqueue", move || match what.songs(&client) {
            Ok(songs) => me.enqueue(songs, next, Some(from)),
            Err(e) => me.note(Note::SongsFailed(e)),
        });
    }

    fn handle(&self) -> Handle {
        Handle { engine: self.engine.clone(), queue: self.core.session.clone(), keeper: self.keeper.clone(), out: self.out.clone() }
    }

    fn edited(&self) {
        self.handle().edited();
    }

    /// Removes the song at list index `index`; if it was playing, the next one takes its place.
    pub fn remove(&self, index: usize) {
        let (current, playing) = self.engine.status_with(|s| (s.index, s.state == State::Playing));
        let change = self.core.session.remove(index as u32, index as u32 + 1);
        self.edited();
        if let (true, Some(at)) = (current == Some(index), change.at) {
            if playing {
                self.engine.play_at(at as usize, 0);
            } else {
                self.engine.go_to(at as usize, 0);
            }
        }
    }

    /// Removes everything after the current song.
    pub fn clear_upcoming(&self) {
        let mut upcoming: Vec<u32> = self.core.session.playlist(|p| p.upcoming().map(|i| i as u32).collect());
        upcoming.sort_unstable_by(|a, b| b.cmp(a));
        for i in upcoming {
            self.core.session.remove(i, i + 1);
        }
        self.edited();
    }

    /// Undoes the removal of `id`; the playing song is unchanged.
    pub fn put_back(&self, id: &str) {
        let was_empty = self.core.session.playlist(|p| p.is_empty());
        if self.core.session.restore(id.to_string()).at.is_none() {
            return self.note(Note::NothingToPutBack);
        }
        self.edited();
        if was_empty {
            self.engine.go_to(0, 0);
        }
    }

    /// Moves the song at list index `from` to `to`.
    pub fn move_song(&self, from: usize, to: usize) {
        self.core.session.move_range(from as u32, from as u32 + 1, to as u32);
        self.edited();
    }

    pub fn shuffle(&self, on: bool) {
        self.core.session.show_shuffle(on);
        self.core.session.shuffle(on);
        self.edited();
    }

    pub fn repeat(&self, mode: u8) {
        // Set on the queue first so the screen shows it at once.
        self.core.session.repeat(mode);
        self.engine.set_repeat(mode);
    }

    fn start_downloads(&self) {
        self.downloader.start(settings_store::shared().prefs(|p| p.parallel_downloads).max(1) as usize);
    }

    /// Queues `songs` for download (their covers fetched to disk too) and starts the downloader.
    pub fn download(&self, songs: Vec<Song>) {
        let songs: Vec<Song> = songs.into_iter().filter(|s| !s.is_provider()).collect();
        warm_covers(&self.core, self.covers.as_ref(), &songs);
        match self.core.download_queue(songs) {
            Ok(q) => self.note(Note::Downloading(q.fresh.len() + q.again.len())),
            Err(e) => self.note(Note::DownloadFailed(e)),
        }
        self.start_downloads();
    }

    pub fn download_later(&self, what: Fetch) {
        let (client, core, downloader, out, covers) = (self.client.clone(), self.core.clone(), self.downloader.clone(), self.out.clone(), self.covers.clone());
        spawn("nori-download-ask", move || match what.songs(&client) {
            Ok(songs) => {
                let songs: Vec<Song> = songs.into_iter().filter(|s| !s.is_provider()).collect();
                warm_covers(&core, covers.as_ref(), &songs);
                let n = songs.len();
                let _ = core.download_queue(songs);
                downloader.start(settings_store::shared().prefs(|p| p.parallel_downloads).max(1) as usize);
                out(Said::Note(Note::Downloading(n)));
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
        let change = nori_core::settings_model::setting_set(name.to_string(), value.to_string())?;
        self.apply(change.effect, &change.prefs);
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

    /// Sets the volume (0 to 1), rebuilding the chain when that changes loudness compensation.
    pub fn set_volume(&self, v: f32) {
        self.volume.set(v);
        if self.loudness.set(volume_db(v)) {
            if let Some(p) = settings_store::shared().current().filter(|p| p.loudness) {
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
        if let Some(p) = settings_store::shared().current() {
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

    /// Requests cover `id` at `px` square; `done` gets the image and, when `colours`, its page colours
    /// (derived on the loader thread). None without a cover loader.
    pub fn cover(&self, id: String, px: u32, colours: bool, done: impl FnOnce(Arc<Image>, Option<Box<CoverColours>>) + Send + 'static) -> Option<Ticket> {
        let url = self.core.cover_address(id, px);
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

    /// Stars or unstars on the server (queued when offline).
    pub fn star(&self, kind: Starrable, id: String, on: bool) {
        let (client, out) = (self.client.clone(), self.out.clone());
        spawn("nori-star", move || {
            let said = match block_on(client.star(kind, id, on, Arc::new(NoMarks))) {
                Ok(()) => Note::Starred(on),
                Err(e) => Note::StarFailed(e),
            };
            out(Said::Note(said));
        });
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
            if client.autofill_arrived(fresh.songs.len() as u32) && !fresh.songs.is_empty() {
                let len = me.queue.playlist(|p| p.len());
                let n = fresh.songs.len();
                me.queue.take(len as u32, fresh.songs.iter().map(|s| s.id.clone()).collect(), vec![Hand::No; n], fresh.from);
                me.edited();
            }
            if me.queue.autofill_landed() {
                me.engine.next();
            }
        });
    }

    /// Next; at the queue's end with autofill on, fetches songs first.
    pub fn next(&self) {
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
        if let Some(m) = &self.mpris {
            m.serve(None);
        }
        self.keep(QueueMoment::Closing);
        self.keeper.stop();
        self.engine.stop();
    }
}

/// The parts of a session worker threads use.
struct Handle {
    engine: Arc<Engine>,
    queue: Arc<nori_core::queue::Session>,
    keeper: Arc<Keeper>,
    out: Out,
}

impl Handle {
    fn note(&self, n: Note) {
        (self.out)(Said::Note(n));
    }

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
        let picked = songs.get(start).map(|s| s.id.clone());
        let songs: Vec<Song> = songs.into_iter().filter(|s| !s.is_provider() || Some(&s.id) == picked.as_ref()).collect();
        if songs.is_empty() {
            return;
        }
        let start = picked.and_then(|id| songs.iter().position(|s| s.id == id)).unwrap_or(0);
        self.queue.register(songs.clone());
        let change = self.queue.set(songs.iter().map(|s| s.id.clone()).collect(), (!shuffle).then_some(start as u32), shuffle, from);
        self.edited();
        self.engine.play_at(change.at.unwrap_or(0) as usize, 0);
    }

    fn enqueue(&self, songs: Vec<Song>, next: bool, from: Option<PageOrigin>) {
        // A single picked song may be a provider's; lists never include them.
        let songs: Vec<Song> = if songs.len() == 1 { songs } else { songs.into_iter().filter(|s| !s.is_provider()).collect() };
        if songs.is_empty() {
            return;
        }
        let n = songs.len();
        self.queue.register(songs.clone());
        let (len, current) = self.queue.playlist(|p| (p.len(), p.current()));
        let at = if next { current.map_or(len, |c| c + 1) } else { len };
        let hand = if next { Hand::Next } else { Hand::Last };
        self.queue.take(at as u32, songs.iter().map(|s| s.id.clone()).collect(), vec![hand; n], from);
        self.edited();
        if len == 0 {
            self.engine.go_to(0, 0);
        }
        self.note(Note::Queued { next, songs: n });
    }
}

/// Fetches covers of downloaded songs to disk (not decoded) so they exist offline.
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
