//! The engine over the core, for a client that links it: the core's queue is the one played, its
//! transition planner and analysis store answer the engine, its settings are the sound, and songs
//! stream from the server's addresses through the client's [`ByteSource`] - or play from the disk, a
//! download or the stream cache, whose order is the core's. The downloads themselves run here too,
//! from the core's bookkeeping. With these a desktop client writes no player logic at all.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// The core's queue (`nori_core::playlist`). Edit it through the core's `playlist_*` calls, then tell
/// the engine ([`crate::Engine::queue_changed`]).
#[derive(Debug, Default, Clone, Copy)]
pub struct CoreQueue;

impl Queue for CoreQueue {
    fn read<R>(&self, f: impl FnOnce(&Playlist) -> R) -> R {
        nori_core::playlist::playlist_read(f)
    }

    fn moved_to(&mut self, index: usize) {
        nori_core::playlist::playlist_moved_to(index as i32);
    }

    fn set_repeat(&mut self, mode: u8) {
        nori_core::playlist::playlist_repeat(mode);
    }

    /// An explicit song with the user's "skip explicit songs" on, as `playlist_transition` decides.
    fn skips(&self, index: usize) -> bool {
        nori_core::playlist::playlist_skips(index)
    }
}

/// The core's transition planner, analysis store and log, as the engine's app.
pub struct CoreApp {
    host: CoreHost<fn()>,
    measurer: Option<Arc<Measurer>>,
    /// For each output device its own sound: the core that keeps the profiles, the outputs seen so far
    /// and the one the music goes to.
    devices: Option<Arc<Core>>,
    known: Vec<String>,
    output: Option<String>,
    /// The client has an offline bridge to hand a song the network would not bring to.
    bridge: bool,
    /// The songs' ReplayGain levels last logged.
    gains_said: Vec<(String, f32)>,
}

impl CoreApp {
    pub fn new() -> CoreApp {
        fn nothing() {}
        CoreApp { host: CoreHost { now_ms: 0, heard_changed: nothing }, measurer: None, devices: None, known: Vec::new(), output: None, bridge: false, gains_said: Vec::new() }
    }

    /// The client runs the offline bridge (`Core::bridge_start` over the queue): a song the network would
    /// not bring is handed to it when the user's setting says so (`Event::Bridge`).
    pub fn bridging(mut self) -> CoreApp {
        self.bridge = true;
        self
    }

    /// Output devices get the sound the core keeps for each (a profile bound to it, the sound from
    /// before a bound device took over), as Android's `DeviceSound` has it.
    pub fn per_device(mut self, core: Arc<Core>) -> CoreApp {
        self.devices = Some(core);
        self.known = nori_player::outputs::initial_known(&[]);
        self
    }

    /// With AutoMix on, the songs coming up that are on the disk are measured ahead by `measurer`.
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

    /// Songs are measured as they play (the engine's analysis tap), and with a measurer the ones coming
    /// up too, when AutoMix is on (the core's `queue_measure` names none otherwise).
    fn auto_mix(&self) -> bool {
        self.measurer.is_some() && !nori_core::rules::queue_measure().is_empty()
    }

    /// The core picks the songs (`queue_measure`: the next few, never a provider's or a radio stream);
    /// the measurer takes those on the disk, on a thread of its own.
    fn measure_ahead<S: nori_player::pipeline::Songs>(&mut self, _songs: &mut S, _ids: &[String]) {
        if let Some(m) = &self.measurer {
            m.update(nori_core::rules::queue_measure(), std::thread::current());
        }
    }

    /// The core names the device and says what arriving on it means (`Core::device_arrive`); a sound
    /// it applies is put into the settings, which the engine then plays with. AutoEQ curves are the
    /// client's to fetch and offer.
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
        // A step may ask for the arrival to be made again once it is done (a sound kept first).
        for _ in 0..2 {
            if let Some(s) = effect.apply.take() {
                let prefs = nori_core::settings_store::settings_current()?.with_sound(s);
                nori_core::settings_store::settings_put(prefs.clone());
                sound = Some(settings(&prefs).sound);
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

    /// The core keeps the window itself, from its own queue and what it knows of each song.
    fn window(&mut self, _window: Vec<WindowSong>, _shuffling: bool) {
        nori_core::playlist::playlist_window();
    }

    /// The core counts the run of songs that would not play, and reads "skip on error" and the offline
    /// bridge's setting itself.
    fn on_error(&mut self, kind: PlaybackError, _has_next: bool) -> Option<OnError> {
        Some(nori_core::rules::queue_error(kind, false, self.bridge))
    }

    fn playing(&mut self) {
        nori_core::rules::queue_playing();
    }

    fn transitions_off(&mut self, off: bool) {
        nori_core::automix::planner::transition_setup(off);
    }

    /// The core's ReplayGain over its own queue and the settings: track, album or automatic, the
    /// pre-amp, and the level for untagged songs.
    fn gain(&mut self, index: usize, id: &str) -> f32 {
        let g = nori_core::playlist::playlist_gain_of(index, false);
        // Said once per song and level, for the log: what a device check reads the levelling from.
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

/// The stream cache's order is the core's (`nori_core::stream_cache`): what this run never used goes
/// first, then the least recently used.
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

/// Songs stream from the server the client is logged in to, at the quality the settings ask for on the
/// network the device is on ([`network_metered`]); with a store, a finished download or a whole cached
/// copy plays from the disk before the network is asked, what streams is kept in the cache, and the
/// songs after the next one are fetched into it ahead of their turn as the core says
/// (`Client::precache_targets`: how many for this network, never a provider's song or a download), as
/// Android's precacher (Precacher.kt) fetches them there.
pub struct CoreLibrary {
    pub client: Arc<Client>,
    pub bytes: Arc<dyn ByteSource>,
    /// Always stream the metered quality, whatever the platform last said of the network.
    pub metered: bool,
    pub store: Option<Arc<Store>>,
}

impl CoreLibrary {
    /// Whether songs opened or fetched now stream at the metered quality.
    fn metered(&self) -> bool {
        self.metered || nori_core::stream::metered()
    }
}

/// The network the device is on is `metered` or not now (a phone's mobile data, a tethered laptop), as
/// the platform says whenever it changes: Android's `EnginePlayer` from its network callback, a desktop
/// client from wherever its system says it (NetworkManager's `Metered`, Windows' cost), or never, and
/// songs stream at the unmetered quality. Answers that quality: the user's setting for the network
/// (bit rate and format, transcoded by the server; 0 and none are the original file).
///
/// It applies to the next song fetched, not to one already on its way: the song playing keeps the
/// bytes it has and the address it came from (a seek reads on from the same file), and so does the one
/// after it once its fetch has begun. Fetching ahead follows at the next song's start: how many songs,
/// and none on a metered network unless the settings allow it.
pub fn network_metered(client: &Client, metered: bool) -> nori_core::stream::StreamQuality {
    nori_core::stream::network_metered(metered);
    client.streaming_quality(metered)
}

/// The container a cache key's quality names (`<id>:192opus` is Opus); none for the original file.
pub fn key_format(key: &str) -> Option<String> {
    let q = key.rsplit_once(':')?.1.trim_start_matches(|c: char| c.is_ascii_digit());
    (!q.is_empty()).then(|| q.to_string())
}

impl Library for CoreLibrary {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let song = nori_core::queue::queue_song(id.to_string());
        let duration_ms = song.as_ref().map(|s| s.duration as i64 * 1000).filter(|&d| d > 0);
        // A download may have been transcoded: the file says what it is.
        let kept = self.store.as_ref().filter(|_| transfers::held(id) == 2).and_then(|s| s.downloaded(id));
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

    /// Never a provider's song: asking for one makes the server download it.
    fn fetch_ahead(&self, id: &str) -> bool {
        fetch_ahead(id)
    }

    /// The songs the core names to fetch ahead, whole into the stream cache, but `next`, which the
    /// engine is fetching itself; nothing without a store. Measured as they come when AutoMix is on.
    fn ahead(&mut self, next: &str) {
        let Some(store) = &self.store else { return };
        store.fetch_ahead(self.bytes.clone(), ahead_songs(self.client.precache_targets(self.metered()), next), Some(measuring_ahead()));
    }

    fn taker(&self, id: &str, hint: Option<&str>) -> Option<Listening> {
        measure_as_it_comes(id, hint, false)
    }

    /// The song played nothing and is opened again from scratch: its stream cache entry goes, a download
    /// stays.
    fn forget(&mut self, id: &str) {
        let Some(store) = &self.store else { return };
        let target = self.client.resolve(id.to_string(), false, self.metered());
        if store.peek(&target.key).is_some() {
            nori_core::alog::info(&format!("{id} is fetched anew: its stream cache entry {} goes", target.key));
            store.drop_cached(&[target.key]);
        }
    }
}

/// The songs to fetch ahead of `fetch` (the core's `precache_targets`, or `nori_core::stream::precache_now`),
/// less `next`, which the engine's loader fetches itself.
pub fn ahead_songs(fetch: Vec<nori_core::stream::Fetch>, next: &str) -> Vec<AheadSong> {
    fetch.into_iter().filter(|f| f.id != next).map(|f| AheadSong { id: f.id, url: f.url, key: f.key }).collect()
}

/// What hears each song fetched ahead: AutoMix's measuring, when it is on and the song is not measured
/// ([`measure_as_it_comes`]). The fetching ahead may wait for it.
pub fn measuring_ahead() -> Takers {
    Arc::new(|song: &AheadSong| measure_as_it_comes(&song.id, key_format(&song.key).or_else(|| nori_core::queue::queue_song(song.id.clone()).map(|s| s.suffix)).as_deref(), true))
}

/// What the transition planner and the seek bar know of `id`, from the core's queue: for a client
/// that writes its own [`Library`] around the core's.
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
        },
        None => WindowSong { id: id.to_string(), title: id.to_string(), radio: id.starts_with(RADIO), ..Default::default() },
    }
}

/// The ids the core gives internet radio stations in the queue.
pub const RADIO: &str = "radio:";

/// Whether `id` is an internet radio station: a live stream, never cached, measured or fetched ahead.
pub fn is_radio(id: &str) -> bool {
    id.starts_with(RADIO)
}

/// Whether `id` may be fetched before anyone asked to hear it: never a provider's song.
pub fn fetch_ahead(id: &str) -> bool {
    !nori_core::queue::queue_fetchable(vec![id.to_string()]).is_empty()
}

/// Downloads, as the core keeps them (`Core::download_queue` and the downloads table): the songs still
/// to come are fetched whole through the client's [`ByteSource`] into the store, a few at a time as the
/// settings say, oldest first; progress, phases and the notification's words are the core's
/// (`transfers`). Its threads exist only while there is something to fetch.
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
    /// Being fetched now, and failed in this run (tried again only when asked again).
    busy: HashSet<String>,
    failed: HashSet<String>,
    threads: Vec<JoinHandle<()>>,
}

/// A download interrupted this many times in a row counts as failed.
const DOWNLOAD_TRIES: u32 = 3;
const DOWNLOAD_CHUNK: usize = 64 * 1024;

impl Downloader {
    pub fn new(core: Arc<Core>, client: Arc<Client>, bytes: Arc<dyn ByteSource>, store: Arc<Store>) -> Arc<Downloader> {
        Arc::new(Downloader { core, client, bytes, store, work: Mutex::new(Work::default()) })
    }

    /// Fetches what is queued, `slots` songs at a time; a call while it runs only fills free slots.
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

    /// Waits until nothing is left to fetch.
    pub fn wait(&self) {
        loop {
            let Some(t) = self.work.lock().threads.pop() else { return };
            let _ = t.join();
        }
    }

    /// The unfinished downloads in the order the queue runs them: oldest first.
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
                // No lyrics are looked up for a download here (Android's are, `lyrics_for_downloads`).
                transfers::work_done(&id, transfers::Work::Lyrics);
                let _ = self.core.download_settle(vec![id.clone()], vec![true]);
                // The streamed copy is the same bytes twice now.
                self.store.drop_cached(&nori_core::stream_cache::copies(&id));
            } else {
                w.failed.insert(id.clone());
                drop(w);
                transfers::followed(&id, transfers::FAILED, now);
            }
        }
    }

    /// Fetches `id` whole into the store, taking up a download left half way where it stopped.
    fn fetch(&self, id: &str) -> bool {
        let now = nori_core::db::now_ms();
        transfers::followed(id, transfers::DOWNLOADING, now);
        let slot = transfers::open(id, now);
        let url = self.client.resolve(id.to_string(), true, false).url;
        let part = self.store.download_part(id);
        let mut chunk = vec![0u8; DOWNLOAD_CHUNK];
        let mut tries = 0;
        // With AutoMix on, measured as it downloads, from its first byte: later mixes need no pass of their
        // own. One taken up half way by an earlier run is measured when it is queued.
        let hint = nori_core::queue::queue_song(id.to_string()).map(|s| s.suffix).filter(|s| !s.is_empty());
        let mut taker = if std::fs::metadata(&part).map_or(0, |m| m.len()) == 0 { measure_as_it_comes(id, hint.as_deref(), true) } else { None };
        loop {
            let have = std::fs::metadata(&part).map_or(0, |m| m.len());
            let body = match self.bytes.open(&url, have) {
                Ok(b) => b,
                // Nothing past what is on the disk: it is all there, short of a length the server
                // promised (an estimate, for a transcode).
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
            // A server that would not do ranges sends it all again: the file starts again too.
            let (mut at, append) = if body.start == have { (have, true) } else { (0, false) };
            if !append && have > 0 {
                // Heard up to where it broke, and now from the start again: what was heard is dropped.
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

/// Where the songs coming up are on the disk, whole: a client's downloads and stream cache.
pub trait Shelf: Send + Sync {
    /// `id`'s bytes if every one of them is on the disk (a short tail of tags after the music aside):
    /// the files that hold them, in order. None while any of it is missing, being fetched or not.
    fn whole(&self, id: &str) -> Option<Whole>;
}

/// A song whole on the disk: its files, one after the other, and what container it is in when the
/// file alone should not decide (a stream's cache key names it); None for a download, whose file says.
pub struct Whole {
    pub files: Vec<std::path::PathBuf>,
    pub hint: Option<String>,
}

/// nori-engine's own disk: a finished download, or a whole copy in the stream cache.
struct StoreShelf {
    client: Arc<Client>,
    store: Arc<Store>,
}

impl Shelf for StoreShelf {
    fn whole(&self, id: &str) -> Option<Whole> {
        if transfers::held(id) == 2 {
            if let Some(p) = self.store.downloaded(id) {
                return Some(Whole { files: vec![p], hint: None });
            }
        }
        // The quality for the network the device is on now first, then the other one's copy.
        let metered = nori_core::stream::metered();
        let (key, path) = [metered, !metered].into_iter().find_map(|m| {
            let key = self.client.resolve(id.to_string(), false, m).key;
            self.store.peek(&key).map(|p| (key, p))
        })?;
        let hint = key_format(&key).or_else(|| nori_core::queue::queue_song(id.to_string()).map(|s| s.suffix)).filter(|s| !s.is_empty());
        Some(Whole { files: vec![path], hint })
    }
}

/// AutoMix's measuring ahead: the songs coming up that have no analysis yet are decoded whole on a
/// thread of the lowest priority and measured, and the result stored through the core, so a
/// transition has both songs' tempo and beats the first time they meet.
///
/// Only a song whole on the disk is measured, its files read straight in large sequential reads: it
/// costs the network nothing, and nothing ever waits on bytes still coming. A song not there yet is
/// looked at again when the client says a song has arrived ([`Measurer::arrived`]) or the songs asked
/// for change, never by polling. Each song is measured once: one that was measured, or could not be,
/// is not tried again until more of it is on the disk. The thread exists only while there is something
/// new to look at, so with nothing changing it costs nothing at all.
///
/// With "Better beat detection" on (a build with the `neural-beats` feature), the same decode keeps the first and
/// last half minute of the song playing and the next one, and Beat This! reads them for the intro and outro grids:
/// each end once (the stored row remembers it). The model is fetched the first time it is needed, loaded when a
/// look needs it and let go when the thread ends; with the switch off none of it exists.
pub struct Measurer {
    /// The core the measurements are stored in, as it is at each look (a profile switch makes another).
    core: Box<dyn Fn() -> Option<Arc<Core>> + Send + Sync>,
    shelf: Box<dyn Shelf>,
    plan: Mutex<Schedule>,
    /// Moves whenever the songs asked for change, read per buffer without the lock: a song being
    /// decoded that is no longer asked for is left half way.
    asked: AtomicU64,
    /// Something was stored since the engine last asked.
    measured: AtomicBool,
    /// Told whenever something was stored, on the measuring thread.
    told: Option<Box<dyn Fn() + Send + Sync>>,
    /// Songs decoded so far, stored or not.
    decoded: AtomicU64,
}

/// When the measurer looks, and at what: its rules, apart from the threads and the disk.
#[derive(Default)]
struct Schedule {
    ids: Vec<String>,
    /// Moves when there is something new to look at: other songs asked for, or a song arrived.
    news: u64,
    /// The news the last look took in: the same news is never looked at twice.
    seen: u64,
    running: bool,
    engine: Option<std::thread::Thread>,
    /// Songs measured or given up on, with how many of their bytes were on the disk then and whether the beat
    /// model has had them too.
    tried: HashMap<String, (u64, bool)>,
}

/// Songs remembered as tried before those no longer asked for are let go.
const TRIED_KEPT: usize = 256;

impl Schedule {
    /// The songs to measure are `ids` now. Whether a thread is to start.
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

    /// A song has arrived on the disk. Whether a thread is to start.
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

    /// The songs to look at now, once per piece of news; None when there is nothing new, and the
    /// thread ends.
    fn next(&mut self) -> Option<Vec<String>> {
        if self.ids.is_empty() || self.news == self.seen {
            self.running = false;
            return None;
        }
        self.seen = self.news;
        Some(self.ids.clone())
    }

    /// Whether `id`, with `bytes` of it on the disk, is still asked for and was not tried with as many, or
    /// is for the beat model (`listen`) and was only measured before it.
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
    /// Measures from nori-engine's own [`Store`]: the downloads and the stream cache there.
    /// A song the store's cache finishes (the next one, fetched after the song playing started or a
    /// queue edit put it there) is looked at as it becomes whole.
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

    /// Measures the songs `shelf` says are whole, storing into `core()` as it is at each look; `told`
    /// hears of each song stored (to plan the transitions again), on the measuring thread.
    pub fn on_shelf(core: impl Fn() -> Option<Arc<Core>> + Send + Sync + 'static, shelf: Box<dyn Shelf>, told: Option<Box<dyn Fn() + Send + Sync>>) -> Arc<Measurer> {
        let m = Arc::new(Measurer { core: Box::new(core), shelf, plan: Mutex::new(Schedule::default()), asked: AtomicU64::new(0), measured: AtomicBool::new(false), told, decoded: AtomicU64::new(0) });
        let mut all = MEASURERS.lock();
        all.retain(|w| w.strong_count() > 0);
        all.push(Arc::downgrade(&m));
        m
    }

    /// A song was measured as it came, elsewhere: what was planned without it is planned again.
    fn stored_elsewhere(&self) {
        self.measured.store(true, Ordering::Release);
        if let Some(t) = &self.plan.lock().engine {
            t.unpark();
        }
        if let Some(told) = &self.told {
            told();
        }
    }

    /// Measures `ids` (the songs coming up) from now on; `engine` is woken when something was stored.
    pub fn update(self: &Arc<Self>, ids: Vec<String>, engine: std::thread::Thread) {
        self.plan.lock().engine = Some(engine);
        self.ask(ids);
    }

    /// Measures `ids` (the songs coming up, the one playing first) from now on, dropping what was asked
    /// before: the point is the next boundary, not completeness. The same songs as before change
    /// nothing; none stops the measuring.
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

    /// A song has become whole on the disk: the songs asked for are looked at again.
    pub fn arrived(self: &Arc<Self>) {
        let start = self.plan.lock().arrived();
        if start {
            self.spawn();
        }
    }

    /// Whether the measuring thread is at work: for a test to wait until it is done.
    pub fn busy(&self) -> bool {
        self.plan.lock().running
    }

    /// How many songs have been decoded so far, stored or not.
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
        lower_priority();
        // The beat model, loaded by the first song of this thread's life that needs it.
        let mut model = Model::default();
        loop {
            let Some(ids) = self.plan.lock().next() else { return };
            let Some(core) = (self.core)() else { continue };
            let missing = core.analysis_missing(ids.clone()).unwrap_or_default();
            let near = listen_to(&core, &ids, &missing);
            let todo: Vec<&String> = ids.iter().filter(|id| missing.contains(id) || near.contains(id)).collect();
            let mut waiting = 0;
            for id in todo {
                // Being measured as it comes: that decode is the one, and its end is news for this.
                if ARRIVING.lock().contains(id) {
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
                // Asked again: the songs before it took a while, and it may have been measured as it came.
                let classical = missing.contains(id) && !core.analysis_missing(vec![id.clone()]).unwrap_or_default().is_empty();
                if !classical && !listen {
                    continue;
                }
                // Left half way: not tried, and looked at again with the next news.
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

    /// Decodes `id` whole into the streaming analyser (`job.classical`) and the beat model's copy of its ends
    /// (`job.model`), and stores what comes out: whether anything was stored, or None when it was left half way
    /// because it is no longer asked for.
    fn measure(&self, core: &Core, id: &str, pieces: crate::pieces::Pieces, hint: Option<&str>, job: Job) -> Option<bool> {
        let expected_ms = nori_core::queue::queue_song(id.to_string()).map_or(0, |s| s.duration as i64 * 1000);
        let mut asked = self.asked.load(Ordering::Acquire);
        let mut left = false;
        let mut stream = None;
        let mut ends = None;
        let mut heard = false;
        let whole = crate::demux::decode_whole(Box::new(pieces), hint, |rate, channels, samples| {
            let now = self.asked.load(Ordering::Acquire);
            if now != asked {
                if !self.plan.lock().asks(id) {
                    left = true;
                    return false;
                }
                asked = now;
            }
            heard = true;
            if job.classical {
                stream.get_or_insert_with(|| nori_core::automix::store::AnalysisStream::new(rate, channels, expected_ms.max(0) as u64)).feed_f32(samples);
            }
            if job.model.is_some() {
                ends.get_or_insert_with(|| Ends::new(rate)).feed(samples, channels);
            }
            true
        });
        if left {
            return None;
        }
        if !heard {
            nori_core::alog::info(&format!("measuring {id} ahead: nothing decoded ({})", whole.err().unwrap_or_default()));
            return Some(false);
        }
        if !matches!(whole, Ok(true)) {
            nori_core::alog::info(&format!("measuring {id} ahead stopped before its end: not stored"));
            return Some(false);
        }
        let measured = stream.is_some_and(|stream| self.finish(core, id, stream, expected_ms));
        let listened = match (job.model, ends) {
            (Some(model), Some(mut ends)) => {
                let adopted = listen(core, id, model, &mut ends);
                drop(ends);
                // The model's run took tens of megabytes a block at a time, all free again now: handed back to
                // the system, rather than kept by the allocator for a run that may not come for minutes.
                crate::arriving::give_memory_back();
                adopted
            }
            _ => false,
        };
        Some(measured || listened)
    }

    /// Stores what the streaming analyser measured of the whole of `id`: whether it was stored.
    fn finish(&self, core: &Core, id: &str, stream: nori_core::automix::store::AnalysisStream, expected_ms: i64) -> bool {
        let a = core.analysis_finish_whole(id, stream, expected_ms).ok().flatten();
        nori_core::alog::info(&match &a {
            Some(t) => format!("analysed {id} ahead: {:.2} bpm (conf {:.2}, stab {:.2})", t.bpm, t.bpm_confidence, t.stability),
            None => format!("analysed {id} ahead: not stored: not the whole song, or too short"),
        });
        a.is_some()
    }
}

// ---- measured as it comes ----

/// The songs being measured as they come now: the measurer leaves them to that decode.
static ARRIVING: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Every measurer, told when a song measured as it came was stored (or was not, and may be looked at).
static MEASURERS: Mutex<Vec<std::sync::Weak<Measurer>>> = Mutex::new(Vec::new());
/// Songs measured as they came and stored, in this process.
static CAME: AtomicU64 = AtomicU64::new(0);

/// How many songs were measured as they came and stored, in this process: for the perf report and tests.
pub fn measured_as_they_came() -> u64 {
    CAME.load(Ordering::Relaxed)
}

/// Whether a song is being measured as it comes now: for a test to wait until none is.
pub fn measuring_as_they_come() -> bool {
    !ARRIVING.lock().is_empty()
}

/// What hears `id`'s bytes as they are fetched, from its first, to measure it for AutoMix on those same
/// bytes in the same burst (`crate::arriving`): when AutoMix is on, the song can be measured at all and is
/// not measured yet, its container can be read as it comes (`hint`: not an MP4, which may keep what it is at
/// its end), and it is not being measured as it comes already. `wait`: the fetch may wait for the decoder
/// (fetching ahead, a download); a loader the player may be reading from must not.
pub fn measure_as_it_comes(id: &str, hint: Option<&str>, wait: bool) -> Option<Listening> {
    if !nori_core::rules::prefs(|p| p.auto_mix) || !nori_core::queue::analysable(id) || !crate::demux::decodes_as_it_comes(hint) {
        return None;
    }
    let core = nori_core::active()?;
    if core.analysis_missing(vec![id.to_string()]).ok()?.is_empty() {
        return None;
    }
    {
        let mut a = ARRIVING.lock();
        if a.iter().any(|i| i == id) {
            return None;
        }
        a.push(id.to_string());
    }
    let expected_ms = nori_core::queue::queue_song(id.to_string()).map_or(0, |s| s.duration as i64 * 1000);
    let heard = Measuring { id: id.to_string(), core, expected_ms, stream: None, cpu_from: None };
    match Listening::start(hint.map(str::to_string), wait, Box::new(heard)) {
        Some(l) => {
            // A download saved before this is over shows it is still being analysed.
            nori_core::transfers::analysing(id, true);
            Some(l)
        }
        None => {
            ARRIVING.lock().retain(|i| i != id);
            None
        }
    }
}

/// A song's samples, as they are decoded from the bytes coming, into the streaming analyser; stored once the
/// whole song was heard.
struct Measuring {
    id: String,
    core: Arc<Core>,
    expected_ms: i64,
    stream: Option<nori_core::automix::store::AnalysisStream>,
    /// The decoding thread's CPU time when it began, for the log.
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
        if stored {
            CAME.fetch_add(1, Ordering::Relaxed);
        }
        ARRIVING.lock().retain(|i| *i != id);
        nori_core::transfers::analysing(&id, false);
        let measurers: Vec<Arc<Measurer>> = MEASURERS.lock().iter().filter_map(|w| w.upgrade()).collect();
        for m in measurers {
            if stored {
                m.stored_elsewhere();
            } else {
                // Not measured here: the measurer may find it whole on the disk.
                m.arrived();
            }
        }
    }
}

/// How many of the songs coming up the beat model reads: the song playing and the next one. The mix coming up needs
/// no more, and a song skipped before its turn would have cost a run for nothing.
const LISTEN_AHEAD: usize = 2;

/// The songs of `ids` the beat model is to read; none while "Better beat detection" is off or the build has no
/// model.
fn listen_to(core: &Core, ids: &[String], missing: &[String]) -> Vec<String> {
    let wanted = beats::AVAILABLE && nori_core::settings_store::with_prefs(|p| p.auto_mix && p.auto_mix_better_beats).unwrap_or(false);
    if !wanted {
        return Vec::new();
    }
    let near = &ids[..ids.len().min(LISTEN_AHEAD)];
    let mut out = core.analysis_neural_missing(near.to_vec()).unwrap_or_default();
    // Not measured yet: the decode that measures it feeds the model too.
    out.extend(near.iter().filter(|id| missing.contains(id)).cloned());
    out
}

/// What one decode of a song is for.
struct Job<'m> {
    /// The song has no current analysis.
    classical: bool,
    /// The beat model is to read its ends.
    model: Option<&'m BeatModel>,
}

/// Beat This! over each end of `id` it has not looked at yet, from the ends kept as the song was decoded: whether a
/// grid was stored.
fn listen(core: &Core, id: &str, model: &BeatModel, ends: &mut Ends) -> bool {
    let Ok(Some(row)) = core.analysis_get(id.to_string()) else { return false };
    // One song's windows at a time in the whole process, whichever measurer asks: each run holds tens of megabytes
    // while it lasts (the model reads 30 s at once; neural.rs), and two at once would hold twice that.
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

/// Held while the beat model reads a song's ends: one song at a time, process-wide.
static LISTENING: Mutex<()> = Mutex::new(());

/// The beat model while the measuring thread lives: fetched and loaded the first time a song needs it, and tried
/// once per thread, so a model that cannot come costs one attempt per look, not one per song.
#[derive(Default)]
struct Model {
    tried: bool,
    loaded: Option<BeatModel>,
}

impl Model {
    /// Whether the model is here to be run, fetching and loading it first when it is not.
    fn ready(&mut self) -> bool {
        if !self.tried {
            self.tried = true;
            self.loaded = BeatModel::load();
        }
        self.loaded.is_some()
    }

    fn get(&self) -> Option<&BeatModel> {
        self.loaded.as_ref()
    }
}

impl Drop for Model {
    /// The measuring thread is done: the model's weights and plan go, and the pages they were on with them.
    fn drop(&mut self) {
        if self.loaded.take().is_some() {
            crate::arriving::give_memory_back();
        }
    }
}

/// The beat model's memory while it is loaded: what the Rust heap grew by as it loaded (its weights and
/// tract's plan; 1 where the heap is not counted), nought while it is not. For the perf report.
static MODEL_BYTES: AtomicU64 = AtomicU64::new(0);

/// Bytes the beat model holds now; nought while it is not loaded.
pub fn beat_model_bytes() -> u64 {
    MODEL_BYTES.load(Ordering::Relaxed)
}

/// Beat This! through tract, in a build with the `neural-beats` feature.
#[cfg(feature = "neural-beats")]
struct BeatModel(nori_player::automix::neural::BeatThis);

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

/// A build without the model's runtime: never loaded, never run.
#[cfg(not(feature = "neural-beats"))]
struct BeatModel;

#[cfg(not(feature = "neural-beats"))]
impl BeatModel {
    fn load() -> Option<BeatModel> {
        None
    }

    fn read(&self, _: &[f32], _: u32, _: MixEnd, _: i64) -> Result<Option<beats::EndGrid>, String> {
        Ok(None)
    }
}

/// The measuring thread yields to everything else: the lowest priority the system has.
fn lower_priority() {
    crate::arriving::lower_priority();
}

/// The sound and the controls as the core's settings ask for them.
pub fn settings(s: &StoredPrefs) -> Settings {
    let bands = if s.eq_enabled { s.eq_bands.iter().map(|b| Band { kind: b.kind as i32, freq: b.freq as f64, gain_db: b.gain_db as f64, q: b.q as f64, channel: b.channel as i32 }).collect() } else { Vec::new() };
    let sound = Sound {
        // The graphic equalizer plays in place of the parametric one, whose bands then stay out.
        graphic: nori_core::dsp::graphic_sliders(s),
        bands: if s.eq_mode == nori_core::settings::EqMode::Graphic { Vec::new() } else { bands },
        effects: s.effects().player(),
        preamp_db: nori_core::dsp::effective_preamp_db(s) as f64,
        crossfeed_db: s.crossfeed_db as f64,
        balance: s.balance as f64,
        mono: s.mono,
        limiter: s.limiter,
        threshold_db: s.limiter_threshold_db as f64,
    };
    Settings {
        sound,
        speed: s.speed,
        pitch: s.pitch,
        skip_silence: s.skip_silence,
        fade_ms: s.fade_ms,
        hi_res: s.hi_res,
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

    /// Runs a look to its end, as the thread does: the songs looked at, or None when the thread ends.
    fn look(s: &mut Schedule) -> Option<Vec<String>> {
        s.next()
    }

    #[test]
    fn the_equalizer_in_use_and_the_effects_reach_the_chain() {
        use nori_core::settings::EqMode;
        let p = StoredPrefs { eq_enabled: true, eq_graphic: vec![3.0; 10], ..StoredPrefs::default() };
        let s = settings(&p).sound;
        assert!(s.graphic.is_empty() && !s.bands.is_empty(), "parametric: the bands play");
        let g = settings(&StoredPrefs { eq_mode: EqMode::Graphic, ..p.clone() }).sound;
        assert!(g.bands.is_empty() && g.graphic == vec![3.0; 10], "graphic: the sliders play, the bands wait");
        assert_eq!(g.preamp_db, -3.0, "the automatic pre-amp pays back the sliders");
        let off = settings(&StoredPrefs { eq_enabled: false, eq_mode: EqMode::Graphic, ..p.clone() }).sound;
        assert!(off.graphic.is_empty() && off.bands.is_empty() && !off.on());
        let fx = settings(&StoredPrefs { volume_boost_db: 4.0, compressor: true, ..StoredPrefs::default() }).sound;
        assert!(fx.on() && fx.effects.boost_db == 4.0 && fx.effects.compressor.is_some() && fx.effects.guard());
    }

    #[test]
    fn the_same_songs_asked_again_are_not_looked_at_again() {
        let mut s = Schedule::default();
        assert!(s.ask(ids(&["a", "b", "c"])), "songs to measure: a thread");
        assert_eq!(look(&mut s), Some(ids(&["a", "b", "c"])));
        assert_eq!(look(&mut s), None, "nothing new: the thread ends");
        assert!(!s.running);
        // Every loading burst, precache and queue event asks again with the same songs.
        for _ in 0..100 {
            assert!(!s.ask(ids(&["a", "b", "c"])), "no thread, no look");
        }
        assert!(s.ask(ids(&["b", "c", "d"])), "the queue moved: a look");
    }

    #[test]
    fn a_song_arriving_is_looked_at_once_even_while_a_look_is_under_way() {
        let mut s = Schedule::default();
        assert!(!s.arrived(), "nothing asked for: nothing to look at");
        assert!(s.ask(ids(&["a", "b"])));
        assert_eq!(look(&mut s), Some(ids(&["a", "b"])));
        // A song arrives while the thread is measuring: no second thread, and the news is kept.
        assert!(!s.arrived());
        assert!(!s.arrived());
        assert_eq!(look(&mut s), Some(ids(&["a", "b"])), "one more look for both arrivals");
        assert_eq!(look(&mut s), None);
        assert!(s.arrived(), "a later arrival starts a thread again");
    }

    #[test]
    fn a_song_is_measured_once_and_one_that_failed_only_again_with_more_of_it() {
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
    fn a_song_measured_before_the_beat_model_is_decoded_once_more_for_it() {
        let mut s = Schedule::default();
        s.ask(ids(&["a", "b"]));
        s.tried("a", 1000, false);
        assert!(!s.worth("a", 1000, false));
        assert!(s.worth("a", 1000, true), "the model has not heard it");
        s.tried("a", 1000, true);
        assert!(!s.worth("a", 1000, true), "heard: never again");
    }
}
