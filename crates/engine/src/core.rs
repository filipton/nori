//! The engine over nori-core, for clients that link it: the core's queue, transition planner, analysis
//! store and settings drive the engine; songs stream through the client's [`ByteSource`] or play from
//! downloads and the stream cache. Downloads and measuring ahead run here too.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use nori_player::automix::analysis::Analyzer;
use nori_player::automix::beats::{self, Ends, MixEnd};
use nori_player::dsp::Band;
use nori_player::engine::{Host, Plan};
use nori_player::pipeline::{App, Queue, Sound};
use nori_player::playlist::Playlist;
use nori_player::queue::{OnError, PlaybackError};
use nori_player::transitions::WindowSong;
use nori_core::client::Client;
use nori_core::queue::Session;
use nori_core::settings::StoredPrefs;

use nori_core::transfers;
use nori_core::Core;
use parking_lot::{Condvar, Mutex};

use crate::ahead::{AheadSong, Takers};
use crate::arriving::{Heard, Listening};
use crate::engine::Settings;
use crate::library::{Library, Located, Source};
use crate::source::{ByteSource, OpenError};
use crate::store::Store;

/// A core session's queue. Edit it there, then call [`crate::Engine::queue_changed`].
#[derive(Clone)]
pub struct CoreQueue(pub Arc<Session>);

impl Queue for CoreQueue {
    fn read<R>(&self, f: impl FnOnce(&Playlist) -> R) -> R {
        self.0.playlist(f)
    }

    fn moved_to(&mut self, index: usize) {
        self.0.moved_to(index as i32);
    }

    fn set_repeat(&mut self, mode: u8) {
        self.0.repeat(mode);
    }

    /// An explicit song with "skip explicit songs" on.
    fn skips(&self, list: &Playlist, index: usize) -> bool {
        self.0.skips(list, index)
    }
}

/// The core's planner, analysis store and log, and a session's queue rules, as the engine's [`App`].
pub struct CoreApp {
    session: Arc<Session>,
    /// The clock the engine's last call was made at.
    now_ms: i64,
    measurer: Option<Arc<Measurer>>,
    /// Per-device sound: the core holding the profiles, the outputs seen, and the current one.
    devices: Option<Arc<Core>>,
    known: Vec<String>,
    output: Option<String>,
    /// The client has an offline bridge.
    bridge: bool,
    /// ReplayGain levels already logged.
    gains_said: Vec<(String, f32)>,
    /// The output volume, for rebuilding settings on a device change.
    volume: Arc<OutputVolume>,
}

impl CoreApp {
    pub fn new(session: Arc<Session>) -> CoreApp {
        CoreApp {
            session,
            now_ms: 0,
            measurer: None,
            devices: None,
            known: Vec::new(),
            output: None,
            bridge: false,
            gains_said: Vec::new(),
            volume: Arc::default(),
        }
    }

    /// Shares the client's output volume (0 dB until told).
    pub fn volume(mut self, volume: Arc<OutputVolume>) -> CoreApp {
        self.volume = volume;
        self
    }

    /// The client runs the offline bridge (`Core::bridge_start`): unreachable songs go to it when the
    /// setting says so (`Event::Bridge`).
    pub fn bridging(mut self) -> CoreApp {
        self.bridge = true;
        self
    }

    /// Each output device gets the sound the core keeps for it, as Android's `DeviceSound`.
    pub fn per_device(mut self, core: Arc<Core>) -> CoreApp {
        self.devices = Some(core);
        self.known = nori_player::outputs::initial_known(&[]);
        self
    }

    /// With AutoMix on, upcoming songs on disk are measured by `measurer`.
    pub fn measuring(mut self, measurer: Arc<Measurer>) -> CoreApp {
        self.measurer = Some(measurer);
        self
    }
}

impl Host for CoreApp {
    fn plan_for(&mut self, outgoing_id: &str) -> Option<Plan> {
        self.session.planner.plan_for(outgoing_id)
    }

    fn wants_analysis(&mut self, song_id: &str) -> Option<u64> {
        self.session.planner.wants_analysis(song_id)
    }

    fn analysed(&mut self, song_id: &str, analyzer: Analyzer, _channels: usize, frames: u64, rate: u32) {
        self.session.planner.analysed(song_id, analyzer, frames, rate);
    }

    fn log(&mut self, message: &str) {
        nori_core::alog::info(message);
    }

    fn now_ms(&self) -> i64 {
        self.now_ms
    }
}

impl App for CoreApp {
    fn clock(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
    }

    /// With a measurer, upcoming songs are measured too when AutoMix is on.
    fn auto_mix(&self) -> bool {
        self.measurer.is_some() && !self.session.measure().is_empty()
    }

    /// The session picks the songs (`Session::measure`); the measurer takes those on disk.
    fn measure_ahead<S: nori_player::pipeline::Songs>(&mut self, _songs: &mut S, _ids: &[String]) {
        if let Some(m) = &self.measurer {
            m.update(self.session.measure(), std::thread::current());
        }
    }

    /// The core names the device and applies its sound (`Core::device_arrive`), stored in the settings.
    fn output_changed(&mut self, kind: nori_player::outputs::OutputKind, name: &str) -> Option<(String, Option<Sound>)> {
        let core = self.devices.clone()?;
        let seen = nori_player::outputs::refresh(&[(kind, name)], &self.known, None);
        if let Some(known) = seen.known {
            self.known = known;
        }
        if self.output.as_ref() == Some(&seen.current) {
            return None;
        }
        self.output = Some(seen.current.clone());
        let mut effect = core.device_arrive(seen.current.clone()).effect;
        let mut sound = None;
        // A step may ask to arrive again once done.
        for _ in 0..2 {
            if let Some(s) = effect.apply.take() {
                let prefs = self.session.settings.current()?.with_sound(s);
                self.session.settings.put(prefs.clone());
                sound = Some(settings(&prefs, self.volume.db()).sound);
            }
            if !effect.arrive {
                break;
            }
            effect = core.device_arrive(seen.current.clone()).effect;
        }
        Some((seen.current, sound))
    }

    fn measured(&mut self) -> bool {
        self.measurer.as_ref().is_some_and(|m| m.measured.swap(false, Ordering::AcqRel))
    }

    fn vocal_mask(&mut self, song_id: &str) -> Option<Arc<nori_player::sing::VocalMask>> {
        self.measurer.as_ref()?.analyses.masks.get(song_id)
    }

    fn masks_made(&mut self) -> bool {
        self.measurer.as_ref().is_some_and(|m| m.masked.swap(false, Ordering::AcqRel))
    }

    /// The core keeps the window itself.
    fn window(&mut self, _window: Vec<WindowSong>, _shuffling: bool) {
        self.session.window();
    }

    /// The core counts failing songs and applies its settings.
    fn on_error(&mut self, kind: PlaybackError, _has_next: bool) -> Option<OnError> {
        Some(self.session.error(kind, false, self.bridge))
    }

    fn playing(&mut self) {
        self.session.playing();
    }

    fn transitions_off(&mut self, off: bool) {
        self.session.planner.transition_setup(off);
    }

    /// The core's ReplayGain for the song at `index` of the list the engine holds.
    fn gain(&mut self, list: &Playlist, index: usize) -> f32 {
        let g = self.session.gain_of(list, index, false);
        let id = &list.ids()[index];
        // Logged once per song and level (device checks read it).
        if !self.gains_said.iter().any(|(i, v)| i == id && *v == g) {
            self.gains_said.retain(|(i, _)| i != id);
            if self.gains_said.len() >= 16 {
                self.gains_said.remove(0);
            }
            self.gains_said.push((id.to_string(), g));
            nori_core::alog::info(&format!("ReplayGain: {id} at {:+.2} dB", 20.0 * g.max(1e-6).log10()));
        }
        g
    }
}

/// Songs from the logged-in server at the network's quality (`Client::metered`). With a store,
/// downloads and whole cached copies play from disk, streams are cached, and later songs are fetched
/// ahead as the core says (`Client::precache_targets`).
pub struct CoreLibrary {
    pub client: Arc<Client>,
    pub bytes: Arc<dyn ByteSource>,
    pub store: Option<Arc<Store>>,
    pub analyses: Arc<Analyses>,
}

/// The container a cache key names (`<id>:192opus` is Opus); None for the original file.
pub fn key_format(key: &str) -> Option<String> {
    let q = key.rsplit_once(':')?.1.trim_start_matches(|c: char| c.is_ascii_digit());
    (!q.is_empty()).then(|| q.to_string())
}

impl Library for CoreLibrary {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let song = self.client.session().song(id);
        let duration_ms = song.as_ref().map(|s| s.duration as i64 * 1000).filter(|&d| d > 0);
        // A download may be transcoded: the file says what it is.
        let kept = self.store.as_ref().filter(|_| self.client.core().transfers().held().state(id) == transfers::HeldState::Done).and_then(|s| s.downloaded(id));
        if let Some(path) = kept {
            return Ok(Located { source: Source::File(vec![path]), hint: None, duration_ms, estimated: false });
        }
        let target = self.client.resolve(id.to_string(), false, self.client.metered());
        let hint = key_format(&target.key).or_else(|| song.as_ref().map(|s| s.suffix.clone())).filter(|s| !s.is_empty());
        let (url, bytes) = (target.url, self.bytes.clone());
        let source = match &self.store {
            Some(store) => Source::Cached { url, bytes, store: store.clone(), key: target.key },
            None => Source::Url { url, bytes },
        };
        Ok(Located { source, hint, duration_ms, estimated: false })
    }

    fn about(&self, id: &str) -> WindowSong {
        about(self.client.session(), id)
    }

    fn fetch_ahead(&self, id: &str) -> bool {
        fetch_ahead(self.client.session(), id)
    }

    /// Fetches the core's precache targets except `next` into the store.
    fn ahead(&mut self, next: &str) {
        let Some(store) = &self.store else { return };
        store.fetch_ahead(self.bytes.clone(), ahead_songs(self.client.precache_targets(self.client.metered()), next), Some(measuring_ahead(&self.analyses, self.client.session().clone())));
    }

    fn taker(&self, id: &str, hint: Option<&str>) -> Option<Listening> {
        self.analyses.measure_as_it_comes(id, hint, false)
    }

    fn forget(&mut self, id: &str) {
        let Some(store) = &self.store else { return };
        let target = self.client.resolve(id.to_string(), false, self.client.metered());
        if store.peek(&target.key).is_some() {
            nori_core::alog::info(&format!("{id} is fetched anew: its stream cache entry {} goes", target.key));
            store.drop_cached(&[target.key]);
        }
    }
}

/// The songs of `fetch` to fetch ahead, less `next` (the engine's loader fetches it).
pub fn ahead_songs(fetch: Vec<nori_core::stream::Fetch>, next: &str) -> Vec<AheadSong> {
    fetch.into_iter().filter(|f| f.id != next).map(|f| AheadSong { id: f.id, url: f.url, key: f.key }).collect()
}

/// Measures each song fetched ahead as it arrives ([`Analyses::measure_as_it_comes`]); the fetch may wait
/// for it.
pub fn measuring_ahead(analyses: &Arc<Analyses>, session: Arc<Session>) -> Takers {
    let analyses = analyses.clone();
    Arc::new(move |song: &AheadSong| analyses.measure_as_it_comes(&song.id, key_format(&song.key).or_else(|| session.song(&song.id).map(|s| s.suffix)).as_deref(), true))
}

/// What the planner and seek bar know of `id`, from `session`'s songs.
pub fn about(session: &Session, id: &str) -> WindowSong {
    nori_core::queue::window_song_of(session.song(id).as_ref(), id)
}

/// Whether `id` may be fetched unasked (never a provider's song).
pub fn fetch_ahead(session: &Session, id: &str) -> bool {
    !session.fetchable(vec![id.to_string()]).is_empty()
}

/// Runs the core's download queue: fetches pending songs whole into the store, a few at a time, oldest
/// first, each taking up what an earlier run left of it at the same quality. Progress is the core's
/// (`transfers`). Threads live only while there is work.
pub struct Downloader {
    client: Arc<Client>,
    bytes: Arc<dyn ByteSource>,
    store: Arc<Store>,
    analyses: Arc<Analyses>,
    work: Mutex<Work>,
}

#[derive(Default)]
struct Work {
    running: usize,
    /// Being fetched, and failed this run (tried again only when asked again).
    busy: HashSet<String>,
    failed: HashSet<String>,
    threads: Vec<JoinHandle<()>>,
}

/// Connections in a row that bring no bytes before a download fails.
const DOWNLOAD_TRIES: u32 = 3;
const DOWNLOAD_CHUNK: usize = 64 * 1024;

impl Downloader {
    /// Downloads for `client`'s core; saved songs are read back through `analyses` from `store`.
    pub fn new(client: Arc<Client>, bytes: Arc<dyn ByteSource>, store: Arc<Store>, analyses: Arc<Analyses>) -> Arc<Downloader> {
        analyses.install(Box::new(StoreShelf { analyses: Arc::downgrade(&analyses), store: store.clone() }));
        Arc::new(Downloader { client, bytes, store, analyses, work: Mutex::new(Work::default()) })
    }

    fn core(&self) -> &Arc<Core> {
        self.client.core()
    }

    /// Fetches the queue, `slots` songs at a time; a call while running only fills free slots.
    pub fn start(self: &Arc<Self>, slots: usize) {
        let mut w = self.work.lock();
        w.failed.clear();
        w.threads.retain(|t| !t.is_finished());
        let pending = self.pending();
        let now = nori_core::db::now_ms();
        for id in pending.iter().filter(|id| !w.busy.contains(*id)) {
            self.core().transfers().with(|t| t.followed(id, transfers::QUEUED, now));
        }
        let want = slots.max(1).min(pending.len());
        while w.running < want {
            w.running += 1;
            let me = self.clone();
            match std::thread::Builder::new().name("nori-download".into()).spawn(move || me.run()) {
                Ok(t) => w.threads.push(t),
                Err(_) => w.running -= 1,
            }
        }
    }

    /// Blocks until nothing is left to fetch or read back.
    pub fn wait(&self) {
        loop {
            let Some(t) = self.work.lock().threads.pop() else { break };
            let _ = t.join();
        }
        self.analyses.wait();
    }

    /// Unfinished downloads, oldest first.
    fn pending(&self) -> Vec<String> {
        let mut ids = self.core().download_ids(false).unwrap_or_default();
        ids.reverse();
        ids
    }

    fn take(&self) -> Option<String> {
        let pending = self.pending();
        let mut w = self.work.lock();
        let id = pending.into_iter().find(|id| !w.busy.contains(id) && !w.failed.contains(id));
        match &id {
            Some(id) => {
                w.busy.insert(id.clone());
            }
            None => w.running -= 1,
        }
        id
    }

    fn run(&self) {
        while let Some(id) = self.take() {
            let ok = self.fetch(&id);
            let now = nori_core::db::now_ms();
            let mut w = self.work.lock();
            w.busy.remove(&id);
            if !ok {
                w.failed.insert(id.clone());
                drop(w);
                self.core().transfers().with(|t| t.followed(&id, transfers::FAILED, now));
                continue;
            }
            drop(w);
            let core = self.core();
            core.transfers().with(|t| {
                t.followed(&id, transfers::COMPLETED, now);
                // No lyrics lookup here (Android does it, `lyrics_for_downloads`).
                t.work_done(&id, transfers::Work::Lyrics);
            });
            let _ = core.download_settle(vec![id.clone()], vec![true]);
            self.store.drop_copies(&id);
            self.analyses.saved(vec![id]);
        }
    }

    /// Fetches `id` whole into the store at the download quality. A broken connection is taken up at
    /// once where it broke, until [`DOWNLOAD_TRIES`] in a row bring nothing; a refused one fails the
    /// download until the next [`Downloader::start`].
    fn fetch(&self, id: &str) -> bool {
        let now = nori_core::db::now_ms();
        let core = self.core().clone();
        let slot = core.transfers().with(|t| {
            t.followed(id, transfers::DOWNLOADING, now);
            t.open(id, now)
        });
        let quality = core.session.settings.prefs(|p| nori_core::stream::StreamQuality { bit_rate: p.download.bit_rate.max(0) as u32, format: p.download.format.clone() });
        let part = self.store.download_part(id, &format!("{}{}", quality.bit_rate, quality.format));
        let url = self.client.download_target(id.to_string(), quality).url;
        let mut chunk = vec![0u8; DOWNLOAD_CHUNK];
        let size = || std::fs::metadata(&part).map_or(0, |m| m.len());
        // Measured as it downloads from the first byte; one taken up is measured from the disk later.
        let hint = core.session.song(id).or_else(|| core.download_song(id)).map(|s| s.suffix).filter(|s| !s.is_empty());
        let mut taker = if size() == 0 { self.analyses.measure_download_as_it_comes(id, hint.as_deref()) } else { None };
        let mut idle = 0;
        loop {
            let have = size();
            let body = match self.bytes.open(&url, have) {
                Ok(b) => b,
                // Nothing past what is on disk: complete (a transcode's length was an estimate).
                Err(OpenError::PastEnd { len }) if have > 0 && len.is_none_or(|l| l == have) => return std::fs::rename(&part, self.store.download_path(id)).is_ok(),
                Err(_) => return false,
            };
            // A rangeless server sends everything again: the file starts over.
            let append = body.start == have;
            if !append && have > 0 {
                taker = None;
            }
            let file = std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&part);
            let Ok(mut file) = file else { return false };
            let (mut at, mut reader) = (if append { have } else { 0 }, body.reader);
            let from = at;
            let broke = loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break false,
                    Ok(n) => {
                        if file.write_all(&chunk[..n]).is_err() {
                            return false;
                        }
                        if let Some(t) = taker.as_mut() {
                            t.take(&chunk[..n]);
                        }
                        at += n as u64;
                        core.transfers().with(|t| t.note(slot, body.len.unwrap_or(0) as i64, at as i64, nori_core::db::now_ms()));
                    }
                    Err(_) => break true,
                }
            };
            if !broke && body.len.is_none_or(|l| at == l) && file.flush().is_ok() {
                drop(file);
                let kept = std::fs::rename(&part, self.store.download_path(id)).is_ok();
                if let Some(t) = taker.take() {
                    t.end(kept);
                }
                return kept;
            }
            idle = if at > from { 0 } else { idle + 1 };
            if idle >= DOWNLOAD_TRIES {
                return false;
            }
        }
    }
}

/// Where whole songs are on disk: a client's downloads and stream cache.
pub trait Shelf: Send + Sync {
    /// The files holding all of `id`, in order; None while anything is missing.
    fn whole(&self, id: &str) -> Option<Whole>;
}

/// A song whole on disk: its files in order, and a container hint when the file alone should not
/// decide (a cache key names it).
pub struct Whole {
    pub files: Vec<std::path::PathBuf>,
    pub hint: Option<String>,
}

/// nori-engine's disk: finished downloads and whole stream cache entries, of the profile `analyses`
/// works for. Weak: the analyses hold their shelf.
struct StoreShelf {
    analyses: std::sync::Weak<Analyses>,
    store: Arc<Store>,
}

impl Shelf for StoreShelf {
    fn whole(&self, id: &str) -> Option<Whole> {
        let client = self.analyses.upgrade()?.client()?;
        if client.core().transfers().held().state(id) == transfers::HeldState::Done {
            if let Some(p) = self.store.downloaded(id) {
                return Some(Whole { files: vec![p], hint: None });
            }
        }
        // The current network's quality first, then the other's.
        let metered = client.metered();
        let (key, path) = [metered, !metered].into_iter().find_map(|m| {
            let key = client.resolve(id.to_string(), false, m).key;
            self.store.peek(&key).map(|p| (key, p))
        })?;
        let hint = key_format(&key).or_else(|| client.session().song(id).map(|s| s.suffix)).filter(|s| !s.is_empty());
        Some(Whole { files: vec![path], hint })
    }
}

/// AutoMix's measuring ahead: upcoming unanalysed songs that are whole on disk are decoded on a
/// lowest-priority thread and their analysis stored, so a transition knows both songs' tempo and beats.
/// Songs not on disk yet are looked at again when one arrives ([`Measurer::arrived`]) or the list
/// changes, never by polling; each is tried once per amount of bytes on disk. The thread lives only
/// while there is news.
///
/// With "Better beat detection" (the `neural-beats` feature), the same decode keeps the ends of the
/// current and next song for Beat This!'s intro and outro grids; the model loads when needed and is
/// dropped with the thread. With Sing on, it makes the current and next song's vocal masks the same
/// way (`crate::sing`), or reads them back from the disk.
pub struct Measurer {
    /// The profile to store into, asked per look (a profile switch replaces it).
    analyses: Arc<Analyses>,
    shelf: Box<dyn Shelf>,
    plan: Mutex<Schedule>,
    /// Signalled when the measuring thread ends, for [`Measurer::wait`].
    idle: Condvar,
    /// Bumped when the list changes; read per buffer so an unwanted decode stops.
    asked: AtomicU64,
    /// Something was stored since the engine last asked.
    measured: AtomicBool,
    /// A vocal mask came since the engine last asked.
    masked: AtomicBool,
    /// Called on the measuring thread whenever something was stored.
    told: Option<Box<dyn Fn() + Send + Sync>>,
    decoded: AtomicU64,
}

/// When the measurer looks and at what, apart from threads and the disk.
#[derive(Default)]
struct Schedule {
    ids: Vec<String>,
    /// Sing is on: masks are wanted too.
    sing: bool,
    /// Bumped on news: a new list, Sing switched, or a song arrived.
    news: u64,
    /// The news last looked at.
    seen: u64,
    running: bool,
    engine: Option<std::thread::Thread>,
    /// Songs tried, with their bytes on disk then and whether the beat model and the vocals model had them.
    tried: HashMap<String, (u64, bool, bool)>,
}

/// Tried songs remembered before those no longer asked for are dropped.
const TRIED_KEPT: usize = 256;

impl Schedule {
    /// The songs to measure are `ids`, with Sing's masks if `sing`; returns whether a thread should start.
    fn ask(&mut self, ids: Vec<String>, sing: bool) -> bool {
        if ids != self.ids || sing != self.sing {
            self.ids = ids;
            self.sing = sing;
            self.news += 1;
            if self.tried.len() > TRIED_KEPT {
                let ids = &self.ids;
                self.tried.retain(|id, _| ids.contains(id));
            }
        }
        self.start()
    }

    /// A song arrived on disk; returns whether a thread should start.
    fn arrived(&mut self) -> bool {
        self.news += 1;
        self.start()
    }

    fn start(&mut self) -> bool {
        if self.running || self.ids.is_empty() || self.news == self.seen {
            return false;
        }
        self.running = true;
        true
    }

    /// The songs to look at, once per news; None ends the thread.
    fn next(&mut self) -> Option<Vec<String>> {
        if self.ids.is_empty() || self.news == self.seen {
            self.running = false;
            return None;
        }
        self.seen = self.news;
        Some(self.ids.clone())
    }

    /// Whether `id` is asked for and untried with `bytes`, or needs the beat model (`listen`) or a vocal
    /// mask (`mask`) it did not get.
    fn worth(&self, id: &str, bytes: u64, listen: bool, mask: bool) -> bool {
        self.asks(id) && self.tried.get(id).is_none_or(|&(had, heard, masked)| bytes > had || (listen && !heard) || (mask && !masked))
    }

    fn asks(&self, id: &str) -> bool {
        self.ids.iter().any(|i| i == id)
    }

    fn tried(&mut self, id: &str, bytes: u64, heard: bool, masked: bool) {
        self.tried.insert(id.to_string(), (bytes, heard, masked));
    }
}

impl Measurer {
    /// Measures from nori-engine's [`Store`], looking again whenever a cached song becomes whole.
    pub fn new(analyses: Arc<Analyses>, store: Arc<Store>) -> Arc<Measurer> {
        let shelf = StoreShelf { analyses: Arc::downgrade(&analyses), store: store.clone() };
        let m = Measurer::on_shelf(analyses, Box::new(shelf), None);
        let weak = Arc::downgrade(&m);
        store.on_whole(Box::new(move || {
            if let Some(m) = weak.upgrade() {
                m.arrived();
            }
        }));
        m
    }

    /// Measures songs `shelf` has whole, storing into the profile `analyses` work for; `told` hears of
    /// each stored song.
    pub fn on_shelf(analyses: Arc<Analyses>, shelf: Box<dyn Shelf>, told: Option<Box<dyn Fn() + Send + Sync>>) -> Arc<Measurer> {
        let m = Arc::new(Measurer {
            analyses,
            shelf,
            plan: Mutex::new(Schedule::default()),
            idle: Condvar::new(),
            asked: AtomicU64::new(0),
            measured: AtomicBool::new(false),
            masked: AtomicBool::new(false),
            told,
            decoded: AtomicU64::new(0),
        });
        m.analyses.arrivals.lock().watch(&m);
        m
    }

    /// A song was measured elsewhere: replan.
    pub(crate) fn stored_elsewhere(&self) {
        self.measured.store(true, Ordering::Release);
        if let Some(t) = &self.plan.lock().engine {
            t.unpark();
        }
        if let Some(told) = &self.told {
            told();
        }
    }

    /// Measures `ids` from now on; `engine` is woken when something is stored.
    pub fn update(self: &Arc<Self>, ids: Vec<String>, engine: std::thread::Thread) {
        self.plan.lock().engine = Some(engine);
        self.ask(ids);
    }

    /// Measures `ids` (current song first), replacing the old list; an empty one stops.
    pub fn ask(self: &Arc<Self>, ids: Vec<String>) {
        let sing = self.analyses.client().is_some_and(|c| c.session().settings.prefs(|p| p.sing));
        let mut plan = self.plan.lock();
        if ids != plan.ids {
            self.asked.fetch_add(1, Ordering::AcqRel);
        }
        let start = plan.ask(ids, sing);
        drop(plan);
        if start {
            self.spawn();
        }
    }

    /// A song became whole on disk: look again.
    pub fn arrived(self: &Arc<Self>) {
        let start = self.plan.lock().arrived();
        if start {
            self.spawn();
        }
    }

    /// Whether the measuring thread runs.
    pub fn busy(&self) -> bool {
        self.plan.lock().running
    }

    /// Blocks until the measuring thread ends.
    pub fn wait(&self) {
        let mut plan = self.plan.lock();
        while plan.running {
            self.idle.wait(&mut plan);
        }
    }

    /// Songs decoded so far.
    pub fn decoded(&self) -> u64 {
        self.decoded.load(Ordering::Relaxed)
    }

    fn spawn(self: &Arc<Self>) {
        let me = self.clone();
        if std::thread::Builder::new().name("nori-measure".into()).spawn(move || me.run()).is_err() {
            self.plan.lock().running = false;
            self.idle.notify_all();
        }
    }

    fn run(&self) {
        crate::arriving::lower_priority();
        let mut model = Model::new(&self.analyses.models);
        let mut unmixer = crate::sing::Unmixer::new();
        loop {
            let Some(ids) = self.plan.lock().next() else {
                self.idle.notify_all();
                return;
            };
            let Some(client) = self.analyses.client() else { continue };
            let core = client.core();
            let (auto_mix, sing) = core.session.settings.with_prefs(|p| (p.auto_mix, p.sing)).unwrap_or_default();
            let missing = if auto_mix { core.analysis_missing(ids.clone()).unwrap_or_default() } else { Vec::new() };
            let near = listen_to(core, &ids, &missing);
            let unmasked = if sing {
                self.unmasked(&client, &ids)
            } else {
                self.analyses.masks.keep_only(&[]);
                Vec::new()
            };
            let todo: Vec<&String> = ids.iter().filter(|id| missing.contains(id) || near.contains(id) || unmasked.contains(id)).collect();
            let mut waiting = 0;
            for id in todo {
                // Measured as it arrives: that decode's end is news here.
                if self.analyses.arrivals.lock().has(id) {
                    waiting += 1;
                    continue;
                }
                let Some(pieces) = self.shelf.whole(id).and_then(|w| Some((crate::pieces::Pieces::open(&w.files).ok()?, w.hint))) else {
                    waiting += 1;
                    continue;
                };
                let (pieces, hint) = pieces;
                let bytes = pieces.len();
                let asked_to_listen = near.contains(id);
                let asked_to_mask = unmasked.contains(id);
                if !self.plan.lock().worth(id, bytes, asked_to_listen, asked_to_mask) {
                    continue;
                }
                let listen = asked_to_listen && model.ready(&client);
                let unmix = if asked_to_mask { unmixer.ready(&client) } else { None };
                // Ask again: it may have been measured as it arrived meanwhile.
                let classical = missing.contains(id) && !core.analysis_missing(vec![id.clone()]).unwrap_or_default().is_empty();
                if !classical && !listen && unmix.is_none() {
                    continue;
                }
                self.decoded.fetch_add(1, Ordering::Relaxed);
                let cpu = crate::arriving::thread_cpu_ms();
                let job = Job { classical, model: listen.then_some(&model), unmix };
                let Some(stored) = self.measure(core, id, pieces, hint.as_deref(), job) else { continue };
                if let (Some(a), Some(b)) = (cpu, crate::arriving::thread_cpu_ms()) {
                    nori_core::alog::info(&format!("measuring {id} ahead from the disk took {} ms of CPU", b.saturating_sub(a)));
                }
                self.plan.lock().tried(id, bytes, listen, unmix.is_some());
                if stored {
                    self.measured.store(true, Ordering::Release);
                    if let Some(t) = &self.plan.lock().engine {
                        t.unpark();
                    }
                    if let Some(told) = &self.told {
                        told();
                    }
                }
            }
            nori_core::alog::info(&format!("measuring ahead: {} of {} unmeasured, {waiting} not on the device yet", missing.len(), ids.len()));
        }
    }

    /// Decodes `id` for the analyser and/or the beat model and stores the results. Returns whether
    /// anything was stored, or None if abandoned (no longer asked for).
    fn measure(&self, core: &Core, id: &str, pieces: crate::pieces::Pieces, hint: Option<&str>, job: Job<'_, '_>) -> Option<bool> {
        let expected_ms = core.session.song(id).map_or(0, |s| s.duration as i64 * 1000);
        let mut asked = self.asked.load(Ordering::Acquire);
        let mut making = job.unmix.map(|u| u.maker());
        let mut feed = |rate: u32, channels: usize, x: &[f32]| {
            if let Some(m) = making.as_mut() {
                m.feed(rate, channels, x);
            }
        };
        let also: Option<&mut dyn FnMut(u32, usize, &[f32])> = if job.unmix.is_some() { Some(&mut feed) } else { None };
        let Decoded { stream, ends } = decode(id, "measuring ahead", pieces, hint, expected_ms, job.classical, job.model.is_some(), also, || {
            let now = self.asked.load(Ordering::Acquire);
            if now != asked {
                if !self.plan.lock().asks(id) {
                    return false;
                }
                asked = now;
            }
            true
        })?;
        let measured = stream.is_some_and(|stream| self.finish(core, id, stream, expected_ms));
        if let Some(m) = making {
            self.keep_mask(core, id, m.finish());
        }
        let listened = match (job.model, ends) {
            (Some(model), Some(mut ends)) => {
                let adopted = listen(core, id, model, &mut ends);
                drop(ends);
                crate::arriving::give_memory_back();
                adopted
            }
            _ => false,
        };
        Some(measured || listened)
    }

    /// Sing's near songs (of `ids`) without a mask in memory, after reading back those kept on the disk;
    /// the masks of songs no longer near are let go.
    fn unmasked(&self, client: &Client, ids: &[String]) -> Vec<String> {
        let near = &ids[..ids.len().min(crate::sing::AHEAD)];
        let masks = &self.analyses.masks;
        masks.keep_only(near);
        let dir = client.session().settings.sing_model.dir();
        let mut out = Vec::new();
        for id in near.iter().filter(|id| masks.get(id).is_none()) {
            match dir.as_deref().and_then(|d| crate::sing::load(d, id)) {
                Some(m) => {
                    masks.put(id, Arc::new(m));
                    self.tell_masked();
                }
                None => out.push(id.clone()),
            }
        }
        out
    }

    /// Keeps the mask made of `id`, on disk and for the player.
    fn keep_mask(&self, core: &Core, id: &str, made: Result<nori_player::sing::VocalMask, String>) {
        let mask = match made {
            Ok(m) => m,
            Err(e) => return nori_core::alog::info(&format!("vocal mask of {id} not made: {e}")),
        };
        if let Some(dir) = core.session.settings.sing_model.dir() {
            if let Err(e) = crate::sing::store(&dir, id, &mask) {
                nori_core::alog::info(&format!("vocal mask of {id} not kept: {e}"));
            }
        }
        nori_core::alog::info(&format!("vocal mask of {id} made: {} frames", mask.frames()));
        self.analyses.masks.put(id, Arc::new(mask));
        self.tell_masked();
    }

    /// The engine hears of a new mask.
    fn tell_masked(&self) {
        self.masked.store(true, Ordering::Release);
        if let Some(t) = &self.plan.lock().engine {
            t.unpark();
        }
        if let Some(told) = &self.told {
            told();
        }
    }

    /// Stores the analysis of all of `id`; returns whether it was stored.
    fn finish(&self, core: &Core, id: &str, stream: nori_core::automix::store::AnalysisStream, expected_ms: i64) -> bool {
        let a = core.analysis_finish_whole(id, stream, expected_ms).ok().flatten();
        nori_core::alog::info(&match &a {
            Some(t) => format!("analysed {id} ahead: {:.2} bpm (conf {:.2}, stab {:.2})", t.bpm, t.bpm_confidence, t.stability),
            None => format!("analysed {id} ahead: not stored: not the whole song, or too short"),
        });
        a.is_some()
    }
}

/// One whole-song decode's results: the fed analyser and the kept ends. Neither if not decoded to the end.
pub(crate) struct Decoded {
    pub stream: Option<nori_core::automix::store::AnalysisStream>,
    pub ends: Option<Ends>,
}

/// Decodes `id` whole from `pieces` for the analyser (`classical`), the beat model (`ends`) and `also`
/// (each buffer's rate, channels and samples) while `go_on` (asked per buffer); None when abandoned.
/// `what` names the work in the log.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode(
    id: &str,
    what: &str,
    pieces: crate::pieces::Pieces,
    hint: Option<&str>,
    expected_ms: i64,
    classical: bool,
    ends: bool,
    mut also: Option<&mut dyn FnMut(u32, usize, &[f32])>,
    mut go_on: impl FnMut() -> bool,
) -> Option<Decoded> {
    let mut left = false;
    let mut stream = None;
    let mut kept = None;
    let mut heard = false;
    let whole = crate::demux::decode_whole(Box::new(pieces), hint, |rate, channels, samples| {
        if !go_on() {
            left = true;
            return false;
        }
        heard = true;
        if classical {
            stream.get_or_insert_with(|| nori_core::automix::store::AnalysisStream::new(rate, channels, expected_ms.max(0) as u64)).feed_f32(samples);
        }
        if ends {
            kept.get_or_insert_with(|| Ends::new(rate)).feed(samples, channels);
        }
        if let Some(f) = also.as_mut() {
            f(rate, channels, samples);
        }
        true
    });
    if left {
        return None;
    }
    if !heard {
        nori_core::alog::info(&format!("{what} {id}: nothing decoded ({})", whole.err().unwrap_or_default()));
        return Some(Decoded { stream: None, ends: None });
    }
    if !matches!(whole, Ok(true)) {
        nori_core::alog::info(&format!("{what} {id}: stopped before its end, not stored"));
        return Some(Decoded { stream: None, ends: None });
    }
    Some(Decoded { stream, ends: kept })
}

// ---- measured as it comes ----

/// The songs being measured as they arrive, and the measurers told when one is stored (or not).
#[derive(Default)]
pub(crate) struct Arrivals {
    songs: Vec<String>,
    measurers: Vec<std::sync::Weak<Measurer>>,
    /// Songs measured as they arrived and stored.
    stored: u64,
}

impl Arrivals {
    pub(crate) fn has(&self, id: &str) -> bool {
        self.songs.iter().any(|i| i == id)
    }

    /// Takes `id` on; false when it is being measured already.
    fn begin(&mut self, id: &str) -> bool {
        if self.has(id) {
            return false;
        }
        self.songs.push(id.to_string());
        true
    }

    fn end(&mut self, id: &str) {
        self.songs.retain(|i| i != id);
    }

    fn watch(&mut self, m: &Arc<Measurer>) {
        self.measurers.retain(|w| w.strong_count() > 0);
        self.measurers.push(Arc::downgrade(m));
    }

    pub(crate) fn measurers(&self) -> Vec<Arc<Measurer>> {
        self.measurers.iter().filter_map(|w| w.upgrade()).collect()
    }
}

/// AutoMix analysis for one client: the songs measured as they arrive, the measurers looking ahead, and
/// the read-back of saved downloads (processing.rs), over the profile `client` gives.
pub struct Analyses {
    client: Box<dyn Fn() -> Option<Arc<Client>> + Send + Sync>,
    pub(crate) arrivals: Mutex<Arrivals>,
    /// Signalled when a song's measuring as it arrives ends, for [`Analyses::wait_arrivals`].
    arrived: Condvar,
    pub(crate) read_back: crate::processing::ReadBack,
    pub(crate) models: Models,
    /// Sing's masks of the near songs.
    pub(crate) masks: crate::sing::VocalMasks,
}

impl Analyses {
    /// Over the profile `client` gives at each use (a profile switch replaces it).
    pub fn new(client: impl Fn() -> Option<Arc<Client>> + Send + Sync + 'static) -> Arc<Analyses> {
        Arc::new(Analyses { client: Box::new(client), arrivals: Mutex::default(), arrived: Condvar::new(), read_back: Default::default(), models: Models::default(), masks: Default::default() })
    }

    /// Over `client` for good.
    pub fn of(client: Arc<Client>) -> Arc<Analyses> {
        Analyses::new(move || Some(client.clone()))
    }

    pub(crate) fn client(&self) -> Option<Arc<Client>> {
        (self.client)()
    }

    /// The heap the loaded beat model holds (1 where the heap is not counted); 0 when none is loaded.
    pub fn model_bytes(&self) -> u64 {
        self.models.held.load(Ordering::Relaxed)
    }

    /// Songs measured as they arrived and stored (perf report, tests).
    pub fn measured_as_they_came(&self) -> u64 {
        self.arrivals.lock().stored
    }

    /// Whether any song is being measured as it arrives.
    pub fn measuring_as_they_come(&self) -> bool {
        !self.arrivals.lock().songs.is_empty()
    }

    /// Blocks until no song is being measured as it arrives.
    pub fn wait_arrivals(&self) {
        let mut a = self.arrivals.lock();
        while !a.songs.is_empty() {
            self.arrived.wait(&mut a);
        }
    }

    /// Measures `id` as its bytes arrive (`crate::arriving`), if AutoMix is on, it is unanalysed, its
    /// container allows it (`hint`: not MP4) and it is not already being measured. `wait`: the fetch may
    /// block on the decoder (not for a loader the player reads).
    pub fn measure_as_it_comes(self: &Arc<Self>, id: &str, hint: Option<&str>, wait: bool) -> Option<Listening> {
        let client = self.client()?;
        if !client.session().settings.prefs(|p| p.auto_mix) {
            return None;
        }
        self.listen_as_it_comes(client.core().clone(), id, hint, wait)
    }

    /// [`Analyses::measure_as_it_comes`] for a download, whatever AutoMix says: every download is
    /// analysed once (`nori_core::transfers::needs`).
    pub fn measure_download_as_it_comes(self: &Arc<Self>, id: &str, hint: Option<&str>) -> Option<Listening> {
        self.listen_as_it_comes(self.client()?.core().clone(), id, hint, true)
    }

    fn listen_as_it_comes(self: &Arc<Self>, core: Arc<Core>, id: &str, hint: Option<&str>, wait: bool) -> Option<Listening> {
        if !nori_core::queue::analysable(id) || !crate::demux::decodes_as_it_comes(hint) {
            return None;
        }
        if core.analysis_missing(vec![id.to_string()]).ok()?.is_empty() {
            return None;
        }
        if !self.arrivals.lock().begin(id) {
            return None;
        }
        let expected_ms = core.session.song(id).map_or(0, |s| s.duration as i64 * 1000);
        let heard = AsItComes { id: id.to_string(), core: core.clone(), analyses: self.clone(), expected_ms, stream: None, cpu_from: None };
        match Listening::start(hint.map(str::to_string), wait, Box::new(heard)) {
            Some(l) => {
                core.transfers().with(|t| t.analysing_began(id));
                Some(l)
            }
            None => {
                self.arrivals.lock().end(id);
                self.arrived.notify_all();
                None
            }
        }
    }
}

/// Feeds decoded samples into the analyser; stores it once the whole song came.
struct AsItComes {
    id: String,
    core: Arc<Core>,
    analyses: Arc<Analyses>,
    expected_ms: i64,
    stream: Option<nori_core::automix::store::AnalysisStream>,
    /// The decoder thread's CPU time at the start, for the log.
    cpu_from: Option<u64>,
}

impl Heard for AsItComes {
    fn samples(&mut self, rate: u32, channels: usize, samples: &[f32]) {
        if self.stream.is_none() {
            self.cpu_from = crate::arriving::thread_cpu_ms();
        }
        let expected = self.expected_ms.max(0) as u64;
        self.stream.get_or_insert_with(|| nori_core::automix::store::AnalysisStream::new(rate, channels, expected)).feed_f32(samples);
    }

    fn done(self: Box<Self>, whole: bool) {
        let AsItComes { id, core, analyses, expected_ms, stream, cpu_from } = *self;
        let stored = match stream {
            Some(stream) if whole => {
                let a = core.analysis_finish_whole(&id, stream, expected_ms).ok().flatten();
                let cpu = match (cpu_from, crate::arriving::thread_cpu_ms()) {
                    (Some(a), Some(b)) => format!(", {} ms of CPU", b.saturating_sub(a)),
                    _ => String::new(),
                };
                nori_core::alog::info(&match &a {
                    Some(t) => format!("analysed {id} as it came: {:.2} bpm (conf {:.2}, stab {:.2}){cpu}", t.bpm, t.bpm_confidence, t.stability),
                    None => format!("analysed {id} as it came: not stored: not the whole song, or too short"),
                });
                a.is_some()
            }
            Some(_) => {
                nori_core::alog::info(&format!("measuring {id} as it came: its bytes did not all come, dropped"));
                false
            }
            None => false,
        };
        let measurers = {
            let mut a = analyses.arrivals.lock();
            a.stored += stored as u64;
            a.end(&id);
            analyses.arrived.notify_all();
            a.measurers()
        };
        core.transfers().with(|t| t.analysing_ended(&id, stored));
        analyses.kick();
        for m in measurers {
            if stored {
                m.stored_elsewhere();
            } else {
                // Not stored: the measurer may find it whole on disk.
                m.arrived();
            }
        }
    }
}

/// Songs the beat model reads: the current and the next (the next mix needs no more).
const LISTEN_AHEAD: usize = 2;

/// The songs of `ids` for the beat model; none when it is off or not built.
fn listen_to(core: &Core, ids: &[String], missing: &[String]) -> Vec<String> {
    let wanted = beats::AVAILABLE && core.session.settings.with_prefs(|p| p.auto_mix && p.auto_mix_better_beats).unwrap_or(false);
    if !wanted {
        return Vec::new();
    }
    let near = &ids[..ids.len().min(LISTEN_AHEAD)];
    let mut out = core.analysis_neural_missing(near.to_vec()).unwrap_or_default();
    // Unmeasured ones: the same decode feeds the model.
    out.extend(near.iter().filter(|id| missing.contains(id)).cloned());
    out
}

/// What one decode is for.
struct Job<'m, 'a> {
    /// No current analysis.
    classical: bool,
    /// The beat model reads its ends.
    model: Option<&'m Model<'a>>,
    /// The vocals model makes its mask.
    unmix: Option<&'m crate::sing::Model>,
}

/// Runs Beat This! over each unread end of `id`; returns whether a grid was stored.
pub(crate) fn listen(core: &Core, id: &str, model: &Model, ends: &mut Ends) -> bool {
    let Some(beats_model) = &model.loaded else { return false };
    let Ok(Some(row)) = core.analysis_get(id.to_string()) else { return false };
    let _one = model.models.running.lock();
    let rate = ends.rate();
    let mut adopted = false;
    for end in [MixEnd::Intro, MixEnd::Outro] {
        if !beats::needs_end(&row, end) {
            continue;
        }
        let (x, start_ms) = match end {
            MixEnd::Intro => (ends.head(), 0),
            MixEnd::Outro => ends.tail(),
        };
        let have = (start_ms, start_ms + x.len() as i64 * 1000 / rate as i64);
        let t0 = std::time::Instant::now();
        let grid = match beats::window(&row, end, have) {
            Some((from, to)) => {
                let at = |ms: i64| ((ms - start_ms) * rate as i64 / 1000).clamp(0, x.len() as i64) as usize;
                match beats_model.read(&x[at(from)..at(to)], rate, end, from) {
                    Ok(g) => g,
                    Err(e) => {
                        nori_core::alog::info(&format!("beat model on the {end:?} of {id}: {e}"));
                        continue;
                    }
                }
            }
            None => None,
        };
        let used = core.analysis_neural_store(id, end, grid).unwrap_or(false);
        nori_core::alog::info(&match grid {
            Some(g) => format!(
                "beat model on the {end:?} of {id}: {:.2} bpm (conf {:.2}, stab {:.2}), {} to the bar, {}, {} ms",
                g.bpm,
                g.confidence,
                g.stability,
                g.beats_per_bar,
                if used { "used" } else { "classical kept" },
                t0.elapsed().as_millis()
            ),
            None => format!("beat model on the {end:?} of {id}: nothing it was sure of"),
        });
        adopted |= used;
    }
    adopted
}

/// The beat model's runs for one [`Analyses`]: one at a time (each holds tens of megabytes), and the heap
/// the loaded model holds, for the memory report.
#[derive(Default)]
pub(crate) struct Models {
    running: Mutex<()>,
    /// The loaded model's heap growth (1 where the heap is not counted); 0 when none is loaded.
    held: AtomicU64,
}

/// The beat model for a thread's life: loaded on first need, tried once per thread.
pub(crate) struct Model<'a> {
    tried: bool,
    loaded: Option<BeatModel>,
    models: &'a Models,
}

impl Model<'_> {
    pub(crate) fn new(models: &Models) -> Model<'_> {
        Model { tried: false, loaded: None, models }
    }

    /// Whether the model is loaded, loading it on first call.
    pub(crate) fn ready(&mut self, client: &Client) -> bool {
        if !self.tried {
            self.tried = true;
            self.loaded = BeatModel::load(client).map(|(model, bytes)| {
                self.models.held.store(bytes, Ordering::Relaxed);
                model
            });
        }
        self.loaded.is_some()
    }

    pub(crate) fn loaded(&self) -> bool {
        self.loaded.is_some()
    }
}

impl Drop for Model<'_> {
    /// Frees the model and returns its pages.
    fn drop(&mut self) {
        if self.loaded.take().is_some() {
            self.models.held.store(0, Ordering::Relaxed);
            crate::arriving::give_memory_back();
        }
    }
}

/// Beat This! through tract (`neural-beats`).
#[cfg(feature = "neural-beats")]
pub(crate) struct BeatModel(nori_player::automix::neural::BeatThis);

#[cfg(feature = "neural-beats")]
impl BeatModel {
    /// The model and the heap it took (1 where the heap is not counted).
    fn load(client: &Client) -> Option<(BeatModel, u64)> {
        let file = nori_core::model_download::ensure(client)?;
        let t0 = std::time::Instant::now();
        let before = nori_core::heap::live_bytes();
        let loaded = nori_core::model_download::read(&nori_core::beat_model::BEAT_THIS, &file).and_then(|bytes| nori_player::automix::neural::BeatThis::from_weights(&bytes).map_err(|e| e.to_string()));
        match loaded {
            Ok(m) => {
                let bytes = before.zip(nori_core::heap::live_bytes()).map_or(1, |(a, b)| (b - a).max(1) as u64);
                nori_core::alog::info(&format!("beat model loaded in {} ms, {} KB", t0.elapsed().as_millis(), bytes / 1024));
                Some((BeatModel(m), bytes))
            }
            Err(e) => {
                nori_core::alog::info(&format!("loading the beat model: {e}"));
                None
            }
        }
    }

    fn read(&self, x: &[f32], rate: u32, end: MixEnd, from_ms: i64) -> Result<Option<beats::EndGrid>, String> {
        beats::read(&self.0, x, rate, end, from_ms)
    }
}

/// Without `neural-beats`: never loaded.
#[cfg(not(feature = "neural-beats"))]
pub(crate) struct BeatModel;

#[cfg(not(feature = "neural-beats"))]
impl BeatModel {
    fn load(_: &Client) -> Option<(BeatModel, u64)> {
        None
    }

    fn read(&self, _: &[f32], _: u32, _: MixEnd, _: i64) -> Result<Option<beats::EndGrid>, String> {
        Ok(None)
    }
}

/// A player's output volume in dB (0 is full), for loudness compensation ([`settings`]). Shared between
/// the client and [`CoreApp::volume`], which rebuilds the settings on a device change.
#[derive(Debug, Default)]
pub struct OutputVolume(AtomicU32);

impl OutputVolume {
    /// Sets the volume (`nori_player::contour::volume_db`); true when it changed by an audible 0.25 dB.
    pub fn set(&self, db: f64) -> bool {
        let db = if db.is_finite() { db.clamp(-96.0, 0.0) as f32 } else { 0.0 };
        let was = f32::from_bits(self.0.swap(db.to_bits(), Ordering::Relaxed));
        (was - db).abs() >= 0.25
    }

    /// dB, 0 until told.
    pub fn db(&self) -> f64 {
        f32::from_bits(self.0.load(Ordering::Relaxed)) as f64
    }
}

/// Engine settings from the core's, with loudness compensation for `volume_db` ([`OutputVolume::db`]).
pub fn settings(s: &StoredPrefs, volume_db: f64) -> Settings {
    let bands = if s.eq_enabled { s.eq_bands.iter().map(|b| Band { kind: b.kind as i32, freq: b.freq as f64, gain_db: b.gain_db as f64, q: b.q as f64, channel: b.channel as i32 }).collect() } else { Vec::new() };
    let sound = if s.sound_bypass { Sound::default() } else { Sound {
        // The graphic equalizer replaces the parametric bands.
        graphic: nori_core::dsp::graphic_sliders(s),
        bands: if s.eq_mode == nori_core::settings::EqMode::Graphic { Vec::new() } else { bands },
        effects: s.effects().player_at(volume_db),
        preamp_db: nori_core::dsp::effective_preamp_db(s) as f64,
        crossfeed_db: s.crossfeed_db as f64,
        crossfeed_hz: s.crossfeed_hz as f64,
        balance: s.balance as f64,
        mono: s.mono,
        limiter: s.limiter,
        threshold_db: s.limiter_threshold_db as f64,
    } };
    Settings {
        sound,
        speed: s.speed,
        pitch: s.pitch,
        skip_silence: s.skip_silence,
        fade_ms: s.fade_ms,
        hi_res: s.hi_res,
        // Bit-perfect plays each song at its own rate.
        max_rate: if s.bit_perfect { 0 } else { s.max_rate.hz() },
        offload: s.offload,
        crossfade_s: s.crossfade_sec,
        auto_mix: s.auto_mix,
        gain_boost_db: if s.gain_prefs().boosts() { s.gain_boost_db } else { 0.0 },
        sing: s.sing.then_some(s.sing_vocal_level),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn settings_build_the_chain() {
        use nori_core::settings::EqMode;
        let p = StoredPrefs { eq_enabled: true, eq_mode: EqMode::Parametric, eq_graphic: vec![3.0; 10], ..StoredPrefs::default() };
        let s = settings(&p, 0.0).sound;
        assert!(s.graphic.is_empty() && !s.bands.is_empty(), "parametric: the bands play");
        let g = settings(&StoredPrefs { eq_mode: EqMode::Graphic, ..p.clone() }, 0.0).sound;
        assert!(g.bands.is_empty() && g.graphic == vec![3.0; 10], "graphic: the sliders play, the bands wait");
        assert_eq!(g.preamp_db, -3.0, "the automatic pre-amp pays back the sliders");
        let off = settings(&StoredPrefs { eq_enabled: false, eq_mode: EqMode::Graphic, ..p.clone() }, 0.0).sound;
        assert!(off.graphic.is_empty() && off.bands.is_empty() && !off.on());
        let fx = settings(&StoredPrefs { volume_boost_db: 4.0, compressor: true, ..StoredPrefs::default() }, 0.0).sound;
        assert!(fx.on() && fx.effects.boost_db == 4.0 && fx.effects.compressor.is_some() && fx.effects.guard());
        // Loudness at the volume last set.
        let loud = StoredPrefs { loudness: true, ..StoredPrefs::default() };
        let v = OutputVolume::default();
        assert!(v.set(-30.0));
        assert!(!v.set(-30.1), "a tenth of a dB is no change");
        assert_eq!(settings(&loud, v.db()).sound.effects.loudness.map(|l| l.volume_db as f32), Some(-30.1));
        assert!(v.set(0.0));
        assert_eq!(settings(&loud, v.db()).sound.effects.loudness.map(|l| l.volume_db), Some(0.0));
        assert_eq!(OutputVolume::default().db(), 0.0, "each player's own: a new one starts all the way up");
        // Bypass: the identity chain.
        let none = settings(&StoredPrefs { sound_bypass: true, limiter: true, crossfeed_db: 6.0, mono: true, ..StoredPrefs { eq_mode: EqMode::Graphic, ..p } }, 0.0);
        assert_eq!(none.sound, nori_player::pipeline::Sound::default());
        assert!(!none.sound.on());
    }

    #[test]
    fn bit_perfect_ignores_max_rate() {
        use nori_core::settings::MaxRate;
        assert_eq!(settings(&StoredPrefs { max_rate: MaxRate::Khz48, ..StoredPrefs::default() }, 0.0).max_rate, 48_000);
        assert_eq!(settings(&StoredPrefs { max_rate: MaxRate::Khz48, bit_perfect: true, ..StoredPrefs::default() }, 0.0).max_rate, 0);
    }

    #[test]
    fn looks() {
        let mut s = Schedule::default();
        assert!(s.ask(ids(&["a", "b", "c"]), false), "songs to measure: a thread");
        assert_eq!(s.next(), Some(ids(&["a", "b", "c"])));
        assert_eq!(s.next(), None, "nothing new: the thread ends");
        assert!(!s.running);
        // Repeated asks with the same list.
        for _ in 0..100 {
            assert!(!s.ask(ids(&["a", "b", "c"]), false), "no thread, no look");
        }
        assert!(s.ask(ids(&["b", "c", "d"]), false), "the queue moved: a look");

        // Arrival during a look is kept.
        let mut s = Schedule::default();
        assert!(!s.arrived(), "nothing asked for: nothing to look at");
        assert!(s.ask(ids(&["a", "b"]), false));
        assert_eq!(s.next(), Some(ids(&["a", "b"])));
        // Arrivals while measuring: no second thread, news kept.
        assert!(!s.arrived());
        assert!(!s.arrived());
        assert_eq!(s.next(), Some(ids(&["a", "b"])), "one more look for both arrivals");
        assert_eq!(s.next(), None);
        assert!(s.arrived(), "a later arrival starts a thread again");
    }

    #[test]
    fn song_retried_only_with_more_bytes() {
        let mut s = Schedule::default();
        s.ask(ids(&["a", "b"]), false);
        assert!(s.worth("a", 1000, false, false));
        s.tried("a", 1000, true, false);
        assert!(!s.worth("a", 1000, false, false), "tried with these bytes: not again, however often it is looked at");
        assert!(s.worth("a", 5000, false, false), "more of it has come since: once more");
        assert!(!s.worth("z", 1000, false, false), "not asked for");
        s.ask(ids(&["b"]), false);
        assert!(!s.worth("a", 5000, false, false), "no longer asked for");
    }

    #[test]
    fn measured_song_decoded_for_model() {
        let mut s = Schedule::default();
        s.ask(ids(&["a", "b"]), false);
        s.tried("a", 1000, false, false);
        assert!(!s.worth("a", 1000, false, false));
        assert!(s.worth("a", 1000, true, false), "the model has not heard it");
        s.tried("a", 1000, true, false);
        assert!(!s.worth("a", 1000, true, false), "heard: never again");
        assert!(s.worth("a", 1000, false, true), "no vocal mask made of it yet");
        s.tried("a", 1000, true, true);
        assert!(!s.worth("a", 1000, true, true), "masked: never again");
    }

    #[test]
    fn sing_switched_is_news() {
        let mut s = Schedule::default();
        assert!(s.ask(ids(&["a", "b"]), false));
        assert_eq!(s.next(), Some(ids(&["a", "b"])));
        assert_eq!(s.next(), None);
        assert!(s.ask(ids(&["a", "b"]), true), "the same songs, now for their masks: a look");
        assert_eq!(s.next(), Some(ids(&["a", "b"])));
    }
}
