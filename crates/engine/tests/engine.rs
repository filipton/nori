//! The engine end to end on the virtual clock: WAV songs from a counting fake server, a recording card,
//! and `nori_player::sim` as the reference it must match sample for sample. Tests wait with
//! [`Rig::wait_for`] and [`Rig::run`], never real sleeps.

use crate::common;

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{Stepper, Virtual};
use nori_engine::{App, AudioOutput, Body, ByteSource, Config, Device, DeviceWatch, Engine, Event, Feed, Library, Located, OutputFacts, OutputFormat, OutputKind, Recent, Settings, ShallowDepth, Source, State, Store};
use nori_player::automix::analysis::Analyzer;
use nori_player::automix::synth::Rng;
use nori_player::automix::ANALYSIS_VERSION;
use nori_player::engine::{Host, Plan};
use nori_player::playlist::Playlist;
use nori_player::sim::{self, prefs_off, Audio};
use nori_player::transitions::{TransitionPrefs, WindowSong};
use nori_player::types::TrackAnalysis;
use parking_lot::Mutex;

const RATE: u32 = 44_100;

/// Music-like samples unique per seed (partials and noise), made once per length and seed.
fn music(secs: f64, seed: u64) -> Vec<i16> {
    type Made = Vec<((u64, u64), Arc<Vec<i16>>)>;
    static MADE: std::sync::Mutex<Made> = std::sync::Mutex::new(Vec::new());
    let key = (secs.to_bits(), seed);
    if let Some((_, m)) = MADE.lock().unwrap().iter().find(|(k, _)| *k == key) {
        return m.to_vec();
    }
    let m = Arc::new(make_music(secs, seed));
    MADE.lock().unwrap().push((key, m.clone()));
    m.to_vec()
}

fn make_music(secs: f64, seed: u64) -> Vec<i16> {
    let mut r = Rng(seed);
    let hz = [110.0, 331.0, 1250.0].map(|h| h * (1.0 + seed as f64 * 0.01));
    let n = (secs * RATE as f64) as usize;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f64 / RATE as f64;
        let mut v = 0.0;
        for (k, h) in hz.iter().enumerate() {
            v += (std::f64::consts::TAU * h * t).sin() * 0.2 / (k + 1) as f64;
        }
        out.push(((v + 0.02 * r.next()) * 32767.0) as i16);
        out.push(((v * 0.8 - 0.02 * r.next()) * 32767.0) as i16);
    }
    out
}

fn wav(samples: &[i16]) -> Vec<u8> {
    common::wav(RATE, samples)
}

fn wav24(samples: &[i32]) -> Vec<u8> {
    common::wav_bits(RATE, 24, samples)
}

/// A file's bytes, shared between requests.
struct Bytes(Arc<Vec<u8>>);

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// A server with ranges that records each request and its offset.
#[derive(Default)]
struct Server {
    files: Mutex<Vec<(String, Arc<Vec<u8>>)>>,
    requests: Mutex<Vec<(String, u64)>>,
    /// Songs whose answer is delayed this long.
    slow: Mutex<Vec<(String, Duration)>>,
    /// Songs whose connection breaks at this byte, for good.
    cut: Mutex<Vec<(String, u64)>>,
    /// Each request after a song's first takes this long on the clock.
    lag: Mutex<Option<(Virtual, Duration)>>,
}

/// A body that errors at the cut.
struct Broken(Cursor<Bytes>, u64);

impl std::io::Read for Broken {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.1.saturating_sub(self.0.position());
        if left == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset"));
        }
        let n = buf.len().min(left as usize);
        self.0.read(&mut buf[..n])
    }
}

impl ByteSource for Server {
    fn open(&self, url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        let again = {
            let mut asked = self.requests.lock();
            asked.push((url.to_string(), from));
            asked.iter().filter(|(u, _)| u == url).count() > 1
        };
        let lag = self.lag.lock().clone();
        if let Some((clock, d)) = lag.filter(|_| again) {
            clock.wait_until(clock.now_ns() + d.as_nanos() as i64);
        }
        let slow = self.slow.lock().iter().find(|(u, _)| u == url).map(|s| s.1);
        if let Some(d) = slow {
            std::thread::sleep(d);
        }
        let file = self.files.lock().iter().find(|(u, _)| u == url).map(|(_, f)| f.clone()).ok_or("404")?;
        let len = file.len() as u64;
        let mut c = Cursor::new(Bytes(file));
        c.set_position(from);
        if let Some(at) = self.cut.lock().iter().find(|(u, _)| u == url).map(|s| s.1) {
            if from >= at {
                return Err("connection refused".into());
            }
            return Ok(Body { start: from, len: Some(len), reader: Box::new(Broken(c, at)) });
        }
        Ok(Body { start: from, len: Some(len), reader: Box::new(c) })
    }
}

/// A shared playlist the test edits, and the songs skipped on arrival (explicit ones).
struct TestQueue {
    list: Arc<Mutex<Playlist>>,
    skip: Vec<String>,
}

impl nori_engine::Queue for TestQueue {
    fn read<R>(&self, f: impl FnOnce(&Playlist) -> R) -> R {
        f(&self.list.lock())
    }

    fn moved_to(&mut self, index: usize) {
        self.list.lock().moved_to(index);
    }

    fn set_repeat(&mut self, mode: u8) {
        self.list.lock().set_repeat(mode);
    }

    fn skips(&self, index: usize) -> bool {
        let list = self.list.lock();
        self.skip.contains(&list.ids()[index]) && list.next_of(index, list.repeat()).is_some()
    }
}

/// What a test sets up beyond the songs.
#[derive(Default)]
struct Extra {
    float: bool,
    skip: Vec<String>,
    server: Arc<Server>,
    /// A stream cache for the songs.
    store: Option<Arc<Store>>,
    /// Idle release, ms.
    idle_release_ms: Option<i64>,
    /// Music seconds per [`Rig::wait_for`] second (default 20).
    pace: Option<f64>,
    /// The device resizes in place (`AudioOutput::resizes`).
    resizes: bool,
    /// `AudioOutput::shallow_depth` (a Bluetooth output).
    depth: Option<ShallowDepth>,
    /// Memory class, MB (default 256).
    memory_mb: Option<u32>,
    watch: Option<Arc<dyn nori_engine::watch::Watch>>,
}

struct Songs {
    server: Arc<Server>,
    lengths: Vec<(String, i64)>,
    store: Option<Arc<Store>>,
}

impl Library for Songs {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let duration_ms = self.lengths.iter().find(|(i, _)| i == id).map(|s| s.1);
        let bytes: Arc<dyn ByteSource> = self.server.clone();
        let url = id.to_string();
        let source = match &self.store {
            Some(store) => Source::Cached { url, bytes, store: store.clone(), key: format!("{id}:0") },
            None => Source::Url { url, bytes },
        };
        Ok(Located { source, hint: Some("wav".into()), duration_ms, estimated: false })
    }

    fn about(&self, id: &str) -> WindowSong {
        let duration_ms = self.lengths.iter().find(|(i, _)| i == id).map_or(0, |s| s.1);
        WindowSong { id: id.into(), title: id.into(), duration_ms, ..Default::default() }
    }
}

/// The card's puller: what it pulls from and everything it played; `underruns` counts short pulls.
struct Card {
    feed: Option<Feed>,
    /// Pulling began (once a block was there).
    started: bool,
    playing: bool,
    /// Takes float, recorded in `heard_f`.
    float: bool,
    due_ns: i64,
    heard: Arc<Mutex<Vec<i16>>>,
    heard_f: Arc<Mutex<Vec<f32>>>,
    underruns: Arc<AtomicU64>,
    /// Set by a test: the device dies at its next pull, reporting `failure`.
    die: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    block: Vec<i16>,
    floats: Vec<f32>,
}

/// Frames the card pulls at a time.
const BLOCK: usize = 128;

impl common::Device for Card {
    fn due_ns(&self) -> i64 {
        self.due_ns
    }

    fn tick(&mut self, now_ns: i64) -> bool {
        let rate = self.feed.as_ref().map_or(RATE, |f| f.format().rate);
        self.due_ns = now_ns + (BLOCK as i64 * 1_000_000_000) / rate as i64;
        let Some(feed) = self.feed.as_mut() else { return false };
        if self.die.swap(false, Ordering::AcqRel) {
            *self.failure.lock() = Some("the sound server died".into());
            feed.wake_engine();
            self.feed = None;
            return true;
        }
        if !self.playing || (!self.started && feed.available() < BLOCK) {
            return false;
        }
        self.started = true;
        // Short of a block: count an underrun rather than record silence.
        if feed.available() < BLOCK && !feed.ending() {
            self.underruns.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let waits = feed.engine_waits();
        let ch = feed.format().channels;
        if self.float {
            self.floats.resize(BLOCK * ch, 0.0);
            let got = feed.pull(&mut self.floats);
            let n = if feed.ending() { got } else { BLOCK };
            self.heard_f.lock().extend_from_slice(&self.floats[..n * ch]);
        } else {
            self.block.resize(BLOCK * ch, 0);
            let got = feed.pull_i16(&mut self.block);
            let n = if feed.ending() { got } else { BLOCK };
            self.heard.lock().extend_from_slice(&self.block[..n * ch]);
        }
        waits && !feed.engine_waits()
    }
}

/// The recording output ([`Card`]).
struct Recorder {
    card: Arc<Mutex<Card>>,
    /// Opens and closes.
    opened: Arc<AtomicU64>,
    shut: Arc<AtomicU64>,
    /// The engine's device watcher.
    watch: Arc<Mutex<Option<DeviceWatch>>>,
    /// Flushes.
    flushes: Arc<AtomicU64>,
    /// The device is kept shallow (tuning).
    shallow: Arc<AtomicBool>,
    resizes: bool,
    depth: Option<ShallowDepth>,
}

impl AudioOutput for Recorder {
    fn watch(&mut self, changed: DeviceWatch) {
        *self.watch.lock() = Some(changed);
    }

    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
        self.opened.fetch_add(1, Ordering::Relaxed);
        Ok(want)
    }

    fn start(&mut self, feed: Feed) -> Result<(), String> {
        let mut c = self.card.lock();
        c.feed = Some(feed);
        c.started = false;
        Ok(())
    }

    fn pause(&mut self) {
        self.card.lock().playing = false;
    }

    fn resume(&mut self) {
        self.card.lock().playing = true;
    }

    fn latency_us(&self) -> u64 {
        0
    }

    fn takes_float(&mut self) -> bool {
        self.card.lock().float
    }

    fn failed(&mut self) -> Option<String> {
        self.card.lock().failure.lock().take()
    }

    fn flush(&mut self) {
        self.flushes.fetch_add(1, Ordering::Relaxed);
    }

    fn shallow(&mut self, on: bool) {
        self.shallow.store(on, Ordering::Relaxed);
    }

    fn resizes(&self) -> bool {
        self.resizes
    }

    fn shallow_depth(&self) -> Option<ShallowDepth> {
        self.depth
    }

    fn close(&mut self) {
        self.shut.fetch_add(1, Ordering::Relaxed);
        self.card.lock().feed = None;
    }
}

struct Rig {
    engine: Engine,
    time: Stepper<Card>,
    pace: f64,
    opened: Arc<AtomicU64>,
    shut: Arc<AtomicU64>,
    watch: Arc<Mutex<Option<DeviceWatch>>>,
    heard: Arc<Mutex<Vec<i16>>>,
    heard_f: Arc<Mutex<Vec<f32>>>,
    underruns: Arc<AtomicU64>,
    server: Arc<Server>,
    events: Arc<Mutex<Vec<Event>>>,
    die: Arc<AtomicBool>,
    flushes: Arc<AtomicU64>,
    shallow: Arc<AtomicBool>,
    card: Arc<Mutex<Card>>,
    /// The queue: edit, then [`Engine::queue_changed`].
    queue: Arc<Mutex<Playlist>>,
}

impl Rig {
    fn new(songs: &[(&str, &[i16])], prefs: TransitionPrefs, settings: Settings) -> Rig {
        let mut app = sim::App::new();
        app.prefs = prefs;
        Rig::with_app(songs, app, settings)
    }

    fn with_app(songs: &[(&str, &[i16])], app: impl App + Send + 'static, settings: Settings) -> Rig {
        let files = songs.iter().map(|(id, s)| (id.to_string(), wav(s), (s.len() / 2) as i64 * 1000 / RATE as i64)).collect();
        Rig::build(files, app, settings, Extra::default())
    }

    /// Songs as (id, file, length ms).
    fn build(files: Vec<(String, Vec<u8>, i64)>, app: impl App + Send + 'static, settings: Settings, extra: Extra) -> Rig {
        let Extra { float, skip, server, store, idle_release_ms, pace, resizes, depth, memory_mb, watch: watching } = extra;
        for (id, f, _) in &files {
            server.files.lock().push((id.clone(), Arc::new(f.clone())));
        }
        let lengths = files.iter().map(|(id, _, ms)| (id.clone(), *ms)).collect();
        let mut list = Playlist::default();
        list.set(files.iter().map(|(id, _, _)| id.clone()).collect(), Some(0), false, 0);
        let list = Arc::new(Mutex::new(list));
        let queue = TestQueue { list: list.clone(), skip };
        let heard = Arc::new(Mutex::new(Vec::new()));
        let heard_f = Arc::new(Mutex::new(Vec::new()));
        let underruns = Arc::new(AtomicU64::new(0));
        let die = Arc::new(AtomicBool::new(false));
        let card = Arc::new(Mutex::new(Card {
            feed: None,
            started: false,
            playing: false,
            float,
            due_ns: 0,
            heard: heard.clone(),
            heard_f: heard_f.clone(),
            underruns: underruns.clone(),
            die: die.clone(),
            failure: Arc::default(),
            block: Vec::new(),
            floats: Vec::new(),
        }));
        let out = Recorder { card: card.clone(), opened: Arc::default(), shut: Arc::default(), watch: Arc::default(), flushes: Arc::default(), shallow: Arc::default(), resizes, depth };
        let (opened, shut, watch, flushes, shallow) = (out.opened.clone(), out.shut.clone(), out.watch.clone(), out.flushes.clone(), out.shallow.clone());
        let clock = Virtual::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let library = Songs { server: server.clone(), lengths, store };
        let mut config = Config { memory_mb: memory_mb.unwrap_or(256), settings, watch: watching.map(nori_engine::watch::Watcher), ..Config::default() };
        config.idle_release_ms = idle_release_ms.unwrap_or(config.idle_release_ms);
        let engine = Engine::start_on(library, app, queue, Box::new(out), None, config, clock.clone(), move |e| seen.lock().push(e));
        let time = Stepper::new(clock, card.clone());
        Rig { engine, time, pace: pace.unwrap_or(20.0), opened, shut, watch, heard, heard_f, underruns, server, events, die, flushes, shallow, card, queue: list }
    }

    /// Underruns so far.
    fn waits(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Runs until `done`, at most `secs` scaled by [`Extra::pace`].
    fn wait_for(&self, secs: u64, mut done: impl FnMut(&Rig) -> bool) -> bool {
        self.time.until(Duration::from_secs_f64(secs as f64 * self.pace), || done(self))
    }

    /// Runs for `ms`.
    fn run(&self, ms: u64) {
        self.time.run(Duration::from_millis(ms));
    }

    /// The clock, ms.
    fn now_ms(&self) -> i64 {
        self.time.clock.now_ns() / 1_000_000
    }

    fn ended(&self) -> bool {
        self.events.lock().contains(&Event::State(State::Ended))
    }
}

/// What `sim::Player` plays of the same songs.
fn reference(songs: &[(&str, &[i16])], prefs: TransitionPrefs) -> Vec<i16> {
    let tracks = songs.iter().map(|(id, s)| sim::Track::new(id, Audio::pcm(RATE, 2, s))).collect();
    let mut p = sim::Player::with_prefs(tracks, prefs);
    p.play_from(0);
    assert!(p.run_to_end(600_000));
    p.sink.heard_samples()
}

fn crossfade(secs: i32) -> TransitionPrefs {
    TransitionPrefs { crossfade_s: secs, keep_albums: true, ..prefs_off() }
}

#[test]
fn jump_or_seek_while_paused_fetches_nothing_until_play() {
    let (a, b, c) = (music(20.0, 91), music(20.0, 92), music(20.0, 93));
    let rig = Rig::new(&[("a", &a), ("b", &b), ("c", &c)], prefs_off(), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    rig.engine.pause();
    assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
    let asked = rig.server.requests.lock().len();
    rig.engine.go_to(1, 0);
    rig.engine.seek(5_000);
    assert!(rig.wait_for(5, |r| { let s = r.engine.status(); s.index == Some(1) && s.position_ms == 5_000 }), "the screen is told the place at once: {:?}", rig.engine.status());
    rig.run(6_000);
    assert_eq!(rig.engine.status().state, State::Paused, "still paused");
    assert_eq!(rig.server.requests.lock().len(), asked, "nothing fetched for a place nobody listens to yet");
    let heard = rig.heard.lock().len();
    rig.engine.play();
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > heard + 2 * RATE as usize), "play goes there");
    let played = rig.heard.lock()[heard..heard + 2 * RATE as usize].to_vec();
    let from = 5 * RATE as usize * 2;
    assert!(b[from..from + 2 * RATE as usize] == played[..], "b from five seconds in");
    // Paused, a skip also plays.
    rig.engine.pause();
    assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
    rig.engine.next();
    assert!(rig.wait_for(5, |r| { let s = r.engine.status(); s.state == State::Playing && s.index == Some(2) }), "{:?}", rig.engine.status());
}

#[test]
fn pause_at_end_waits_on_next_song() {
    let (a, b) = (music(30.0, 95), music(4.0, 96));
    let rig = Rig::new(&[("a", &a), ("b", &b)], crossfade(2), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| !r.heard.lock().is_empty()));
    rig.engine.pause_at_end(true);
    assert!(rig.wait_for(10, |r| r.events.lock().iter().any(|e| matches!(e, Event::Stopped { .. }))), "{:?}", rig.events.lock());
    assert!(rig.wait_for(5, |r| { let s = r.engine.status(); s.state == State::Paused && s.index == Some(1) && s.position_ms == 0 }), "{:?}", rig.engine.status());
    let heard = rig.heard.lock().clone();
    // The recorder may miss a frame as the output pauses.
    assert!(heard.len() <= a.len() && heard.len() + 8 >= a.len() && heard[..] == a[..heard.len()], "a to its end, nothing of b and no mix into it: {} of {} samples", heard.len(), a.len());
    rig.engine.play();
    assert!(rig.wait_for(10, Rig::ended), "play goes on with b");
    assert!(rig.heard.lock()[heard.len()..] == b[..], "b from its start");
}

#[test]
fn slow_song_reports_buffering() {
    let a = music(10.0, 81);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let extra = Extra::default();
    extra.server.slow.lock().push(("a".into(), Duration::from_millis(1_500)));
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize), "the song plays in the end");
    assert!(rig.wait_for(5, |r| r.events.lock().contains(&Event::Buffering(false))), "{:?}", rig.events.lock());
    let events = rig.events.lock().clone();
    let on = events.iter().position(|e| *e == Event::Buffering(true)).unwrap_or_else(|| panic!("said while it waits: {events:?}"));
    let off = events.iter().position(|e| *e == Event::Buffering(false)).unwrap_or_else(|| panic!("and when it comes: {events:?}"));
    assert!(on < off, "{events:?}");
}

#[test]
fn dead_device_stops_and_play_reopens() {
    let a = music(90.0, 71);
    let rig = Rig::new(&[("a", &a)], prefs_off(), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2), "music is heard");
    rig.die.store(true, Ordering::Release);
    let stopped = |r: &Rig| {
        let e = r.events.lock();
        e.iter().any(|e| matches!(e, Event::Error { message, .. } if message.contains("the output stopped: the sound server died"))) && e.last() == Some(&Event::State(State::Idle))
    };
    assert!(rig.wait_for(10, stopped), "the engine stops and says so: {:?}", rig.events.lock());
    assert!(rig.wait_for(5, |r| r.shut.load(Ordering::Relaxed) >= 1), "and lets the dead device go");
    let (opened, heard) = (rig.opened.load(Ordering::Relaxed), rig.heard.lock().len());
    rig.engine.play();
    assert!(rig.wait_for(10, |r| r.opened.load(Ordering::Relaxed) > opened && r.heard.lock().len() > heard + RATE as usize), "play opens another and the music goes on");
}

#[test]
fn gapless_join_and_one_fetch_each() {
    let whole = music(50.0, 1);
    let cut = RATE as usize * 2 * 23 + 2 * 317;
    let songs: [(&str, &[i16]); 2] = [("a", &whole[..cut]), ("b", &whole[cut..])];
    let rig = Rig::new(&songs, prefs_off(), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "played to the end: {:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert_eq!(heard.len(), whole.len(), "not a sample more or less ({} waits)", rig.waits());
    assert!(heard == whole, "the join is exact");
    // a's and b's loaders start together; either may ask first.
    let mut requests = rig.server.requests.lock().clone();
    requests.sort();
    assert_eq!(requests, vec![("a".to_string(), 0), ("b".to_string(), 0)], "one request per song, the whole song in one burst");
    let events = rig.events.lock().clone();
    assert!(events.iter().any(|e| matches!(e, Event::Song { index: 1, id, .. } if id == "b")), "{events:?}");
}

#[test]
fn crossfade_matches_simulation() {
    let (a, b) = (music(40.0, 2), music(40.0, 3));
    let songs: [(&str, &[i16]); 2] = [("a", &a), ("b", &b)];
    let expected = reference(&songs, crossfade(6));
    let rig = Rig::new(&songs, crossfade(6), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "played to the end: {:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert_eq!(heard.len(), expected.len(), "80 s less the 6 s overlap ({} waits)", rig.waits());
    assert_eq!(heard.len(), a.len() + b.len() - RATE as usize * 2 * 6);
    let first = heard.iter().zip(&expected).position(|(x, y)| x != y);
    assert_eq!(first, None, "the mix starts at the planned sample and sounds the same");
}

#[test]
fn seek_is_sample_exact() {
    let a = music(30.0, 4);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let rig = Rig::new(&songs, prefs_off(), Settings::default());
    rig.engine.play_at(0, 20_000);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert!(heard[..] == a[RATE as usize * 2 * 20..], "from 20 s to the end");
}

#[test]
fn seek_reports_switching_during_dip() {
    let a = music(30.0, 22);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let rig = Rig::new(&songs, prefs_off(), Settings { fade_ms: 400, ..Settings::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    rig.engine.seek(20_000);
    assert!(rig.wait_for(5, |r| r.engine.status().switching), "the dip is on");
    assert!(rig.engine.status().position_ms < 5_000, "and the place is still the one before the seek");
    assert!(rig.wait_for(5, |r| !r.engine.status().switching && r.engine.status().position_ms >= 20_000), "then it is the one asked for");
}

#[test]
fn pause_and_resume_keep_place() {
    let a = music(30.0, 5);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let rig = Rig::new(&songs, prefs_off(), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 5));
    rig.engine.pause();
    assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
    assert!(!rig.events.lock().iter().any(|e| matches!(e, Event::Stopped { .. })), "a pause asked for is not one the engine made by itself");
    rig.run(2_000);
    let at = rig.heard.lock().len();
    rig.run(6_000);
    assert_eq!(rig.heard.lock().len(), at, "nothing plays while paused");
    // The place is where playback stopped, though the engine slept before.
    let heard_ms = (at / 2) as i64 * 1000 / RATE as i64;
    let place = rig.engine.status().position_now();
    assert!((place - heard_ms).abs() < 300, "paused at {place} ms, the ear at {heard_ms} ms");
    rig.engine.play();
    assert!(rig.wait_for(30, Rig::ended));
    let heard = rig.heard.lock().clone();
    assert!(heard == a, "every sample once, in order");
}

/// The status is as old as the last wake; `look` refreshes it at once.
#[test]
fn look_refreshes_place_between_bursts() {
    let a = vec![8000i16; RATE as usize * 2 * 90];
    let rig = Rig::new(&[("a", &a)], prefs_off(), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 5));
    let heard_ms = |r: &Rig| (r.heard.lock().len() / 2) as i64 * 1000 / RATE as i64;
    // Between two wakes the status lags a second or more.
    let mut behind = 0;
    for _ in 0..400 {
        rig.run(50);
        behind = heard_ms(&rig) - rig.engine.status().position_ms;
        if behind >= 1_000 {
            break;
        }
    }
    assert!(behind >= 1_000, "the engine sleeps between bursts: {behind} ms behind at most");
    rig.engine.look();
    // Within 20 ms, not at the next burst.
    assert!(rig.time.until(Duration::from_millis(20), || (heard_ms(&rig) - rig.engine.status().position_ms).abs() < 100), "the ear at {} ms, the status {:?}", heard_ms(&rig), rig.engine.status());
    assert_eq!(rig.engine.status().state, State::Playing, "and nothing else changed");
}

#[test]
fn pause_and_play_fade() {
    let a = vec![8000i16; RATE as usize * 2 * 20];
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let rig = Rig::new(&songs, prefs_off(), Settings { fade_ms: 400, ..Settings::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 4));
    rig.engine.pause();
    assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
    rig.run(4_000);
    let at = rig.heard.lock().len();
    let heard = rig.heard.lock().clone();
    // Faded, not cut: quiet at the end, with levels between full and silent.
    assert!(heard[at - 2].abs() < 100, "faded to silence: {}", heard[at - 2]);
    assert!(heard.iter().any(|&v| v > 2000 && v < 6000), "a ramp down, not a cut");
    rig.engine.play();
    assert!(rig.wait_for(30, Rig::ended));
    let heard = rig.heard.lock().clone();
    assert!(heard[at..at + 200].iter().all(|&v| v < 8000), "back in from silence");
    assert_eq!(*heard.last().unwrap(), 8000);
}

/// A new queue started at the same index says its song, playing and paused.
#[test]
fn new_queue_at_same_index_says_song() {
    for paused in [false, true] {
        let (a, b, c) = (music(30.0, 61), music(20.0, 62), music(20.0, 63));
        let rig = Rig::new(&[("a", &a), ("b", &b), ("c", &c)], prefs_off(), Settings::default());
        rig.queue.lock().set(vec!["a".into()], Some(0), false, 0);
        rig.engine.queue_changed();
        rig.engine.play_at(0, 0);
        assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 3));
        if paused {
            rig.engine.pause();
            assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
        }
        let from = rig.events.lock().len();
        rig.queue.lock().set(vec!["b".into(), "c".into()], Some(0), false, 0);
        rig.engine.queue_changed();
        let jump = rig.engine.go_to(0, 0);
        let said = |r: &Rig| r.events.lock()[from..].iter().any(|e| matches!(e, Event::Song { index: 0, id, jumps } if id == "b" && *jumps >= jump));
        assert!(rig.wait_for(5, said), "paused {paused}: {:?}", &rig.events.lock()[from..]);
        assert_eq!(rig.engine.status().id.as_deref(), Some("b"), "paused {paused}");
        rig.engine.play();
        rig.run(2_000);
        let songs = rig.events.lock()[from..].iter().filter(|e| matches!(e, Event::Song { .. })).count();
        assert_eq!(songs, 1, "said once, paused {paused}: {:?}", &rig.events.lock()[from..]);
        rig.engine.stop();
    }
}

/// A song chosen during a pause fade is the one play brings.
#[test]
fn song_chosen_during_pause_fade_plays() {
    let a = vec![8000i16; RATE as usize * 2 * 20];
    let b = vec![-8000i16; RATE as usize * 2 * 20];
    let rig = Rig::new(&[("a", &a), ("b", &b)], prefs_off(), Settings { fade_ms: 400, ..Settings::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 3));
    rig.engine.pause();
    rig.engine.go_to(1, 0);
    rig.engine.play();
    let at = rig.heard.lock().len();
    rig.run(3_000);
    let heard = rig.heard.lock().clone();
    assert_eq!(rig.engine.status().index, Some(1), "{:?}", rig.events.lock());
    assert_eq!(*heard.last().unwrap(), -8000, "b is heard, not a again");
    assert!(heard.len() > at + RATE as usize * 2, "and it plays");
}

/// Buffering(true) is ended when the music stops waiting another way (held elsewhere, released).
#[test]
fn buffering_ends_when_music_moves_elsewhere() {
    let (a, b) = (music(20.0, 71), music(20.0, 72));
    let songs: [(&str, &[i16]); 2] = [("a", &a), ("b", &b)];
    let extra = Extra::default();
    extra.server.slow.lock().push(("a".into(), Duration::from_secs(60)));
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(5, |r| r.events.lock().contains(&Event::Buffering(true))), "{:?}", rig.events.lock());
    rig.engine.pause();
    rig.engine.go_to(1, 0);
    assert!(rig.wait_for(5, |r| r.events.lock().contains(&Event::Buffering(false))), "{:?}", rig.events.lock());
    rig.engine.stop();
}

/// Headphones out: pause at once, whatever the fade setting.
#[test]
fn pause_now_cuts_at_once() {
    let a = vec![8000i16; RATE as usize * 2 * 20];
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let rig = Rig::new(&songs, prefs_off(), Settings { fade_ms: 1_000, ..Settings::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 4));
    rig.engine.pause_now();
    assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
    rig.run(4_000);
    let heard = rig.heard.lock().clone();
    assert!(heard.iter().all(|&v| v == 8000), "cut, not faded: no sample on the way down");
    rig.engine.stop();
}

/// Headphones out during a pause fade stop it there.
#[test]
fn pause_now_cuts_fade_short() {
    let a = vec![8000i16; RATE as usize * 2 * 20];
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let rig = Rig::new(&songs, prefs_off(), Settings { fade_ms: 3_000, ..Settings::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 4));
    rig.engine.pause();
    let from = rig.heard.lock().len();
    rig.run(300);
    rig.engine.pause_now();
    rig.run(4_000);
    let heard = rig.heard.lock().len() - from;
    assert!(heard < RATE as usize * 2 * 2, "stopped well before the 3 s fade was over: {} ms", heard / 2 * 1000 / RATE as usize);
}

#[test]
fn replay_gain_per_song() {
    let (a, b) = (music(12.0, 7), music(12.0, 8));
    let songs: [(&str, &[i16]); 2] = [("a", &a), ("b", &b)];
    let mut app = sim::App::new();
    app.gains.insert("a".into(), 0.5);
    let rig = Rig::with_app(&songs, app, Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert_eq!(heard.len(), a.len() + b.len(), "the volume leaves the timing alone");
    // The level changes on b's first sample; turned down, a is dithered back to 16 bits.
    let off = heard[..a.len()].iter().zip(&a).position(|(h, s)| (*h as i32 - (*s as f64 / 2.0).round() as i32).abs() > 1);
    assert_eq!(off, None, "a at half its level, -6 dB");
    assert!(heard[a.len()..] == b[..], "b untouched at full volume");
}

fn rms(s: &[i16]) -> f64 {
    (s.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / s.len().max(1) as f64).sqrt()
}

#[test]
fn replay_gain_boost_uses_floats_and_limiter() {
    // Peaks near 0.4; a turned up 9 dB (to 1.1), b down 6 dB.
    let (a, b) = (music(12.0, 51), music(12.0, 52));
    let up = 10f32.powf(9.0 / 20.0);
    let mut app = sim::App::new();
    app.gains.insert("a".into(), up);
    app.gains.insert("b".into(), 0.5);
    let rig = Rig::with_app(&[("a", &a), ("b", &b)], app, Settings { gain_boost_db: 9.0, ..Settings::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert!(rig.engine.status().chain, "the limiter is in the path");
    let heard = rig.heard.lock().clone();
    // The limiter's look-ahead delays the music by 5 ms.
    let delay = 220 * 2;
    assert_eq!(heard.len(), a.len() + b.len() + delay);
    let ceiling = 10f64.powf(-1.0 / 20.0) * 32768.0;
    let peak = heard.iter().map(|v| (*v as f64).abs()).fold(0.0, f64::max);
    assert!(peak <= ceiling + 1.0, "nothing past the -1 dB ceiling: {peak} of {ceiling}");
    let (ha, hb) = (&heard[delay..delay + a.len()], &heard[delay + a.len()..]);
    let louder = 20.0 * (rms(ha) / rms(&a)).log10();
    assert!(louder > 7.0 && louder < 9.1, "a {louder:.2} dB louder: the 9 asked, less what the limiter took off its peaks");
    let quieter = 20.0 * (rms(hb) / rms(&b)).log10();
    assert!((quieter + 6.02).abs() < 0.05, "b 6 dB quieter: {quieter:.2}");
    // Without a boost cap, a plays at its own level.
    let mut app = sim::App::new();
    app.gains.insert("a".into(), up);
    let rig = Rig::with_app(&[("a", &a)], app, Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert!(*rig.heard.lock() == a, "every sample as it is");
}

/// 24-bit music: 16-bit music with its own low byte.
fn music24(secs: f64, seed: u64) -> Vec<i32> {
    music(secs, seed).iter().enumerate().map(|(i, v)| ((*v as i32) << 8) | (i as i32 * 37 & 0xFF)).collect()
}

fn loud_eq() -> Settings {
    let bands = vec![nori_player::dsp::Band { kind: nori_player::dsp::PEAKING, freq: 1000.0, gain_db: 6.0, q: 1.0, channel: 0 }];
    Settings { sound: nori_engine::Sound { bands, ..Default::default() }, ..Settings::default() }
}

#[test]
fn hi_res_keeps_24_bits() {
    let a = music24(8.0, 9);
    let files = vec![("a".to_string(), wav24(&a), 8_000)];
    // Nothing in the chain: samples pass as they are.
    let rig = Rig::build(files, sim::App::new(), Settings { hi_res: true, ..Settings::default() }, Extra { float: true, ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard_f.lock().clone();
    assert_eq!(heard.len(), a.len());
    let off = heard.iter().zip(&a).position(|(h, s)| *h != *s as f32 / 8_388_608.0);
    assert_eq!(off, None, "every one of the 24 bits, in float");
}

/// Hi-res with the equalizer runs the chain on all 24 bits (the 16-bit chain lost the low byte).
#[test]
fn hi_res_equalizer_on_24_bits() {
    let a = music24(8.0, 9);
    let files = vec![("a".to_string(), wav24(&a), 8_000)];
    let rig = Rig::build(files, sim::App::new(), Settings { hi_res: true, ..loud_eq() }, Extra { float: true, ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard_f.lock().clone();
    assert!(rig.engine.status().chain, "{:?}", rig.engine.status());
    // The chain over 24 bits, and over the song cut to 16 bits.
    let chain = |x: &[f32]| {
        let mut eq = nori_player::dsp::Equalizer::new(RATE, 2);
        loud_eq().sound.apply(&mut eq);
        let mut y = vec![0f32; x.len()];
        eq.process_f32(x, &mut y);
        y
    };
    let want = chain(&a.iter().map(|s| *s as f32 / 8_388_608.0).collect::<Vec<_>>());
    let cut = chain(&a.iter().map(|s| (*s as f32 / 256.0).round_ties_even() / 32768.0).collect::<Vec<_>>());
    assert_eq!(heard.len(), want.len());
    let worst = |x: &[f32]| x.iter().zip(&heard).map(|(w, h)| (w - h).abs()).fold(0.0f32, f32::max) * 8_388_608.0;
    assert!(worst(&want) <= 1.0, "to the 24-bit step: {} steps off", worst(&want));
    assert!(worst(&cut) > 64.0, "the low byte is heard: {} 24-bit steps from the 16-bit song's", worst(&cut));
}

fn wav_at(samples: &[i16], rate: u32) -> Vec<u8> {
    common::wav(rate, samples)
}

/// A gapless join into another rate reopens the device at that rate instead of resampling.
#[test]
fn gapless_rate_change_reopens_device() {
    let (a, b) = (music(6.0, 51), music(6.0, 52));
    let files = vec![("a".to_string(), wav(&a), 6_000), ("b".to_string(), wav_at(&b, 48_000), (b.len() / 2) as i64 * 1000 / 48_000)];
    let rig = Rig::build(files, sim::App::new(), Settings::default(), Extra::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert_eq!(rig.opened.load(Ordering::Relaxed), 2, "opened again for b");
    assert_eq!(rig.card.lock().feed.as_ref().map(|f| f.format().rate), Some(48_000));
    assert!(heard[..a.len()] == a[..], "a whole");
    assert!(heard[a.len()..] == b[..], "b sample for sample, at 48 kHz");
}

/// With a 48 kHz maximum, 96 kHz is halved at its own pitch and level; 44.1 kHz passes.
#[test]
fn max_rate_converts_within_family() {
    let secs = 3.0;
    let tone: Vec<i16> = (0..(96_000.0 * secs) as usize).flat_map(|i| [((i as f64 * 1000.0 * std::f64::consts::TAU / 96_000.0).sin() * 16000.0).round() as i16; 2]).collect();
    let files = vec![("hi".to_string(), wav_at(&tone, 96_000), 3_000)];
    let rig = Rig::build(files, sim::App::new(), Settings { max_rate: 48_000, ..Settings::default() }, Extra::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert_eq!(rig.card.lock().feed.as_ref().map(|f| f.format().rate), Some(48_000), "opened at 48 kHz");
    let heard: Vec<f64> = rig.heard.lock().chunks_exact(2).map(|c| c[0] as f64).collect();
    assert!((heard.len() as f64 - 48_000.0 * secs).abs() < 200.0, "three seconds at 48 kHz: {} frames", heard.len());
    // Fitted over 1500 cycles: level kept, only 16-bit rounding left.
    let mid = &heard[12_000..12_000 + 48 * 1_500];
    let w = 1000.0 * std::f64::consts::TAU / 48_000.0;
    let (mut s, mut c) = (0.0, 0.0);
    for (i, v) in mid.iter().enumerate() {
        s += v * (w * i as f64).sin();
        c += v * (w * i as f64).cos();
    }
    let amp = 2.0 * (s * s + c * c).sqrt() / mid.len() as f64;
    assert!((amp - 16000.0).abs() < 16.0, "at its level: {amp:.1}");
    let (a, b) = ((s * 2.0 / mid.len() as f64), (c * 2.0 / mid.len() as f64));
    let resid = (mid.iter().enumerate().map(|(i, v)| (v - a * (w * i as f64).sin() - b * (w * i as f64).cos()).powi(2)).sum::<f64>() / mid.len() as f64).sqrt();
    assert!(resid < 1.0, "the tone and the rounding: {resid:.2} left");

    let a = music(4.0, 53);
    let rig = Rig::build(vec![("a".to_string(), wav(&a), 4_000)], sim::App::new(), Settings { max_rate: 48_000, ..Settings::default() }, Extra::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert!(*rig.heard.lock() == a, "44.1 kHz, under the maximum: as it is");
}

#[test]
fn device_16_bit_gets_16_bit_chain() {
    let a = music24(8.0, 10);
    let files = vec![("a".to_string(), wav24(&a), 8_000)];
    let rig = Rig::build(files, sim::App::new(), Settings { hi_res: true, ..Settings::default() }, Extra::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    let expected: Vec<i16> = a.iter().map(|v| (*v as f32 / 256.0).round_ties_even() as i16).collect();
    assert!(heard == expected, "rounded to 16 bits as the decoder rounds");
    assert!(rig.heard_f.lock().is_empty());
}

fn files(songs: &[(&str, &[i16])]) -> Vec<(String, Vec<u8>, i64)> {
    songs.iter().map(|(id, s)| (id.to_string(), wav(s), (s.len() / 2) as i64 * 1000 / RATE as i64)).collect()
}

#[test]
fn failed_connection_reported_and_skipped() {
    let (a, b) = (music(10.0, 11), music(6.0, 12));
    let songs: [(&str, &[i16]); 2] = [("a", &a), ("b", &b)];
    let extra = Extra::default();
    // Four seconds of a come, then the connection breaks for good.
    extra.server.cut.lock().push(("a".into(), 44 + RATE as u64 * 4 * 4));
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(40, Rig::ended), "{:?}", rig.events.lock());
    let events = rig.events.lock().clone();
    assert!(events.iter().any(|e| matches!(e, Event::Error { id, .. } if id == "a")), "{events:?}");
    let heard = rig.heard.lock().clone();
    // All but the packet being read when the bytes stopped.
    let four = RATE as usize * 2 * 39 / 10;
    assert!(heard[..four] == a[..four], "what came of a played: {} heard, {:?}", heard.len(), heard.iter().zip(&a).position(|(h, s)| h != s));
    assert!(heard[heard.len() - b.len()..] == b[..], "then b, whole");
}

#[test]
fn loading_song_does_not_block_engine() {
    let (slow, a) = (music(6.0, 13), music(6.0, 14));
    let songs: [(&str, &[i16]); 2] = [("slow", &slow), ("a", &a)];
    let extra = Extra::default();
    // Slower than the test's limit: an engine that waited would fail.
    extra.server.slow.lock().push(("slow".into(), Duration::from_secs(60)));
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    rig.engine.next();
    assert!(rig.wait_for(40, Rig::ended), "next was taken while slow was opening: {:?}", rig.events.lock());
    assert!(*rig.heard.lock() == a, "a, whole");
}

#[test]
fn skipped_explicit_song_never_heard() {
    let (a, x, b) = (music(5.0, 15), music(5.0, 16), music(5.0, 17));
    let songs: [(&str, &[i16]); 3] = [("a", &a), ("x", &x), ("b", &b)];
    let extra = Extra { skip: vec!["x".into()], ..Extra::default() };
    let server = extra.server.clone();
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert!(*rig.heard.lock() == [a.clone(), b.clone()].concat(), "a joined straight to b");
    assert!(!server.requests.lock().iter().any(|(u, _)| u == "x"), "x not even fetched");
}

#[test]
fn cached_song_replays_without_network() {
    let a = music(8.0, 18);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let dir = nori_testdir::TempDir::new("cache");
    let store = Store::open(dir.path(), 64 << 20, Box::new(Recent::default())).unwrap();
    let first = Extra { store: Some(store.clone()), ..Extra::default() };
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), first);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert_eq!(rig.server.requests.lock().len(), 1);
    drop(rig);
    assert!(store.cached("a:0").is_some(), "kept as it loaded");
    let again = Extra { store: Some(store.clone()), ..Extra::default() };
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), again);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert!(rig.server.requests.lock().is_empty(), "{:?}", rig.server.requests.lock());
    assert!(*rig.heard.lock() == a, "the same song, from the disk");
}

#[test]
fn idle_release_and_reopen() {
    let a = music(12.0, 19);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let extra = Extra { idle_release_ms: Some(300), ..Extra::default() };
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 3));
    rig.engine.pause();
    assert!(rig.wait_for(5, |r| r.shut.load(Ordering::Relaxed) == 1), "let go after the idle time");
    assert_eq!(rig.engine.status().releases, 1);
    assert_eq!(rig.engine.status().state, State::Paused);
    rig.engine.play();
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert_eq!(rig.opened.load(Ordering::Relaxed), 2, "opened again on play");
    let heard = rig.heard.lock().clone();
    // Resumed at the millisecond it stopped.
    let again = heard.len() - a.len();
    assert!(again <= RATE as usize * 2 / 1000, "{again} samples heard again");
    let (head, tail) = (RATE as usize * 2 * 3, a.len() - RATE as usize * 2 * 6);
    assert!(heard[..head] == a[..head] && heard[heard.len() - tail..] == a[a.len() - tail..], "from the start, and on to the end");
}

#[test]
fn device_gets_own_sound() {
    let a = vec![8000i16; RATE as usize * 2 * 16];
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let mut app = sim::App::new();
    // Headphones with a -6 dB profile.
    let quieter = nori_engine::Sound { preamp_db: -6.0206, ..Default::default() };
    app.device_sounds.insert("Wired headphones".into(), quieter);
    let rig = Rig::with_app(&songs, app, Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2));
    // Paused, so the new chain applies at once.
    rig.engine.pause();
    assert!(rig.wait_for(10, |r| r.engine.status().state == State::Paused));
    let at = rig.heard.lock().len();
    let watch = rig.watch.lock();
    watch.as_ref().expect("the engine watches the output")(Device { kind: OutputKind::Wired, name: "Jack".into() });
    drop(watch);
    assert!(rig.wait_for(10, |r| r.events.lock().contains(&Event::Output { name: "Wired headphones".into() })));
    rig.engine.play();
    assert!(rig.wait_for(60, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert!(heard[..at].iter().all(|&v| v == 8000), "the speaker's sound up to the pause");
    let off = heard[at..].iter().position(|&v| (v - 4000).abs() > 1);
    assert_eq!(off, None, "the headphones' sound from then on");
}

#[test]
fn id3_tag_is_skipped() {
    // A cover-sized tag full of valid-looking MPEG layer I frames.
    let frames: Vec<u8> = (0..40_000).flat_map(|_| [[0xff, 0xff, 0x10, 0x00].as_slice(), &[0; 28]].concat()).collect();
    let syncsafe = |n: usize| [(n >> 21) as u8 & 0x7f, (n >> 14) as u8 & 0x7f, (n >> 7) as u8 & 0x7f, n as u8 & 0x7f];
    let a = music(1.0, 1);
    let mut file = [b"ID3".as_slice(), &[3, 0, 0], &syncsafe(frames.len())].concat();
    file.extend_from_slice(&frames);
    file.extend_from_slice(&wav(&a));
    let mut d = nori_engine::demux::Demuxed::open(Box::new(Cursor::new(file)), None, 0, None, nori_player::pcm::Encoding::Pcm16).unwrap();
    let mut out = Vec::new();
    while nori_player::pipeline::Reading::fill(&mut d) {
        out.extend(nori_player::pipeline::Reading::buffer(&d).chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
    }
    assert!(out == a, "the song after the tag, sample for sample: {} of {} samples", out.len(), a.len());
}

/// A stop from before a later play is superseded, and that play retries the song. Regression: a client
/// honouring the stale stop showed paused over playing music.
#[test]
fn stop_before_later_play_is_superseded() {
    let (a, b) = (music(20.0, 23), music(6.0, 24));
    let songs: [(&str, &[i16]); 2] = [("a", &a), ("b", &b)];
    let extra = Extra::default();
    extra.server.cut.lock().push(("b".into(), 0));
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2));
    rig.engine.go_to(1, 0);
    let first = rig.engine.play();
    assert!(rig.wait_for(30, |r| r.events.lock().iter().any(|e| matches!(e, Event::Stopped { .. }))), "{:?}", rig.events.lock());
    let stop = rig.events.lock().iter().find(|e| matches!(e, Event::Stopped { .. })).cloned().expect("said");
    assert_eq!(stop, Event::Stopped { plays: first }, "said after the tap's play was taken");
    assert!(!rig.engine.superseded(&stop), "no play asked for since: the stop stands");
    // A second play sent before the stop was read; the network is back.
    rig.server.cut.lock().clear();
    let second = rig.engine.play();
    assert!(second > first);
    assert!(rig.engine.superseded(&stop), "a play was asked for after it: the stop is over");
    assert!(rig.wait_for(20, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert!(heard.len() >= b.len() && heard[heard.len() - b.len()..] == b[..], "b, whole, after the stop");
    let events = rig.events.lock().clone();
    let at = events.iter().position(|e| *e == stop).expect("said");
    assert!(events[at..].contains(&Event::State(State::Playing)), "playing again after the stop: {events:?}");
    // A stop after the last play is not superseded.
    assert!(!rig.engine.superseded(&Event::Stopped { plays: second }));
    assert!(!rig.engine.superseded(&Event::Bridge { plays: second }));
}

#[test]
fn play_retries_failed_song() {
    let a = music(3.0, 21);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let extra = Extra::default();
    extra.server.cut.lock().push(("a".into(), 0));
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, |r| r.events.lock().iter().any(|e| matches!(e, Event::Error { id, .. } if id == "a"))), "{:?}", rig.events.lock());
    assert!(rig.wait_for(10, |r| r.engine.status().state == State::Paused), "stopped there: {:?}", rig.events.lock());
    assert!(rig.events.lock().iter().any(|e| matches!(e, Event::Stopped { .. })), "and says it stopped by itself: {:?}", rig.events.lock());
    rig.server.cut.lock().clear();
    rig.engine.play();
    assert!(rig.wait_for(20, Rig::ended), "{:?}", rig.events.lock());
    assert!(*rig.heard.lock() == a, "a, whole, not the clock run over nothing");
}

/// `s` scaled by `gain`, rounded as the player rounds.
fn at(s: &[i16], gain: f32) -> Vec<i16> {
    s.iter().map(|&v| (v as f32 * gain).round() as i16).collect()
}

/// The first sample of `heard` further than dither allows from `ideal`.
fn beyond_dither(heard: &[i16], ideal: &[i16]) -> Option<usize> {
    heard.iter().zip(ideal).position(|(h, i)| (*h as i32 - *i as i32).abs() > 2)
}

#[test]
fn replay_gain_per_song_through_crossfade() {
    let (a, b) = (music(40.0, 2), music(40.0, 3));
    // Each song at its own gain, then mixed. Regression: one output volume put a's gain on b.
    let ideal = reference(&[("a", &at(&a, 0.5)), ("b", &b)], crossfade(6));
    let mut app = sim::App::new();
    app.prefs = crossfade(6);
    app.gains.insert("a".into(), 0.5);
    let rig = Rig::with_app(&[("a", &a), ("b", &b)], app, Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert_eq!(heard.len(), ideal.len());
    assert_eq!(beyond_dither(&heard, &ideal), None, "every sample as a at half its level mixed into b");
}

// ---- settings changed while music plays ----

/// A shared `sim::App` the test changes while the engine plays.
#[derive(Clone)]
struct Live(Arc<Mutex<sim::App>>);

impl Live {
    fn new(prefs: TransitionPrefs) -> Live {
        let mut app = sim::App::new();
        app.prefs = prefs;
        Live(Arc::new(Mutex::new(app)))
    }
}

impl Host for Live {
    fn plan_for(&mut self, outgoing_id: &str) -> Option<Plan> {
        self.0.lock().plan_for(outgoing_id)
    }

    fn wants_analysis(&mut self, song_id: &str) -> Option<u64> {
        self.0.lock().wants_analysis(song_id)
    }

    fn analysed(&mut self, song_id: &str, analyzer: Analyzer, channels: usize, frames: u64, rate: u32) {
        self.0.lock().analysed(song_id, analyzer, channels, frames, rate);
    }

    fn log(&mut self, message: &str) {
        self.0.lock().log(message);
    }

    fn now_ms(&self) -> i64 {
        self.0.lock().now_ms()
    }
}

impl App for Live {
    fn clock(&mut self, now_ms: i64) {
        self.0.lock().clock(now_ms);
    }

    fn auto_mix(&self) -> bool {
        self.0.lock().auto_mix()
    }

    fn window(&mut self, window: Vec<WindowSong>, shuffling: bool) {
        self.0.lock().window(window, shuffling);
    }

    fn transitions_off(&mut self, off: bool) {
        self.0.lock().transitions_off(off);
    }

    fn gain(&mut self, index: usize, id: &str) -> f32 {
        self.0.lock().gain(index, id)
    }
}

/// Plays `songs` until two seconds were heard.
fn playing(songs: &[(&str, &[i16])], app: impl App + Send + 'static, settings: Settings) -> Rig {
    let rig = Rig::with_app(songs, app, settings);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2));
    rig
}

#[test]
fn fade_setting_applies_to_next_pause() {
    let a = vec![8000i16; RATE as usize * 2 * 20];
    let rig = playing(&[("a", &a)], sim::App::new(), Settings::default());
    // Nothing happens until the next pause.
    rig.engine.set_settings(Settings { fade_ms: 400, ..Settings::default() });
    rig.engine.pause();
    assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
    rig.run(4_000);
    let heard = rig.heard.lock().clone();
    assert!(heard[heard.len() - 2].abs() < 100, "faded to silence: {}", heard[heard.len() - 2]);
    assert!(heard.iter().any(|&v| v > 2000 && v < 6000), "a ramp down, not a cut");
}

#[test]
fn replay_gain_change_reaches_buffered_music() {
    let a = music(30.0, 31);
    let live = Live::new(prefs_off());
    let rig = playing(&[("a", &a)], live.clone(), Settings::default());
    live.0.lock().gains.insert("a".into(), 0.5);
    let asked = rig.heard.lock().len();
    rig.engine.gain_changed();
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert_eq!(heard.len(), a.len());
    let quiet = at(&a, 0.5);
    // Buffered music is rescaled in place, not played out at the old level.
    let k = heard.iter().zip(&a).position(|(h, s)| h != s).expect("the level changed");
    // New level to the end, including the partly taken buffer (`TransitionEngine::rescale`); at most a
    // few ms of ramp.
    let off: Vec<usize> = (k..heard.len()).filter(|&i| (heard[i] as i32 - quiet[i] as i32).abs() > 1).collect();
    if let (Some(&first), Some(&last)) = (off.first(), off.last()) {
        let secs = |i: usize| i as f64 / 2.0 / RATE as f64;
        assert!(last - first < 2 * RATE as usize * 5 / 1000, "at the new level from there to the end: {:.4} s to {:.4} s is not", secs(first), secs(last));
        let between = |i: usize| (heard[i] as i32 - quiet[i] as i32).signum() * (heard[i] as i32 - a[i] as i32).signum() <= 0;
        assert!(off.iter().all(|&i| between(i) && heard[i] != a[i]), "and what is not is a ramp between the two, not the old level");
    }
    assert!(k < asked + RATE as usize * 2 * 2, "heard within two seconds of the change, not ten: {} s after", (k as f64 - asked as f64) / 2.0 / RATE as f64);
}

#[test]
fn bit_perfect_drops_replay_gain_from_next_song() {
    let (a, b) = (music(20.0, 32), music(10.0, 33));
    let mut app = sim::App::new();
    app.gains.insert("a".into(), 0.5);
    app.gains.insert("b".into(), 0.5);
    let rig = playing(&[("a", &a), ("b", &b)], app, Settings::default());
    rig.engine.set_output(OutputFacts { bit_perfect: true, ..OutputFacts::default() });
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert!(heard[heard.len() - b.len()..] == b[..], "b untouched, at its own level");
}

#[test]
fn longer_crossfade_applies_to_next_mix() {
    let (a, b) = (music(30.0, 34), music(20.0, 35));
    let live = Live::new(crossfade(2));
    let rig = playing(&[("a", &a), ("b", &b)], live.clone(), Settings::default());
    live.0.lock().prefs = crossfade(6);
    rig.engine.replan();
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert_eq!(rig.heard.lock().len(), a.len() + b.len() - RATE as usize * 2 * 6, "six seconds of overlap, not two");
}

#[test]
fn crossfade_off_joins_gaplessly() {
    let (a, b) = (music(30.0, 36), music(10.0, 37));
    let live = Live::new(crossfade(6));
    let rig = playing(&[("a", &a), ("b", &b)], live.clone(), Settings::default());
    live.0.lock().prefs = prefs_off();
    rig.engine.replan();
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert!(heard.len() == a.len() + b.len() && heard[..a.len()] == a[..] && heard[a.len()..] == b[..], "a then b, every sample");
}

/// An analysis of steady music at `bpm`.
fn measured(id: &str, bpm: f64, ms: i64) -> TrackAnalysis {
    TrackAnalysis {
        song_id: id.into(),
        analysis_version: ANALYSIS_VERSION,
        duration_ms: ms,
        bpm,
        bpm_confidence: 1.0,
        beat_offset_ms: 250.0,
        stability: 1.0,
        downbeat_confidence: 1.0,
        lufs: -14.0,
        silence_end_ms: ms,
        mixramp_end_ms: ms,
        intro_end_ms: 250,
        outro_start_ms: ms - 16_000,
        outro_bpm: bpm,
        outro_bpm_confidence: 1.0,
        outro_beat_offset_ms: 250.0,
        outro_stability: 1.0,
        intro_bpm: bpm,
        intro_bpm_confidence: 1.0,
        intro_beat_offset_ms: 250.0,
        intro_stability: 1.0,
        ..Default::default()
    }
}

#[test]
fn automix_on_mixes_current_song() {
    let (a, b) = (music(40.0, 38), music(40.0, 39));
    let live = Live::new(prefs_off());
    let rig = playing(&[("a", &a), ("b", &b)], live.clone(), Settings::default());
    {
        let mut app = live.0.lock();
        app.analyses.insert("a".into(), measured("a", 120.0, 40_000));
        app.analyses.insert("b".into(), measured("b", 120.0, 40_000));
        app.prefs = TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() };
    }
    rig.engine.replan();
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let log = live.0.lock().log.clone();
    assert!(log.iter().any(|l| l.contains("transition a -> b: BeatMatched")), "{log:?}");
    assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{log:?}");
    assert!(rig.heard.lock().len() < a.len() + b.len(), "the songs overlap");
}

#[test]
fn replay_gain_per_song_through_automix() {
    // A stretched AutoMix between songs at 0.5 and 0.8 gain: each at its own gain before mixing.
    let (a, b) = (music(40.0, 44), music(40.0, 45));
    let run = |songs: &[(&str, &[i16])], gains: &[(&str, f32)]| {
        let live = Live::new(TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() });
        {
            let mut app = live.0.lock();
            for t in [measured("a", 120.0, 40_000), measured("b", 123.0, 40_000)] {
                app.analyses.insert(t.song_id.clone(), t);
            }
            for (id, g) in gains {
                app.gains.insert(id.to_string(), *g);
            }
        }
        let rig = Rig::with_app(songs, live.clone(), Settings::default());
        rig.engine.play_at(0, 0);
        assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
        let log = live.0.lock().log.clone();
        assert!(log.iter().any(|l| l.contains("transition a -> b: BeatMatched") && l.contains("tempo x0.976")), "{log:?}");
        let heard = rig.heard.lock().clone();
        heard
    };
    let (qa, qb) = (at(&a, 0.5), at(&b, 0.8));
    let ideal = run(&[("a", &qa), ("b", &qb)], &[]);
    let heard = run(&[("a", &a), ("b", &b)], &[("a", 0.5), ("b", 0.8)]);
    assert_eq!(heard.len(), ideal.len());
    // Sample-exact up to the stretch (dither may move its splices); after, by level every 100 ms.
    let stretch = beyond_dither(&heard, &ideal).unwrap_or(heard.len());
    assert!(stretch > RATE as usize * 2 * 25, "every sample as a at half its level up to the mix: {} s", stretch as f64 / 2.0 / RATE as f64);
    let db = |x: &[i16]| 10.0 * (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).log10();
    let w = RATE as usize / 5;
    let worst = heard.chunks(w).zip(ideal.chunks(w)).map(|(h, i)| (db(h) - db(i)).abs()).fold(0.0, f64::max);
    assert!(worst < 0.1, "every tenth of a second at the gain-then-mix level: {worst:.3} dB off");
}

#[test]
fn speed_change_is_heard() {
    let a = music(30.0, 40);
    let rig = playing(&[("a", &a)], sim::App::new(), Settings::default());
    rig.engine.set_settings(Settings { speed: 2.0, ..Settings::default() });
    assert!(rig.wait_for(5, |r| r.engine.status().speed == 2.0), "{:?}", rig.engine.status());
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    // Buffered music at the old speed, the rest at twice.
    let heard = rig.heard.lock().len();
    assert!(heard < a.len() * 3 / 4, "{:.1} s of 30", heard as f64 / 2.0 / RATE as f64);
}

#[test]
fn silence_skipping_on_skips_ahead() {
    let mut a = music(40.0, 41);
    let (from, to) = (RATE as usize * 2 * 24, RATE as usize * 2 * 34);
    a[from..to].fill(0);
    let rig = playing(&[("a", &a)], sim::App::new(), Settings::default());
    rig.engine.set_settings(Settings { skip_silence: true, ..Settings::default() });
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().len();
    assert!(heard < a.len() - RATE as usize * 2 * 6, "ten seconds of silence mostly skipped: {:.1} s of 40", heard as f64 / 2.0 / RATE as f64);
}

#[test]
fn equalizer_on_is_heard() {
    let a = music(30.0, 42);
    let rig = playing(&[("a", &a)], sim::App::new(), Settings::default());
    rig.engine.set_settings(loud_eq());
    assert!(rig.wait_for(5, |r| r.engine.status().chain), "{:?}", rig.engine.status());
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    // Remade from the playback position (to the ms, but for the dip).
    assert!(heard.len().abs_diff(a.len()) <= RATE as usize * 2 / 20, "{} samples of {}", heard.len(), a.len());
    // Changed at 2 s: unchanged before, equalized after.
    let end = RATE as usize * 2;
    assert!(heard[..end] == a[..end], "as it is before");
    let end = RATE as usize * 2 * 5;
    assert!(heard[heard.len() - end..] != a[a.len() - end..], "through the equalizer after");
}

/// Settings with a pre-amp of `db` (the chain on, only turning down).
fn quieter(db: f64) -> Settings {
    Settings { sound: nori_engine::Sound { preamp_db: db, ..Default::default() }, ..Settings::default() }
}

/// Where `heard[from..]` matches `song` to the end: the run's first sample and its offset in the song.
fn as_the_song(heard: &[i16], song: &[i16], from: usize) -> Option<(usize, isize)> {
    let tail = 2 * RATE as usize / 10;
    let end = heard.len() - heard.len() % 2;
    let probe = &heard[end - tail..end];
    let at = (0..=song.len() - tail).step_by(2).find(|&k| song[k..k + tail] == *probe)?;
    let shift = at as isize - (end - tail) as isize;
    let mut k = end - tail;
    while k > from && song.get((k as isize - 2 + shift) as usize..(k as isize + shift) as usize) == Some(&heard[k - 2..k]) {
        k -= 2;
    }
    Some((k, shift))
}

/// Limiter and mono switched while playing: the music goes on and each change is heard.
#[test]
fn limiter_and_mono_switch_without_gaps() {
    let a = music(60.0, 46);
    let files = vec![("a".to_string(), wav(&a), 60_000)];
    let rig = Rig::build(files, sim::App::new(), Settings::default(), Extra { pace: Some(1.0), ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2), "it plays");
    let waits = rig.waits();
    let sound = |limiter: bool, mono: bool| Settings { sound: nori_engine::Sound { limiter, mono, ..Default::default() }, ..Settings::default() };
    // The last second: mono or not, and its level.
    let last = |r: &Rig| {
        let h = r.heard.lock();
        let s = &h[h.len() - RATE as usize * 2..];
        let same = s.chunks(2).all(|f| f[0] == f[1]);
        (same, s.iter().map(|&v| (v as f64).powi(2)).sum::<f64>().sqrt())
    };
    let (split, level) = last(&rig);
    assert!(!split, "the song's two channels differ");

    for (limiter, mono) in [(true, false), (true, true), (false, true), (false, false)] {
        let before = rig.heard.lock().len();
        rig.engine.set_settings(sound(limiter, mono));
        // The chain stays in, so switching off is heard at once too.
        assert!(rig.wait_for(2, |r| r.engine.status().chain), "limiter {limiter}, mono {mono}: {:?}", rig.engine.status());
        rig.run(2_000);
        assert!(rig.heard.lock().len() >= before + RATE as usize * 2 * 19 / 10, "limiter {limiter}, mono {mono}: the music goes on");
        let (same, loud) = last(&rig);
        assert_eq!(same, mono, "limiter {limiter}, mono {mono}: the channels are one only in mono");
        if limiter {
            let gr = rig.engine.status().gain_reduction_db;
            assert!(gr < 6.0, "the limiter only catches peaks: {gr} dB");
            assert!(loud > level * 0.5, "and leaves the music its level: {loud} against {level}");
        }
    }
    assert!(rig.waits() <= waits + 8, "no gap past the switches' dips: {} waits", rig.waits() - waits);
    rig.engine.stop();
}

#[test]
fn equalizer_off_heard_at_once() {
    let a = music(20.0, 45);
    let files = vec![("a".to_string(), wav(&a), 20_000)];
    let rig = Rig::build(files, sim::App::new(), quieter(-12.0), Extra { pace: Some(1.0), ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2), "it plays");
    let (asked, waits) = (rig.heard.lock().len(), rig.waits());
    rig.engine.set_settings(Settings::default());
    rig.run(1_000);
    let heard = rig.heard.lock().clone();
    let (k, shift) = as_the_song(&heard, &a, asked).expect("the song itself, untouched, after the change");
    let ms = |samples: usize| samples as f64 * 1000.0 / 2.0 / RATE as f64;
    // The buffered old sound is remade behind a 30 ms dip.
    assert!(ms(k - asked) <= 200.0, "heard {:.0} ms after the change", ms(k - asked));
    assert!(shift.unsigned_abs() <= 2 * RATE as usize / 1000 * 2, "on from where the ear was: {:.1} ms off", ms(shift.unsigned_abs()));
    assert!(rig.waits() <= waits + 2, "no gap past the dip: {} waits", rig.waits() - waits);
    // Before the change: 12 dB quieter.
    let before = &heard[asked - 2_000..asked];
    let loud = |s: &[i16]| s.iter().map(|&v| (v as f64).powi(2)).sum::<f64>().sqrt();
    let ratio = loud(before) / loud(&a[asked - 2_000..asked]);
    assert!((ratio - 0.251).abs() < 0.02, "{ratio}");
    rig.engine.stop();
}

#[test]
fn slider_drag_remakes_at_most_every_150_ms() {
    let a = music(20.0, 46);
    let files = vec![("a".to_string(), wav(&a), 20_000)];
    let rig = Rig::build(files, sim::App::new(), quieter(-3.0), Extra { pace: Some(1.0), ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    let flushes = rig.flushes.load(Ordering::Relaxed);
    // Ten steps in 100 ms, as a dragged slider.
    let started = rig.now_ms();
    for k in 0..10 {
        rig.engine.set_settings(quieter(-4.0 - k as f64));
        rig.run(10);
    }
    let took = (rig.now_ms() - started) as u64;
    rig.run(1_500);
    let made = rig.flushes.load(Ordering::Relaxed) - flushes;
    assert!(made >= 1 && made <= took / 150 + 2, "{made} times made again for {took} ms of changes");
    // Now at the last step's level, steady.
    let heard = rig.heard.lock().clone();
    let loud = |s: &[i16]| (s.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
    let n = heard.len() - heard.len() % 2;
    let ratio = loud(&heard[n - RATE as usize..n]) / loud(&a);
    assert!((ratio - 10f64.powf(-13.0 / 20.0)).abs() < 0.012, "{ratio}");
    rig.engine.stop();
}

/// A song larger than the memory cap, so reopening it a minute in refetches (each request `lag_ms`):
/// played from a minute in with `settings` and 100 ms position events.
fn slow_to_open_again(settings: Settings, lag_ms: u64) -> (Rig, Vec<i16>) {
    let a = music(120.0, 47);
    let files = vec![("a".to_string(), wav(&a), 120_000)];
    let extra = Extra { pace: Some(1.0), memory_mb: Some(64), ..Extra::default() };
    let server = extra.server.clone();
    let rig = Rig::build(files, sim::App::new(), settings, extra);
    *server.lag.lock() = Some((rig.time.clock.clone(), Duration::from_millis(lag_ms)));
    rig.engine.position_updates(Some(Duration::from_millis(100)));
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(80, |r| r.heard.lock().len() > RATE as usize * 2 * 60), "a minute in");
    (rig, a)
}

/// Milliseconds of stereo `samples`.
fn ms_of(samples: usize) -> i64 {
    samples as i64 * 1000 / 2 / RATE as i64
}

/// Positions since event `from` advance, the last within 150 ms of `heard_ms`.
fn places_run_on(rig: &Rig, from: usize, heard_ms: i64) {
    let places: Vec<i64> = rig.events.lock()[from..].iter().filter_map(|e| if let Event::Position { ms, .. } = e { Some(*ms) } else { None }).collect();
    assert!(places.len() >= 5, "{places:?}");
    assert!(places.windows(2).all(|w| w[1] >= w[0] && w[1] - w[0] <= 250), "on and on: {places:?}");
    let last = *places.last().expect("some");
    assert!((heard_ms - last).abs() <= 150, "the place {last} ms is the music heard, {heard_ms} ms: {places:?}");
}

/// Equalizer on over a slow-to-reopen song: opened ahead while the output plays on, swapped in behind
/// a dip. Regression: a silence as long as the reopen.
#[test]
fn equalizer_on_over_slow_reopen_has_no_gap() {
    let (rig, a) = slow_to_open_again(Settings::default(), 300);
    let asked = rig.server.requests.lock().len();
    let (at, waits, t0, seen) = (rig.heard.lock().len(), rig.waits(), rig.now_ms(), rig.events.lock().len());
    rig.engine.set_settings(loud_eq());
    rig.run(2_000);
    assert!(rig.server.requests.lock().len() > asked, "the song was fetched again to open it: {:?}", rig.server.requests.lock());
    let heard = rig.heard.lock().clone();
    let elapsed = rig.now_ms() - t0;
    let got = ms_of(heard.len() - at);
    assert!(rig.waits() <= waits + 2, "no gap: the card found nothing to play {} times ({} ms of music in {elapsed} ms)", rig.waits() - waits, got);
    assert!(elapsed - got <= 10, "{got} ms heard in {elapsed} ms");
    // Unchanged until the dip, then equalized.
    let changed = (at..heard.len()).find(|&k| heard[k] != a[k]).expect("the equalizer is heard");
    let after = ms_of(changed - at);
    assert!(after <= 3_000, "the dip began {after} ms after the change, once the song was open again");
    assert!(heard[heard.len() - 2 * RATE as usize..] != a[heard.len() - 2 * RATE as usize..heard.len()], "through the equalizer after");
    // Positions follow the music heard.
    places_run_on(&rig, seen, ms_of(heard.len()));
    rig.engine.stop();
}

/// Equalizer off over a slow-to-reopen song: after the dip, exactly where playback was.
#[test]
fn equalizer_off_over_slow_reopen_keeps_place() {
    let (rig, a) = slow_to_open_again(quieter(-12.0), 300);
    let (at, waits, t0) = (rig.heard.lock().len(), rig.waits(), rig.now_ms());
    rig.engine.set_settings(Settings::default());
    rig.run(2_000);
    let heard = rig.heard.lock().clone();
    let elapsed = rig.now_ms() - t0;
    assert!(rig.waits() <= waits + 2, "no gap: the card found nothing to play {} times", rig.waits() - waits);
    assert!(elapsed - ms_of(heard.len() - at) <= 10, "{} ms heard in {elapsed} ms", ms_of(heard.len() - at));
    let (k, shift) = as_the_song(&heard, &a, at).expect("the song itself, untouched, after the change");
    assert!(ms_of(k - at) <= 3_000, "heard {} ms after the change", ms_of(k - at));
    assert!(shift.unsigned_abs() <= 2 * RATE as usize / 1000 * 2, "on from where the ear was: {:.1} ms off", shift as f64 * 1000.0 / 2.0 / RATE as f64);
    rig.engine.stop();
}

/// Every sound switch and AutoMix over a slow-to-reopen song: each behind a dip, never stopping.
#[test]
fn repeated_sound_changes_over_slow_reopen_never_stop() {
    let (rig, _) = slow_to_open_again(Settings::default(), 200);
    let limiter = |on: bool| Settings { sound: nori_engine::Sound { limiter: on, threshold_db: -1.0, ..Default::default() }, ..Settings::default() };
    let steps = [loud_eq(), Settings::default(), limiter(true), limiter(false), Settings { auto_mix: true, ..Settings::default() }, Settings::default(), quieter(-6.0)];
    for s in steps {
        let (at, waits, t0, seen) = (rig.heard.lock().len(), rig.waits(), rig.now_ms(), rig.events.lock().len());
        rig.engine.set_settings(s.clone());
        rig.run(2_000);
        let (heard, elapsed) = (rig.heard.lock().len(), rig.now_ms() - t0);
        assert!(rig.waits() <= waits + 2, "no gap for {s:?}: the card found nothing to play {} times", rig.waits() - waits);
        assert!(elapsed - ms_of(heard - at) <= 10, "{} ms heard in {elapsed} ms for {s:?}", ms_of(heard - at));
        places_run_on(&rig, seen, ms_of(heard));
    }
    rig.engine.stop();
}

#[test]
fn hi_res_on_takes_next_song_untouched() {
    // a is long enough that b opens well after the change.
    let (a, b) = (music24(30.0, 43), music24(8.0, 44));
    let files = vec![("a".to_string(), wav24(&a), 30_000), ("b".to_string(), wav24(&b), 8_000)];
    let rig = Rig::build(files, sim::App::new(), Settings::default(), Extra { float: true, ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard_f.lock().len() > RATE as usize * 2 * 2));
    rig.engine.set_settings(Settings { hi_res: true, ..Settings::default() });
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard_f.lock().clone();
    let tail = &heard[heard.len() - b.len()..];
    let off = tail.iter().zip(&b).position(|(h, s)| *h != *s as f32 / 8_388_608.0);
    assert_eq!(off, None, "b in all its 24 bits, in float");
    assert!(heard[..RATE as usize * 2].iter().zip(&a).all(|(h, s)| *h == (*s as f32 / 256.0).round_ties_even() / 32768.0), "a as 16 bits before");
}

#[test]
fn tuning_makes_output_shallow_then_deep() {
    let a = music(30.0, 47);
    let files = vec![("a".to_string(), wav(&a), 30_000)];
    let rig = Rig::build(files, sim::App::new(), loud_eq(), Extra { pace: Some(1.0), ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    let (flushes, waits) = (rig.flushes.load(Ordering::Relaxed), rig.waits());
    rig.engine.set_tuning(true);
    // At once, behind a dip.
    assert!(rig.wait_for(2, |r| r.shallow.load(Ordering::Relaxed) && r.flushes.load(Ordering::Relaxed) > flushes), "shallow at once");
    rig.run(1_500);
    assert!(rig.waits() <= waits + 2, "a shallow ring kept up with: {} waits", rig.waits() - waits);
    rig.engine.set_tuning(false);
    assert!(rig.wait_for(2, |r| !r.shallow.load(Ordering::Relaxed)), "deep again as the screen closes");
    rig.engine.stop();
}

/// Tuning toggled `rounds` times 1.5 s apart over a resizing device; the rig left playing.
fn tuned_in_place(rounds: usize) -> Rig {
    let a = music(60.0, 48);
    let files = vec![("a".to_string(), wav(&a), 60_000)];
    let rig = Rig::build(files, sim::App::new(), loud_eq(), Extra { pace: Some(1.0), resizes: true, ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    for round in 0..rounds {
        rig.engine.set_tuning(true);
        assert!(rig.wait_for(2, |r| r.shallow.load(Ordering::Relaxed)), "round {round}: shallow at once");
        rig.run(1_500);
        rig.engine.set_tuning(false);
        assert!(rig.wait_for(2, |r| !r.shallow.load(Ordering::Relaxed)), "round {round}: deep again at once");
        rig.run(1_500);
    }
    rig
}

#[test]
fn tuning_on_resizing_device_is_inaudible() {
    let tuned = tuned_in_place(4);
    let heard = tuned.heard.lock().clone();
    let (flushes, waits) = (tuned.flushes.load(Ordering::Relaxed), tuned.waits());
    tuned.engine.stop();
    assert_eq!(flushes, 0, "nothing dropped or made again");
    assert_eq!(waits, 0, "the device never found too little to play");
    // Identical to never tuning: no dip, gap or jump.
    let plain = tuned_in_place(0);
    plain.run(12_000);
    let untouched = plain.heard.lock().clone();
    plain.engine.stop();
    let n = heard.len().min(untouched.len());
    assert!(n > RATE as usize * 2 * 12, "{} s heard", n / 2 / RATE as usize);
    let off = heard[..n].iter().zip(&untouched[..n]).position(|(a, b)| a != b);
    assert_eq!(off, None, "the same music, sample for sample");
}

#[test]
fn tuned_in_place_remakes_until_deep_audio_plays_out() {
    let rig = tuned_in_place(0);
    rig.engine.set_tuning(true);
    assert!(rig.wait_for(2, |r| r.shallow.load(Ordering::Relaxed)));
    let flushes = rig.flushes.load(Ordering::Relaxed);
    // Buffered seconds with the old bands are remade behind a dip.
    let mut quieter = loud_eq();
    quieter.sound.bands[0].gain_db = 3.0;
    rig.engine.set_settings(quieter.clone());
    assert!(rig.wait_for(2, |r| r.flushes.load(Ordering::Relaxed) > flushes), "made again at once");
    // Once shallow, band moves need no remake.
    rig.run(12_000);
    let flushes = rig.flushes.load(Ordering::Relaxed);
    quieter.sound.bands[0].gain_db = 1.0;
    rig.engine.set_settings(quieter);
    rig.run(1_000);
    assert_eq!(rig.flushes.load(Ordering::Relaxed), flushes, "heard as it is");
    rig.engine.stop();
}

#[test]
fn tuned_in_place_first_change_lands_shallow() {
    let rig = tuned_in_place(0);
    // The first change arrives before tuning: remade into the deep buffer.
    let flushes = rig.flushes.load(Ordering::Relaxed);
    let mut changed = loud_eq();
    changed.sound.bands[0].gain_db = 3.0;
    rig.engine.set_settings(changed.clone());
    assert!(rig.wait_for(2, |r| r.flushes.load(Ordering::Relaxed) > flushes), "made again at once");
    rig.run(200);
    // Then tuning: remade once more, shallow.
    let flushes = rig.flushes.load(Ordering::Relaxed);
    rig.engine.set_tuning(true);
    assert!(rig.wait_for(2, |r| r.flushes.load(Ordering::Relaxed) > flushes), "made again into the shallow buffer");
    rig.run(1_000);
    let flushes = rig.flushes.load(Ordering::Relaxed);
    changed.sound.bands[0].gain_db = 1.0;
    rig.engine.set_settings(changed);
    rig.run(1_000);
    assert_eq!(rig.flushes.load(Ordering::Relaxed), flushes, "heard as it is");
    // Tuning long after a change remakes nothing.
    rig.engine.set_tuning(false);
    rig.run(3_000);
    let flushes = rig.flushes.load(Ordering::Relaxed);
    rig.engine.set_tuning(true);
    rig.run(1_000);
    assert_eq!(rig.flushes.load(Ordering::Relaxed), flushes, "nothing made again for the tuning alone");
    rig.engine.stop();
}

/// The most the ring held over two seconds while tuned over a device reporting `depth`, ms.
fn tuned_ring_ms(depth: Option<ShallowDepth>) -> u64 {
    let a = music(40.0, 49);
    let files = vec![("a".to_string(), wav(&a), 40_000)];
    let rig = Rig::build(files, sim::App::new(), loud_eq(), Extra { pace: Some(1.0), resizes: true, depth, ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    rig.engine.set_tuning(true);
    assert!(rig.wait_for(2, |r| r.shallow.load(Ordering::Relaxed)));
    rig.run(14_000);
    let waits = rig.waits();
    let mut most = 0;
    for _ in 0..100 {
        rig.run(20);
        most = most.max(rig.card.lock().feed.as_ref().map_or(0, |f| f.available()));
    }
    assert_eq!(rig.waits(), waits, "kept fed");
    rig.engine.stop();
    most as u64 * 1000 / RATE as u64
}

#[test]
fn tuned_ring_follows_device_depth() {
    let speaker = tuned_ring_ms(None);
    assert!(speaker <= nori_engine::output::SHALLOW_US as u64 / 1000 + 5, "the speaker's ring as it was: {speaker} ms");
    let bluetooth = tuned_ring_ms(Some(ShallowDepth { device_us: 550_000, ring_us: 200_000 }));
    assert!((150..=205).contains(&bluetooth), "a Bluetooth output's ring: {bluetooth} ms");
}

// ---- skips pressed in a hurry ----

/// `n` songs of `secs`, named a, b, c...
fn many(n: usize, secs: f64, seed: u64) -> Vec<(String, Vec<i16>)> {
    (0..n).map(|k| (((b'a' + k as u8) as char).to_string(), music(secs, seed + k as u64))).collect()
}

fn listed(songs: &[(String, Vec<i16>)]) -> Vec<(&str, &[i16])> {
    songs.iter().map(|(id, s)| (id.as_str(), s.as_slice())).collect()
}

/// Plays the songs from `from_ms` into the first, at real-time pace.
fn at_pace(songs: &[(&str, &[i16])], app: impl App + Send + 'static, from_ms: i64) -> Rig {
    let files = songs.iter().map(|(id, s)| (id.to_string(), wav(s), (s.len() / 2) as i64 * 1000 / RATE as i64)).collect();
    let rig = Rig::build(files, app, Settings::default(), Extra { pace: Some(1.0), ..Extra::default() });
    rig.engine.play_at(0, from_ms);
    assert!(rig.wait_for(10, |r| !r.heard.lock().is_empty()), "it plays");
    rig
}

/// A client's shown song: moves on a press, then follows song events taken late ([`Shown::take`]).
#[derive(Default)]
struct Shown {
    current: usize,
    expecting: Option<usize>,
    sent: u64,
    taken: usize,
    /// Every change of the shown song.
    changes: Vec<usize>,
}

impl Shown {
    fn press(&mut self, to: usize, jump: u64) {
        if to != self.current {
            self.expecting = Some(to);
            self.current = to;
            self.changes.push(to);
        }
        self.sent = jump;
    }

    fn take(&mut self, events: &[Event]) {
        for e in &events[self.taken..] {
            let Event::Song { index, jumps, .. } = e else { continue };
            // From before the last jump.
            if *jumps < self.sent {
                continue;
            }
            self.expecting = None;
            if *index != self.current {
                self.current = *index;
                self.changes.push(*index);
            }
        }
        self.taken = events.len();
    }
}

/// The position in `song` at the end of what was heard, ms; None if another song.
fn heard_in(rig: &Rig, song: &[i16]) -> Option<i64> {
    let heard = rig.heard.lock().clone();
    let (_, shift) = as_the_song(&heard, song, heard.len().saturating_sub(RATE as usize))?;
    Some(((heard.len() as isize + shift) / 2) as i64 * 1000 / RATE as i64)
}

#[test]
fn fast_next_moves_one_song_per_press() {
    let songs = many(6, 60.0, 400);
    for gap in [0u64, 30, 130, 200] {
        let rig = at_pace(&listed(&songs), sim::App::new(), 0);
        assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
        let mut shown = Shown::default();
        shown.take(&rig.events.lock());
        for k in 1..=3 {
            // Next after the shown song, as media3's seekToNext.
            let to = shown.current + 1;
            shown.press(to, rig.engine.go_to(to, 0));
            rig.run(gap);
            if gap >= 200 && k == 2 {
                shown.take(&rig.events.lock());
            }
        }
        assert!(rig.wait_for(5, |r| r.engine.status().index == Some(3)), "gap {gap}: the engine ends on the third song after: {:?}", rig.events.lock());
        rig.run(500);
        shown.take(&rig.events.lock());
        assert_eq!(shown.changes, vec![1, 2, 3], "gap {gap}: one change per press, never back: {:?}", rig.events.lock());
        let said: Vec<usize> = rig.events.lock().iter().filter_map(|e| if let Event::Song { index, .. } = e { Some(*index) } else { None }).collect();
        assert!(said.windows(2).all(|w| w[0] < w[1]) && said.last() == Some(&3), "gap {gap}: the engine went forwards only: {said:?}");
        let ms = heard_in(&rig, &songs[3].1).unwrap_or_else(|| panic!("gap {gap}: the ear is on d"));
        assert!(ms < 3_000, "gap {gap}: d from its start, {ms} ms in");
        assert!(heard_in(&rig, &songs[4].1).is_none(), "gap {gap}: never on to e");
        rig.engine.stop();
    }
}

#[test]
fn engine_next_moves_one_song_per_press() {
    let songs = many(6, 60.0, 410);
    let rig = at_pace(&listed(&songs), sim::App::new(), 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    for _ in 0..3 {
        rig.engine.next();
        rig.run(40);
    }
    assert!(rig.wait_for(5, |r| r.engine.status().index == Some(3)), "{:?}", rig.events.lock());
    rig.run(1_000);
    assert_eq!(rig.engine.status().index, Some(3), "{:?}", rig.events.lock());
    let ms = heard_in(&rig, &songs[3].1).expect("the ear is on d");
    assert!(ms < 3_000, "d from its start, {ms} ms in");
    rig.engine.stop();
}

/// The planned mix start out of `a` into `b`, ms.
fn planned(live: &Live) -> Option<i64> {
    let log = live.0.lock().log.clone();
    let line = log.iter().find(|l| l.starts_with("transition a -> b"))?;
    line.split(" at ").nth(1)?.split(',').next()?.trim().parse().ok()
}

/// Next pressed just before the planned end: one change, to the next song's start, and nothing of the
/// planned ending after it.
fn next_before_the_end(prefs: TransitionPrefs, measured_songs: bool, name: &str) {
    let songs = many(3, 40.0, 420);
    for lead in [500i64, 300, 100] {
        let live = Live::new(prefs.clone());
        if measured_songs {
            let mut app = live.0.lock();
            for (id, _) in &songs {
                app.analyses.insert(id.clone(), measured(id, 120.0, 40_000));
            }
        }
        let from = 22_000;
        let rig = at_pace(&listed(&songs), live.clone(), from);
        let end = if prefs.auto_mix || prefs.crossfade_s > 0 {
            assert!(rig.wait_for(10, |_| planned(&live).is_some()), "{name}: a mix is planned: {:?}", live.0.lock().log);
            planned(&live).expect("checked")
        } else {
            40_000
        };
        assert!(end > from + 1_000, "{name}: planned at {end}");
        let ear = |r: &Rig| from + (r.heard.lock().len() / 2) as i64 * 1000 / RATE as i64;
        assert!(rig.wait_for(30, |r| ear(r) >= end - lead), "{name}: reaches the press");
        let mut shown = Shown::default();
        shown.take(&rig.events.lock());
        assert_eq!(rig.engine.status().index, Some(0), "{name} lead {lead}: still on a");
        let jump = rig.engine.go_to(1, 0);
        shown.press(1, jump);
        // Past a's planned ending, into b.
        rig.run(lead as u64 + 3_000);
        shown.take(&rig.events.lock());
        let events = rig.events.lock().clone();
        let after: Vec<usize> = events.iter().filter_map(|e| if let Event::Song { index, jumps, .. } = e { (*jumps >= jump).then_some(*index) } else { None }).collect();
        assert!(after.is_empty() || after == [1], "{name} lead {lead}: at most b said after the press: {events:?}");
        assert_eq!(shown.changes, vec![1], "{name} lead {lead}: one change of song: {events:?}");
        assert!(!events.iter().any(|e| matches!(e, Event::Song { index: 2, .. })), "{name} lead {lead}: never on to c: {events:?}");
        assert_eq!(rig.engine.status().index, Some(1), "{name} lead {lead}");
        let ms = heard_in(&rig, &songs[1].1).unwrap_or_else(|| panic!("{name} lead {lead}: the ear is on b"));
        assert!((2_000..6_000).contains(&ms), "{name} lead {lead}: b from its start, {ms} ms in");
        rig.engine.stop();
    }
}

#[test]
fn next_before_gapless_end_changes_once() {
    next_before_the_end(prefs_off(), false, "gapless");
}

#[test]
fn next_before_crossfade_changes_once() {
    next_before_the_end(crossfade(6), false, "crossfade");
}

#[test]
fn next_before_automix_changes_once() {
    let prefs = TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() };
    next_before_the_end(prefs, true, "automix");
}

// ---- settings changed with the song's ending already made ----

/// Plays `songs` until a wake finds playback `secs` into the first (the ring holding ten seconds more).
fn playing_until(songs: &[(&str, &[i16])], app: impl App + Send + 'static, settings: Settings, secs: f64) -> Rig {
    let files = songs.iter().map(|(id, s)| (id.to_string(), wav(s), (s.len() / 2) as i64 * 1000 / RATE as i64)).collect();
    let rig = Rig::build(files, app, settings, Extra { pace: Some(5.0), ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(20, |r| r.engine.status().position_ms >= (secs * 1000.0) as i64), "{:?}", rig.engine.status());
    rig
}

#[test]
fn crossfade_on_after_ending_made_still_mixes() {
    let (a, b) = (music(30.0, 50), music(20.0, 51));
    let live = Live::new(prefs_off());
    // At 20 s the ring holds a's end joined gaplessly to b.
    let rig = playing_until(&[("a", &a), ("b", &b)], live.clone(), Settings::default(), 20.0);
    live.0.lock().prefs = crossfade(6);
    rig.engine.set_settings(Settings { crossfade_s: 6, ..Settings::default() });
    rig.engine.replan();
    assert!(rig.wait_for(60, Rig::ended), "{:?} {:?} {:?}", rig.events.lock(), rig.engine.status(), live.0.lock().log);
    let log = live.0.lock().log.clone();
    let heard = rig.heard.lock().len();
    let overlap = (a.len() + b.len()).saturating_sub(heard) as f64 / 2.0 / RATE as f64;
    assert!((overlap - 6.0).abs() < 0.2, "six seconds of overlap, not {overlap:.2}: {log:?}");
}

#[test]
fn crossfade_off_after_mix_made_joins_gaplessly() {
    let (a, b) = (music(30.0, 52), music(20.0, 53));
    let live = Live::new(crossfade(6));
    // At 20 s a's ending from 24 s is held or already mixed.
    let rig = playing_until(&[("a", &a), ("b", &b)], live.clone(), Settings { crossfade_s: 6, ..Settings::default() }, 20.0);
    live.0.lock().prefs = prefs_off();
    rig.engine.set_settings(Settings::default());
    rig.engine.replan();
    assert!(rig.wait_for(60, Rig::ended), "{:?} {:?} {:?}", rig.events.lock(), rig.engine.status(), live.0.lock().log);
    let log = live.0.lock().log.clone();
    let heard = rig.heard.lock().clone();
    // Remade behind a dip: a whole, then b whole.
    assert!(heard.len().abs_diff(a.len() + b.len()) <= RATE as usize * 2 / 10, "{:.2} s heard of {:.2}: {log:?}", heard.len() as f64 / 2.0 / RATE as f64, (a.len() + b.len()) as f64 / 2.0 / RATE as f64);
    assert!(heard[heard.len() - b.len()..] == b[..], "b whole after a, not mixed into it: {log:?}");
    assert!(log.iter().any(|l| l.contains("the ending of a is made again: gapless now")), "{log:?}");
}

#[test]
fn automix_on_near_end_mixes() {
    let (a, b) = (music(40.0, 54), music(40.0, 55));
    let live = Live::new(prefs_off());
    // At 26 s (mix at 28 s) the ring holds a's end and b.
    let rig = playing_until(&[("a", &a), ("b", &b)], live.clone(), Settings::default(), 26.0);
    {
        let mut app = live.0.lock();
        app.analyses.insert("a".into(), measured("a", 120.0, 40_000));
        app.analyses.insert("b".into(), measured("b", 120.0, 40_000));
        app.prefs = TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() };
    }
    rig.engine.set_settings(Settings { auto_mix: true, ..Settings::default() });
    rig.engine.replan();
    assert!(rig.wait_for(60, Rig::ended), "{:?} {:?} {:?}", rig.events.lock(), rig.engine.status(), live.0.lock().log);
    let log = live.0.lock().log.clone();
    assert!(log.iter().any(|l| l.contains("transition a -> b: BeatMatched")), "{log:?}");
    assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{log:?}");
    assert!(rig.heard.lock().len() < a.len() + b.len() - RATE as usize * 2 * 4, "the songs overlap: {log:?}");
}

// ---- a song queued while another plays ----

/// [`Live`] with a background measurer (steady 120 BPM, [`measured`]); the engine replans on
/// [`App::measured`].
#[derive(Clone)]
struct Measuring {
    live: Live,
    /// Asked for, not measured yet.
    asked: Arc<Mutex<Vec<String>>>,
}

impl Measuring {
    fn new(prefs: TransitionPrefs) -> Measuring {
        Measuring { live: Live::new(prefs), asked: Arc::default() }
    }

    fn log(&self) -> Vec<String> {
        self.live.0.lock().log.clone()
    }
}

impl Host for Measuring {
    fn plan_for(&mut self, outgoing_id: &str) -> Option<Plan> {
        self.live.plan_for(outgoing_id)
    }

    fn wants_analysis(&mut self, _song_id: &str) -> Option<u64> {
        None
    }

    fn analysed(&mut self, song_id: &str, analyzer: Analyzer, channels: usize, frames: u64, rate: u32) {
        self.live.analysed(song_id, analyzer, channels, frames, rate);
    }

    fn log(&mut self, message: &str) {
        self.live.log(message);
    }

    fn now_ms(&self) -> i64 {
        self.live.now_ms()
    }
}

impl App for Measuring {
    fn clock(&mut self, now_ms: i64) {
        self.live.clock(now_ms);
    }

    fn auto_mix(&self) -> bool {
        self.live.auto_mix()
    }

    fn window(&mut self, window: Vec<WindowSong>, shuffling: bool) {
        self.live.window(window, shuffling);
    }

    fn measure_ahead<S: nori_player::pipeline::Songs>(&mut self, _songs: &mut S, ids: &[String]) {
        let app = self.live.0.lock();
        let mut asked = self.asked.lock();
        for id in ids {
            if !app.analyses.contains_key(id) && !asked.contains(id) {
                asked.push(id.clone());
            }
        }
    }

    fn measured(&mut self) -> bool {
        let asked = std::mem::take(&mut *self.asked.lock());
        let mut app = self.live.0.lock();
        for id in &asked {
            let ms = app.window.iter().find(|s| s.id == *id).map_or(0, |s| s.duration_ms);
            app.analyses.insert(id.clone(), measured(id, 120.0, ms));
            app.log.push(format!("measured {id} ahead"));
        }
        !asked.is_empty()
    }

    fn transitions_off(&mut self, off: bool) {
        self.live.transitions_off(off);
    }

    fn gain(&mut self, index: usize, id: &str) -> f32 {
        self.live.gain(index, id)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Queued {
    /// Play next.
    PlayNext,
    /// Add to queue: after earlier manual additions, before the rest of the list.
    AddToQueue,
    /// At the very end, as a controller inserts.
    AtTheEnd,
}

/// Plays `a` (a minute, AutoMix on; `c` after it if `with_c`) and queues `b` at fraction `at` as `how`
/// says. Returns the rig, the app and the songs in the order heard.
fn queued_while_playing(at: f64, with_c: bool, how: Queued) -> (Rig, Measuring, Vec<String>) {
    queued_while_playing_with(TransitionPrefs { auto_mix: true, auto_mix_max_s: 8, echo_out: false, ..prefs_off() }, at, with_c, how)
}

fn queued_while_playing_with(prefs: TransitionPrefs, at: f64, with_c: bool, how: Queued) -> (Rig, Measuring, Vec<String>) {
    let (a, b, c) = (music(60.0, 80), music(30.0, 81), music(30.0, 82));
    let app = Measuring::new(prefs);
    let files = [("a", &a), ("b", &b), ("c", &c)].iter().map(|(id, s)| (id.to_string(), wav(s), (s.len() / 2) as i64 * 1000 / RATE as i64)).collect();
    let rig = Rig::build(files, app.clone(), Settings { auto_mix: prefs.auto_mix, ..Settings::default() }, Extra { pace: Some(5.0), ..Extra::default() });
    let first: Vec<String> = if with_c { vec!["a".into(), "c".into()] } else { vec!["a".into()] };
    rig.queue.lock().set(first, Some(0), false, 0);
    rig.engine.queue_changed();
    rig.engine.play_at(0, 0);
    let ms = (at * 60_000.0) as i64;
    assert!(rig.wait_for(30, |r| r.engine.status().position_ms >= ms), "{:?}", rig.engine.status());
    let flushes = rig.flushes.load(Ordering::Relaxed);
    {
        let mut q = rig.queue.lock();
        match how {
            Queued::PlayNext => drop(q.add(vec!["b".into()], nori_player::playlist::Hand::Next)),
            Queued::AddToQueue => drop(q.add(vec!["b".into()], nori_player::playlist::Hand::Last)),
            Queued::AtTheEnd => {
                let end = q.len();
                q.insert(end, vec!["b".into()], nori_player::playlist::Hand::No);
            }
        }
    }
    rig.engine.queue_changed();
    assert!(rig.wait_for(120, Rig::ended), "{:?} {:?} {:?}", rig.events.lock(), rig.engine.status(), app.log());
    let order = rig.events.lock().iter().filter_map(|e| if let Event::Song { id, .. } = e { Some(id.clone()) } else { None }).collect();
    let made_again = rig.flushes.load(Ordering::Relaxed) != flushes;
    assert_eq!(made_again, app.log().iter().any(|l| l.contains("the ending of a is made again")), "the output is emptied only to make the ending again: {:?}", app.log());
    (rig, app, order)
}

/// Asserts `from -> to` was beat-matched with `to` measured first.
fn mixed(app: &Measuring, from: &str, to: &str) {
    let log = app.log();
    let line = format!("transition {from} -> {to}: BeatMatched");
    let planned = log.iter().rposition(|l| l.contains(&format!("transition {from} -> ")));
    assert!(planned.is_some_and(|k| log[k].contains(&line)), "the last plan out of {from} is a mix into {to}: {log:?}");
    let measured_at = log.iter().position(|l| l == &format!("measured {to} ahead"));
    let first_mix = log.iter().position(|l| l.contains(&line));
    assert!(measured_at.is_some() && measured_at < first_mix, "{to} measured before the mix was planned: {log:?}");
}

fn made_again(app: &Measuring) -> bool {
    app.log().iter().any(|l| l.contains("the ending of a is made again"))
}

#[test]
fn song_queued_early_is_mixed_into() {
    // At 30 % nothing of a's ending is made: replanned, nothing remade.
    for how in [Queued::PlayNext, Queued::AddToQueue] {
        for with_c in [false, true] {
            let (_rig, app, order) = queued_while_playing(0.3, with_c, how);
            mixed(&app, "a", "b");
            assert_eq!(order, if with_c { vec!["a", "b", "c"] } else { vec!["a", "b"] }, "{:?}", app.log());
            if with_c {
                mixed(&app, "b", "c");
            }
            assert!(!made_again(&app), "{:?}", app.log());
        }
    }
}

#[test]
fn song_queued_late_remakes_ending() {
    // At 80 % a's ending is made as the end of music: remade into b.
    for how in [Queued::PlayNext, Queued::AddToQueue] {
        let (_rig, app, order) = queued_while_playing(0.8, false, how);
        mixed(&app, "a", "b");
        assert_eq!(order, vec!["a", "b"], "{:?}", app.log());
        assert!(made_again(&app), "the ending in the output was made for nothing after a: {:?}", app.log());
    }
}

#[test]
fn song_queued_ahead_of_next_is_mixed_into() {
    // Before a's mix into c is made: b takes c's place.
    for how in [Queued::PlayNext, Queued::AddToQueue] {
        let (_rig, app, order) = queued_while_playing(0.65, true, how);
        mixed(&app, "a", "b");
        mixed(&app, "b", "c");
        assert_eq!(order, vec!["a", "b", "c"], "{:?}", app.log());
    }
}

#[test]
fn song_queued_behind_next_keeps_ending() {
    // a's mix into c is made; b goes after c, nothing remade.
    let (_rig, app, order) = queued_while_playing(0.72, true, Queued::AtTheEnd);
    mixed(&app, "a", "c");
    mixed(&app, "c", "b");
    assert_eq!(order, vec!["a", "c", "b"], "{:?}", app.log());
    assert!(!made_again(&app), "{:?}", app.log());
    assert!(app.log().iter().any(|l| l.contains("holding the ending")), "{:?}", app.log());
}

#[test]
fn play_next_after_read_on_is_played() {
    // Gapless at 55 s the output holds a's end and c's start; play next remakes c's start as b's.
    let (rig, app, order) = queued_while_playing_with(prefs_off(), 55.0 / 60.0, true, Queued::PlayNext);
    assert_eq!(order, vec!["a", "b", "c"], "{:?}", app.log());
    assert!(app.log().iter().any(|l| l.contains("the ending of a is made again: another song follows it now")), "{:?}", app.log());
    let heard = rig.heard.lock().len() as f64 / 2.0 / RATE as f64;
    assert!((heard - 120.0).abs() < 0.2, "every song whole, one after the other: {heard:.2} s");
}

/// Next pressed 10-15 times, 0-200 ms apart (sometimes inside a held mix ending), with the equalizer
/// and AutoMix on and songs cached whole, partly, or slow: the last song pressed to plays on.
#[test]
fn next_spam_with_equalizer_and_automix_keeps_playing() {
    const SECS: f64 = 40.0;
    let songs = many(16, SECS, 900);
    let ms = (SECS * 1000.0) as i64;
    for round in 0..4u64 {
        let mut rng = Rng(0x9E37_79B9 + round * 7919);
        let mut roll = |max: u64| ((rng.next() + 1.0) / 2.0 * max as f64) as u64;
        let dir = nori_testdir::TempDir::new("skips");
        let store = Store::open(dir.path(), 256 << 20, Box::new(Recent::default())).unwrap();
        let extra = Extra { store: Some(store.clone()), ..Extra::default() };
        for (k, (id, s)) in songs.iter().enumerate() {
            let bytes = wav(s);
            match k % 3 {
                0 => {
                    let mut w = store.writer(&format!("{id}:0")).unwrap();
                    assert!(w.write(0, &bytes));
                    assert!(w.finish(bytes.len() as u64));
                }
                // Partly cached.
                1 => {
                    let mut w = store.writer(&format!("{id}:0")).unwrap();
                    assert!(w.write(0, &bytes[..bytes.len() / 3]));
                    w.leave();
                }
                _ => extra.server.slow.lock().push((id.clone(), Duration::from_millis(200))),
            }
        }
        let mut app = sim::App::new();
        app.prefs = TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, keep_albums: false, ..prefs_off() };
        for (id, _) in &songs {
            app.analyses.insert(id.clone(), measured(id, 120.0, ms));
        }
        let files = songs.iter().map(|(id, s)| (id.clone(), wav(s), ms)).collect();
        let rig = Rig::build(files, app, Settings { auto_mix: true, ..loud_eq() }, extra);
        rig.engine.play_at(0, 0);
        assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2), "round {round}: a plays");
        if round % 2 == 1 {
            // Start inside a held mix ending.
            rig.engine.seek(ms - 18_000);
            rig.run(300 + roll(1_500));
        }
        let presses = 10 + roll(5) as usize;
        for k in 1..=presses {
            // As the app skips: to the song after the shown one.
            rig.engine.go_to(k, 0);
            rig.run(5 + roll(195));
        }
        let heard = rig.heard.lock().len();
        let plays = |r: &Rig| r.heard.lock().len() > heard + RATE as usize * 2 * 3;
        assert!(rig.wait_for(1, plays), "round {round}, {presses} presses: music after them: {:?} {:?}", rig.engine.status(), rig.events.lock());
        assert!(rig.wait_for(1, |r| r.engine.status().index == Some(presses)), "round {round}: on the song the last press asked for: {:?}", rig.engine.status());
        let at = rig.engine.status().position_ms;
        assert!(rig.wait_for(1, |r| r.engine.status().position_ms >= at + 2_000), "round {round}: and the place moves on from {at} ms: {:?}", rig.engine.status());
        rig.engine.stop();
    }
}

#[test]
fn seek_near_end_plays_through_fade() {
    // A seek 15 s before the end with AutoMix unmeasured and slow songs: the next song must still play
    // (regression: its reader slept out its timeout). Rounds side by side vary which reader wakes last
    // (source.rs `abandoned_readers_leave_waiter_woken` pins the order).
    std::thread::scope(|s| {
        let rounds: Vec<_> = (0..4).map(|_| s.spawn(seek_near_the_end_through_the_fade)).collect();
        for r in rounds {
            if let Err(e) = r.join() {
                std::panic::resume_unwind(e);
            }
        }
    });
}

fn seek_near_the_end_through_the_fade() {
    let songs: Vec<Vec<i16>> = (0..3).map(|k| music(40.0, 90 + k)).collect();
    let files: Vec<(String, Vec<u8>, i64)> = ["a", "b", "c"].iter().zip(&songs).map(|(id, s)| (id.to_string(), wav(s), 40_000)).collect();
    let mut app = sim::App::new();
    app.prefs = TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, keep_albums: false, echo_out: false, ..prefs_off() };
    let extra = Extra::default();
    for id in ["a", "b", "c"] {
        extra.server.slow.lock().push((id.into(), Duration::from_millis(300)));
    }
    let rig = Rig::build(files, app, Settings { auto_mix: true, ..Settings::default() }, extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2), "a plays: {:?} {:?}", rig.engine.status(), rig.events.lock());
    rig.engine.go_to(0, 25_000);
    let heard_b = |r: &Rig| r.events.lock().iter().any(|e| matches!(e, Event::Song { id, .. } if id == "b"));
    // The fade reports mixing.
    assert!(rig.wait_for(10, |r| r.engine.status().mixing), "the fade is said to be mixing: {:?} {:?}", rig.engine.status(), rig.events.lock());
    assert!(rig.wait_for(10, heard_b), "b is heard out of the fade: {:?} {:?}", rig.engine.status(), rig.events.lock());
    assert!(rig.wait_for(10, |r| r.engine.status().index == Some(1) && r.engine.status().position_ms > 5_000), "b plays on: {:?}", rig.engine.status());
    assert!(!rig.events.lock().iter().any(|e| matches!(e, Event::Error { .. })), "nothing failed: {:?}", rig.events.lock());
}

#[test]
fn seek_into_distant_mix_plays_on() {
    let (a, b) = (music(40.0, 60), music(40.0, 61));
    let live = Live::new(TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() });
    {
        let mut app = live.0.lock();
        app.analyses.insert("a".into(), measured("a", 120.0, 40_000));
        app.analyses.insert("b".into(), measured("b", 120.0, 40_000));
    }
    let rig = playing(&[("a", &a), ("b", &b)], live.clone(), Settings { auto_mix: true, ..Settings::default() });
    // Mix 26.25-38.25 s; b is read once a is 10 s from its end. Regression: from 27 s everything was
    // held and the player waited in silence for a clock that never moved.
    rig.engine.seek(27_000);
    assert!(rig.wait_for(30, Rig::ended), "{:?} {:?}", rig.engine.status(), live.0.lock().log);
    let log = live.0.lock().log.clone();
    assert!(log.iter().any(|l| l.contains("late hold")), "{log:?}");
    assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{log:?}");
}

#[test]
fn same_plan_again_changes_nothing() {
    let (a, b) = (music(30.0, 56), music(20.0, 57));
    let live = Live::new(crossfade(6));
    let rig = playing_until(&[("a", &a), ("b", &b)], live.clone(), Settings { crossfade_s: 6, ..Settings::default() }, 20.0);
    let flushes = rig.flushes.load(Ordering::Relaxed);
    // A new analysis yields the same plan: nothing remade.
    rig.engine.replan();
    rig.engine.replan();
    assert!(rig.wait_for(60, Rig::ended), "{:?} {:?} {:?}", rig.events.lock(), rig.engine.status(), live.0.lock().log);
    assert_eq!(rig.flushes.load(Ordering::Relaxed), flushes, "{:?}", live.0.lock().log);
    assert_eq!(rig.heard.lock().len(), a.len() + b.len() - RATE as usize * 2 * 6, "six seconds of overlap");
}

#[test]
fn crossfade_on_while_paused_mixes_on_resume() {
    let (a, b) = (music(30.0, 58), music(20.0, 59));
    let live = Live::new(prefs_off());
    let rig = playing_until(&[("a", &a), ("b", &b)], live.clone(), Settings::default(), 16.0);
    rig.engine.pause();
    assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
    live.0.lock().prefs = crossfade(6);
    rig.engine.set_settings(Settings { crossfade_s: 6, ..Settings::default() });
    rig.engine.replan();
    rig.run(500);
    rig.engine.play();
    assert!(rig.wait_for(60, Rig::ended), "{:?} {:?} {:?}", rig.events.lock(), rig.engine.status(), live.0.lock().log);
    let heard = rig.heard.lock().len();
    let overlap = (a.len() + b.len()).saturating_sub(heard) as f64 / 2.0 / RATE as f64;
    assert!((overlap - 6.0).abs() < 0.2, "six seconds of overlap, not {overlap:.2}: {:?}", live.0.lock().log);
}

/// A watcher: whether it wants reports, and those received.
#[derive(Default)]
struct Watching {
    on: AtomicBool,
    seen: Mutex<Vec<nori_engine::watch::Seen>>,
}

impl nori_engine::watch::Watch for Watching {
    fn wanted(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    fn seen(&self, seen: &nori_engine::watch::Seen) {
        self.seen.lock().push(seen.clone());
    }
}

#[test]
fn watch_reports_only_when_wanted() {
    let a = music(90.0, 71);
    let files = vec![("a".to_string(), wav(&a), 90_000)];
    let watching = Arc::new(Watching::default());
    let mut app = sim::App::new();
    app.prefs = prefs_off();
    let rig = Rig::build(files, app, Settings::default(), Extra { watch: Some(watching.clone()), ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2));
    watching.on.store(true, Ordering::Relaxed);
    // A minute of music is several bursts.
    assert!(rig.wait_for(30, |r| r.heard.lock().len() > RATE as usize * 2 * 60));
    watching.on.store(false, Ordering::Relaxed);
    let mine = || watching.seen.lock().clone();
    let played: Vec<_> = mine().into_iter().filter(|s| s.playing && s.index == Some(0) && !s.offloaded).collect();
    assert!(played.windows(2).all(|w| w[1].now_ms >= w[0].now_ms), "in the engine's own time: {played:?}");
    let moved = played.len() >= 2 && played.last().unwrap().position_ms > played[0].position_ms && played.iter().any(|s| s.in_output_ms > 0);
    assert!(moved, "the ear moved on between wakes, with music waiting in the output: {played:?}");
    let state = &played.last().unwrap().state;
    assert!(state.starts_with("Playing; playing on 0 (a) at ") && state.contains("reading 0 (a)") && state.contains("transition engine passing") && state.contains("loaders: a: "), "and says where it stands: {state}");
    let told = mine().len();
    rig.run(3_000);
    assert_eq!(mine().len(), told, "not wanted, nothing is made or told");
}

/// With the equalizer on, the chain stays in the path whatever else changes (AutoMix, hi-res, bands).
#[test]
fn equalizer_chain_stays_through_other_changes() {
    let a = music(60.0, 71);
    let files = vec![("a".to_string(), wav(&a), 60_000)];
    let rig = Rig::build(files, sim::App::new(), loud_eq(), Extra { float: true, ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard_f.lock().len() > RATE as usize * 2 * 2));
    assert!(rig.engine.status().chain && rig.engine.status().on_cpu, "{:?}", rig.engine.status());
    let steps = [
        Settings { auto_mix: true, ..loud_eq() },
        Settings { auto_mix: true, crossfade_s: 6, ..loud_eq() },
        Settings { auto_mix: true, hi_res: true, ..loud_eq() },
        Settings { auto_mix: true, ..loud_eq() },
        Settings { auto_mix: true, offload: true, ..loud_eq() },
        Settings { auto_mix: false, ..loud_eq() },
    ];
    for (k, s) in steps.into_iter().enumerate() {
        rig.engine.set_settings(s);
        rig.run(1_000);
        let st = rig.engine.status();
        assert!(st.state == State::Playing && st.index == Some(0) && st.on_cpu, "{k}: {st:?}");
        // Hi-res keeps it too, in float.
        assert!(st.chain, "{k}: a second after the change the chain is in the path: {st:?}");
    }
    rig.engine.stop();
}

/// The status already says each event, and a seek says its place.
#[test]
fn status_current_on_events() {
    let (a, b) = (music(4.0, 90), music(4.0, 91));
    let server = Arc::new(Server::default());
    for (id, s) in [("a", &a), ("b", &b)] {
        server.files.lock().push((id.into(), Arc::new(wav(s))));
    }
    let mut list = Playlist::default();
    list.set(vec!["a".into(), "b".into()], Some(0), false, 0);
    let queue = TestQueue { list: Arc::new(Mutex::new(list)), skip: Vec::new() };
    let library = Songs { server, lengths: vec![("a".into(), 4_000), ("b".into(), 4_000)], store: None };
    let card = common::card::Card::new();
    let clock = Virtual::default();
    let cell: Arc<std::sync::OnceLock<Arc<Engine>>> = Arc::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (engine_of, said) = (cell.clone(), seen.clone());
    let engine = Arc::new(Engine::start_on(library, sim::App::new(), queue, Box::new(card.clone()), None, Config::default(), clock.clone(), move |e| {
        if let Some(engine) = engine_of.get() {
            let (state, index) = engine.status_with(|s| (s.state, s.index));
            said.lock().push((e, state, index));
        }
    }));
    let _ = cell.set(engine.clone());
    let time = Stepper::new(clock, card.pull.clone());
    engine.play_at(0, 0);
    assert!(time.until(Duration::from_secs(10), || engine.status().index == Some(0) && card.secs() > 1.0));
    engine.seek(2_000);
    assert!(time.until(Duration::from_secs(20), || engine.status().state == State::Ended), "{:?}", seen.lock());
    engine.stop();
    let seen = seen.lock();
    for (e, state, index) in seen.iter() {
        match e {
            Event::State(s) => assert_eq!(s, state, "{seen:?}"),
            Event::Song { index: i, .. } => assert_eq!(Some(*i), *index, "{seen:?}"),
            _ => {}
        }
    }
    assert!(seen.iter().any(|(e, ..)| matches!(e, Event::Position { index: 0, ms: 2_000 })), "the seek said its place: {seen:?}");
    assert!(seen.iter().any(|(e, ..)| matches!(e, Event::Song { index: 1, .. })), "{seen:?}");
}

// ---- the same song twice in a row (issue #19) ----

/// `a` queued twice with AutoMix on, played from `from_ms` in real-time pace.
fn twice(secs: f64, from_ms: i64) -> (Rig, Live) {
    let a = music(secs, 77);
    let ms = (secs * 1000.0) as i64;
    let live = Live::new(TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() });
    live.0.lock().analyses.insert("a".into(), measured("a", 120.0, ms));
    let rig = at_pace(&[("a", &a), ("a", &a)], live.clone(), from_ms);
    (rig, live)
}

/// The planned mix start out of `a` into its second copy, ms.
fn planned_into_itself(live: &Live) -> Option<i64> {
    let log = live.0.lock().log.clone();
    let line = log.iter().find(|l| l.starts_with("transition a -> a"))?;
    line.split(" at ").nth(1)?.split(',').next()?.trim().parse().ok()
}

fn song_events(rig: &Rig) -> Vec<usize> {
    rig.events.lock().iter().filter_map(|e| if let Event::Song { index, .. } = e { Some(*index) } else { None }).collect()
}

/// The song and position read afresh.
fn place(rig: &Rig) -> (Option<usize>, i64) {
    rig.engine.look();
    rig.run(1);
    rig.engine.status_with(|s| (s.index, s.position_ms))
}

#[test]
fn same_song_twice_mixes_at_plan() {
    let (rig, live) = twice(60.0, 30_000);
    assert!(rig.wait_for(10, |_| planned_into_itself(&live).is_some()), "a mix is planned: {:?}", live.0.lock().log);
    let at = planned_into_itself(&live).expect("checked");
    let ear = |r: &Rig| 30_000 + (r.heard.lock().len() / 2) as i64 * 1000 / RATE as i64;
    assert!(rig.wait_for(60, |r| ear(r) >= at - 1_000), "reaches the mix");
    assert_eq!(song_events(&rig), [0], "still on the first copy a second before the mix");
    assert!(rig.wait_for(60, |r| r.engine.status().index == Some(1)), "on to the second copy: {:?}", rig.events.lock());
    let switched = ear(&rig);
    assert!(switched >= at, "the second copy is said once the mix is heard ({switched} ms), not before the plan ({at} ms)");
    let (i1, p1) = place(&rig);
    rig.run(5_000);
    let (i2, p2) = place(&rig);
    assert_eq!((i1, i2), (Some(1), Some(1)), "{:?}", rig.events.lock());
    assert!(p1 < 20_000 && (4_500..5_500).contains(&(p2 - p1)), "the second copy plays on from its start: {p1} -> {p2}");
    assert_eq!(song_events(&rig), [0, 1], "one change of song");
    rig.engine.stop();
}

#[test]
fn same_song_twice_seek_stays_in_first_copy() {
    let (rig, live) = twice(90.0, 20_000);
    assert!(rig.wait_for(10, |_| planned_into_itself(&live).is_some()), "a mix is planned: {:?}", live.0.lock().log);
    // A seek bar at -0:50.
    rig.engine.seek(40_000);
    rig.run(3_000);
    let (index, ms) = place(&rig);
    assert_eq!(index, Some(0), "still the first copy: {:?}", rig.events.lock());
    assert!((42_500..43_500).contains(&ms), "plays on from the seek: {ms}");
    assert_eq!(song_events(&rig), [0], "no change of song");
    assert!(rig.wait_for(60, |r| r.engine.status().index == Some(1)), "on to the second copy: {:?}", rig.events.lock());
    let (_, p1) = place(&rig);
    rig.run(3_000);
    let (_, p2) = place(&rig);
    assert!(p1 < 20_000 && p2 >= p1 + 2_500, "the second copy plays on: {p1} -> {p2}");
    rig.engine.stop();
}

#[test]
fn repeat_one_with_automix_plays_on() {
    let a = music(60.0, 78);
    let live = Live::new(TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() });
    live.0.lock().analyses.insert("a".into(), measured("a", 120.0, 60_000));
    let rig = at_pace(&[("a", &a)], live.clone(), 30_000);
    rig.engine.set_repeat(nori_player::playlist::REPEAT_ONE);
    assert!(rig.wait_for(60, |r| r.events.lock().iter().any(|e| matches!(e, Event::Looped { .. }))), "loops: {:?}", rig.events.lock());
    // Past the mix, playing on from near the start.
    rig.run(15_000);
    let (i1, p1) = place(&rig);
    rig.run(3_000);
    let (i2, p2) = place(&rig);
    assert_eq!((i1, i2), (Some(0), Some(0)));
    assert!(p1 < 30_000 && p2 >= p1 + 2_500, "plays on from the start: {p1} -> {p2}");
    assert_eq!(rig.events.lock().iter().filter(|e| matches!(e, Event::Looped { .. })).count(), 1, "one loop: {:?}", rig.events.lock());
    assert!(!rig.ended());
    rig.engine.stop();
}

include!("perf_bench.rs");
