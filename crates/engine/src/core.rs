//! The engine over nori-core, for clients that link it: the core's queue, transition planner, analysis
//! store and settings drive the engine; songs stream through the client's [`ByteSource`] or play from
//! downloads and the stream cache. Downloads and measuring ahead run here too.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use nori_player::automix::analysis::Analyzer;
use nori_player::automix::beats::{self, Ends, MixEnd};
use nori_player::dsp::Band;
use nori_player::engine::{Host, Plan};
use nori_player::pipeline::{App, Queue, Sound};
use nori_player::playlist::Playlist;
use nori_player::queue::{OnError, PlaybackError};
use nori_player::transitions::WindowSong;
use nori_core::automix::host::CoreHost;
use nori_core::client::Client;
use nori_core::settings::StoredPrefs;

use nori_core::transfers;
use nori_core::Core;
use parking_lot::Mutex;

use crate::ahead::{AheadSong, Takers};
use crate::arriving::{Heard, Listening};
use crate::engine::Settings;
use crate::library::{Library, Located, Source};
use crate::source::{ByteSource, OpenError};
use crate::store::{Order, Store};

/// The core's queue (`nori_core::playlist`). Edit it with `playlist_*`, then call
/// [`crate::Engine::queue_changed`].
#[derive(Debug, Default, Clone, Copy)]
pub struct CoreQueue;

impl Queue for CoreQueue {
    fn read<R>(&self, f: impl FnOnce(&Playlist) -> R) -> R {
        nori_core::playlist::with(f)
    }

    fn moved_to(&mut self, index: usize) {
        nori_core::playlist::playlist_moved_to(index as i32);
    }

    fn set_repeat(&mut self, mode: u8) {
        nori_core::playlist::playlist_repeat(mode);
    }

    /// An explicit song with "skip explicit songs" on.
    fn skips(&self, index: usize) -> bool {
        nori_core::playlist::playlist_skips(index)
    }
}

/// The core's planner, analysis store and log as the engine's [`App`].
pub struct CoreApp {
    host: CoreHost<fn()>,
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
    pub fn new() -> CoreApp {
        fn nothing() {}
        CoreApp {
            host: CoreHost { now_ms: 0, heard_changed: nothing },
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

impl Default for CoreApp {
    fn default() -> Self {
        CoreApp::new()
    }
}

impl Host for CoreApp {
    fn plan_for(&mut self, outgoing_id: &str) -> Option<Plan> {
        self.host.plan_for(outgoing_id)
    }

    fn wants_analysis(&mut self, song_id: &str) -> Option<u64> {
        self.host.wants_analysis(song_id)
    }

    fn analysed(&mut self, song_id: &str, analyzer: Analyzer, channels: usize, frames: u64, rate: u32) {
        self.host.analysed(song_id, analyzer, channels, frames, rate);
    }

    fn log(&mut self, message: &str) {
        self.host.log(message);
    }

    fn now_ms(&self) -> i64 {
        self.host.now_ms()
    }
}

impl App for CoreApp {
    fn clock(&mut self, now_ms: i64) {
        self.host.now_ms = now_ms;
    }

    /// With a measurer, upcoming songs are measured too when AutoMix is on.
    fn auto_mix(&self) -> bool {
        self.measurer.is_some() && !nori_core::rules::queue_measure().is_empty()
    }

    /// The core picks the songs (`queue_measure`); the measurer takes those on disk.
    fn measure_ahead<S: nori_player::pipeline::Songs>(&mut self, _songs: &mut S, _ids: &[String]) {
        if let Some(m) = &self.measurer {
            m.update(nori_core::rules::queue_measure(), std::thread::current());
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
                let prefs = nori_core::settings_store::settings_current()?.with_sound(s);
                nori_core::settings_store::settings_put(prefs.clone());
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

    /// The core keeps the window itself.
    fn window(&mut self, _window: Vec<WindowSong>, _shuffling: bool) {
        nori_core::playlist::playlist_window();
    }

    /// The core counts failing songs and applies its settings.
    fn on_error(&mut self, kind: PlaybackError, _has_next: bool) -> Option<OnError> {
        Some(nori_core::rules::queue_error(kind, false, self.bridge))
    }

    fn playing(&mut self) {
        nori_core::rules::queue_playing();
    }

    fn transitions_off(&mut self, off: bool) {
        nori_core::automix::planner::transition_setup(off);
    }

    /// The core's ReplayGain for the queue's song.
    fn gain(&mut self, index: usize, id: &str) -> f32 {
        let g = nori_core::playlist::playlist_gain_of(index, false);
        // Logged once per song and level (device checks read it).
        if !self.gains_said.iter().any(|(i, v)| i == id && *v == g) {
            self.gains_said.retain(|(i, _)| i != id);
            if self.gains_said.len() >= 16 {
                self.gains_said.remove(0);
            }
            self.gains_said.push((id.to_string(), g));
            self.host.log(&format!("ReplayGain: {id} at {:+.2} dB", 20.0 * g.max(1e-6).log10()));
        }
        g
    }
}

/// The core's stream cache order (`nori_core::stream_cache`).
pub struct CoreOrder;

impl Order for CoreOrder {
    fn touch(&self, key: &str) {
        nori_core::stream_cache::touch(key);
    }

    fn seed(&self, held: &[String]) {
        nori_core::stream_cache::seed(held.iter().map(String::as_str));
    }

    fn next(&self) -> Option<String> {
        nori_core::stream_cache::next()
    }

    fn clear(&self) {
        nori_core::stream_cache::clear();
    }
}

/// Songs from the logged-in server at the network's quality ([`network_metered`]). With a store,
/// downloads and whole cached copies play from disk, streams are cached, and later songs are fetched
/// ahead as the core says (`Client::precache_targets`).
pub struct CoreLibrary {
    pub client: Arc<Client>,
    pub bytes: Arc<dyn ByteSource>,
    /// Always stream at the metered quality.
    pub metered: bool,
    pub store: Option<Arc<Store>>,
}

impl CoreLibrary {
    /// Whether songs stream at the metered quality now.
    fn metered(&self) -> bool {
        self.metered || nori_core::stream::metered()
    }
}

/// The platform says whether the network is metered now; returns the streaming quality for it (0 and
/// no format: the original file). Applies to songs fetched from now on, not ones already on their way.
pub fn network_metered(client: &Client, metered: bool) -> nori_core::stream::StreamQuality {
    nori_core::stream::network_metered(metered);
    client.streaming_quality(metered)
}

/// The container a cache key names (`<id>:192opus` is Opus); None for the original file.
pub fn key_format(key: &str) -> Option<String> {
    let q = key.rsplit_once(':')?.1.trim_start_matches(|c: char| c.is_ascii_digit());
    (!q.is_empty()).then(|| q.to_string())
}

impl Library for CoreLibrary {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let song = nori_core::queue::queue_song(id.to_string());
        let duration_ms = song.as_ref().map(|s| s.duration as i64 * 1000).filter(|&d| d > 0);
        // A download may be transcoded: the file says what it is.
        let kept = self.store.as_ref().filter(|_| transfers::held(id) == transfers::HeldState::Done).and_then(|s| s.downloaded(id));
        if let Some(path) = kept {
            return Ok(Located { source: Source::File(path), hint: None, duration_ms, estimated: false });
        }
        let target = self.client.resolve(id.to_string(), false, self.metered());
        let hint = key_format(&target.key).or_else(|| song.as_ref().map(|s| s.suffix.clone())).filter(|s| !s.is_empty());
        let estimated = nori_core::stream::length_estimated(&target.url);
        let (url, bytes) = (target.url, self.bytes.clone());
        let source = match &self.store {
            Some(store) => Source::Cached { url, bytes, store: store.clone(), key: target.key },
            None => Source::Url { url, bytes },
        };
        Ok(Located { source, hint, duration_ms, estimated })
    }

    fn about(&self, id: &str) -> WindowSong {
        about(id)
    }

    fn fetch_ahead(&self, id: &str) -> bool {
        fetch_ahead(id)
    }

    /// Fetches the core's precache targets except `next` into the store.
    fn ahead(&mut self, next: &str) {
        let Some(store) = &self.store else { return };
        store.fetch_ahead(self.bytes.clone(), ahead_songs(self.client.precache_targets(self.metered()), next), Some(measuring_ahead()));
    }

    fn taker(&self, id: &str, hint: Option<&str>) -> Option<Listening> {
        measure_as_it_comes(id, hint, false)
    }

    fn forget(&mut self, id: &str) {
        let Some(store) = &self.store else { return };
        let target = self.client.resolve(id.to_string(), false, self.metered());
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

/// Measures each song fetched ahead as it arrives ([`measure_as_it_comes`]); the fetch may wait for it.
pub fn measuring_ahead() -> Takers {
    Arc::new(|song: &AheadSong| measure_as_it_comes(&song.id, key_format(&song.key).or_else(|| nori_core::queue::queue_song(song.id.clone()).map(|s| s.suffix)).as_deref(), true))
}

/// What the planner and seek bar know of `id`, from the core's queue.
pub fn about(id: &str) -> WindowSong {
    match nori_core::queue::queue_song(id.to_string()) {
        Some(s) => WindowSong {
            id: s.id,
            title: s.title,
            duration_ms: s.duration as i64 * 1000,
            album_id: s.album_id,
            disc: s.disc_number as i32,
            track: s.track as i32,
            tag_bpm: s.bpm as f32,
            radio: false,
            // Stamped by the window (`playlist_window`).
            album_run: 0,
        },
        None => WindowSong { id: id.to_string(), title: id.to_string(), radio: id.starts_with(RADIO), ..Default::default() },
    }
}

/// Id prefix of internet radio stations in the queue.
pub const RADIO: &str = "radio:";

/// Whether `id` is an internet radio station.
pub fn is_radio(id: &str) -> bool {
    id.starts_with(RADIO)
}

/// Whether `id` may be fetched unasked (never a provider's song).
pub fn fetch_ahead(id: &str) -> bool {
    !nori_core::queue::queue_fetchable(vec![id.to_string()]).is_empty()
}

/// Runs the core's download queue: fetches pending songs whole into the store, a few at a time, oldest
/// first. Progress is the core's (`transfers`). Threads live only while there is work.
pub struct Downloader {
    core: Arc<Core>,
    client: Arc<Client>,
    bytes: Arc<dyn ByteSource>,
    store: Arc<Store>,
    work: Mutex<Work>,
}

#[derive(Default)]
struct Work {
    running: usize,
    /// Being fetched, and failed this run (retried only when asked again).
    busy: HashSet<String>,
    failed: HashSet<String>,
    threads: Vec<JoinHandle<()>>,
}

/// Interruptions in a row before a download fails.
const DOWNLOAD_TRIES: u32 = 3;
const DOWNLOAD_CHUNK: usize = 64 * 1024;

impl Downloader {
    pub fn new(core: Arc<Core>, client: Arc<Client>, bytes: Arc<dyn ByteSource>, store: Arc<Store>) -> Arc<Downloader> {
        crate::processing::install(Box::new(StoreShelf { client: client.clone(), store: store.clone() }));
        Arc::new(Downloader { core, client, bytes, store, work: Mutex::new(Work::default()) })
    }

    /// Fetches the queue, `slots` songs at a time; a call while running only fills free slots.
    pub fn start(self: &Arc<Self>, slots: usize) {
        let mut w = self.work.lock();
        w.failed.clear();
        w.threads.retain(|t| !t.is_finished());
        let pending = self.pending();
        for id in &pending {
            transfers::followed(id, transfers::QUEUED, nori_core::db::now_ms());
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
        crate::processing::wait();
    }

    /// Unfinished downloads, oldest first.
    fn pending(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.core.downloads(false).unwrap_or_default().into_iter().map(|s| s.id).collect();
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
            if ok {
                drop(w);
                transfers::followed(&id, transfers::COMPLETED, now);
                // No lyrics lookup here (Android does it, `lyrics_for_downloads`).
                transfers::work_done(&id, transfers::Work::Lyrics);
                let _ = self.core.download_settle(vec![id.clone()], vec![true]);
                self.store.drop_cached(&nori_core::stream_cache::copies(&id));
                crate::processing::saved(vec![id.clone()]);
            } else {
                w.failed.insert(id.clone());
                drop(w);
                transfers::followed(&id, transfers::FAILED, now);
            }
        }
    }

    /// Fetches `id` whole into the store, resuming a partial download.
    fn fetch(&self, id: &str) -> bool {
        let now = nori_core::db::now_ms();
        transfers::followed(id, transfers::DOWNLOADING, now);
        let slot = transfers::open(id, now);
        let url = self.client.resolve(id.to_string(), true, false).url;
        let part = self.store.download_part(id);
        let mut chunk = vec![0u8; DOWNLOAD_CHUNK];
        let mut tries = 0;
        // Measured as it downloads from the first byte; a resumed one is measured from disk later.
        let hint = nori_core::queue::queue_song(id.to_string()).or_else(|| self.core.download_song(id)).map(|s| s.suffix).filter(|s| !s.is_empty());
        let mut taker = if std::fs::metadata(&part).map_or(0, |m| m.len()) == 0 { measure_download_as_it_comes(id, hint.as_deref()) } else { None };
        loop {
            let have = std::fs::metadata(&part).map_or(0, |m| m.len());
            let body = match self.bytes.open(&url, have) {
                Ok(b) => b,
                // Nothing past what is on disk: complete (a transcode's length was an estimate).
                Err(OpenError::PastEnd { len }) if have > 0 && len.is_none_or(|l| l == have) => {
                    return std::fs::rename(&part, self.store.download_path(id)).is_ok();
                }
                Err(_) => {
                    tries += 1;
                    if tries >= DOWNLOAD_TRIES {
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(500 << tries));
                    continue;
                }
            };
            // A rangeless server resends everything: start the file over.
            let (mut at, append) = if body.start == have { (have, true) } else { (0, false) };
            if !append && have > 0 {
                taker = None;
            }
            let file = std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&part);
            let Ok(mut file) = file else { return false };
            let mut reader = body.reader;
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
                        transfers::note(slot, body.len.unwrap_or(0) as i64, at as i64, nori_core::db::now_ms());
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
            tries += 1;
            if tries >= DOWNLOAD_TRIES {
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

/// nori-engine's disk: finished downloads and whole stream cache entries.
struct StoreShelf {
    client: Arc<Client>,
    store: Arc<Store>,
}

impl Shelf for StoreShelf {
    fn whole(&self, id: &str) -> Option<Whole> {
        if transfers::held(id) == transfers::HeldState::Done {
            if let Some(p) = self.store.downloaded(id) {
                return Some(Whole { files: vec![p], hint: None });
            }
        }
        // The current network's quality first, then the other's.
        let metered = nori_core::stream::metered();
        let (key, path) = [metered, !metered].into_iter().find_map(|m| {
            let key = self.client.resolve(id.to_string(), false, m).key;
            self.store.peek(&key).map(|p| (key, p))
        })?;
        let hint = key_format(&key).or_else(|| nori_core::queue::queue_song(id.to_string()).map(|s| s.suffix)).filter(|s| !s.is_empty());
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
/// dropped with the thread.
pub struct Measurer {
    /// The core to store into, fetched per look (a profile switch replaces it).
    core: Box<dyn Fn() -> Option<Arc<Core>> + Send + Sync>,
    shelf: Box<dyn Shelf>,
    plan: Mutex<Schedule>,
    /// Bumped when the list changes; read per buffer so an unwanted decode stops.
    asked: AtomicU64,
    /// Something was stored since the engine last asked.
    measured: AtomicBool,
    /// Called on the measuring thread whenever something was stored.
    told: Option<Box<dyn Fn() + Send + Sync>>,
    decoded: AtomicU64,
}

/// When the measurer looks and at what, apart from threads and the disk.
#[derive(Default)]
struct Schedule {
    ids: Vec<String>,
    /// Bumped on news: a new list, or a song arrived.
    news: u64,
    /// The news last looked at.
    seen: u64,
    running: bool,
    engine: Option<std::thread::Thread>,
    /// Songs tried, with their bytes on disk then and whether the beat model had them.
    tried: HashMap<String, (u64, bool)>,
}

/// Tried songs remembered before those no longer asked for are dropped.
const TRIED_KEPT: usize = 256;

impl Schedule {
    /// The songs to measure are `ids`; returns whether a thread should start.
    fn ask(&mut self, ids: Vec<String>) -> bool {
        if ids != self.ids {
            self.ids = ids;
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

    /// Whether `id` is asked for and untried with `bytes`, or needs the beat model (`listen`) it did not get.
    fn worth(&self, id: &str, bytes: u64, listen: bool) -> bool {
        self.asks(id) && self.tried.get(id).is_none_or(|&(had, heard)| bytes > had || (listen && !heard))
    }

    fn asks(&self, id: &str) -> bool {
        self.ids.iter().any(|i| i == id)
    }

    fn tried(&mut self, id: &str, bytes: u64, heard: bool) {
        self.tried.insert(id.to_string(), (bytes, heard));
    }
}

impl Measurer {
    /// Measures from nori-engine's [`Store`], looking again whenever a cached song becomes whole.
    pub fn new(core: Arc<Core>, client: Arc<Client>, store: Arc<Store>) -> Arc<Measurer> {
        let m = Measurer::on_shelf(move || Some(core.clone()), Box::new(StoreShelf { client, store: store.clone() }), None);
        let weak = Arc::downgrade(&m);
        store.on_whole(Box::new(move || {
            if let Some(m) = weak.upgrade() {
                m.arrived();
            }
        }));
        m
    }

    /// Measures songs `shelf` has whole, storing into `core()`; `told` hears of each stored song.
    pub fn on_shelf(core: impl Fn() -> Option<Arc<Core>> + Send + Sync + 'static, shelf: Box<dyn Shelf>, told: Option<Box<dyn Fn() + Send + Sync>>) -> Arc<Measurer> {
        let m = Arc::new(Measurer { core: Box::new(core), shelf, plan: Mutex::new(Schedule::default()), asked: AtomicU64::new(0), measured: AtomicBool::new(false), told, decoded: AtomicU64::new(0) });
        ARRIVALS.lock().watch(&m);
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
        let mut plan = self.plan.lock();
        if ids != plan.ids {
            self.asked.fetch_add(1, Ordering::AcqRel);
        }
        let start = plan.ask(ids);
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

    /// Songs decoded so far.
    pub fn decoded(&self) -> u64 {
        self.decoded.load(Ordering::Relaxed)
    }

    fn spawn(self: &Arc<Self>) {
        let me = self.clone();
        if std::thread::Builder::new().name("nori-measure".into()).spawn(move || me.run()).is_err() {
            self.plan.lock().running = false;
        }
    }

    fn run(&self) {
        crate::arriving::lower_priority();
        let mut model = Model::default();
        loop {
            let Some(ids) = self.plan.lock().next() else { return };
            let Some(core) = (self.core)() else { continue };
            let missing = core.analysis_missing(ids.clone()).unwrap_or_default();
            let near = listen_to(&core, &ids, &missing);
            let todo: Vec<&String> = ids.iter().filter(|id| missing.contains(id) || near.contains(id)).collect();
            let mut waiting = 0;
            for id in todo {
                // Measured as it arrives: that decode's end is news here.
                if ARRIVALS.lock().has(id) {
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
                if !self.plan.lock().worth(id, bytes, asked_to_listen) {
                    continue;
                }
                let listen = asked_to_listen && model.ready();
                // Ask again: it may have been measured as it arrived meanwhile.
                let classical = missing.contains(id) && !core.analysis_missing(vec![id.clone()]).unwrap_or_default().is_empty();
                if !classical && !listen {
                    continue;
                }
                self.decoded.fetch_add(1, Ordering::Relaxed);
                let cpu = crate::arriving::thread_cpu_ms();
                let job = Job { classical, model: if listen { model.get() } else { None } };
                let Some(stored) = self.measure(&core, id, pieces, hint.as_deref(), job) else { continue };
                if let (Some(a), Some(b)) = (cpu, crate::arriving::thread_cpu_ms()) {
                    nori_core::alog::info(&format!("measuring {id} ahead from the disk took {} ms of CPU", b.saturating_sub(a)));
                }
                self.plan.lock().tried(id, bytes, listen);
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
    fn measure(&self, core: &Core, id: &str, pieces: crate::pieces::Pieces, hint: Option<&str>, job: Job) -> Option<bool> {
        let expected_ms = nori_core::queue::queue_song(id.to_string()).map_or(0, |s| s.duration as i64 * 1000);
        let mut asked = self.asked.load(Ordering::Acquire);
        let Decoded { stream, ends } = decode(id, "measuring ahead", pieces, hint, expected_ms, job.classical, job.model.is_some(), || {
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

/// Decodes `id` whole from `pieces` for the analyser (`classical`) and the beat model (`ends`) while
/// `go_on` (asked per buffer); None when abandoned. `what` names the work in the log.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode(id: &str, what: &str, pieces: crate::pieces::Pieces, hint: Option<&str>, expected_ms: i64, classical: bool, ends: bool, mut go_on: impl FnMut() -> bool) -> Option<Decoded> {
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

/// Process-wide: songs arrive from loaders, fetching ahead and downloads (a JNI entry point), none of
/// which holds the measurers.
pub(crate) static ARRIVALS: Mutex<Arrivals> = Mutex::new(Arrivals { songs: Vec::new(), measurers: Vec::new(), stored: 0 });

/// Songs measured as they arrived and stored, in this process (perf report, tests).
pub fn measured_as_they_came() -> u64 {
    ARRIVALS.lock().stored
}

/// Whether any song is being measured as it arrives.
pub fn measuring_as_they_come() -> bool {
    !ARRIVALS.lock().songs.is_empty()
}

/// Measures `id` as its bytes arrive (`crate::arriving`), if AutoMix is on, it is unanalysed, its
/// container allows it (`hint`: not MP4) and it is not already being measured. `wait`: the fetch may
/// block on the decoder (not for a loader the player reads).
pub fn measure_as_it_comes(id: &str, hint: Option<&str>, wait: bool) -> Option<Listening> {
    if !nori_core::rules::prefs(|p| p.auto_mix) {
        return None;
    }
    listen_as_it_comes(id, hint, wait)
}

/// [`measure_as_it_comes`] for a download, whatever AutoMix says: every download is analysed once
/// (`nori_core::transfers::needs`).
pub fn measure_download_as_it_comes(id: &str, hint: Option<&str>) -> Option<Listening> {
    listen_as_it_comes(id, hint, true)
}

fn listen_as_it_comes(id: &str, hint: Option<&str>, wait: bool) -> Option<Listening> {
    if !nori_core::queue::analysable(id) || !crate::demux::decodes_as_it_comes(hint) {
        return None;
    }
    let core = nori_core::active()?;
    if core.analysis_missing(vec![id.to_string()]).ok()?.is_empty() {
        return None;
    }
    if !ARRIVALS.lock().begin(id) {
        return None;
    }
    let expected_ms = nori_core::queue::queue_song(id.to_string()).map_or(0, |s| s.duration as i64 * 1000);
    let heard = Measuring { id: id.to_string(), core, expected_ms, stream: None, cpu_from: None };
    match Listening::start(hint.map(str::to_string), wait, Box::new(heard)) {
        Some(l) => {
            nori_core::transfers::analysing_began(id);
            Some(l)
        }
        None => {
            ARRIVALS.lock().end(id);
            None
        }
    }
}

/// Feeds decoded samples into the analyser; stores it once the whole song came.
struct Measuring {
    id: String,
    core: Arc<Core>,
    expected_ms: i64,
    stream: Option<nori_core::automix::store::AnalysisStream>,
    /// The decoder thread's CPU time at the start, for the log.
    cpu_from: Option<u64>,
}

impl Heard for Measuring {
    fn samples(&mut self, rate: u32, channels: usize, samples: &[f32]) {
        if self.stream.is_none() {
            self.cpu_from = crate::arriving::thread_cpu_ms();
        }
        let expected = self.expected_ms.max(0) as u64;
        self.stream.get_or_insert_with(|| nori_core::automix::store::AnalysisStream::new(rate, channels, expected)).feed_f32(samples);
    }

    fn done(self: Box<Self>, whole: bool) {
        let Measuring { id, core, expected_ms, stream, cpu_from } = *self;
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
            let mut a = ARRIVALS.lock();
            a.stored += stored as u64;
            a.end(&id);
            a.measurers()
        };
        nori_core::transfers::analysing_ended(&id, stored);
        crate::processing::kick();
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
    let wanted = beats::AVAILABLE && nori_core::settings_store::with_prefs(|p| p.auto_mix && p.auto_mix_better_beats).unwrap_or(false);
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
struct Job<'m> {
    /// No current analysis.
    classical: bool,
    /// The beat model reads its ends.
    model: Option<&'m BeatModel>,
}

/// Runs Beat This! over each unread end of `id`; returns whether a grid was stored.
pub(crate) fn listen(core: &Core, id: &str, model: &BeatModel, ends: &mut Ends) -> bool {
    let Ok(Some(row)) = core.analysis_get(id.to_string()) else { return false };
    // One run at a time per process: each holds tens of megabytes.
    let _one = LISTENING.lock();
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
                match model.read(&x[at(from)..at(to)], rate, end, from) {
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

/// Held during a beat model run. Process-wide by design: it caps memory for the whole process.
static LISTENING: Mutex<()> = Mutex::new(());

/// The beat model for a thread's life: loaded on first need, tried once per thread.
#[derive(Default)]
pub(crate) struct Model {
    tried: bool,
    loaded: Option<BeatModel>,
}

impl Model {
    /// Whether the model is loaded, loading it on first call.
    pub(crate) fn ready(&mut self) -> bool {
        if !self.tried {
            self.tried = true;
            self.loaded = BeatModel::load();
        }
        self.loaded.is_some()
    }

    pub(crate) fn get(&self) -> Option<&BeatModel> {
        self.loaded.as_ref()
    }
}

impl Drop for Model {
    /// Frees the model and returns its pages.
    fn drop(&mut self) {
        if self.loaded.take().is_some() {
            crate::arriving::give_memory_back();
        }
    }
}

/// The loaded beat model's heap growth (1 where the heap is not counted), for the perf report.
/// Process-wide, like the report.
static MODEL_BYTES: AtomicU64 = AtomicU64::new(0);

/// Bytes the beat model holds now; 0 when not loaded.
pub fn beat_model_bytes() -> u64 {
    MODEL_BYTES.load(Ordering::Relaxed)
}

/// Beat This! through tract (`neural-beats`).
#[cfg(feature = "neural-beats")]
pub(crate) struct BeatModel(nori_player::automix::neural::BeatThis);

#[cfg(feature = "neural-beats")]
impl Drop for BeatModel {
    fn drop(&mut self) {
        MODEL_BYTES.store(0, Ordering::Relaxed);
    }
}

#[cfg(feature = "neural-beats")]
impl BeatModel {
    fn load() -> Option<BeatModel> {
        let file = nori_core::beat_download::ensure()?;
        let t0 = std::time::Instant::now();
        let before = nori_core::heap::live_bytes();
        let loaded = nori_core::beat_download::read(&file).and_then(|bytes| nori_player::automix::neural::BeatThis::from_weights(&bytes).map_err(|e| e.to_string()));
        match loaded {
            Ok(m) => {
                let bytes = before.zip(nori_core::heap::live_bytes()).map_or(1, |(a, b)| (b - a).max(1) as u64);
                MODEL_BYTES.store(bytes, Ordering::Relaxed);
                nori_core::alog::info(&format!("beat model loaded in {} ms, {} KB", t0.elapsed().as_millis(), bytes / 1024));
                Some(BeatModel(m))
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
    fn load() -> Option<BeatModel> {
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
    fn same_list_is_not_looked_at_again() {
        let mut s = Schedule::default();
        assert!(s.ask(ids(&["a", "b", "c"])), "songs to measure: a thread");
        assert_eq!(s.next(), Some(ids(&["a", "b", "c"])));
        assert_eq!(s.next(), None, "nothing new: the thread ends");
        assert!(!s.running);
        // Repeated asks with the same list.
        for _ in 0..100 {
            assert!(!s.ask(ids(&["a", "b", "c"])), "no thread, no look");
        }
        assert!(s.ask(ids(&["b", "c", "d"])), "the queue moved: a look");
    }

    #[test]
    fn arrival_during_a_look_is_kept() {
        let mut s = Schedule::default();
        assert!(!s.arrived(), "nothing asked for: nothing to look at");
        assert!(s.ask(ids(&["a", "b"])));
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
        s.ask(ids(&["a", "b"]));
        assert!(s.worth("a", 1000, false));
        s.tried("a", 1000, true);
        assert!(!s.worth("a", 1000, false), "tried with these bytes: not again, however often it is looked at");
        assert!(s.worth("a", 5000, false), "more of it has come since: once more");
        assert!(!s.worth("z", 1000, false), "not asked for");
        s.ask(ids(&["b"]));
        assert!(!s.worth("a", 5000, false), "no longer asked for");
    }

    #[test]
    fn measured_song_decoded_again_for_beat_model() {
        let mut s = Schedule::default();
        s.ask(ids(&["a", "b"]));
        s.tried("a", 1000, false);
        assert!(!s.worth("a", 1000, false));
        assert!(s.worth("a", 1000, true), "the model has not heard it");
        s.tried("a", 1000, true);
        assert!(!s.worth("a", 1000, true), "heard: never again");
    }
}
