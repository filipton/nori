//! The engine end to end on the virtual clock: WAV songs from a counting fake server, a recording card,
//! and `nori_player::sim` as the reference it must match sample for sample. Tests wait with
//! [`Rig::wait_for`] and [`Rig::run`], never real sleeps.

use crate::common;

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{Signal, Stepper, Virtual};
use nori_engine::{App, AudioOutput, Body, ByteSource, Config, Device, DeviceWatch, Engine, Event, Feed, Library, Located, OutputFacts, OutputFormat, OutputKind, Settings, Source, State, Store};
use nori_player::automix::analysis::Analyzer;
use nori_player::automix::synth::Rng;
use nori_player::automix::ANALYSIS_VERSION;
use nori_player::engine::{Host, Plan};
use nori_player::playlist::Playlist;
use nori_player::sim::{self, prefs_off, Audio};
use nori_player::compressor::CompressorPreset;
use nori_player::dsp::Effects;
use nori_player::pipeline::ChainSettings;
use common::reference;
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
    /// Songs whose answer is delayed this long on the rig's clock.
    slow: Mutex<Vec<(String, Duration)>>,
    clock: Mutex<Option<Virtual>>,
    /// Songs whose connection breaks at this byte, for good.
    cut: Mutex<Vec<(String, u64)>>,
    /// Songs whose first answer breaks at this byte and whose later answers come this long after.
    gap: Mutex<Vec<(String, u64, Duration)>>,
    /// Plain answers the loader let go of (read whole, or given up).
    let_go: Arc<Signal>,
}

/// A plain answer, telling the server when it is let go of.
struct Answer(Cursor<Bytes>, Arc<Signal>);

impl std::io::Read for Answer {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Drop for Answer {
    fn drop(&mut self) {
        self.1.bump();
    }
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
        self.requests.lock().push((url.to_string(), from));
        let first = self.requests.lock().iter().filter(|(u, _)| u == url).count() == 1;
        let gap = self.gap.lock().iter().find(|(u, ..)| u == url).map(|g| (g.1, g.2));
        let slow = match gap {
            Some((_, d)) if !first => Some(d),
            _ => self.slow.lock().iter().find(|(u, _)| u == url).map(|s| s.1),
        };
        if let (Some(d), Some(clock)) = (slow, self.clock.lock().clone()) {
            clock.wait_until(clock.now_ns() + d.as_nanos() as i64);
        }
        let file = self.files.lock().iter().find(|(u, _)| u == url).map(|(_, f)| f.clone()).ok_or("404")?;
        let len = file.len() as u64;
        let mut c = Cursor::new(Bytes(file));
        c.set_position(from);
        if let Some((at, _)) = gap.filter(|_| first) {
            return Ok(Body { start: from, len: Some(len), reader: Box::new(Broken(c, at)) });
        }
        if let Some(at) = self.cut.lock().iter().find(|(u, _)| u == url).map(|s| s.1) {
            if from >= at {
                return Err("connection refused".into());
            }
            return Ok(Body { start: from, len: Some(len), reader: Box::new(Broken(c, at)) });
        }
        Ok(Body { start: from, len: Some(len), reader: Box::new(Answer(c, self.let_go.clone())) })
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

    fn skips(&self, list: &Playlist, index: usize) -> bool {
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
    /// The songs are on disk, each cut into [`PIECES`] files.
    on_disk: bool,
    /// Idle release, ms.
    idle_release_ms: Option<i64>,
    /// Music seconds per [`Rig::wait_for`] second (default 20).
    pace: Option<f64>,
    /// Memory class, MB (default 256).
    memory_mb: Option<u32>,
    watch: Option<Arc<dyn nori_engine::watch::Watch>>,
    /// The device holds this much music taken from the ring before it plays it, ms, as a phone's
    /// AudioTrack does; [`SHALLOW_MS`] at most while shallow.
    hold_ms: Option<usize>,
    /// The songs' file type (default WAV).
    hint: Option<&'static str>,
}

/// Files each song is cut into with [`Extra::on_disk`].
const PIECES: usize = 3;

struct Songs {
    server: Arc<Server>,
    lengths: Vec<(String, i64)>,
    store: Option<Arc<Store>>,
    pieces: Option<Arc<nori_testdir::TempDir>>,
    hint: &'static str,
}

impl Library for Songs {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let duration_ms = self.lengths.iter().find(|(i, _)| i == id).map(|s| s.1);
        let bytes: Arc<dyn ByteSource> = self.server.clone();
        let url = id.to_string();
        let source = match (&self.store, &self.pieces) {
            (_, Some(dir)) => Source::File((0..PIECES).map(|k| dir.join(format!("{id}.{k}"))).collect()),
            (Some(store), None) => Source::Cached { url, bytes, store: store.clone(), key: format!("{id}:0") },
            (None, None) => Source::Url { url, bytes },
        };
        Ok(Located { source, hint: Some(self.hint.into()), duration_ms, estimated: false })
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
    /// Frames it holds before playing them deep ([`Extra::hold_ms`]) and now, and those it holds.
    deep: usize,
    hold: usize,
    held: std::collections::VecDeque<i16>,
}

/// Frames the card pulls at a time.
const BLOCK: usize = 128;
/// What a holding card holds while shallow, ms: a phone's track over Bluetooth.
const SHALLOW_MS: usize = 400;

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
        let ch = feed.format().channels;
        if self.deep > 0 {
            return self.held_tick();
        }
        let waits = feed.engine_waits();
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

impl Card {
    /// A device holding music: keeps [`Card::hold`] frames taken ahead, plays a block of them. On a
    /// flush it drops what it holds and gives back what it did not play.
    fn held_tick(&mut self) -> bool {
        let Some(feed) = self.feed.as_mut() else { return false };
        let ch = feed.format().channels;
        let mut woke = false;
        while self.held.len() / ch < self.hold && feed.available() > 0 {
            let waits = feed.engine_waits();
            self.block.resize(BLOCK * ch, 0);
            let got = feed.pull_i16(&mut self.block);
            woke |= waits && !feed.engine_waits();
            if feed.flushed() {
                let back = self.held.len() / ch + got;
                self.held.clear();
                feed.rewind(back as u64);
                continue;
            }
            self.held.extend(&self.block[..got * ch]);
        }
        let n = (BLOCK * ch).min(self.held.len());
        if n < BLOCK * ch && !feed.ending() {
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
        self.heard.lock().extend(self.held.drain(..n));
        woke
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
        let c = self.card.lock();
        (c.held.len() / 2) as u64 * 1_000_000 / RATE as u64
    }

    fn holding(&self) -> bool {
        !self.card.lock().held.is_empty()
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

    /// Shallow, it takes no more until it has played down to [`SHALLOW_MS`].
    fn shallow(&mut self, on: bool) {
        self.shallow.store(on, Ordering::Relaxed);
        let mut c = self.card.lock();
        c.hold = if on { c.deep.min(SHALLOW_MS * RATE as usize / 1000) } else { c.deep };
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
        let Extra { float, skip, server, store, on_disk, idle_release_ms, pace, memory_mb, watch: watching, hold_ms, hint } = extra;
        for (id, f, _) in &files {
            server.files.lock().push((id.clone(), Arc::new(f.clone())));
        }
        let pieces = on_disk.then(|| Arc::new(nori_testdir::TempDir::new("pieces")));
        for (dir, (id, f, _)) in pieces.iter().flat_map(|d| files.iter().map(move |f| (d, f))) {
            for (k, piece) in f.chunks(f.len().div_ceil(PIECES)).enumerate() {
                std::fs::write(dir.join(format!("{id}.{k}")), piece).unwrap();
            }
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
            deep: hold_ms.unwrap_or(0) * RATE as usize / 1000,
            hold: hold_ms.unwrap_or(0) * RATE as usize / 1000,
            held: Default::default(),
        }));
        let out = Recorder { card: card.clone(), opened: Arc::default(), shut: Arc::default(), watch: Arc::default(), flushes: Arc::default(), shallow: Arc::default() };
        let (opened, shut, watch, flushes, shallow) = (out.opened.clone(), out.shut.clone(), out.watch.clone(), out.flushes.clone(), out.shallow.clone());
        let clock = Virtual::default();
        *server.clock.lock() = Some(clock.clone());
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let library = Songs { server: server.clone(), lengths, store, pieces, hint: hint.unwrap_or("wav") };
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
fn simulated(songs: &[(&str, &[i16])], prefs: TransitionPrefs) -> Vec<i16> {
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
fn paused_jump_fetches_nothing() {
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

/// Well into a song, previous restarts it, or goes to the song before with "previous always skips";
/// playing or paused.
#[test]
fn previous_follows_the_setting() {
    let (a, b) = (music(20.0, 97), music(20.0, 98));
    let mut wrong = Vec::new();
    for (always_skips, paused, want) in [(false, false, 1), (true, false, 0), (false, true, 1), (true, true, 0)] {
        let rig = Rig::new(&[("a", &a), ("b", &b)], prefs_off(), Settings { previous_always_skips: always_skips, ..Settings::default() });
        rig.engine.play_at(1, 0);
        assert!(rig.wait_for(10, |r| r.engine.status().position_ms > 5_000));
        if paused {
            rig.engine.pause();
            assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
        }
        rig.engine.previous();
        if !rig.wait_for(5, |r| { let s = r.engine.status(); s.index == Some(want) && s.position_ms < 3_000 }) {
            wrong.push(format!("always skips {always_skips}, paused {paused}: {:?}", rig.engine.status()));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
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
    let expected = simulated(&songs, crossfade(6));
    let rig = Rig::new(&songs, crossfade(6), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "played to the end: {:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert_eq!(heard.len(), expected.len(), "80 s less the 6 s overlap ({} waits)", rig.waits());
    assert_eq!(heard.len(), a.len() + b.len() - RATE as usize * 2 * 6);
    let first = heard.iter().zip(&expected).position(|(x, y)| x != y);
    assert_eq!(first, None, "the mix starts at the planned sample and sounds the same");
}

/// The next song is said when its mix takes over whether or not a screen asks for positions (the
/// notification, a car's display and scrobbling follow it with the screen off). In a crossfade and in
/// an AutoMix: milliseconds of the clock at which `b` was said.
#[test]
fn mixed_song_said_on_time() {
    let (a, b) = (music(30.0, 61), music(30.0, 62));
    let said_at = |prefs: TransitionPrefs, settings: Settings, screen: bool| {
        let mut app = sim::App::new();
        app.prefs = prefs;
        app.analyses.insert("a".into(), measured("a", 120.0, 30_000));
        app.analyses.insert("b".into(), measured("b", 120.0, 30_000));
        let rig = Rig::with_app(&[("a", &a), ("b", &b)], app, settings);
        if screen {
            rig.engine.position_updates(Some(Duration::from_millis(250)));
        }
        rig.engine.play_at(0, 0);
        let said = |r: &Rig| r.events.lock().iter().any(|e| matches!(e, Event::Song { id, .. } if id == "b"));
        assert!(rig.wait_for(60, said), "{:?}", rig.events.lock());
        rig.now_ms()
    };
    let automix = TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() };
    for (name, prefs, settings) in [("crossfade", crossfade(6), Settings { crossfade_s: 6, ..Settings::default() }), ("automix", automix, Settings { auto_mix: true, ..Settings::default() })] {
        let (on, off) = (said_at(prefs, settings.clone(), true), said_at(prefs, settings, false));
        assert!((on - off).abs() <= 10, "{name}: said at {on} ms with the screen on, {off} ms with it off");
    }
}

#[test]
fn seeks_land() {
    let a = music(30.0, 4);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let rig = Rig::new(&songs, prefs_off(), Settings::default());
    rig.engine.play_at(0, 20_000);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert!(heard[..] == a[RATE as usize * 2 * 20..], "from 20 s to the end");

    // Every seek lands, however the engine is busy when it is asked: `ask` runs once the rig plays (or
    // not, per case) and returns the song and place asked for; the ear must be there.
    let (a, b, c) = (music(30.0, 61), music(30.0, 62), music(30.0, 63));
    type Ask = fn(&Rig) -> (usize, i64);
    let fade = Settings { fade_ms: 400, ..Settings::default() };
    let cases: [(&str, TransitionPrefs, Settings, bool, Ask); 9] = [
        ("while opening", prefs_off(), Settings::default(), false, |r| { r.engine.play_at(0, 0); r.engine.go_to(0, 12_000); (0, 12_000) }),
        ("before the first bytes", prefs_off(), Settings::default(), false, |r| {
            r.server.slow.lock().push(("a".into(), Duration::from_millis(1_500)));
            r.engine.play_at(0, 0);
            r.run(300);
            r.engine.go_to(0, 12_000);
            (0, 12_000)
        }),
        ("by seek while opening", prefs_off(), Settings::default(), false, |r| { r.engine.play_at(0, 0); r.engine.seek(12_000); (0, 12_000) }),
        ("playing", prefs_off(), Settings::default(), true, |r| { r.engine.go_to(0, 12_000); (0, 12_000) }),
        ("in a dip", prefs_off(), fade.clone(), true, |r| { r.engine.go_to(0, 5_000); r.run(100); r.engine.go_to(0, 12_000); (0, 12_000) }),
        ("after a skip in its dip", prefs_off(), fade.clone(), true, |r| { r.engine.next(); r.engine.go_to(1, 7_000); (1, 7_000) }),
        ("paused", prefs_off(), Settings::default(), true, |r| {
            r.engine.pause();
            r.run(100);
            r.engine.go_to(0, 12_000);
            r.run(500);
            r.engine.play();
            (0, 12_000)
        }),
        ("in a mix", crossfade(6), Settings::default(), true, |r| {
            assert!(r.wait_for(20, |r| r.engine.status().mixing));
            let shown = place(r).0.expect("a song shown");
            r.engine.go_to(shown, 3_000);
            (shown, 3_000)
        }),
        ("with the queue edited in its dip", prefs_off(), fade.clone(), true, |r| {
            r.engine.go_to(0, 8_000);
            r.run(50);
            r.queue.lock().insert(0, vec!["c".into()], nori_player::playlist::Hand::No);
            r.engine.queue_changed();
            (1, 8_000)
        }),
    ];
    for (name, prefs, settings, play_first, ask) in cases {
        let rig = Rig::new(&[("a", &a), ("b", &b), ("c", &c)], prefs, settings);
        if play_first {
            rig.engine.play_at(0, 0);
            assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2), "{name}: plays");
        }
        let (index, ms) = ask(&rig);
        let id = rig.queue.lock().ids()[index].clone();
        let song = match id.as_str() { "a" => &a, "b" => &b, _ => &c };
        let heard = rig.heard.lock().len();
        assert!(rig.wait_for(10, |r| r.heard.lock().len() > heard + 3 * RATE as usize * 2), "{name}: music after the seek");
        let at = heard_in(&rig, song).unwrap_or_else(|| panic!("{name}: {id} is not what plays: {:?}", rig.events.lock()));
        assert!((ms + 1_000..ms + 4_000).contains(&at), "{name}: {id} plays at {at} ms, the seek asked for {ms}");
        assert_eq!(place(&rig).0, Some(index), "{name}: shown on its song");
    }
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

/// Playing, the engine wakes once per burst: the ring drained to its low mark.
#[test]
fn plain_playback_wakes_once_per_burst() {
    let a = vec![8000i16; RATE as usize * 2 * 90];
    let rig = Rig::new(&[("a", &a)], prefs_off(), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 10));
    // The song is in memory, as a network faster than playback has it: no burst waits for its bytes.
    rig.server.let_go.reach(1);
    let sleeps = rig.time.clock.sleeps();
    rig.run(60_000);
    let bursts = 60_000_000 / (nori_player::burst::BUFFER_US - nori_engine::output::WAKE_LOW_US) as u64 + 1;
    assert!(rig.time.clock.sleeps() - sleeps <= bursts, "{} wakes in a minute", rig.time.clock.sleeps() - sleeps);
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

fn placed(rig: &Rig) -> Vec<i64> {
    rig.events.lock().iter().filter_map(|e| if let Event::Placed { ms, .. } = e { Some(*ms) } else { None }).collect()
}

/// Behind an output holding seconds of music (a phone's track), the place is the one heard, not the one
/// written: a device mirroring this one shows what the listener hears.
#[test]
fn a_deep_output_says_the_place_heard() {
    let a = vec![8000i16; RATE as usize * 2 * 60];
    let files = vec![("a".to_string(), wav(&a), 60_000)];
    let rig = Rig::build(files, sim::App::new(), Settings::default(), Extra { hold_ms: Some(2_000), ..Extra::default() });
    rig.engine.play_at(0, 0);
    let heard_ms = |r: &Rig| (r.heard.lock().len() / 2) as i64 * 1000 / RATE as i64;
    assert!(rig.wait_for(10, |r| heard_ms(r) > 10_000));
    rig.engine.look();
    assert!(rig.time.until(Duration::from_millis(20), || (heard_ms(&rig) - rig.engine.status().position_ms).abs() < 30), "{} ms heard, the status {:?}", heard_ms(&rig), rig.engine.status());
    assert_eq!(placed(&rig), [0; 0], "the place heard ran on as said");
}

/// The output holding the music back a moment (a glitch, a slow clock) takes the place heard away from
/// where the one said runs on to: it is said again, so a client running it on follows.
#[test]
fn a_place_heard_away_from_the_one_said_is_said_again() {
    let a = vec![8000i16; RATE as usize * 2 * 60];
    let rig = Rig::new(&[("a", &a)], prefs_off(), Settings::default());
    rig.engine.play_at(0, 0);
    let heard_ms = |r: &Rig| (r.heard.lock().len() / 2) as i64 * 1000 / RATE as i64;
    assert!(rig.wait_for(10, |r| heard_ms(r) > 5_000));
    rig.card.lock().playing = false;
    rig.run(300);
    rig.card.lock().playing = true;
    rig.engine.look();
    assert!(rig.time.until(Duration::from_millis(50), || !placed(&rig).is_empty()), "{:?}", rig.events.lock());
    let said = placed(&rig)[0];
    assert!((said - heard_ms(&rig)).abs() < 30, "said again at {said} ms, {} ms heard", heard_ms(&rig));
}

#[test]
fn pause_fades() {
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

    // Fade setting applies to next pause.
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
        let said = |r: &Rig| r.events.lock()[from..].iter().any(|e| matches!(e, Event::Song { index: 0, id, jumps, .. } if id == "b" && *jumps >= jump));
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

/// A seek into bytes not yet fetched says it waits, holds the place it was sent to while nothing is
/// heard, and plays on from there once they come.
#[test]
fn seek_waiting_for_bytes_holds_the_place() {
    let a = music(60.0, 83);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let extra = Extra::default();
    extra.server.gap.lock().push(("a".into(), 10 * RATE as u64 * 4, Duration::from_secs(3)));
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    let before = rig.events.lock().len();
    rig.engine.seek(40_000);
    assert!(rig.wait_for(5, |r| r.events.lock()[before..].contains(&Event::Buffering(true))), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().len();
    rig.run(1_000);
    assert_eq!(rig.heard.lock().len(), heard, "nothing is heard while the bytes are on their way");
    assert_eq!(rig.engine.status().position_ms, 40_000, "the place stands where it was sent");
    assert!(rig.wait_for(30, |r| r.heard.lock().len() > heard + 1000), "{:?}", rig.events.lock());
    assert!(rig.events.lock()[before..].contains(&Event::Buffering(false)), "and says it waits no more");
    assert!(rig.engine.status().position_ms >= 40_000, "{:?}", rig.engine.status());
}

/// Buffering(true) is ended when the music stops waiting another way (held elsewhere, released).
#[test]
fn buffering_ends_on_move() {
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

/// Play fades in from silence by the fade setting, whether the song's first bytes came before the device
/// opened or after.
#[test]
fn play_fades_in() {
    let a = vec![8000i16; RATE as usize * 2 * 10];
    for (case, on_disk, late) in [("bytes at once", true, false), ("bytes late", false, true)] {
        let extra = Extra { on_disk, ..Extra::default() };
        if late {
            extra.server.slow.lock().push(("a".into(), Duration::from_millis(500)));
        }
        let rig = Rig::build(files(&[("a", &a)]), sim::App::new(), Settings { fade_ms: 1_000, ..Settings::default() }, extra);
        rig.engine.play_at(0, 0);
        assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2));
        let heard = rig.heard.lock().clone();
        let half = RATE as usize;
        assert!(heard[0] < 100 && (3_000..5_000).contains(&heard[half]), "{case}: up from silence, halfway at half a second: {} then {}", heard[0], heard[half]);
        assert!(heard[RATE as usize * 2 * 11 / 10..].iter().all(|&v| v == 8000), "{case}: then the song as it is");
    }
}

#[test]
fn pause_now_cuts() {
    // Headphones out: pause at once, whatever the fade setting.
    let a = vec![8000i16; RATE as usize * 2 * 20];
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let rig = Rig::new(&songs, prefs_off(), Settings { fade_ms: 1_000, ..Settings::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 4));
    rig.engine.pause_now();
    assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
    rig.run(4_000);
    let heard = rig.heard.lock().clone();
    // Past the fade in from play.
    assert!(heard[RATE as usize * 2 * 11 / 10..].iter().all(|&v| v == 8000), "cut, not faded: no sample on the way down");
    rig.engine.stop();

    // Headphones out during a pause fade stop it there.
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

    // Replay gain per song through crossfade.
    let (a, b) = (music(40.0, 2), music(40.0, 3));
    // Each song at its own gain, then mixed. Regression: one output volume put a's gain on b.
    let ideal = simulated(&[("a", &at(&a, 0.5)), ("b", &b)], crossfade(6));
    let mut app = sim::App::new();
    app.prefs = crossfade(6);
    app.gains.insert("a".into(), 0.5);
    let rig = Rig::with_app(&[("a", &a), ("b", &b)], app, Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert_eq!(heard.len(), ideal.len());
    assert_eq!(beyond_dither(&heard, &ideal), None, "every sample as a at half its level mixed into b");

    // Replay gain per song through automix.
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

fn rms(s: &[i16]) -> f64 {
    (s.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / s.len().max(1) as f64).sqrt()
}

#[test]
fn gain_boost_uses_float_limiter() {
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
    music(secs, seed).iter().enumerate().map(|(i, v)| ((*v as i32) << 8) | ((i as i32 * 37) & 0xFF)).collect()
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

    // Hi-res with the equalizer runs the chain on all 24 bits (the 16-bit chain lost the low byte).
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

/// Regression: a 48 kHz song with a 44.1 kHz one read ahead (the device waiting to open again at its
/// rate) went on at 44.1 kHz after previous restarted it, a seek or a jump to it: too slow; with a
/// device holding music, nothing played at all. Through the dip or not; a jump to the next song too.
#[test]
fn moves_while_the_next_rate_waits() {
    let (a, b) = (common::sine(48_000, 440.0, 30.0, 8_000.0), common::sine(RATE, 660.0, 30.0, 8_000.0));
    let files = || vec![("a".to_string(), wav_at(&a, 48_000), 30_000), ("b".to_string(), wav(&b), 30_000)];
    // What is done, and the rate and tone heard after it.
    let moves = [
        ("previous", (|e: &Engine| _ = e.previous()) as fn(&Engine), 48_000, 440.0),
        ("a seek to 0", |e| e.seek(0), 48_000, 440.0),
        ("a jump to a", |e| _ = e.go_to(0, 0), 48_000, 440.0),
        ("a jump to b", |e| _ = e.go_to(1, 0), RATE, 660.0),
    ];
    let mut wrong = Vec::new();
    for (what, mv, want_rate, want_hz) in moves {
        for (fade_ms, hold_ms) in [(0, None), (150, None), (0, Some(1_500))] {
            let case = format!("{what}, a {fade_ms} ms dip, the device holding {hold_ms:?} ms");
            let rig = Rig::build(files(), sim::App::new(), Settings { fade_ms, ..Settings::default() }, Extra { hold_ms, ..Extra::default() });
            rig.engine.play_at(0, 0);
            // Read on into b well before a ends.
            assert!(rig.wait_for(60, |r| r.engine.status().position_ms > 24_000), "{case}");
            mv(&rig.engine);
            // The device's rate, the tone of the last half second heard at it, and the frames heard.
            let heard = |rig: &Rig| {
                let rate = rig.card.lock().feed.as_ref().map_or(0, |f| f.format().rate);
                let left: Vec<f64> = rig.heard.lock().iter().step_by(2).map(|&v| v as f64).collect();
                let last = &left[left.len().saturating_sub(rate as usize / 2)..];
                let ups = last.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
                (rate, ups as f64 * 2.0, left.len())
            };
            if !rig.wait_for(10, |r| (2_000..10_000).contains(&r.engine.status().position_ms)) {
                let s = rig.engine.status();
                wrong.push(format!("{case}: stuck on {:?} at {} ms", s.id, s.position_ms));
                continue;
            }
            let before = heard(&rig);
            rig.engine.pause();
            rig.run(1_000);
            rig.engine.play();
            rig.run(3_000);
            let after = heard(&rig);
            for (when, (rate, hz, _)) in [("", before), (" after a pause", after)] {
                if rate != want_rate || (hz - want_hz).abs() > 4.0 {
                    wrong.push(format!("{case}{when}: {hz} Hz at {rate} Hz"));
                }
            }
            if after.2 < before.2 + want_rate as usize * 2 {
                wrong.push(format!("{case}: {} frames heard in the 3 s after the pause", after.2 - before.2));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Regression: the device was told the music ends where a song at another rate was read ahead (it plays
/// out before opening again), and kept being told so when another song at its own rate came to follow
/// instead: a starts-when-full AudioTrack was stopped whenever the ring ran empty between bursts.
#[test]
fn music_goes_on_where_the_next_rate_no_longer_follows() {
    let (a, b, x) = (common::sine(48_000, 440.0, 30.0, 8_000.0), common::sine(RATE, 660.0, 30.0, 8_000.0), common::sine(48_000, 550.0, 30.0, 8_000.0));
    let files = vec![("a".to_string(), wav_at(&a, 48_000), 30_000), ("b".to_string(), wav(&b), 30_000), ("x".to_string(), wav_at(&x, 48_000), 30_000)];
    let rig = Rig::build(files, sim::App::new(), Settings::default(), Extra::default());
    rig.queue.lock().set(vec!["a".into(), "b".into()], Some(0), false, 0);
    rig.engine.queue_changed();
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(60, |r| r.card.lock().feed.as_ref().is_some_and(|f| f.ending())), "b is read ahead: the device plays a out");
    rig.queue.lock().insert(1, vec!["x".into()], nori_player::playlist::Hand::No);
    rig.engine.queue_changed();
    rig.engine.replan();
    rig.run(200);
    let mut told_ending_ms = None;
    assert!(rig.wait_for(60, |r| {
        let s = r.engine.status();
        if told_ending_ms.is_none() && s.index == Some(0) && r.card.lock().feed.as_ref().is_some_and(|f| f.ending()) {
            told_ending_ms = Some(s.position_ms);
        }
        s.index == Some(1) && s.position_ms > 2_000
    }), "{:?}", rig.engine.status());
    assert_eq!(told_ending_ms, None, "the device was told the music ends in a, with x to follow");
    assert_eq!(rig.card.lock().feed.as_ref().map(|f| f.format().rate), Some(48_000));
    let left: Vec<f64> = rig.heard.lock().iter().step_by(2).map(|&v| v as f64).collect();
    let ups = left[left.len() - 48_000..].windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    assert!((ups as i64 - 550).abs() <= 2, "x at its pitch: {ups} Hz");
}

#[test]
fn device_format_choices() {
    // A gapless join into another rate reopens the device at that rate instead of resampling.
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

    // With a 48 kHz maximum, 96 kHz is halved at its own pitch and level; 44.1 kHz passes.
    let secs = 3.0;
    let tone: Vec<i16> = (0..(96_000.0 * secs) as usize).flat_map(|i| [((i as f64 * 1000.0 * std::f64::consts::TAU / 96_000.0).sin() * 16000.0).round() as i16; 2]).collect();
    let files = vec![("hi".to_string(), wav_at(&tone, 96_000), 3_000)];
    let rig = Rig::build(files, sim::App::new(), Settings { max_rate: 48_000, ..Settings::default() }, Extra::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert_eq!(rig.card.lock().feed.as_ref().map(|f| f.format().rate), Some(48_000), "opened at 48 kHz");
    let heard: Vec<f64> = rig.heard.lock().as_chunks::<2>().0.iter().map(|c| c[0] as f64).collect();
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

    // Device 16 bit gets 16 bit chain.
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
fn failed_songs() {
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

    // Play retries failed song.
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
    let store = Store::open(dir.path(), 64 << 20).unwrap();
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
fn released_at_once_when_the_music_moves_away() {
    let a = music(12.0, 19);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), Extra::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 3));
    rig.engine.release_now();
    assert!(rig.wait_for(5, |r| r.shut.load(Ordering::Relaxed) == 1), "let go without the idle time");
    assert_eq!((rig.engine.status().releases, rig.engine.status().state), (1, State::Paused));
    rig.engine.play();
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert_eq!(rig.opened.load(Ordering::Relaxed), 2, "opened again on play");
}

#[test]
fn idle_release_after_queue_ends() {
    let a = music(2.0, 19);
    let songs: [(&str, &[i16]); 1] = [("a", &a)];
    let extra = Extra { idle_release_ms: Some(300), ..Extra::default() };
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert!(rig.wait_for(5, |r| r.shut.load(Ordering::Relaxed) == 1), "let go after the idle time");
    assert_eq!(rig.engine.status().releases, 1);
    assert!(rig.wait_for(5, |r| r.engine.held().songs == 0), "the song's bytes let go: {:?}", rig.engine.held());
    rig.heard.lock().clear();
    rig.engine.play();
    assert!(rig.wait_for(30, |r| r.heard.lock().len() >= a.len() && r.engine.status().state == State::Ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    let head = RATE as usize * 2;
    assert!(heard[..head] == a[..head], "played again from its start");
}

#[test]
fn song_in_pieces_plays_from_disk() {
    let (a, b) = (music(6.0, 19), music(4.0, 20));
    let songs: [(&str, &[i16]); 2] = [("a", &a), ("b", &b)];
    let extra = Extra { on_disk: true, ..Extra::default() };
    let rig = Rig::build(files(&songs), sim::App::new(), Settings::default(), extra);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    assert_eq!(rig.engine.held(), nori_engine::Held::default(), "no song's bytes in memory");
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let joined = [a, b].concat();
    assert!(*rig.heard.lock() == joined, "both songs whole, across their pieces");
    assert!(rig.server.requests.lock().is_empty(), "nothing asked of the network");
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
    // Faded in over the chain's 10 ms.
    let off = heard[at + 2 * RATE as usize / 50..].iter().position(|&v| (v - 4000).abs() > 1);
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
    let mut d = nori_engine::demux::Demuxed::open(Box::new(Cursor::new(file)), None, 0, None, nori_player::pcm::Encoding::Pcm16, None).unwrap();
    let mut out = Vec::new();
    while nori_player::pipeline::Reading::fill(&mut d) {
        out.extend(nori_player::pipeline::Reading::buffer(&d).as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)));
    }
    assert!(out == a, "the song after the tag, sample for sample: {} of {} samples", out.len(), a.len());
}

/// A stop from before a later play is superseded, and that play retries the song. Regression: a client
/// honouring the stale stop showed paused over playing music.
#[test]
fn stop_before_play_superseded() {
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

/// `s` scaled by `gain`, rounded as the player rounds.
fn at(s: &[i16], gain: f32) -> Vec<i16> {
    s.iter().map(|&v| (v as f32 * gain).round() as i16).collect()
}

/// The first sample of `heard` further than dither allows from `ideal`.
fn beyond_dither(heard: &[i16], ideal: &[i16]) -> Option<usize> {
    heard.iter().zip(ideal).position(|(h, i)| (*h as i32 - *i as i32).abs() > 2)
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

    fn gain(&mut self, list: &nori_player::playlist::Playlist, index: usize) -> f32 {
        self.0.lock().gain(list, index)
    }

    fn spliced(&mut self, what: &str, at: nori_player::pipeline::Splice) {
        self.0.lock().spliced(what, at);
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
fn bit_perfect_drops_replay_gain() {
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
fn crossfade_setting_changes() {
    let (a, b) = (music(30.0, 36), music(10.0, 37));
    let live = Live::new(crossfade(6));
    let rig = playing(&[("a", &a), ("b", &b)], live.clone(), Settings::default());
    live.0.lock().prefs = prefs_off();
    rig.engine.replan();
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().clone();
    assert!(heard.len() == a.len() + b.len() && heard[..a.len()] == a[..] && heard[a.len()..] == b[..], "a then b, every sample");

    // Longer crossfade applies to next mix.
    let (a, b) = (music(30.0, 34), music(20.0, 35));
    let live = Live::new(crossfade(2));
    let rig = playing(&[("a", &a), ("b", &b)], live.clone(), Settings::default());
    live.0.lock().prefs = crossfade(6);
    rig.engine.replan();
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    assert_eq!(rig.heard.lock().len(), a.len() + b.len() - RATE as usize * 2 * 6, "six seconds of overlap, not two");
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
fn automix_switched_on() {
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

    // Automix on near end mixes.
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

// ---- the sound changed while music plays: heard as rendered offline ----

/// The chain the engine builds from `s` (not bit-perfect, no turned-up gain).
fn chain_of(s: &Settings) -> ChainSettings {
    ChainSettings { sound: s.sound.clone(), speed: s.speed, pitch: s.pitch, skip_silence: s.skip_silence, keep_eq: true }
}

/// Plays `raw` as the one song (or the songs `songs` whose chain input `raw` is), changing the settings
/// to each of `steps` once the card has heard its ms, and returns the rig and its log.
fn stepped(songs: &[(&str, &[i16])], prefs: TransitionPrefs, first: Settings, steps: &[(i64, Settings)]) -> (Rig, Live, Vec<usize>) {
    let live = Live::new(prefs);
    let rig = Rig::with_app(songs, live.clone(), first);
    rig.engine.play_at(0, 0);
    let mut asked = Vec::new();
    for (ms, s) in steps {
        let frames = (*ms * RATE as i64 / 1000) as usize;
        assert!(rig.wait_for(60, |r| r.heard.lock().len() >= frames * 2), "{ms} ms heard");
        asked.push(rig.heard.lock().len() / 2);
        rig.engine.set_settings(s.clone());
        // Apart by more than the engine gathers changes.
        rig.run(150);
    }
    assert!(rig.wait_for(120, Rig::ended), "{:?}", rig.events.lock());
    (rig, live, asked)
}

/// What the card heard is `raw` through the chain as first set, then as each step set it from where the
/// engine said the step started, spliced in there; each started within `late_ms` of the ear (the chain
/// is run again in steps, and a stage holding input makes output in bursts).
fn heard_as_rendered(rig: &Rig, live: &Live, raw: &[i16], first: &Settings, steps: &[(i64, Settings)], asked: &[usize], late_ms: usize) {
    let splices = live.0.lock().splices.clone();
    assert_eq!(splices.len(), steps.len(), "one splice per change: {splices:?}");
    let mut changes = vec![(0, chain_of(first))];
    let mut want = reference::render(raw, RATE, &changes);
    for ((splice, (_, s)), asked) in splices.iter().zip(steps).zip(asked) {
        assert!(splice.output as usize >= *asked && splice.output as usize <= asked + late_ms * RATE as usize / 1000, "the change starts at the ear ({asked}): {splice:?}");
        changes.push((splice.input, chain_of(s)));
        want = reference::spliced(&want, &reference::render(raw, RATE, &changes), splice.output as usize, RATE);
    }
    let heard = rig.heard.lock().clone();
    // The compressor interpolates its gain over runs that end with each buffer, and the engine's buffers
    // are not the rendering's: the gain may differ a little where they split differently.
    let compressed = std::iter::once(first).chain(steps.iter().map(|(_, s)| s)).any(|s| s.sound.effects.compressor.is_some());
    if let Some(at) = reference::first_difference(&heard, &want, if compressed { 32 } else { 0 }) {
        panic!("heard is not what was rendered, {}", reference::describe(&heard, &want, at, RATE));
    }
    assert_eq!(rig.waits(), 0, "the card never found too little to play");
}

fn with_sound(sound: nori_engine::Sound) -> Settings {
    Settings { sound, ..Settings::default() }
}

/// Each change is heard from the ear on, as the chain renders it offline, and none fetches the song
/// again: (song, first settings, steps, how late a change may start in ms).
#[test]
fn sound_changes_seamlessly() {
    let squeeze = |preset: CompressorPreset| with_sound(nori_engine::Sound { effects: Effects { compressor: Some(preset.settings()), ..Effects::default() }, limiter: true, ..Default::default() });
    let mut gaps = music(30.0, 45);
    gaps[RATE as usize * 2 * 8..RATE as usize * 2 * 12].fill(0);
    gaps[RATE as usize * 2 * 20..RATE as usize * 2 * 23].fill(0);
    type Case = (&'static str, Vec<i16>, Settings, Vec<(i64, Settings)>, usize);
    let cases: Vec<Case> = vec![
        ("equalizer", music(20.0, 42), Settings::default(), vec![(3_000, loud_eq()), (7_000, with_sound(nori_engine::Sound { preamp_db: -6.0, limiter: true, mono: true, ..Default::default() })), (12_000, Settings::default())], 6),
        ("compressor", music(15.0, 43), Settings::default(), vec![(3_000, squeeze(CompressorPreset::Strong)), (8_000, squeeze(CompressorPreset::Balanced))], 6),
        ("speed", music(20.0, 44), Settings::default(), vec![(3_000, Settings { speed: 1.5, ..Settings::default() }), (6_000, Settings { speed: 0.8, pitch: 1.1, ..Settings::default() }), (9_000, Settings::default())], 20),
        // On before the first silence, off in the middle of the second.
        ("silence skipping", gaps, loud_eq(), vec![(3_000, Settings { skip_silence: true, ..loud_eq() }), (17_500, loud_eq())], 120),
        ("all at once", music(20.0, 50), Settings::default(), vec![(2_000, loud_eq()), (4_000, Settings { speed: 1.3, ..loud_eq() }), (6_000, Settings { skip_silence: true, ..Settings::default() }), (8_000, quieter(-6.0))], 120),
    ];
    for (what, a, first, steps, late_ms) in cases {
        let (rig, live, asked) = stepped(&[("a", &a)], prefs_off(), first.clone(), &steps);
        heard_as_rendered(&rig, &live, &a, &first, &steps, &asked, late_ms);
        assert_eq!(rig.server.requests.lock().len(), 1, "{what}: {:?}", rig.server.requests.lock());
        if what == "silence skipping" {
            assert!(rig.heard.lock().len() < a.len() - RATE as usize * 2 * 3, "the first silence mostly skipped");
        }
    }
}

#[test]
fn eq_change_in_a_mix() {
    let (a, b) = (music(20.0, 46), music(20.0, 47));
    let songs: [(&str, &[i16]); 2] = [("a", &a), ("b", &b)];
    // The crossfade runs from 14 s to 20 s of what is heard.
    let raw = simulated(&songs, crossfade(6));
    let steps = [(16_000, loud_eq())];
    let (rig, live, asked) = stepped(&songs, crossfade(6), Settings::default(), &steps);
    heard_as_rendered(&rig, &live, &raw, &Settings::default(), &steps, &asked, 6);

    // Equalizer changes seamlessly in a stretched automix.
    let (a, b) = (music(40.0, 44), music(40.0, 45));
    let songs: [(&str, &[i16]); 2] = [("a", &a), ("b", &b)];
    let prefs = TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() };
    let run = |steps: &[(i64, Settings)]| {
        let live = Live::new(prefs);
        for t in [measured("a", 120.0, 40_000), measured("b", 123.0, 40_000)] {
            live.0.lock().analyses.insert(t.song_id.clone(), t);
        }
        let rig = Rig::with_app(&songs, live.clone(), Settings::default());
        rig.engine.play_at(0, 0);
        let mut asked = Vec::new();
        for (ms, s) in steps {
            let frames = (*ms * RATE as i64 / 1000) as usize;
            assert!(rig.wait_for(60, |r| r.heard.lock().len() >= frames * 2));
            asked.push(rig.heard.lock().len() / 2);
            assert!(rig.engine.status().mixing, "changed in the mix");
            rig.engine.set_settings(s.clone());
        }
        assert!(rig.wait_for(120, Rig::ended));
        let log = live.0.lock().log.clone();
        assert!(log.iter().any(|l| l.contains("transition a -> b: BeatMatched") && l.contains("tempo x0.976")), "{log:?}");
        (rig, live, asked)
    };
    // What the chain is given, as a run without a change hears it.
    let (plain, _, _) = run(&[]);
    let raw = plain.heard.lock().clone();
    // In the middle of the mix, the incoming song stretched.
    let steps = [(33_000, loud_eq())];
    let (rig, live, asked) = run(&steps);
    heard_as_rendered(&rig, &live, &raw, &Settings::default(), &steps, &asked, 6);
}

#[test]
fn eq_change_after_seek_or_pause() {
    let a = music(20.0, 48);
    let live = Live::new(prefs_off());
    let rig = Rig::with_app(&[("a", &a)], live.clone(), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    rig.engine.seek(10_000);
    assert!(rig.wait_for(5, |r| r.engine.status().position_ms >= 10_000));
    let heard = rig.heard.lock().len();
    assert!(rig.wait_for(5, |r| r.heard.lock().len() >= heard + 2 * 2 * BLOCK));
    let asked = rig.heard.lock().len();
    rig.engine.set_settings(loud_eq());
    assert!(rig.wait_for(30, Rig::ended));
    // The seek flushed the output: the chain's input starts at 10 s.
    let raw = &a[RATE as usize * 2 * 10..];
    let heard = rig.heard.lock().windows(256).position(|w| *w == raw[..256]).expect("the seek landed");
    let played: Vec<i16> = rig.heard.lock()[heard..].to_vec();
    let splices = live.0.lock().splices.clone();
    assert_eq!(splices.len(), 1, "{splices:?}");
    let s = splices[0];
    assert!((s.output as usize) * 2 + heard >= asked && (s.output as usize) * 2 + heard <= asked + 4 * BLOCK, "{s:?}");
    let want = reference::spliced(&reference::render(raw, RATE, &[(0, chain_of(&Settings::default()))]), &reference::render(raw, RATE, &[(0, chain_of(&Settings::default())), (s.input, chain_of(&loud_eq()))]), s.output as usize, RATE);
    if let Some(at) = reference::first_difference(&played, &want, 0) {
        panic!("{}", reference::describe(&played, &want, at, RATE));
    }

    // Equalizer change while paused heard on play.
    let a = music(15.0, 49);
    let live = Live::new(prefs_off());
    let rig = Rig::with_app(&[("a", &a)], live.clone(), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 3));
    rig.engine.pause();
    assert!(rig.wait_for(5, |r| r.engine.status().state == State::Paused));
    rig.run(1_000);
    let asked = rig.heard.lock().len() / 2;
    rig.engine.set_settings(loud_eq());
    rig.run(1_000);
    rig.engine.play();
    assert!(rig.wait_for(30, Rig::ended));
    heard_as_rendered(&rig, &live, &a, &Settings::default(), &[(0, loud_eq())], &[asked], 6);
}

#[test]
fn replay_gain_change_heard_at_once() {
    let a = music(15.0, 31);
    let live = Live::new(prefs_off());
    let rig = Rig::with_app(&[("a", &a)], live.clone(), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 3));
    live.0.lock().gains.insert("a".into(), 0.5);
    let asked = rig.heard.lock().len() / 2;
    rig.engine.gain_changed();
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let splices = live.0.lock().splices.clone();
    assert_eq!(splices.len(), 1, "{splices:?}");
    let s = splices[0];
    assert!(s.output as usize >= asked && s.output as usize <= asked + 2 * BLOCK, "{s:?}");
    // From there, every sample at its new level; before it as it was.
    let mut want = a.clone();
    for v in &mut want[s.input as usize * 2..] {
        *v = (*v as f32 * 0.5).round() as i16;
    }
    let want = reference::spliced(&a, &want, s.output as usize, RATE);
    // Read after the change, a song is turned down with dither.
    let heard = rig.heard.lock().clone();
    if let Some(at) = reference::first_difference(&heard, &want, 1) {
        panic!("{}", reference::describe(&heard, &want, at, RATE));
    }
}

#[test]
fn replay_gain_change_heard_at_once_through_the_equalizer() {
    // The equalizer applies the song's gain: from the change on it runs at the new one, its state going on.
    let a = music(15.0, 32);
    let live = Live::new(prefs_off());
    live.0.lock().gains.insert("a".into(), 0.7);
    let rig = Rig::with_app(&[("a", &a)], live.clone(), loud_eq());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 3));
    live.0.lock().gains.insert("a".into(), 0.5);
    let asked = rig.heard.lock().len() / 2;
    rig.engine.gain_changed();
    assert!(rig.wait_for(30, Rig::ended), "{:?}", rig.events.lock());
    let splices = live.0.lock().splices.clone();
    assert_eq!(splices.len(), 1, "{splices:?}");
    let s = splices[0];
    assert!(s.output as usize >= asked && s.output as usize <= asked + 2 * BLOCK, "{s:?}");
    let input: Vec<u8> = a.iter().flat_map(|v| v.to_le_bytes()).collect();
    let render = |gains: &[(usize, f32)]| -> Vec<i16> {
        let mut eq = nori_player::dsp::Equalizer::new(RATE, 2);
        loud_eq().sound.apply(&mut eq);
        let mut out = vec![0u8; input.len()];
        for (k, &(from, gain)) in gains.iter().enumerate() {
            let to = gains.get(k + 1).map_or(input.len(), |n| n.0);
            eq.process_bytes(&input[from..to], &mut out[from..to], false, gain);
        }
        out.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
    };
    let want = reference::spliced(&render(&[(0, 0.7)]), &render(&[(0, 0.7), (s.input as usize * 4, 0.5)]), s.output as usize, RATE);
    let heard = rig.heard.lock().clone();
    if let Some(at) = reference::first_difference(&heard, &want, 0) {
        panic!("{}", reference::describe(&heard, &want, at, RATE));
    }
}

/// What the app does while music plays on a device holding seconds.
#[derive(Clone, Debug)]
enum Step {
    /// The app came in sight (the device shallow) or left it.
    Shallow(bool),
    Sound(Box<Settings>),
}

fn eq_at(db: f64) -> Settings {
    let bands = vec![nori_player::dsp::Band { kind: nori_player::dsp::PEAKING, freq: 1000.0, gain_db: db, q: 1.0, channel: 0 }];
    with_sound(nori_engine::Sound { bands, ..Default::default() })
}

/// On a device holding seconds (a phone's AudioTrack, 2 s deep and [`SHALLOW_MS`] shallow here) every
/// sound change is made in place in the ring, from no sooner than the ear, and heard as the chain renders
/// it offline, nothing dropped or heard twice: deep, shallow, while it drains from deep to shallow, deep
/// again, one change after another (each landing in the blend of the one before while the device takes
/// nothing), in a mix. Shallow, a change is heard within what the device holds.
#[test]
fn changes_on_a_holding_device() {
    use Step::*;
    let graphic = |s: Vec<f64>| with_sound(nori_engine::Sound { graphic: s, ..Default::default() });
    let (a, b) = (music(20.0, 46), music(20.0, 47));
    let one: Vec<(&str, &[i16])> = vec![("a", &a)];
    let two: Vec<(&str, &[i16])> = vec![("a", &a), ("b", &b)];
    // The crossfade runs from 14 s to 20 s of what is heard.
    let mixed = simulated(&two, crossfade(6));
    // (what, the songs, what the chain is given, transitions, steps at ms heard)
    type Case<'a> = (&'static str, &'a [(&'a str, &'a [i16])], &'a [i16], TransitionPrefs, Vec<(i64, Step)>);
    let cases: Vec<Case> = vec![
        ("deep", &one, &a, prefs_off(), vec![(3_000, Sound(Box::new(loud_eq()))), (7_000, Sound(Box::default()))]),
        ("shallow", &one, &a, prefs_off(), vec![(1_000, Shallow(true)), (4_000, Sound(Box::new(loud_eq()))), (6_000, Sound(Box::new(eq_at(-4.0)))), (8_000, Sound(Box::default()))]),
        ("draining", &one, &a, prefs_off(), vec![(3_000, Shallow(true)), (3_100, Sound(Box::new(loud_eq()))), (7_000, Sound(Box::default()))]),
        ("back to back while draining", &one, &a, prefs_off(), vec![(3_000, Shallow(true)), (3_100, Sound(Box::new(eq_at(3.0)))), (3_250, Sound(Box::new(eq_at(6.0)))), (3_400, Sound(Box::new(eq_at(-6.0))))]),
        ("back to back shallow", &one, &a, prefs_off(), vec![(1_000, Shallow(true)), (4_000, Sound(Box::new(eq_at(3.0)))), (4_000, Sound(Box::new(eq_at(6.0)))), (4_000, Sound(Box::new(eq_at(-2.0)))), (4_000, Sound(Box::new(eq_at(9.0))))]),
        ("toggled", &one, &a, prefs_off(), vec![(1_000, Shallow(true)), (4_000, Sound(Box::new(loud_eq()))), (5_000, Sound(Box::default())), (6_000, Sound(Box::new(loud_eq()))), (6_200, Sound(Box::default()))]),
        ("preamp and mode", &one, &a, prefs_off(), vec![(1_000, Shallow(true)), (4_000, Sound(Box::new(quieter(-6.0)))), (5_000, Sound(Box::new(graphic(vec![6.0, -3.0, 0.0, 4.0, -6.0])))), (6_000, Sound(Box::new(eq_at(4.0)))), (7_000, Sound(Box::new(graphic(vec![0.0, 2.0, 0.0, 2.0, 0.0]))))]),
        ("deep again", &one, &a, prefs_off(), vec![(1_000, Shallow(true)), (4_000, Shallow(false)), (4_100, Sound(Box::new(loud_eq()))), (9_000, Shallow(true)), (9_100, Sound(Box::default()))]),
        ("in a mix, shallow", &two, &mixed, crossfade(6), vec![(1_000, Shallow(true)), (15_000, Sound(Box::new(loud_eq()))), (17_000, Sound(Box::new(eq_at(-3.0))))]),
        ("in a mix, draining", &two, &mixed, crossfade(6), vec![(15_000, Shallow(true)), (15_200, Sound(Box::new(loud_eq()))), (18_500, Sound(Box::default()))]),
    ];
    for (what, songs, raw, prefs, steps) in cases {
        let live = Live::new(prefs);
        let files = songs.iter().map(|(id, s)| (id.to_string(), wav(s), (s.len() / 2) as i64 * 1000 / RATE as i64)).collect();
        let rig = Rig::build(files, live.clone(), Settings::default(), Extra { hold_ms: Some(2_000), ..Extra::default() });
        rig.engine.play_at(0, 0);
        // Each change: the ear when it was asked, and what the device held then, frames.
        let mut asked = Vec::new();
        let mut sounds = Vec::new();
        for (ms, step) in &steps {
            let frames = (*ms * RATE as i64 / 1000) as usize;
            assert!(rig.wait_for(60, |r| r.heard.lock().len() >= frames * 2), "{what}: {ms} ms heard");
            match step {
                Shallow(on) => rig.engine.set_shallow(*on),
                Sound(s) => {
                    let held = rig.card.lock().held.len() / 2;
                    asked.push((rig.heard.lock().len() / 2, held));
                    sounds.push(*s.clone());
                    rig.engine.set_settings(*s.clone());
                }
            }
            // Apart by more than the engine gathers changes.
            rig.run(110);
        }
        assert!(rig.wait_for(120, Rig::ended), "{what}: {:?}", rig.events.lock());
        let splices = live.0.lock().splices.clone();
        assert_eq!(splices.len(), sounds.len(), "{what}: one splice per change: {splices:?}");
        let mut changes = vec![(0, chain_of(&Settings::default()))];
        let mut want = reference::render(raw, RATE, &changes);
        // What was there before the last splice, and where it was: one made at the same place blends
        // from that.
        let mut before = (usize::MAX, want.clone());
        let shallow = SHALLOW_MS * RATE as usize / 1000;
        for ((splice, s), &(ear, held)) in splices.iter().zip(&sounds).zip(&asked) {
            let at = splice.output as usize;
            assert!(at >= ear && at <= ear + held + 2 * BLOCK, "{what}: made from the ear ({ear}) past what the device held ({held}): {splice:?}");
            if held <= shallow {
                assert!(at - ear <= shallow + 2 * BLOCK, "{what}: shallow, heard within what the device holds: {splice:?}");
            }
            changes.push((splice.input, chain_of(s)));
            if before.0 != at {
                before = (at, want);
            }
            want = reference::spliced(&before.1, &reference::render(raw, RATE, &changes), at, RATE);
        }
        let heard = rig.heard.lock().clone();
        if let Some(at) = reference::first_difference(&heard, &want, 0) {
            panic!("{what}: heard is not what was rendered, {}", reference::describe(&heard, &want, at, RATE));
        }
        assert_eq!((rig.waits(), rig.flushes.load(Ordering::Relaxed)), (0, 0), "{what}: never ran dry, nothing dropped");
    }
}

/// A slider dragged: ten changes in 100 ms are made at most every 100 ms, the music never stops or
/// clicks, and it ends at the last.
#[test]
fn slider_drag_changes_seamlessly() {
    let a = common::sine(RATE, 440.0, 8.0, 12_000.0);
    let files = vec![("a".to_string(), wav(&a), 8_000)];
    let live = Live::new(prefs_off());
    let rig = Rig::build(files, live.clone(), quieter(-3.0), Extra { pace: Some(1.0), ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
    let started = rig.now_ms();
    for k in 0..10 {
        rig.engine.set_settings(quieter(-4.0 - k as f64));
        rig.run(10);
    }
    let took = rig.now_ms() - started;
    assert!(rig.wait_for(30, Rig::ended));
    let splices = live.0.lock().splices.clone().len() as i64;
    assert!(splices >= 2 && splices <= took / 100 + 2, "{splices} changes made for {took} ms of dragging");
    let heard = rig.heard.lock().clone();
    assert_eq!(heard.len(), a.len(), "not a sample more or less");
    assert_eq!(reference::clicks(&heard, RATE, 4.0), Vec::<usize>::new(), "no clicks");
    let loud = |s: &[i16]| (s.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
    let end = heard.len() - RATE as usize;
    let ratio = loud(&heard[end - RATE as usize..end]) / loud(&a[end - RATE as usize..end]);
    assert!((ratio - 10f64.powf(-13.0 / 20.0)).abs() < 0.01, "at the last step's level: {ratio}");
    assert_eq!(rig.waits(), 0);
}

/// Settings with a pre-amp of `db` (the chain on, only turning down).
fn quieter(db: f64) -> Settings {
    Settings { sound: nori_engine::Sound { preamp_db: db, ..Default::default() }, ..Settings::default() }
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
fn shallow_changes_nothing() {
    let a = music(12.0, 47);
    let files = vec![("a".to_string(), wav(&a), 12_000)];
    let rig = Rig::build(files, sim::App::new(), Settings::default(), Extra { pace: Some(1.0), hold_ms: Some(2_000), ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 3));
    for on in [true, false, true, false] {
        rig.engine.set_shallow(on);
        assert!(rig.wait_for(2, |r| r.shallow.load(Ordering::Relaxed) == on), "shallow {on} at once");
        rig.run(1_500);
    }
    assert!(rig.wait_for(30, Rig::ended));
    assert!(*rig.heard.lock() == a, "every sample once, in order");
    assert_eq!((rig.waits(), rig.flushes.load(Ordering::Relaxed)), (0, 0), "never ran dry, nothing dropped");
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
    let tail = 2 * RATE as usize / 10;
    let end = heard.len() - heard.len() % 2;
    let probe = heard.get(end.checked_sub(tail)?..end)?;
    let at = (0..=song.len().checked_sub(tail)?).step_by(2).find(|&k| song[k..k + tail] == *probe)?;
    Some(((at + tail) / 2) as i64 * 1000 / RATE as i64)
}

#[test]
fn nexts_move_one_song_each() {
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

    // Engine next moves one song per press.
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
        let live = Live::new(prefs);
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
fn next_before_end_changes_once() {
    next_before_the_end(prefs_off(), false, "gapless");

    // Next before crossfade changes once.
    next_before_the_end(crossfade(6), false, "crossfade");

    // Next before automix changes once.
    let prefs = TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() };
    next_before_the_end(prefs, true, "automix");
}

// ---- settings changed with the song's ending already made ----

/// Plays `songs` until `secs` of the first were heard (the ring holding ten seconds more).
fn playing_until(songs: &[(&str, &[i16])], app: impl App + Send + 'static, settings: Settings, secs: f64) -> Rig {
    let files = songs.iter().map(|(id, s)| (id.to_string(), wav(s), (s.len() / 2) as i64 * 1000 / RATE as i64)).collect();
    let rig = Rig::build(files, app, settings, Extra { pace: Some(5.0), ..Extra::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(20, |r| r.heard.lock().len() >= (secs * RATE as f64) as usize * 2), "{:?}", rig.engine.status());
    rig
}

#[test]
fn crossfade_switched_late() {
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

    // Crossfade off after mix made joins gaplessly.
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
    // Made again from where the mix began: a whole, then b whole, every sample once.
    // The mix's first frames blend into a's own over the splice's blend.
    let mut whole: Vec<i16> = a.iter().chain(&b).copied().collect();
    let blend = 2 * (RATE as i64 * nori_player::pipeline::BLEND_US / 1_000_000) as usize;
    let at = 2 * 24 * RATE as usize;
    assert!(heard[at..at + blend].iter().zip(&whole[at..at + blend]).all(|(h, w)| (h - w).abs() <= 8), "{log:?}");
    whole[at..at + blend].copy_from_slice(&heard[at..at + blend]);
    if let Some(at) = reference::first_difference(&heard, &whole, 0) {
        panic!("{}: {log:?}", reference::describe(&heard, &whole, at, RATE));
    }
    assert!(log.iter().any(|l| l.contains("the ending of a is made again from 24000 ms: gapless now")), "{log:?}");
    assert_eq!(rig.waits(), 0);
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

    fn gain(&mut self, list: &nori_player::playlist::Playlist, index: usize) -> f32 {
        self.live.gain(list, index)
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
    assert_eq!(rig.flushes.load(Ordering::Relaxed), flushes, "an ending made again replaces what is ahead of the ear, the output is not emptied");
    assert_eq!(rig.waits(), 0, "and never runs dry");
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
fn songs_queued_near_end() {
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

    // Song queued late remakes ending.
    // At 80 % a's ending is made as the end of music: remade into b.
    for how in [Queued::PlayNext, Queued::AddToQueue] {
        let (_rig, app, order) = queued_while_playing(0.8, false, how);
        mixed(&app, "a", "b");
        assert_eq!(order, vec!["a", "b"], "{:?}", app.log());
        assert!(made_again(&app), "the ending in the output was made for nothing after a: {:?}", app.log());
    }

    // Song queued ahead of next is mixed into.
    // Before a's mix into c is made: b takes c's place.
    for how in [Queued::PlayNext, Queued::AddToQueue] {
        let (_rig, app, order) = queued_while_playing(0.65, true, how);
        mixed(&app, "a", "b");
        mixed(&app, "b", "c");
        assert_eq!(order, vec!["a", "b", "c"], "{:?}", app.log());
    }

    // Song queued behind next keeps ending.
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
    assert!(app.log().iter().any(|l| l.contains("the ending of a is made again from 60000 ms: another song follows it now")), "{:?}", app.log());
    let heard = rig.heard.lock().len() as f64 / 2.0 / RATE as f64;
    assert!((heard - 120.0).abs() < 0.2, "every song whole, one after the other: {heard:.2} s");
}

/// Next pressed 10-15 times, 0-200 ms apart (sometimes inside a held mix ending), with the equalizer
/// and AutoMix on and songs cached whole, partly, or slow: the last song pressed to plays on.
#[test]
fn next_spam_keeps_playing() {
    const SECS: f64 = 40.0;
    let songs = many(16, SECS, 900);
    let ms = (SECS * 1000.0) as i64;
    for round in 0..4u64 {
        let mut rng = Rng(0x9E37_79B9 + round * 7919);
        let mut roll = |max: u64| ((rng.next() + 1.0) / 2.0 * max as f64) as u64;
        let dir = nori_testdir::TempDir::new("skips");
        let store = Store::open(dir.path(), 256 << 20).unwrap();
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
fn crossfade_on_paused_mixes_later() {
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
fn eq_chain_stays_through_changes() {
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

/// The status already says each event, and a seek says its place. Resumed, its place runs on from the
/// moment it plays again, not from the pause.
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
    let library = Songs { server, lengths: vec![("a".into(), 4_000), ("b".into(), 4_000)], store: None, pieces: None, hint: "wav" };
    let card = common::card::Card::new();
    let clock = Virtual::default();
    let cell: Arc<std::sync::OnceLock<Arc<Engine>>> = Arc::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (engine_of, said) = (cell.clone(), seen.clone());
    let engine = Arc::new(Engine::start_on(library, sim::App::new(), queue, Box::new(card.clone()), None, Config::default(), clock.clone(), move |e| {
        if let Some(engine) = engine_of.get() {
            let (state, index, at) = engine.status_with(|s| (s.state, s.index, s.at));
            said.lock().push((e, state, index, at));
        }
    }));
    let _ = cell.set(engine.clone());
    let time = Stepper::new(clock, card.pull.clone());
    engine.play_at(0, 0);
    assert!(time.until(Duration::from_secs(10), || engine.status().index == Some(0) && card.secs() > 1.0));
    engine.pause();
    assert!(time.until(Duration::from_secs(5), || engine.status().state == State::Paused));
    let resumed = std::time::Instant::now();
    engine.play();
    assert!(time.until(Duration::from_secs(5), || engine.status().state == State::Playing));
    engine.seek(2_000);
    assert!(time.until(Duration::from_secs(20), || engine.status().state == State::Ended), "{:?}", seen.lock());
    engine.stop();
    let seen = seen.lock();
    let (_, _, _, at) = seen.iter().rev().find(|(e, ..)| *e == Event::State(State::Playing)).expect("resumed");
    assert!(*at >= resumed, "the place reads from when it plays again");
    for (e, state, index, _) in seen.iter() {
        match e {
            Event::State(s) => assert_eq!(s, state, "{seen:?}"),
            Event::Song { index: i, .. } => assert_eq!(Some(*i), *index, "{seen:?}"),
            _ => {}
        }
    }
    assert!(seen.iter().any(|(e, ..)| matches!(e, Event::Position { index: 0, ms: 2_000, .. })), "the seek said its place: {seen:?}");
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
fn same_song_twice() {
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

    // Same song twice seek stays in first copy.
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

#[test]
fn same_song_before_keeps_entry() {
    let (a, b) = (music(4.0, 79), music(4.0, 80));
    let rig = Rig::new(&[("a", &a), ("b", &b)], prefs_off(), Settings::default());
    rig.queue.lock().set(vec!["a".into(), "b".into(), "a".into()], Some(0), false, 0);
    rig.engine.queue_changed();
    rig.engine.play_at(2, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2), "the second a plays");
    // Another a, before everything: the one playing is now the fourth entry, the last.
    rig.queue.lock().insert(0, vec!["a".into()], nori_player::playlist::Hand::No);
    rig.engine.queue_changed();
    assert!(rig.wait_for(20, Rig::ended), "{:?}", rig.events.lock());
    assert_eq!(rig.engine.status().index, Some(3));
    // Its event, said before the edit, names the entry now last.
    let seq = rig.events.lock().iter().find_map(|e| match e { Event::Song { index: 2, seq, .. } => *seq, _ => None });
    assert_eq!(seq.and_then(|s| rig.queue.lock().index_of(s)), Some(3));
    assert!(!rig.events.lock().iter().any(|e| matches!(e, Event::Song { id, .. } if id == "b")), "nothing after the last entry: {:?}", rig.events.lock());
}

#[test]
fn new_list_around_song_plays_on() {
    let (a, b, c) = (music(4.0, 81), music(4.0, 82), music(4.0, 83));
    let rig = Rig::new(&[("a", &a), ("b", &b), ("c", &c)], prefs_off(), Settings::default());
    rig.queue.lock().set(vec!["a".into(), "b".into()], Some(0), false, 0);
    rig.engine.queue_changed();
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2), "a plays");
    // Its album's list, tapped on a: a goes on as the second of [c, a, b].
    rig.queue.lock().set(vec!["c".into(), "a".into(), "b".into()], Some(1), false, 0);
    rig.engine.queue_changed();
    assert!(rig.wait_for(20, Rig::ended), "{:?}", rig.events.lock());
    let songs: Vec<(usize, String)> = rig.events.lock().iter().filter_map(|e| match e { Event::Song { index, id, .. } => Some((*index, id.clone())), _ => None }).collect();
    assert_eq!(songs, [(0, "a".to_string()), (2, "b".to_string())]);
}

// ---- seeks at the edges ----

/// The heard samples from `from` on are `song` from `ms` on, sample exact for `frames` frames.
fn heard_from(rig: &Rig, from: usize, song: &[i16], ms: i64, frames: usize) -> Result<(), String> {
    let heard = rig.heard.lock();
    let at = (ms * RATE as i64 / 1000) as usize * 2;
    let got = heard.get(from..from + frames * 2).ok_or_else(|| format!("{} samples heard after the seek", heard.len() - from))?;
    let want = &song[at..at + frames * 2];
    match reference::first_difference(got, want, 0) {
        None => Ok(()),
        Some(k) => Err(reference::describe(got, want, k, RATE)),
    }
}

#[test]
fn seek_after_the_end_plays_there() {
    let (a, b) = (music(4.0, 70), music(6.0, 71));
    let rig = Rig::new(&[("a", &a), ("b", &b)], prefs_off(), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(20, Rig::ended), "{:?}", rig.events.lock());
    let heard = rig.heard.lock().len();
    rig.engine.seek(2_000);
    assert!(rig.wait_for(10, |r| r.engine.status().state == State::Playing), "a seek after the end plays: {:?} {:?}", rig.engine.status(), rig.events.lock());
    assert!(rig.wait_for(10, |r| r.heard.lock().len() >= heard + 3 * RATE as usize * 2), "and is heard: {:?}", rig.engine.status());
    heard_from(&rig, heard, &b, 2_000, 3 * RATE as usize).unwrap();
    assert!(rig.wait_for(10, |r| r.events.lock().iter().filter(|e| **e == Event::State(State::Ended)).count() == 2), "and ends again: {:?}", rig.events.lock());
    assert_eq!(rig.engine.status().index, Some(1));
}

#[test]
fn seek_in_mix_plays_shown_song() {
    let (a, b) = (music(20.0, 72), music(20.0, 73));
    let rig = Rig::new(&[("a", &a), ("b", &b)], crossfade(6), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(20, |r| r.engine.status().mixing), "{:?}", rig.events.lock());
    rig.run(1_000);
    let (shown, _) = place(&rig);
    let song = if shown == Some(0) { &a } else { &b };
    let heard = rig.heard.lock().len();
    rig.engine.seek(3_000);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() >= heard + 2 * RATE as usize * 2), "{:?}", rig.engine.status());
    heard_from(&rig, heard, song, 3_000, 2 * RATE as usize).unwrap();
    let (index, ms) = place(&rig);
    assert_eq!(index, shown, "the seek bar stays on the song it showed: {:?}", rig.events.lock());
    let at = heard_in(&rig, song).expect("the shown song plays on");
    assert!((ms - at).abs() < 150, "the seek bar at {ms}, the ear at {at}");
    assert!(!rig.engine.status().mixing, "the mix is gone");

    // Seek paused in a mix plays the shown song.
    let (a, b) = (music(20.0, 72), music(20.0, 73));
    let rig = Rig::new(&[("a", &a), ("b", &b)], crossfade(6), Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(20, |r| r.engine.status().mixing), "{:?}", rig.events.lock());
    rig.run(1_000);
    let (shown, _) = place(&rig);
    rig.engine.pause();
    rig.engine.seek(3_000);
    assert!(rig.wait_for(5, |r| r.engine.status().position_ms == 3_000), "{:?}", rig.engine.status());
    assert_eq!(rig.engine.status().index, shown, "the seek bar stays on the song it showed");
    let heard = rig.heard.lock().len();
    rig.engine.play();
    assert!(rig.wait_for(10, |r| r.heard.lock().len() >= heard + 2 * RATE as usize * 2), "{:?}", rig.engine.status());
    heard_from(&rig, heard, if shown == Some(0) { &a } else { &b }, 3_000, 2 * RATE as usize).unwrap();
}

#[test]
fn seek_after_queue_edit() {
    let (a, b, c) = (music(20.0, 74), music(20.0, 75), music(20.0, 76));
    let rig = Rig::new(&[("a", &a), ("b", &b)], prefs_off(), Settings { fade_ms: 400, ..Settings::default() });
    rig.engine.play_at(1, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2), "b plays");
    rig.engine.seek(8_000);
    assert!(rig.wait_for(5, |r| r.engine.status().switching), "the dip is on");
    // A song put before b while the seek waits in its dip.
    rig.server.files.lock().push(("c".into(), Arc::new(wav(&c))));
    rig.queue.lock().insert(0, vec!["c".into()], nori_player::playlist::Hand::No);
    rig.engine.queue_changed();
    assert!(rig.wait_for(5, |r| !r.engine.status().switching), "the seek lands");
    let heard = rig.heard.lock().len();
    rig.run(2_000);
    assert!(rig.heard.lock().len() > heard + RATE as usize * 2);
    let at = heard_in(&rig, &b).expect("b plays on");
    assert!((9_800..10_500).contains(&at), "b from the seek on: {at}");
    let (index, ms) = place(&rig);
    assert_eq!(index, Some(2), "b is at 2 now: {:?}", rig.events.lock());
    assert!((ms - at).abs() < 150, "the seek bar at {ms}, the ear at {at}");

    // Seek after its song went plays what took its place.
    let (a, b) = (music(20.0, 77), music(20.0, 78));
    let rig = Rig::new(&[("a", &a), ("b", &b)], prefs_off(), Settings { fade_ms: 400, ..Settings::default() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2), "a plays");
    rig.engine.seek(8_000);
    assert!(rig.wait_for(5, |r| r.engine.status().switching), "the dip is on");
    rig.queue.lock().remove(0, 1);
    rig.engine.queue_changed();
    assert!(rig.wait_for(5, |r| !r.engine.status().switching), "the dip ends");
    rig.run(2_000);
    let (index, ms) = place(&rig);
    // What is heard and what is shown agree on one song and place.
    match (index, heard_in(&rig, &a), heard_in(&rig, &b)) {
        (Some(0), None, Some(at)) => assert!((ms - at).abs() < 150, "b at {at}, shown {ms}"),
        (Some(0), Some(at), None) => panic!("a, gone from the queue, plays on at {at} while b is shown at {ms}"),
        other => panic!("heard and shown disagree: {other:?} {:?}", rig.events.lock()),
    }
}

include!("perf_bench.rs");

/// Songs never heard before, the equalizer on: the ending is held for a blind fade, and the songs'
/// analyses come in while it is held (the playing song's from its own tap, the next measured as it is
/// fetched), with the hold just begun, the next song queued as the mix and the mix under way. The
/// ending is planned again; however it goes, both songs are heard together, never one jumping to the
/// other.
#[test]
fn analyses_mid_hold_still_mix() {
    let (a, b) = (music(40.0, 70), music(40.0, 71));
    for at_s in [16.0, 20.0, 26.0, 29.0] {
        let live = Live::new(TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() });
        let rig = playing_until(&[("a", &a), ("b", &b)], live.clone(), Settings { auto_mix: true, ..loud_eq() }, at_s);
        {
            let mut app = live.0.lock();
            app.analyses.insert("a".into(), measured("a", 120.0, 40_000));
            app.analyses.insert("b".into(), measured("b", 120.0, 40_000));
        }
        rig.engine.replan();
        assert!(rig.wait_for(60, Rig::ended), "{at_s} s: {:?}", live.0.lock().log);
        let log = live.0.lock().log.clone();
        assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{at_s} s: {log:?}");
        assert!(!log.iter().any(|l| l.contains("letting the ending play") || l.contains("abandon")), "{at_s} s: {log:?}");
        let heard = rig.heard.lock().len();
        assert!(heard < a.len() + b.len() - RATE as usize * 2 * 4, "{at_s} s: the songs overlap, {} s heard: {log:?}", heard / 2 / RATE as usize);
    }
}

// ---- a MixRamp fade out of a song with no beat grid, left before its end ----

/// An analysis with no usable beat grid (as the phone's Go Slowly and Black Star): leaving at `exit_ms`
/// (0: at the end), a quiet head of `head_ms`.
fn gridless(id: &str, ms: i64, exit_ms: i64, head_ms: i64) -> TrackAnalysis {
    TrackAnalysis { bpm_confidence: 0.3, stability: 0.2, outro_bpm_confidence: 0.3, outro_stability: 0.2, intro_bpm_confidence: 0.3, intro_stability: 0.2, exit_ms, mixramp_start_ms: head_ms, mixramp_end_ms: ms - 1_000, ..measured(id, 108.0, ms) }
}

/// The level of `hz` in each 20 ms of the left channel of `heard`.
fn levels(heard: &[i16], hz: f64) -> Vec<f64> {
    let w = RATE as usize / 50;
    let tau = std::f64::consts::TAU;
    heard.chunks_exact(2 * w).map(|c| {
        let (mut re, mut im, mut sum) = (0.0, 0.0, 0.0);
        for (i, v) in c.iter().step_by(2).enumerate() {
            let h = 0.5 - 0.5 * (tau * i as f64 / w as f64).cos();
            let t = tau * hz * i as f64 / RATE as f64;
            sum += h;
            re += *v as f64 / 32768.0 * h * t.cos();
            im += *v as f64 / 32768.0 * h * t.sin();
        }
        2.0 * (re * re + im * im).sqrt() / sum
    }).collect()
}

/// What the card heard from sample `from` on, the outgoing song a tone at `out_hz` mixing into the
/// incoming one at `in_hz` over `dur_ms`: the outgoing steady, then falling smoothly to silence, never
/// cut; the incoming rising and staying; the two heard together for most of the mix; no click.
fn fades_through(heard: &[i16], from: usize, out_hz: f64, in_hz: f64, dur_ms: i64, what: &str) {
    let heard = &heard[from & !1..];
    let (a, b) = (levels(heard, out_hz), levels(heard, in_hz));
    let full = a[..25].iter().copied().fold(0.0, f64::max);
    assert!(full > 0.05, "{what}: the outgoing song heard after the seek: {:?}", &a[..25]);
    let first_b = b.iter().position(|&v| v > 0.02 * full).unwrap_or_else(|| panic!("{what}: the incoming song is heard"));
    let last_a = a.iter().rposition(|&v| v > 0.02 * full).expect("the outgoing song is heard");
    for (w, pair) in a[..=last_a + 1].windows(2).enumerate() {
        let near = &a[w.saturating_sub(10)..(w + 10).min(a.len())];
        assert!(pair[1] >= pair[0] - 0.06 * full, "{what}: the outgoing song cut {:.2} s after the seek ({:.3} -> {:.3} of {full:.3}), the mix from {:.2} s: {near:?}", (w + 1) as f64 / 50.0, pair[0], pair[1], first_b as f64 / 50.0);
    }
    let overlap_ms = (last_a as i64 - first_b as i64) * 20;
    assert!(overlap_ms >= dur_ms * 8 / 10, "{what}: heard together {overlap_ms} ms of a {dur_ms} ms mix (from {:.2} s)", first_b as f64 / 50.0);
    let after = &b[last_a + 5..b.len() - 5];
    let top = after.iter().copied().fold(0.0, f64::max);
    assert!(after.iter().all(|&v| v > 0.9 * top), "{what}: the incoming song steady after the mix");
    assert_eq!(reference::clicks(heard, RATE, 4.0), Vec::<usize>::new(), "{what}: no clicks");
}

/// What comes while an ending is ahead, held or mixed ([`mixes_keep_their_length`]).
#[derive(Debug, Clone, Copy)]
enum Meddle {
    /// A seek to so many ms from the mix's start.
    Seek(i64),
    /// The ending planned again, to the same plan.
    Replan,
    /// AutoMix switched off and, so many ms later, on again.
    AutoMixOffOn(i64),
}

/// A seek, a replan or AutoMix switched off and on again, before the mix, while its ending is held or in
/// the mix, on a device holding nothing or seconds, never makes the engine think the outgoing song ended
/// early: the mix is never made to fit, and runs its whole length from where it was heard.
#[test]
fn mixes_keep_their_length() {
    let (a, b) = (common::sine(RATE, 200.0, 40.0, 6_000.0), common::sine(RATE, 900.0, 40.0, 6_000.0));
    let automix = |on: bool| TransitionPrefs { auto_mix: on, auto_mix_max_s: 12, echo_out: false, keep_albums: false, ..prefs_off() };
    use Meddle::*;
    // (ms heard from the mix's start when it comes (before it: negative), what comes, what the device holds)
    let cases = [
        (-6_000, Replan, 0),
        (-1_000, Replan, 2_000),
        (1_500, Replan, 0),
        (1_500, Replan, 2_000),
        (-6_000, AutoMixOffOn(300), 0),
        (-6_000, AutoMixOffOn(300), 2_000),
        (-1_000, AutoMixOffOn(300), 0),
        (-1_000, AutoMixOffOn(300), 2_000),
        (-3_000, AutoMixOffOn(1_500), 2_000),
        (1_500, AutoMixOffOn(300), 0),
        (1_500, AutoMixOffOn(300), 2_000),
        (-6_000, Seek(-3_000), 0),
        (-1_000, Seek(-4_000), 2_000),
        (-3_000, Seek(1_000), 0),
        (1_500, Seek(-2_000), 0),
        (1_500, Seek(2_500), 2_000),
    ];
    for (when_ms, meddle, hold_ms) in cases {
        let what = format!("{meddle:?} {when_ms} ms from the mix, the device holding {hold_ms} ms");
        let live = Live::new(automix(true));
        {
            let mut app = live.0.lock();
            app.analyses.insert("a".into(), gridless("a", 40_000, 30_000, 0));
            app.analyses.insert("b".into(), gridless("b", 40_000, 0, 0));
        }
        let files = vec![("a".to_string(), wav(&a), 40_000), ("b".to_string(), wav(&b), 40_000)];
        let rig = Rig::build(files, live.clone(), Settings { auto_mix: true, ..Settings::default() }, Extra { pace: Some(5.0), hold_ms: Some(hold_ms), ..Extra::default() });
        rig.engine.play_at(0, 15_000);
        assert!(rig.wait_for(30, |_| live.0.lock().log.iter().any(|l| l.contains("transition a -> b"))), "{what}: {:?}", live.0.lock().log);
        let plan = live.0.lock().log.iter().find(|l| l.contains("transition a -> b")).cloned().expect("planned");
        let num = |after: &str| plan.split(after).nth(1).and_then(|s| s.split([' ', ',']).next()).and_then(|n| n.parse::<i64>().ok()).expect("a number");
        let (dur, start) = (num("MixRampFade "), num(" ms at "));
        let heard_ms = |ms: i64| (ms * RATE as i64 / 1000) as usize * 2;
        assert!(rig.wait_for(60, |r| r.heard.lock().len() >= heard_ms(start - 15_000 + when_ms)), "{what}");
        // Where the outgoing song is heard steady a second before the mix, and how much of the mix is heard.
        let mut from = heard_ms(start - 15_000 - 1_000);
        let mut mixed = dur - when_ms.max(0);
        match meddle {
            Replan => rig.engine.replan(),
            AutoMixOffOn(gap) => {
                live.0.lock().prefs = automix(false);
                rig.engine.set_settings(Settings::default());
                rig.run(gap as u64);
                live.0.lock().prefs = automix(true);
                rig.engine.set_settings(Settings { auto_mix: true, ..Settings::default() });
            }
            Seek(to) => {
                let asked = rig.heard.lock().len();
                rig.engine.seek(start + to);
                assert!(rig.wait_for(10, |r| r.heard.lock().len() > asked + heard_ms(500)), "{what}");
                from = asked + heard_ms((-to - 1_000).max(0));
                // In the mix the seek is in the song the bar shows, which may be the incoming one.
                mixed = if when_ms > 0 { 0 } else { dur - to.max(0) };
            }
        }
        assert!(rig.wait_for(60, Rig::ended), "{what}: {:?}", live.0.lock().log);
        let log = live.0.lock().log.clone();
        assert!(!log.iter().any(|l| l.contains("made to fit")), "{what}: the outgoing song did not end early: {log:?}");
        assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{what}: {log:?}");
        if !matches!(meddle, Seek(_)) {
            assert_eq!(rig.heard.lock().len(), heard_ms(start - 15_000 + 40_000), "{what}: a to the mix, then the whole of b, nothing skipped");
        }
        if mixed < dur || from >= rig.heard.lock().len() {
            continue;
        }
        fades_through(&rig.heard.lock(), from, 200.0, 900.0, mixed, &what);
    }
}

/// What happens while a MixRamp fade is under way ([`mixramp_fades_out_after_seek`]), so many ms after
/// the seek lands.
#[derive(Debug, Clone, Copy)]
enum Meanwhile {
    Nothing,
    /// The equalizer changed.
    Eq(i64),
    /// The incoming song turns out to start loud (a shorter fade), the ending planned again.
    Analysis(i64),
    /// The app came in sight: the output turns shallow.
    Shallow(i64),
}

/// The phone's Go Slowly into Black Star: no reliable grid, an exit before the end, a MixRamp fade;
/// the songs at 48 kHz into an output a 44.1 kHz song opened, a seek landing before or in the mix, a
/// device holding music or not, and changes while the ending plays. The outgoing song fades out over
/// the mix as the incoming one fades in.
#[test]
fn mixramp_fades_out_after_seek() {
    let x = common::sine(RATE, 3_000.0, 12.0, 6_000.0);
    let (a, b) = (common::sine(48_000, 200.0, 40.0, 6_000.0), common::sine(48_000, 900.0, 40.0, 6_000.0));
    let gentler = || {
        let bands = vec![nori_player::dsp::Band { kind: nori_player::dsp::PEAKING, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }];
        Settings { auto_mix: true, sound: nori_engine::Sound { bands, ..Default::default() }, ..Settings::default() }
    };
    use Meanwhile::*;
    // (the incoming song's quiet head, where the seek lands before the mix, what the device holds, where
    // the outgoing song's music really ends (it says 40 s), ms; what happens meanwhile)
    let cases = [
        (8_600, 6_600, 0, 40_000, Nothing),
        (0, 6_600, 0, 40_000, Nothing),
        (8_600, 2_000, 0, 40_000, Nothing),
        (8_600, -3_000, 0, 40_000, Nothing),
        (8_600, 6_600, 1_500, 40_000, Nothing),
        (8_600, 6_600, 0, 26_000, Nothing),
        (8_600, -2_000, 0, 26_000, Nothing),
        (8_600, 6_600, 0, 40_000, Eq(3_000)),
        (8_600, 6_600, 0, 40_000, Eq(9_000)),
        (8_600, 6_600, 0, 40_000, Analysis(3_000)),
        (8_600, 6_600, 0, 40_000, Analysis(9_000)),
        (8_600, 6_600, 1_500, 40_000, Analysis(9_000)),
        (8_600, 6_600, 0, 40_000, Shallow(3_000)),
        (8_600, 6_600, 1_500, 40_000, Shallow(9_000)),
    ];
    for (head_ms, before_ms, hold_ms, end_ms, meanwhile) in cases {
        let what = format!("head {head_ms} ms, seek {before_ms} ms before the mix, the device holding {hold_ms} ms, the music to {end_ms} ms, {meanwhile:?}");
        let a = &a[..end_ms as usize * 48 * 2];
        let live = Live::new(TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, keep_albums: false, ..prefs_off() });
        {
            let mut app = live.0.lock();
            app.analyses.insert("a".into(), gridless("a", 40_000, 30_000, 0));
            app.analyses.insert("b".into(), gridless("b", 40_000, 0, head_ms));
        }
        let files = vec![("x".to_string(), wav(&x), 12_000), ("a".to_string(), wav_at(a, 48_000), 40_000), ("b".to_string(), wav_at(&b, 48_000), 40_000)];
        let rig = Rig::build(files, live.clone(), Settings { auto_mix: true, ..loud_eq() }, Extra { pace: Some(5.0), hold_ms: Some(hold_ms), ..Extra::default() });
        rig.engine.play_at(0, 0);
        assert!(rig.wait_for(30, |r| r.engine.status().index == Some(1) && r.engine.status().position_ms > 3_000), "{what}: {:?}", live.0.lock().log);
        let plan = live.0.lock().log.iter().find(|l| l.contains("transition a -> b")).cloned().unwrap_or_else(|| panic!("{what}: {:?}", live.0.lock().log));
        assert!(plan.contains("MixRampFade"), "{what}: {plan}");
        let num = |after: &str| plan.split(after).nth(1).and_then(|s| s.split([' ', ',']).next()).and_then(|n| n.parse::<i64>().ok()).expect("a number");
        let (dur, start) = (num("MixRampFade "), num(" ms at "));
        assert_eq!(start + dur, 30_000, "{what}: leaves at the exit: {plan}");
        rig.engine.go_to(1, start - before_ms);
        assert!(rig.wait_for(10, |r| (start - before_ms..start - before_ms + 1_000).contains(&r.engine.status().position_ms)), "{what}: {:?}", rig.engine.status());
        let landed = rig.heard.lock().len();
        let from = landed + RATE as usize;
        let at = |ms: i64| assert!(rig.wait_for(30, |r| r.heard.lock().len() >= landed + (ms * RATE as i64 / 1000) as usize * 2), "{what}");
        match meanwhile {
            Nothing => {}
            Eq(ms) => {
                at(ms);
                rig.engine.set_settings(gentler());
            }
            Analysis(ms) => {
                at(ms);
                live.0.lock().analyses.insert("b".into(), gridless("b", 40_000, 0, 0));
                rig.engine.replan();
            }
            Shallow(ms) => {
                at(ms);
                rig.engine.set_shallow(true);
            }
        }
        assert!(rig.wait_for(60, Rig::ended), "{what}: {:?}", live.0.lock().log);
        let log = live.0.lock().log.clone();
        assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{what}: {log:?}");
        let mixed_ms = if matches!(meanwhile, Analysis(_)) { 5_000 } else { (start + dur).min(end_ms) - start.max(start - before_ms + 500) };
        fades_through(&rig.heard.lock(), from, 200.0, 900.0, mixed_ms, &what);
    }
}

/// A seek near the end, as Android sends it (a jump, with or without a dip): into the music before an
/// ending held with a long runway, into the mix, or past where the mix leaves. Its landing is said with
/// its number at once, and the place moves on from there through the held ending.
#[test]
fn seek_into_ending_lands() {
    let (a, b) = (music(40.0, 80), music(40.0, 81));
    for (to_ms, fade_ms) in [(4_100, 0), (4_100, 150), (12_000, 150), (24_000, 0), (24_000, 150), (31_000, 150)] {
        let what = format!("to {to_ms} ms, a {fade_ms} ms dip");
        let live = Live::new(TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, keep_albums: false, ..prefs_off() });
        {
            let mut app = live.0.lock();
            app.analyses.insert("a".into(), gridless("a", 40_000, 30_000, 0));
            app.analyses.insert("b".into(), gridless("b", 40_000, 0, 8_600));
        }
        let rig = Rig::build(files(&[("a", &a), ("b", &b)]), live.clone(), Settings { auto_mix: true, fade_ms, ..loud_eq() }, Extra { pace: Some(5.0), ..Extra::default() });
        rig.engine.play_at(0, 0);
        assert!(rig.wait_for(10, |r| r.engine.status().position_ms > 2_000), "{what}");
        let n = rig.engine.go_to(0, to_ms);
        let asked = rig.now_ms();
        assert!(rig.wait_for(5, |r| r.events.lock().iter().any(|e| matches!(e, Event::Position { jumps, .. } if *jumps == n))), "{what}: the seek said it landed: {:?} {:?}", rig.events.lock(), live.0.lock().log);
        let took = rig.now_ms() - asked;
        assert!(took < 1_000, "{what}: said {took} ms after it was asked");
        let place = |r: &Rig| (r.engine.status().index, r.engine.status().position_ms);
        let (_, mut ms) = place(&rig);
        assert!((to_ms - 100..to_ms + 1_500).contains(&ms), "{what}: {:?}", rig.engine.status());
        let mut places = vec![ms];
        while rig.engine.status().index == Some(0) && ms < 40_000 {
            rig.run(1_000);
            rig.engine.look();
            rig.run(1);
            let (index, now) = place(&rig);
            places.push(now);
            assert!(index != Some(0) || (now - ms - 1_000).abs() < 300, "{what}: the place moves on, {places:?}: {:?}", live.0.lock().log);
            ms = now;
        }
    }
}

/// Played again after a pause, the song is heard at once, shallow or deep, faded or not, and played
/// again during the fade or after it.
#[test]
fn heard_again_after_pause() {
    for shallow in [false, true] {
        for fade_ms in [0, 400] {
            for wait_ms in [100, 3_000, 6 * 60_000] {
                let a = vec![8000i16; RATE as usize * 2 * 60];
                let rig = Rig::new(&[("a", &a)], prefs_off(), Settings { fade_ms, ..Settings::default() });
                rig.engine.set_shallow(shallow);
                rig.engine.play_at(0, 0);
                assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 5));
                rig.engine.pause();
                rig.run(wait_ms);
                rig.engine.play();
                rig.run(3_000);
                let heard = rig.heard.lock().clone();
                let what = format!("shallow {shallow}, fade {fade_ms} ms, played again after {wait_ms} ms: {:?}", rig.engine.status());
                assert!(heard.len() > RATE as usize * 2 * 7, "it plays on: {what}");
                assert_eq!(heard[heard.len() - 2..], [8000, 8000], "at full level: {what}");
            }
        }
    }
}

#[test]
fn a_queue_returned_after_release_is_heard() {
    let (a, b) = (music(12.0, 19), music(12.0, 20));
    for fade_ms in [0, 350] {
        let rig = Rig::build(files(&[("a", &a), ("b", &b)]), sim::App::new(), Settings { fade_ms, ..Settings::default() }, Extra::default());
        rig.engine.play_at(0, 0);
        assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2));
        rig.engine.release_now();
        assert!(rig.wait_for(5, |r| r.shut.load(Ordering::Relaxed) == 1));
        let before = rig.heard.lock().len();
        rig.engine.queue_changed();
        rig.engine.play_at(1, 3_000);
        assert!(rig.wait_for(10, |r| r.heard.lock().len() > before + RATE as usize * 2), "fade {fade_ms}: {:?}", rig.events.lock());
        let heard = rig.heard.lock();
        assert!(heard[before..].iter().any(|&v| v.abs() > 1000), "fade {fade_ms}: the returned queue must be audible");
        assert_eq!(rig.engine.status().index, Some(1));
    }
}
