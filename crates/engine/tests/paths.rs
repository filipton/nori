//! The engine's other paths on the virtual clock: audio offload to a simulated offload AudioTrack
//! ([`Fake`]), bit-perfect output, a live stream with ICY titles, the offline bridge, and repeat-one
//! loops. Compressed songs come from ffmpeg; tests needing it pass trivially without it.

use crate::common;

use std::collections::VecDeque;
use std::io::{Cursor, Read};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread::Thread;
use std::time::Duration;

use common::{Stepper, Virtual};
use nori_engine::{AudioOutput, Body, ByteSource, Coded, Coding, Config, Engine, Event, Feed, Library, Located, OffloadOutput, OutputFacts, OutputFormat, Settings, SharedQueue, Source, State, Support};
use nori_player::dsp::Band;
use nori_player::engine::{Host, Plan};
use nori_player::automix::analysis::Analyzer;
use nori_player::pipeline::{App, Sound};
use nori_player::playlist::REPEAT_ONE;
use nori_player::queue::{OnError, PlaybackError};
use nori_player::sim;
use nori_player::transitions::WindowSong;
use parking_lot::Mutex;

// ---- songs ----

use common::ffmpeg;

/// A tone encoded by ffmpeg with `codec` as `ext`, made once per tone and codec.
fn made(dir: &Path, name: &str, secs: u32, hz: u32, codec: &[&str], ext: &str) -> Vec<u8> {
    type Made = Vec<((u32, u32, String), Arc<Vec<u8>>)>;
    static MADE: std::sync::Mutex<Made> = std::sync::Mutex::new(Vec::new());
    let key = (secs, hz, format!("{} .{ext}", codec.join(" ")));
    if let Some((_, m)) = MADE.lock().unwrap().iter().find(|(k, _)| *k == key) {
        return m.to_vec();
    }
    let m = Arc::new(encode(dir, name, secs, hz, codec, ext));
    MADE.lock().unwrap().push((key, m.clone()));
    m.to_vec()
}

fn encode(dir: &Path, name: &str, secs: u32, hz: u32, codec: &[&str], ext: &str) -> Vec<u8> {
    let out = dir.join(format!("{name}.{ext}"));
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={hz}:sample_rate=44100:duration={secs}"), "-ac", "2"])
        .args(codec)
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg made {name}");
    std::fs::read(out).unwrap()
}

/// A temporary directory.
fn dir() -> nori_testdir::TempDir {
    nori_testdir::TempDir::new("paths")
}

fn mp3(dir: &Path, name: &str, secs: u32, hz: u32) -> Vec<u8> {
    made(dir, name, secs, hz, &["-c:a", "libmp3lame", "-b:a", "128k"], "mp3")
}

/// [`mp3`] at 320 kbps (32 KB is 819 ms).
fn mp3_320(dir: &Path, name: &str, secs: u32, hz: u32) -> Vec<u8> {
    made(dir, name, secs, hz, &["-c:a", "libmp3lame", "-b:a", "320k"], "mp3")
}

fn flac(dir: &Path, name: &str, secs: u32, hz: u32) -> Vec<u8> {
    made(dir, name, secs, hz, &["-c:a", "flac"], "flac")
}

use common::wav_bits as wav;

/// Distinct sample values across the whole range of `bits`.
fn ramp(frames: usize, bits: u32, seed: i64) -> Vec<i32> {
    let max = (1i64 << (bits - 1)) - 1;
    (0..frames * 2).map(|i| (((i as i64 * 7919 + seed * 104_729) % (2 * max)) - max) as i32).collect()
}

// ---- where songs come from ----

#[derive(Default)]
struct Server {
    files: Mutex<Vec<(String, Arc<Vec<u8>>)>>,
    /// Unreachable songs.
    down: Mutex<Vec<String>>,
    /// Songs refused with an error status.
    refused: Mutex<Vec<(String, u16)>>,
}

impl ByteSource for Server {
    fn open(&self, url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        if self.down.lock().iter().any(|d| d == url) {
            return Err("the network is gone".into());
        }
        if let Some((_, status)) = self.refused.lock().iter().find(|(u, _)| u == url) {
            return Err(nori_engine::OpenError::Status(*status));
        }
        let f = self.files.lock().iter().find(|(u, _)| u == url).map(|(_, f)| f.clone()).ok_or("404")?;
        let len = f.len() as u64;
        Ok(Body { start: from, len: Some(len), reader: Box::new(Cursor::new(f[from as usize..].to_vec())) })
    }
}

/// Songs (id, container, length ms) and album memberships (id, album, track).
struct Songs(Arc<Server>, Vec<(String, String, i64)>, Vec<(String, String, i32)>);

impl Library for Songs {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let (_, hint, ms) = self.1.iter().find(|(i, _, _)| i == id).cloned().ok_or("no such song")?;
        Ok(Located { source: Source::Url { url: id.to_string(), bytes: self.0.clone() }, hint: Some(hint), duration_ms: Some(ms), estimated: false })
    }

    fn about(&self, id: &str) -> WindowSong {
        let ms = self.1.iter().find(|(i, _, _)| i == id).map_or(0, |s| s.2);
        let (album_id, track) = self.2.iter().find(|(i, _, _)| i == id).map_or((None, 0), |a| (Some(a.1.clone()), a.2));
        WindowSong { id: id.into(), title: id.into(), duration_ms: ms, album_id, disc: 1, track, ..Default::default() }
    }
}

// ---- the CPU's output ----

/// The CPU card's puller.
#[derive(Default)]
struct Pull {
    feed: Option<Feed>,
    playing: bool,
    due_ns: i64,
    heard: Arc<Mutex<Vec<f32>>>,
    block: Vec<f32>,
    /// The offload track, advanced on the same clock when it runs on its own ([`Fake::runs`]).
    chip: Option<Fake>,
}

/// Frames the card pulls at a time.
const BLOCK: usize = 512;

impl common::Device for Pull {
    fn due_ns(&self) -> i64 {
        self.due_ns
    }

    fn tick(&mut self, now_ns: i64) -> bool {
        let rate = self.feed.as_ref().map_or(44_100, |f| f.format().rate);
        self.due_ns = now_ns + (BLOCK as i64 * 1_000_000_000) / rate as i64;
        // The offload track's request for more wakes the engine too.
        let asked = self.chip.as_ref().is_some_and(|c| c.0.lock().sync(now_ns));
        let Some(feed) = self.feed.as_mut() else { return asked };
        if !self.playing || (feed.available() < BLOCK && !feed.ending()) {
            return false;
        }
        let ch = feed.format().channels;
        self.block.resize(BLOCK * ch, 0.0);
        let waits = feed.engine_waits();
        let got = feed.pull(&mut self.block);
        self.heard.lock().extend_from_slice(&self.block[..got * ch]);
        asked || waits && !feed.engine_waits()
    }
}

/// A card recording what it plays (float) and every format it opened in.
#[derive(Clone, Default)]
struct Card {
    heard: Arc<Mutex<Vec<f32>>>,
    opened: Arc<Mutex<Vec<OutputFormat>>>,
    pull: Arc<Mutex<Pull>>,
}

impl Card {
    fn new() -> Card {
        let heard: Arc<Mutex<Vec<f32>>> = Arc::default();
        Card { heard: heard.clone(), opened: Arc::default(), pull: Arc::new(Mutex::new(Pull { heard, ..Pull::default() })) }
    }
}

impl AudioOutput for Card {
    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
        self.opened.lock().push(want);
        Ok(want)
    }

    fn start(&mut self, feed: Feed) -> Result<(), String> {
        self.pull.lock().feed = Some(feed);
        Ok(())
    }

    fn pause(&mut self) {
        self.pull.lock().playing = false;
    }

    fn resume(&mut self) {
        self.pull.lock().playing = true;
    }

    fn latency_us(&self) -> u64 {
        0
    }

    fn takes_float(&mut self) -> bool {
        true
    }

    fn close(&mut self) {
        self.pull.lock().feed = None;
    }
}

// ---- the offload output ----

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Open(Coded),
    DelayPadding(u32, u32),
    Write(usize, u64),
    EndOfStream,
    Play,
    Pause,
    Flush,
    Volume(f32),
    Close,
}

/// A simulated offload AudioTrack: buffers what fits, plays as far as the test says, can be torn down.
#[derive(Default)]
struct Chip {
    support: Vec<(Coding, Support)>,
    calls: Vec<Call>,
    capacity: usize,
    /// Unplayed bytes and their frames.
    held: VecDeque<(usize, u64)>,
    written: u64,
    bytes: Vec<u8>,
    head: u64,
    /// Frames written up to the last end of stream.
    ended_at: Option<u64>,
    torn: bool,
    open: bool,
    engine: Option<Thread>,
    /// The engine is woken through the clock so the test waits for it.
    clock: Option<Virtual>,
    /// Playing (an end of stream is accepted only then).
    playing: bool,
    /// Head readings given before the true count, one per call (None: a failed call).
    readings: VecDeque<Option<u64>>,
    /// Frames played when the count restarted from zero (standby).
    offset: u64,
    /// Claims everything was presented regardless (a late signal about another song).
    stale_presented: bool,
    /// Speed limit of the play head relative to real time (None: unlimited).
    pace: Option<f64>,
    /// Multiple of the requested size it grants.
    takes: usize,
    /// Perf notes.
    notes: Vec<String>,
    /// Ends of stream refused while playing (still stopping from the previous one).
    refuse_eos: u32,
    /// Plays by itself at real-time pace on the clock (None: the test moves it).
    runs: bool,
    /// Last advance, ns, and the fractional frame owed.
    synced_ns: Option<i64>,
    owed: f64,
    /// Bytes granted per track, whatever was asked (64 KB on a Galaxy S22).
    grant: Option<usize>,
    /// The play head always reads zero (as on a Galaxy S22).
    head_stuck: bool,
    /// Gives timestamps: the true count, or this fixed value.
    stamps: bool,
    frozen_stamp: Option<u64>,
    /// Asked for more since the engine looked, and whether it will ask again below half its buffer.
    requested: bool,
    armed: bool,
    /// Frames it ran dry while playing, before the end of what it was given.
    starved: u64,
    /// Galaxy S22 timestamp behaviour ([`Fake::jittery`]).
    jittery: bool,
    /// Timestamp readings queued before the true count (a start glitch, or stale values after a flush).
    stamp_script: VecDeque<u64>,
    /// Floor of the timestamp during the start glitch.
    stamp_floor: u64,
    /// The last timestamp and the jitter RNG state.
    last_stamp: u64,
    seed: u64,
    /// Flushes.
    flushes: u32,
    /// Frames played across tracks.
    played: u64,
    /// Extra bytes the DSP buffers beyond the granted track.
    dsp: usize,
    /// `onDataRequest` count.
    requests: u64,
    /// Stalled: plays and asks for nothing.
    stalled: bool,
    /// Final frames held back until more is written or an end of stream is said.
    holds_back: u64,
    /// The play head frozen at this value.
    frozen_head: Option<u64>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Chip>>);

impl Chip {
    /// Plays up to `frames`; returns the frames played.
    fn play_frames(&mut self, frames: u64) -> u64 {
        if self.stalled {
            return 0;
        }
        let presentable = if self.ended_at == Some(self.written) { self.written } else { self.written.saturating_sub(self.holds_back) };
        let to = (self.head + frames).min(presentable).max(self.head);
        let played = to - self.head;
        self.played += played;
        let mut n = played;
        self.head = to;
        while n > 0 {
            let Some((bytes, f)) = self.held.front_mut() else { break };
            if *f <= n {
                n -= *f;
                self.held.pop_front();
            } else {
                let part = (*bytes as u128 * n as u128 / *f as u128) as usize;
                *bytes -= part;
                *f -= n;
                n = 0;
            }
        }
        played
    }

    /// Timestamp readings at a track start, after `stale` twice.
    fn start_script(&mut self, stale: Option<u64>) {
        self.stamp_script.clear();
        if !self.jittery {
            return;
        }
        if let Some(s) = stale {
            self.stamp_script.extend([s, s]);
        }
        self.stamp_script.extend([10, 6, 5, 4, 4, 6, 7_074, 7_066, 7_065, 7_067, 7_065, 7_066, 7_064]);
        self.stamp_floor = 7_056;
        self.last_stamp = 0;
    }

    /// A jittery timestamp reading.
    fn jittery_stamp(&mut self) -> u64 {
        let truth = self.head - self.offset;
        // Start glitches last 300 ms.
        if truth > 13_230 {
            self.stamp_script.clear();
        }
        if self.playing {
            if let Some(s) = self.stamp_script.pop_front() {
                self.last_stamp = s;
                return s;
            }
        }
        if truth >= self.stamp_floor {
            self.stamp_floor = 0;
        }
        // xorshift64
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        let back = if self.seed.is_multiple_of(3) { 1 + (self.seed >> 8) % 80 } else { 0 };
        // Step back from the previous reading, or lag the true count.
        let stamp = match back {
            0 => truth.max(self.stamp_floor),
            _ if self.last_stamp + 1_024 >= truth => self.last_stamp.saturating_sub(back),
            _ => (truth - back).max(self.stamp_floor),
        };
        self.last_stamp = stamp;
        stamp
    }

    fn held_bytes(&self) -> usize {
        self.held.iter().map(|h| h.0).sum()
    }

    /// Advances a self-running track to `now_ns`, counting dry frames; true when it asks for more.
    fn sync(&mut self, now_ns: i64) -> bool {
        if !self.runs {
            return false;
        }
        let last = self.synced_ns.replace(now_ns).unwrap_or(now_ns);
        if !self.open || !self.playing || self.torn {
            self.owed = 0.0;
            return false;
        }
        let due = (now_ns - last).max(0) as f64 * 44_100.0 / 1e9 + self.owed;
        let frames = due as u64;
        self.owed = due - frames as f64;
        let played = self.play_frames(frames);
        // Ran dry before the last end of stream.
        if played < frames && self.ended_at != Some(self.written) {
            self.starved += frames - played;
        }
        if self.armed && !self.stalled && self.held_bytes() < self.capacity / 2 {
            self.armed = false;
            self.requested = true;
            self.requests += 1;
            return true;
        }
        false
    }

    /// [`Chip::sync`] to the clock from the engine's calls.
    fn sync_now(&mut self) {
        let Some(now) = self.clock.as_ref().map(Virtual::now_ns) else { return };
        if self.sync(now) {
            self.wake();
        }
    }

    /// Wakes the engine, as the platform does.
    fn wake(&self) {
        match (&self.clock, &self.engine) {
            (Some(c), _) => c.woke_engine(),
            (None, Some(t)) => t.unpark(),
            (None, None) => {}
        }
    }
}

impl Fake {
    fn new(support: &[(Coding, Support)]) -> Fake {
        let f = Fake::default();
        f.0.lock().support = support.to_vec();
        f
    }

    /// Plays `frames` and wakes the engine.
    fn advance(&self, frames: u64) {
        self.play_on(frames, true);
    }

    /// Plays `frames` without waking the engine.
    fn advance_quietly(&self, frames: u64) {
        self.play_on(frames, false);
    }

    fn play_on(&self, frames: u64, wake: bool) {
        let mut c = self.0.lock();
        c.play_frames(frames);
        if wake {
            c.wake();
        }
    }

    /// A Galaxy S22's offload track: 64 KB granted, self-running, asks below half, head stuck at zero,
    /// true timestamps.
    fn phone(&self, stamps: bool, head_stuck: bool) {
        let mut c = self.0.lock();
        c.runs = true;
        c.pace = Some(1.0);
        c.grant = Some(64 * 1024);
        c.stamps = stamps;
        c.head_stuck = head_stuck;
    }

    /// Galaxy S22 timestamps (perf10): 10, 6, 5, 4, 4, 6 frames at a track start, then 160 ms ahead until
    /// the true count passes; about a third of readings step back 1-80 frames; the first two after a
    /// flush are stale.
    fn jittery(&self) {
        let mut c = self.0.lock();
        c.jittery = true;
        c.seed = 0x9e37_79b9_7f4a_7c15;
        c.start_script(None);
    }

    fn starved_ms(&self) -> u64 {
        self.0.lock().starved * 1000 / 44_100
    }

    fn tear_down(&self) {
        let mut c = self.0.lock();
        c.torn = true;
        c.wake();
    }

    fn calls(&self) -> Vec<Call> {
        self.0.lock().calls.iter().filter(|c| !matches!(c, Call::Volume(_))).cloned().collect()
    }

    fn written(&self) -> u64 {
        self.0.lock().written
    }

    /// Queues head readings before the true count.
    fn read_as(&self, readings: &[Option<u64>]) {
        let mut c = self.0.lock();
        c.readings.extend(readings.iter().copied());
        c.wake();
    }

    /// Restarts the count from zero here.
    fn count_again(&self) {
        let mut c = self.0.lock();
        c.offset = c.head;
    }

    fn pace(&self, pace: Option<f64>) {
        self.0.lock().pace = pace;
    }

    fn notes(&self) -> Vec<String> {
        self.0.lock().notes.clone()
    }
}

impl OffloadOutput for Fake {
    fn supports(&mut self, coded: Coded) -> Support {
        self.0.lock().support.iter().find(|(c, _)| *c == coded.coding).map_or(Support::No, |s| s.1)
    }

    fn open(&mut self, coded: Coded, bytes: usize) -> Result<usize, String> {
        let mut c = self.0.lock();
        c.calls.push(Call::Open(coded));
        let bytes = c.grant.unwrap_or(bytes);
        c.capacity = bytes * c.takes.max(1) + c.dsp;
        c.armed = true;
        c.requested = false;
        c.held.clear();
        c.written = 0;
        c.head = 0;
        c.offset = 0;
        c.ended_at = None;
        c.torn = false;
        c.open = true;
        c.playing = false;
        c.engine = Some(std::thread::current());
        c.start_script(None);
        Ok(bytes)
    }

    fn write(&mut self, data: &[u8], frames: u64) -> Result<usize, i32> {
        let mut c = self.0.lock();
        c.sync_now();
        if c.torn {
            return Err(-6);
        }
        let held: usize = c.held.iter().map(|h| h.0).sum();
        let n = data.len().min(c.capacity.saturating_sub(held));
        if n > 0 {
            let f = if n == data.len() { frames } else { (frames as u128 * n as u128 / data.len() as u128) as u64 };
            c.held.push_back((n, f));
            c.written += f;
            c.calls.push(Call::Write(n, f));
            c.bytes.extend_from_slice(&data[..n]);
        }
        if c.held_bytes() >= c.capacity / 2 {
            c.armed = true;
        }
        Ok(n)
    }

    fn delay_padding(&mut self, delay: u32, padding: u32) {
        self.0.lock().calls.push(Call::DelayPadding(delay, padding));
    }

    fn end_of_stream(&mut self) -> bool {
        let mut c = self.0.lock();
        if !c.playing {
            return false;
        }
        if c.refuse_eos > 0 {
            c.refuse_eos -= 1;
            return false;
        }
        c.calls.push(Call::EndOfStream);
        c.ended_at = Some(c.written);
        c.stale_presented = false;
        true
    }

    fn play(&mut self) {
        let mut c = self.0.lock();
        c.sync_now();
        c.calls.push(Call::Play);
        c.playing = true;
    }

    fn pause(&mut self) {
        let mut c = self.0.lock();
        c.sync_now();
        c.calls.push(Call::Pause);
        c.playing = false;
    }

    fn flush(&mut self) {
        let mut c = self.0.lock();
        c.calls.push(Call::Flush);
        let stale = c.head - c.offset;
        c.start_script(Some(stale));
        c.flushes += 1;
        c.held.clear();
        c.written = 0;
        c.head = 0;
        c.offset = 0;
        c.ended_at = None;
    }

    fn set_volume(&mut self, volume: f32) {
        self.0.lock().calls.push(Call::Volume(volume));
    }

    fn head(&mut self) -> Option<u64> {
        let mut c = self.0.lock();
        c.sync_now();
        match c.readings.pop_front() {
            Some(r) => r,
            None if c.frozen_head.is_some() => c.frozen_head,
            None if c.head_stuck => Some(0),
            None => Some(c.head - c.offset),
        }
    }

    fn timestamp(&mut self) -> Option<u64> {
        let mut c = self.0.lock();
        c.sync_now();
        if let Some(f) = c.frozen_stamp {
            return Some(f);
        }
        if c.jittery {
            return Some(c.jittery_stamp());
        }
        c.stamps.then(|| c.head - c.offset)
    }

    fn data_requested(&mut self) -> bool {
        let mut c = self.0.lock();
        c.sync_now();
        std::mem::take(&mut c.requested)
    }

    fn presented(&mut self) -> bool {
        let c = self.0.lock();
        c.stale_presented || c.ended_at.is_some_and(|e| c.head >= e)
    }

    fn note(&mut self, what: &str) {
        self.0.lock().notes.push(what.to_string());
    }

    fn pace(&self) -> f64 {
        self.0.lock().pace.unwrap_or(1e6)
    }

    fn torn_down(&mut self) -> bool {
        self.0.lock().torn
    }

    fn close(&mut self) {
        let mut c = self.0.lock();
        c.calls.push(Call::Close);
        c.open = false;
        c.playing = false;
    }
}

// ---- the rig ----

struct Rig {
    engine: Engine,
    time: Stepper<Pull>,
    card: Card,
    queue: SharedQueue,
    events: Arc<Mutex<Vec<Event>>>,
}

impl Rig {
    fn new(server: Arc<Server>, songs: Vec<(String, String, i64)>, app: impl App + Send + 'static, fake: Option<Fake>, settings: Settings) -> Rig {
        Rig::albums(server, songs, Vec::new(), app, fake, settings)
    }

    /// [`Rig::new`] with album memberships (id, album, track).
    fn albums(server: Arc<Server>, songs: Vec<(String, String, i64)>, albums: Vec<(String, String, i32)>, app: impl App + Send + 'static, fake: Option<Fake>, settings: Settings) -> Rig {
        let queue = SharedQueue::default();
        queue.0.lock().set(songs.iter().map(|s| s.0.clone()).collect(), Some(0), false, 0);
        // Adjacent songs of an album were queued as that album.
        let album_of = |id: &str| albums.iter().find(|a| a.0 == id).map(|a| a.1.clone());
        let mut from = 0;
        for k in 1..=songs.len() {
            if k == songs.len() || album_of(&songs[k].0) != album_of(&songs[from].0) {
                if album_of(&songs[from].0).is_some() {
                    queue.0.lock().as_album(from, k);
                }
                from = k;
            }
        }
        let card = Card::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let config = Config { memory_mb: 256, settings, ..Config::default() };
        let clock = Virtual::default();
        if let Some(f) = &fake {
            f.0.lock().clock = Some(clock.clone());
            card.pull.lock().chip = Some(f.clone());
        }
        let offload = fake.map(|f| Box::new(f) as Box<dyn OffloadOutput>);
        let engine = Engine::start_on(Songs(server, songs, albums), app, queue.clone(), Box::new(card.clone()), offload, config, clock.clone(), move |e| seen.lock().push(e));
        engine.queue_changed();
        Rig { engine, time: Stepper::new(clock, card.pull.clone()), card, queue, events }
    }

    /// Runs until `done`, at most `secs` times 20 of clock time.
    fn wait(&self, secs: u64, mut done: impl FnMut(&Rig) -> bool) -> bool {
        self.time.until(Duration::from_secs(secs * 20), || done(self))
    }

    /// Runs for `ms`.
    fn run(&self, ms: u64) {
        self.time.run(Duration::from_millis(ms));
    }

    fn now_ms(&self) -> i64 {
        self.time.clock.now_ns() / 1_000_000
    }

    fn heard_song(&self, id: &str) -> bool {
        self.events.lock().iter().any(|e| matches!(e, Event::Song { id: i, .. } if i == id))
    }
}

fn app() -> sim::App {
    let mut a = sim::App::new();
    a.prefs = sim::prefs_off();
    a
}

/// [`app`] shared so a test can read the engine's log.
#[derive(Clone)]
struct Logged(Arc<Mutex<sim::App>>);

impl Logged {
    fn new() -> Logged {
        Logged(Arc::new(Mutex::new(app())))
    }

    fn log(&self) -> Vec<String> {
        self.0.lock().log.clone()
    }
}

impl Host for Logged {
    fn plan_for(&mut self, id: &str) -> Option<Plan> {
        self.0.lock().plan_for(id)
    }
    fn wants_analysis(&mut self, id: &str) -> Option<u64> {
        self.0.lock().wants_analysis(id)
    }
    fn analysed(&mut self, id: &str, a: Analyzer, channels: usize, frames: u64, rate: u32) {
        self.0.lock().analysed(id, a, channels, frames, rate)
    }
    fn log(&mut self, message: &str) {
        self.0.lock().log(message)
    }
    fn now_ms(&self) -> i64 {
        self.0.lock().now_ms()
    }
}

impl App for Logged {
    fn clock(&mut self, now_ms: i64) {
        self.0.lock().clock(now_ms)
    }
    fn auto_mix(&self) -> bool {
        self.0.lock().auto_mix()
    }
    fn window(&mut self, window: Vec<WindowSong>, shuffling: bool) {
        self.0.lock().window(window, shuffling)
    }
    fn measure_ahead<S: nori_player::pipeline::Songs>(&mut self, songs: &mut S, ids: &[String]) {
        self.0.lock().measure_ahead(songs, ids)
    }
    fn transitions_off(&mut self, off: bool) {
        self.0.lock().transitions_off(off)
    }
    fn gain(&mut self, index: usize, id: &str) -> f32 {
        self.0.lock().gain(index, id)
    }
}

fn offload() -> Settings {
    Settings { offload: true, ..Settings::default() }
}

fn serve(server: &Server, files: &[(&str, &[u8])]) {
    for (id, f) in files {
        server.files.lock().push((id.to_string(), Arc::new(f.to_vec())));
    }
}

const MP3_ONLY: &[(Coding, Support)] = &[(Coding::Mp3, Support::Gapless)];

/// Offload tracks opened.
fn opens(fake: &Fake) -> usize {
    fake.calls().iter().filter(|c| matches!(c, Call::Open(_))).count()
}

/// Calls on the last opened offload track.
fn after_last_open(calls: &[Call]) -> Vec<&Call> {
    let from = calls.iter().rposition(|c| matches!(c, Call::Open(_))).unwrap_or(0);
    calls[from..].iter().collect()
}

// ---- offload ----

#[test]
fn offload_joins_songs_on_one_track() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b) = (mp3(&d, "a", 20, 440), mp3(&d, "b", 20, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let fake = Fake::new(MP3_ONLY);
    let mut app = app();
    app.gains.insert("b".into(), 0.5);
    let songs = vec![("a".into(), "mp3".into(), 20_000), ("b".into(), "mp3".into(), 20_000)];
    let rig = Rig::new(server, songs, app, Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    // Both songs fit the track: written at once, closed by the end of the queue.
    assert!(rig.wait(10, |_| fake.calls().iter().filter(|c| **c == Call::EndOfStream).count() == 2), "{:?}", fake.calls());
    let calls = fake.calls();
    let opens: Vec<&Call> = calls.iter().filter(|c| matches!(c, Call::Open(_))).collect();
    assert_eq!(opens.len(), 1, "one track for both: {calls:?}");
    assert!(matches!(opens[0], Call::Open(Coded { coding: Coding::Mp3, rate: 44_100, channels: 2, .. })));
    // Delay and padding before each song, end of stream between.
    let marks: Vec<&Call> = calls.iter().filter(|c| matches!(c, Call::DelayPadding(..) | Call::EndOfStream)).collect();
    assert_eq!(marks.len(), 4, "{marks:?}");
    let (Call::DelayPadding(d1, p1), Call::EndOfStream, Call::DelayPadding(d2, p2), Call::EndOfStream) = (marks[0], marks[1], marks[2], marks[3]) else { panic!("{marks:?}") };
    // LAME's delay (576) and padding, as media3 passes them.
    assert_eq!((*d1, *d2), (576, 576), "the LAME tag's delay");
    assert!(*p1 > 0 && *p2 > 0);
    let frames = fake.written();
    assert_eq!(frames, 2 * 20 * 44_100, "the music, delay and padding cut: {frames}");
    // The second song is said, at its own volume.
    fake.advance(20 * 44_100 + 100);
    assert!(rig.wait(5, |r| r.heard_song("b")), "{:?}", rig.events.lock());
    assert!(rig.wait(5, |_| fake.0.lock().calls.contains(&Call::Volume(0.5))), "b plays at its ReplayGain volume");
    assert!(rig.engine.status().offloaded);
    let at = rig.engine.status().position_ms;
    assert!((0..100).contains(&at), "at the start of b: {at}");
    fake.advance(20 * 44_100);
    assert!(rig.wait(5, |r| r.events.lock().contains(&Event::State(State::Ended))), "{:?}", rig.events.lock());
    assert!(rig.card.opened.lock().is_empty(), "the CPU's output was never opened");
    rig.engine.stop();
}

#[test]
fn torn_down_track_hands_to_cpu() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 20, 440);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(MP3_ONLY);
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 20_000)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.written() > 0));
    fake.advance(5 * 44_100);
    assert!(rig.wait(5, |r| r.engine.status().position_ms >= 4_900), "{:?}", rig.engine.status());
    fake.tear_down();
    assert!(rig.wait(10, |r| r.card.heard.lock().len() > 44_100), "the CPU took over");
    assert!(fake.calls().contains(&Call::Close), "the torn track was let go");
    let s = rig.engine.status();
    assert!(!s.offloaded && s.position_ms >= 5_000, "{s:?}");
    rig.engine.stop();
}

#[test]
fn offload_follows_settings_both_ways() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b) = (mp3(&d, "a", 120, 440), mp3(&d, "b", 60, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let fake = Fake::new(MP3_ONLY);
    let songs = vec![("a".into(), "mp3".into(), 120_000), ("b".into(), "mp3".into(), 60_000)];
    // Offload on with the equalizer: the CPU plays.
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let rig = Rig::new(server, songs, app(), Some(fake.clone()), Settings { sound: eq.clone(), ..offload() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.card.heard.lock().len() > 2 * 44_100), "the CPU plays a");
    // The position from what the card played.
    let (before, asked) = ((rig.card.heard.lock().len() / 2) as i64 * 1000 / 44_100, rig.now_ms());
    rig.engine.set_settings(offload());
    // At once, behind a dip.
    assert!(rig.wait(10, |r| r.engine.status().offloaded), "{:?}", rig.engine.status().pcm_why);
    let s = rig.engine.status();
    // The offload track starts at the packet the position lands in (a few frames early for the
    // decoder's reservoir).
    let moved = rig.now_ms() - asked + 500;
    assert!(s.index == Some(0) && s.position_ms >= before - 200 && s.position_ms <= before + moved, "a from where it was ({before} ms): {s:?}");
    let calls = fake.calls();
    assert!(matches!(calls.iter().find(|c| matches!(c, Call::DelayPadding(..))), Some(Call::DelayPadding(0, _))), "a from part way in, no delay to cut: {calls:?}");
    // Equalizer on: off offload at once.
    fake.advance(10 * 44_100);
    let at = rig.engine.status().position_ms;
    assert!(rig.wait(5, |r| r.engine.status().position_ms >= at + 9_900));
    let heard = rig.card.heard.lock().len();
    rig.engine.set_settings(Settings { sound: eq, ..offload() });
    assert!(rig.wait(10, |r| r.card.heard.lock().len() > heard + 44_100), "the CPU plays a on");
    assert!(fake.calls().contains(&Call::Close));
    assert!(rig.wait(5, |r| !r.engine.status().offloaded), "the status follows once the burst is in");
    // The CPU resumes where the offload track was.
    let s = rig.engine.status();
    assert!(s.index == Some(0) && s.position_ms >= at + 9_990, "{s:?}");
    rig.engine.stop();
}

#[test]
fn undecodable_song_plays_on_cpu_between() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b, c) = (mp3(&d, "a", 10, 440), flac(&d, "b", 30, 550), mp3(&d, "c", 10, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b), ("c", &c)]);
    let fake = Fake::new(MP3_ONLY);
    let songs = vec![("a".into(), "mp3".into(), 10_000), ("b".into(), "flac".into(), 30_000), ("c".into(), "mp3".into(), 10_000)];
    let rig = Rig::new(server, songs, app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.calls().contains(&Call::EndOfStream)), "{:?}", fake.calls());
    assert_eq!(fake.calls().iter().filter(|c| matches!(c, Call::DelayPadding(..))).count(), 1, "the FLAC song is not written to the chip");
    // The FLAC song on the CPU.
    fake.advance(10 * 44_100);
    assert!(rig.wait(10, |r| r.heard_song("b") && !r.card.heard.lock().is_empty()), "{:?}", rig.events.lock());
    assert!(fake.calls().contains(&Call::Close));
    // Offloaded again for c after b.
    assert!(rig.wait(20, |r| r.heard_song("c")), "{:?}", rig.events.lock());
    assert!(rig.wait(5, |r| r.engine.status().offloaded), "c is the chip's");
    let heard = rig.card.heard.lock().len() / 2;
    assert!(heard + 44_100 / 2 >= 30 * 44_100, "b whole on the CPU: {heard}");
    rig.engine.stop();
}

#[test]
fn boosted_song_plays_on_cpu_between() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b, c) = (mp3(&d, "a", 10, 440), mp3(&d, "b", 20, 550), mp3(&d, "c", 10, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b), ("c", &c)]);
    let fake = Fake::new(MP3_ONLY);
    let app = Logged::new();
    // b is boosted 6 dB (needs samples); c is turned down (a volume can).
    app.0.lock().gains.insert("b".into(), 2.0);
    app.0.lock().gains.insert("c".into(), 0.5);
    let songs = vec![("a".into(), "mp3".into(), 10_000), ("b".into(), "mp3".into(), 20_000), ("c".into(), "mp3".into(), 10_000)];
    let rig = Rig::new(server, songs, app.clone(), Some(fake.clone()), Settings { gain_boost_db: 6.0, ..offload() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.calls().contains(&Call::EndOfStream)), "{:?}", fake.calls());
    assert_eq!(fake.calls().iter().filter(|c| matches!(c, Call::DelayPadding(..))).count(), 1, "b is not written to the chip after a: {:?}", fake.calls());
    fake.advance(10 * 44_100);
    assert!(rig.wait(10, |r| r.heard_song("b") && !r.card.heard.lock().is_empty()), "{:?}", rig.events.lock());
    let log = app.log();
    let why = log.iter().filter_map(|l| l.strip_prefix("playing on the CPU: ")).next().unwrap_or_default();
    assert!(why.contains("ReplayGain turns it up"), "{why}: {log:?}");
    // Offloaded again for c, at its volume.
    assert!(rig.wait(30, |r| r.heard_song("c")), "{:?}", rig.events.lock());
    assert!(rig.wait(5, |r| r.engine.status().offloaded), "c is the chip's");
    assert!(rig.wait(5, |_| fake.0.lock().calls.contains(&Call::Volume(0.5))), "c at its ReplayGain volume");
    // b louder than the file, under the limiter's ceiling.
    let heard = rig.card.heard.lock().clone();
    let peak = heard.iter().map(|v| (*v as f64).abs()).fold(0.0, f64::max);
    assert!(peak <= 10f64.powf(-1.0 / 20.0) * 32768.0 + 1.0, "{peak}");
    assert!(heard.len() / 2 + 44_100 / 2 >= 20 * 44_100, "b whole on the CPU: {}", heard.len() / 2);
    rig.engine.stop();
}

/// Regression (0.4.6, "muted mid-track"): after a pause offloaded and a CPU song, the next offloaded song
/// played at the pause's silence.
#[test]
fn offload_volume_restored_after_pause_and_cpu_song() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b, c) = (mp3(&d, "a", 10, 440), flac(&d, "b", 10, 550), mp3(&d, "c", 10, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b), ("c", &c)]);
    // b is FLAC: the CPU plays it.
    let fake = Fake::new(MP3_ONLY);
    let songs = vec![("a".into(), "mp3".into(), 10_000), ("b".into(), "flac".into(), 10_000), ("c".into(), "mp3".into(), 10_000)];
    let rig = Rig::new(server, songs, app(), Some(fake.clone()), Settings { fade_ms: 300, ..offload() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.engine.status().offloaded), "a is the chip's: {:?}", rig.engine.status());
    rig.run(1_200);
    fake.advance(44_100);
    assert!(rig.wait(5, |r| r.engine.status().position_ms >= 990), "{:?}", rig.engine.status());
    rig.engine.pause();
    assert!(rig.wait(5, |r| r.engine.status().state == State::Paused));
    rig.run(1_000);
    rig.engine.next();
    rig.engine.play();
    assert!(rig.wait(10, |r| r.heard_song("b") && !r.card.heard.lock().is_empty()), "b on the CPU: {:?}", rig.events.lock());
    assert!(rig.wait(30, |r| r.heard_song("c")), "{:?}", rig.events.lock());
    assert!(rig.wait(5, |r| r.engine.status().offloaded), "c is the chip's");
    rig.run(1_000);
    // Volume calls since c's track opened.
    let raw = fake.0.lock().calls.clone();
    let opened = raw.iter().rposition(|c| matches!(c, Call::Open(_))).expect("c's track");
    let volumes: Vec<f32> = raw[opened..].iter().filter_map(|c| if let Call::Volume(v) = c { Some(*v) } else { None }).collect();
    assert!(volumes.last().is_none_or(|v| *v == 1.0), "c is heard at full volume on the chip, not silent: {volumes:?}");
    rig.engine.stop();
}

/// Regression: after an idle release while paused offloaded, the new track stayed at the pause's silence.
#[test]
fn offload_volume_restored_after_release() {
    let d = dir();
    let Some((rig, fake)) = two_on_the_chip(&d, 20) else { return };
    rig.engine.set_settings(Settings { fade_ms: 300, ..offload() });
    rig.run(200);
    rig.engine.pause();
    assert!(rig.wait(5, |r| r.engine.status().state == State::Paused));
    rig.run(6 * 60_000);
    assert!(rig.engine.status().releases > 0, "the track was let go: {:?}", rig.engine.status());
    rig.engine.play();
    assert!(rig.wait(10, |r| r.engine.status().offloaded && r.engine.status().state == State::Playing), "{:?}", rig.engine.status());
    rig.run(1_000);
    let raw = fake.0.lock().calls.clone();
    let opened = raw.iter().rposition(|c| matches!(c, Call::Open(_))).expect("a track");
    let volumes: Vec<f32> = raw[opened..].iter().filter_map(|c| if let Call::Volume(v) = c { Some(*v) } else { None }).collect();
    assert!(volumes.last().is_none_or(|v| *v == 1.0), "heard at full volume on the new track: {volumes:?} {raw:?}");
    rig.engine.stop();
}

/// Regression: back on offload after a CPU takeover, the track stayed at the takeover's silence.
#[test]
fn offload_volume_restored_after_cpu_takeover() {
    let d = dir();
    let Some((rig, fake)) = two_on_the_chip(&d, 20) else { return };
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let heard = rig.card.heard.lock().len();
    rig.engine.set_settings(Settings { sound: eq, ..offload() });
    assert!(rig.wait(10, |r| r.card.heard.lock().len() > heard + 44_100), "the CPU plays a on");
    assert!(rig.wait(5, |r| !r.engine.status().offloaded));
    rig.engine.set_settings(offload());
    assert!(rig.wait(10, |r| r.engine.status().offloaded), "back on the chip: {:?}", rig.engine.status().pcm_why);
    rig.run(1_000);
    let raw = fake.0.lock().calls.clone();
    let opened = raw.iter().rposition(|c| matches!(c, Call::Open(_))).expect("a track");
    let volumes: Vec<f32> = raw[opened..].iter().filter_map(|c| if let Call::Volume(v) = c { Some(*v) } else { None }).collect();
    assert!(volumes.last().is_none_or(|v| *v == 1.0), "heard at full volume on the chip again: {volumes:?} {raw:?}");
    rig.engine.stop();
}

#[test]
fn offloaded_song_boosted_moves_to_cpu() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 20, 440);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(MP3_ONLY);
    let app = Logged::new();
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 20_000)], app.clone(), Some(fake.clone()), Settings { gain_boost_db: 6.0, ..offload() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.engine.status().offloaded), "a is the chip's: {:?}", rig.engine.status());
    fake.advance(5 * 44_100);
    // a now wants +6 dB.
    app.0.lock().gains.insert("a".into(), 2.0);
    rig.engine.gain_changed();
    assert!(rig.wait(10, |r| !r.engine.status().offloaded && r.card.heard.lock().len() > 44_100), "the CPU took over: {:?}", rig.engine.status());
    let s = rig.engine.status();
    assert!(s.position_ms >= 4_000 && s.position_ms < 20_000, "from where the ear was: {s:?}");
    let log = app.log();
    assert!(log.iter().any(|l| l.contains("ReplayGain turns it up")), "{log:?}");
    rig.engine.stop();
}

#[test]
fn offload_seek_restarts_at_packet() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 30, 440);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(MP3_ONLY);
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 30_000)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    // All written with its end of stream said (the track is stopping).
    assert!(rig.wait(10, |_| fake.calls().contains(&Call::EndOfStream)), "{:?}", fake.calls());
    rig.engine.seek(12_000);
    // So the seek opens a new track rather than flushing, as media3 does.
    assert!(rig.wait(5, |_| opens(&fake) == 2 && fake.written() > 0), "{:?}", fake.calls());
    let calls = fake.calls();
    assert!(!calls.contains(&Call::Flush), "a stopped track is not flushed: {calls:?}");
    assert!(calls.iter().position(|c| *c == Call::Close) < calls.iter().rposition(|c| matches!(c, Call::Open(_))), "{calls:?}");
    let after = after_last_open(&calls);
    assert!(matches!(after.iter().find(|c| matches!(c, Call::DelayPadding(..))), Some(Call::DelayPadding(0, _))), "no delay to cut part way in: {after:?}");
    // The position is the packet the seek landed in; written from there.
    let at = rig.engine.status().position_ms;
    assert!((11_800..=12_000).contains(&at), "{at}");
    let frames = fake.written() as i64;
    assert!((frames + at * 44_100 / 1000 - 30 * 44_100).abs() <= 1152, "{frames} frames from {at} ms");
    rig.engine.stop();
}

#[test]
fn offload_follows_queue_edits() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b, c, x) = (mp3(&d, "a", 5, 440), mp3(&d, "b", 5, 550), mp3(&d, "c", 5, 660), mp3(&d, "x", 5, 770));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b), ("c", &c), ("x", &x)]);
    let fake = Fake::new(MP3_ONLY);
    let songs = ["a", "b", "c"].map(|id| (id.to_string(), "mp3".to_string(), 5_000)).to_vec();
    let rig = Rig::new(server, songs, app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    let delays = |f: &Fake| f.calls().iter().filter(|c| matches!(c, Call::DelayPadding(..))).count();
    assert!(rig.wait(10, |_| delays(&fake) == 3), "all three on the track: {:?}", fake.calls());
    fake.advance(44_100);
    rig.run(100);
    // A song put before them: what is written stays.
    let opened = opens(&fake);
    rig.queue.0.lock().insert(0, vec!["x".into()], nori_player::playlist::Hand::No);
    rig.engine.queue_changed();
    rig.run(100);
    assert_eq!(opens(&fake), opened, "no track opened again: {:?}", fake.calls());
    assert!(rig.wait(5, |r| r.engine.status().index == Some(1)), "a is at 1 now: {:?}", rig.engine.status());
    // The song written after a taken out: the track starts again where the ear is, without it.
    rig.queue.0.lock().remove(2, 3);
    rig.engine.queue_changed();
    assert!(rig.wait(5, |_| fake.calls().iter().rposition(|c| matches!(c, Call::Flush | Call::Open(_))).is_some_and(|k| k + 1 < fake.calls().len())), "{:?}", fake.calls());
    for _ in 0..3 {
        fake.advance(5 * 44_100);
        rig.run(100);
    }
    let songs: Vec<String> = rig.events.lock().iter().filter_map(|e| if let Event::Song { id, .. } = e { Some(id.clone()) } else { None }).collect();
    assert!(!songs.contains(&"b".to_string()) && songs.contains(&"c".to_string()), "b went, c followed a: {songs:?}");
    rig.engine.stop();
}

#[test]
fn offload_repeat_one_reports_loops() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 5, 440);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(MP3_ONLY);
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 5_000)], app(), Some(fake.clone()), offload());
    rig.engine.set_repeat(REPEAT_ONE);
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.calls().iter().filter(|c| matches!(c, Call::DelayPadding(..))).count() >= 2), "{:?} {:?}", fake.calls().iter().filter(|c| !matches!(c, Call::Write(..))).collect::<Vec<_>>(), rig.events.lock());
    for _ in 0..2 {
        fake.advance(5 * 44_100);
        rig.run(100);
    }
    assert!(rig.wait(5, |r| r.events.lock().iter().filter(|e| matches!(e, Event::Looped { index: 0, .. })).count() == 2), "{:?}", rig.events.lock());
    rig.engine.stop();
}

#[test]
fn offload_sleep_timer_takes_back_next_song() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b) = (mp3(&d, "a", 10, 440), mp3(&d, "b", 10, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let fake = Fake::new(MP3_ONLY);
    let songs = vec![("a".into(), "mp3".into(), 10_000), ("b".into(), "mp3".into(), 10_000)];
    let rig = Rig::new(server, songs, app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.written() == 20 * 44_100), "both written: {}", fake.written());
    fake.advance(3 * 44_100);
    assert!(rig.wait(5, |r| r.engine.status().position_ms >= 2_900));
    rig.engine.pause_at_end(true);
    // b cannot be unwritten: a restarts alone on a new track.
    assert!(rig.wait(5, |_| opens(&fake) == 2 && fake.written() > 0), "{:?}", fake.calls());
    let after_flush = |f: &Fake| after_last_open(&f.calls()).into_iter().filter(|c| matches!(c, Call::EndOfStream | Call::DelayPadding(..))).cloned().collect::<Vec<_>>();
    assert!(rig.wait(5, |_| after_flush(&fake).contains(&Call::EndOfStream)), "{:?}", fake.calls());
    assert_eq!(after_flush(&fake).len(), 2, "a's rest, closed: {:?}", after_flush(&fake));
    let frames = fake.written() as i64;
    // From the packet the position lands in.
    assert!((frames - 7 * 44_100).abs() <= 4 * 1152, "the rest of a only: {frames}");
    fake.advance(frames as u64);
    assert!(rig.wait(5, |r| r.events.lock().iter().any(|e| matches!(e, Event::Stopped { .. }))), "{:?}", rig.events.lock());
    assert!(rig.wait(5, |r| { let s = r.engine.status(); s.state == State::Paused && s.index == Some(1) && s.position_ms == 0 }), "{:?}", rig.engine.status());
    rig.engine.stop();
}

#[test]
fn offload_sleep_timer_follows_its_song_through_an_edit() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b) = (mp3(&d, "a", 10, 440), mp3(&d, "b", 10, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let fake = Fake::new(MP3_ONLY);
    let songs = vec![("a".into(), "mp3".into(), 10_000), ("b".into(), "mp3".into(), 10_000)];
    let rig = Rig::new(server, songs, app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.written() == 20 * 44_100), "both written: {}", fake.written());
    rig.engine.pause_at_end(true);
    assert!(rig.wait(5, |_| after_last_open(&fake.calls()).contains(&&Call::EndOfStream)), "a alone: {:?}", fake.calls());
    rig.queue.0.lock().insert(0, vec!["b".into()], nori_player::playlist::Hand::No);
    rig.engine.queue_changed();
    rig.run(100);
    let written = fake.written();
    fake.advance(written as u64);
    assert!(rig.wait(5, |r| r.events.lock().iter().any(|e| matches!(e, Event::Stopped { .. }))), "{:?}", rig.events.lock());
    assert_eq!(fake.written(), written, "nothing after a: {:?}", fake.calls());
    rig.engine.stop();
}

#[test]
fn offload_opus_in_ogg_pages() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = made(&d, "a", 3, 440, &["-c:a", "libopus", "-b:a", "96k"], "opus");
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(&[(Coding::Opus, Support::Gapless)]);
    let rig = Rig::new(server, vec![("a".into(), "opus".into(), 3_000)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.calls().contains(&Call::EndOfStream)), "{:?}", fake.calls());
    assert!(matches!(fake.calls()[0], Call::Open(Coded { coding: Coding::Opus, rate: 48_000, channels: 2 })));
    let bytes = fake.0.lock().bytes.clone();
    // OpusHead, OpusTags, then one packet per page with its granule.
    let mut at = 0;
    let mut pages = Vec::new();
    while at < bytes.len() {
        assert_eq!(&bytes[at..at + 4], b"OggS", "a page at {at}");
        let segments = bytes[at + 26] as usize;
        let body: usize = bytes[at + 27..at + 27 + segments].iter().map(|&s| s as usize).sum();
        let granule = u64::from_le_bytes(bytes[at + 6..at + 14].try_into().unwrap());
        let start = at + 27 + segments;
        pages.push((granule, bytes[start..start + body].to_vec()));
        at = start + body;
    }
    assert!(pages[0].1.starts_with(b"OpusHead") && pages[1].1.starts_with(b"OpusTags"));
    assert!(pages[2..].windows(2).all(|w| w[1].0 > w[0].0), "the stamps only grow");
    // Three seconds plus the pre-skip (told in the header).
    let last = pages.last().unwrap().0;
    let skip = u16::from_le_bytes([pages[0].1[10], pages[0].1[11]]) as u64;
    assert!(last >= 3 * 48_000 + skip && last < 3 * 48_000 + skip + 960 * 3, "{last}");
    assert_eq!(fake.written(), 3 * 48_000, "the frames heard, pre-skip and padding cut");
    rig.engine.stop();
}

#[test]
fn offload_mp4_edit_list_as_delay_padding() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = made(&d, "a", 3, 440, &["-c:a", "aac", "-b:a", "128k"], "m4a");
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(&[(Coding::Aac, Support::Gapless)]);
    let rig = Rig::new(server, vec![("a".into(), "m4a".into(), 3_000)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.calls().contains(&Call::EndOfStream)), "{:?}", fake.calls());
    let calls = fake.calls();
    assert!(matches!(calls[0], Call::Open(Coded { coding: Coding::Aac, rate: 44_100, channels: 2 })), "{calls:?}");
    let Some(Call::DelayPadding(delay, padding)) = calls.iter().find(|c| matches!(c, Call::DelayPadding(..))) else { panic!("{calls:?}") };
    // ffmpeg primes 1024 frames (the edit start) and trims the last block in the sample table: no
    // padding, as media3 reads it.
    assert_eq!((*delay, *padding), (1024, 0));
    assert_eq!(fake.written(), 3 * 44_100, "the frames heard");
    rig.engine.stop();
}

// ---- bit-perfect ----

#[test]
fn bit_perfect_is_exact() {
    let a = ramp(44_100, 24, 1);
    let b = ramp(48_000, 16, 2);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &wav(44_100, 24, &a)), ("b", &wav(48_000, 16, &b))]);
    let mut app = app();
    app.gains.insert("a".into(), 0.5);
    app.gains.insert("b".into(), 0.25);
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 6.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let songs = vec![("a".into(), "wav".into(), 1_000), ("b".into(), "wav".into(), 1_000)];
    let rig = Rig::new(server, songs, app, None, Settings { sound: eq, ..Settings::default() });
    rig.engine.set_output(OutputFacts { usb: true, bit_perfect: true });
    rig.run(50);
    rig.engine.play_at(0, 0);
    assert!(rig.wait(20, |r| r.events.lock().contains(&Event::State(State::Ended))), "{:?}", rig.events.lock());
    let opened = rig.card.opened.lock().clone();
    assert_eq!(opened.iter().map(|f| (f.rate, f.bits)).collect::<Vec<_>>(), [(44_100, 24), (48_000, 16)], "a device for each song's own format");
    let heard = rig.card.heard.lock().clone();
    let (first, second) = heard.split_at(a.len());
    assert!(first.iter().zip(&a).all(|(v, s)| (v * 8_388_608.0) as i32 == *s), "the 24-bit song, bit for bit");
    assert!(second.iter().zip(&b).all(|(v, s)| (v * 32_768.0) as i32 == *s), "the 16-bit song, bit for bit");
    assert_eq!(second.len(), b.len(), "every sample of it");
    rig.engine.stop();
}

// ---- a live stream ----

/// An endless WAV stream with ICY blocks every 4 KB; the title changes at 64 KB.
struct Station;

struct Live {
    at: u64,
    since: usize,
    header: Vec<u8>,
    /// An ICY block still being handed over.
    meta: Vec<u8>,
}

const EVERY: usize = 4096;

impl Read for Live {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.meta.is_empty() {
            let n = buf.len().min(self.meta.len());
            buf[..n].copy_from_slice(&self.meta[..n]);
            self.meta.drain(..n);
            return Ok(n);
        }
        if !self.header.is_empty() {
            let n = buf.len().min(self.header.len()).min(EVERY - self.since);
            buf[..n].copy_from_slice(&self.header[..n]);
            self.header.drain(..n);
            self.since += n;
            return Ok(n);
        }
        if self.since == EVERY {
            self.since = 0;
            let music = self.at;
            let text = if (60_000..60_000 + EVERY as u64 * 2).contains(&music) { b"StreamTitle='Artist - Song';".to_vec() } else { Vec::new() };
            let blocks = text.len().div_ceil(16);
            self.meta = vec![blocks as u8];
            self.meta.extend_from_slice(&text);
            self.meta.resize(1 + blocks * 16, 0);
            return self.read(buf);
        }
        let n = buf.len().min(EVERY - self.since).min(4096) & !3;
        for (k, b) in buf[..n].iter_mut().enumerate() {
            *b = ((self.at + k as u64) / 4) as u8;
        }
        self.at += n as u64;
        self.since += n;
        Ok(n)
    }
}

impl ByteSource for Station {
    fn open(&self, _: &str, _: u64) -> Result<Body, nori_engine::OpenError> {
        Err("a live stream is opened live".into())
    }

    fn open_live(&self, _: &str) -> Result<(Body, Option<usize>), String> {
        let mut header = wav(44_100, 16, &[]);
        // Endless: the largest WAV length.
        header[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        header[40..44].copy_from_slice(&(u32::MAX - 36).to_le_bytes());
        Ok((Body { start: 0, len: None, reader: Box::new(Live { at: 0, since: 0, header, meta: Vec::new() }) }, Some(EVERY)))
    }
}

struct Radio;

impl Library for Radio {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        Ok(Located { source: Source::Live { url: id.into(), bytes: Arc::new(Station) }, hint: Some("wav".into()), duration_ms: None, estimated: false })
    }

    fn about(&self, id: &str) -> WindowSong {
        WindowSong { id: id.into(), title: id.into(), radio: true, ..Default::default() }
    }
}

#[test]
fn live_stream_strips_titles() {
    let queue = SharedQueue::default();
    queue.0.lock().set(vec!["radio:1".into()], Some(0), false, 0);
    let card = Card::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let clock = Virtual::default();
    let engine = Engine::start_on(Radio, app(), queue, Box::new(card.clone()), None, Config::default(), clock.clone(), move |e| seen.lock().push(e));
    let time = Stepper::new(clock, card.pull.clone());
    engine.queue_changed();
    engine.play_at(0, 0);
    time.until(Duration::from_secs(400), || card.heard.lock().len() >= 200_000);
    let heard = card.heard.lock().clone();
    assert!(heard.len() >= 200_000, "it plays: {} samples", heard.len());
    // No ICY bytes in the music.
    for (k, v) in heard.iter().enumerate().take(200_000) {
        let at = k as u64 * 2;
        let byte = |i: u64| (i / 4) as u8;
        let want = i16::from_le_bytes([byte(at), byte(at + 1)]) as f32 / 32768.0;
        assert_eq!(*v, want, "sample {k}");
    }
    // Said once playback reaches it.
    time.until(Duration::from_secs(400), || events.lock().contains(&Event::Title("Artist - Song".into())));
    assert!(events.lock().contains(&Event::Title("Artist - Song".into())), "{:?}", events.lock());
    assert_eq!(engine.status().state, State::Playing, "it goes on");
    engine.stop();
}

#[test]
fn stream_title_parsing() {
    assert_eq!(nori_engine::source::stream_title(b"StreamTitle='Muse - Uprising';StreamUrl='';\0\0"), Some("Muse - Uprising".into()));
    assert_eq!(nori_engine::source::stream_title(b"StreamTitle='';\0"), None);
    assert_eq!(nori_engine::source::stream_title(b"StreamTitle='Sigur R\xf3s - Hopp\xedpolla';"), Some("Sigur Rós - Hoppípolla".into()), "Latin-1");
    assert_eq!(nori_engine::source::stream_title(b"StreamTitle='Guns N' Roses - Patience';"), Some("Guns N' Roses - Patience".into()));
}

// ---- the offline bridge ----

/// `sim::App` whose error rule hands unreachable songs to the bridge.
struct Bridging(sim::App);

impl Host for Bridging {
    fn plan_for(&mut self, id: &str) -> Option<Plan> {
        self.0.plan_for(id)
    }
    fn wants_analysis(&mut self, id: &str) -> Option<u64> {
        self.0.wants_analysis(id)
    }
    fn analysed(&mut self, id: &str, a: Analyzer, channels: usize, frames: u64, rate: u32) {
        self.0.analysed(id, a, channels, frames, rate)
    }
    fn now_ms(&self) -> i64 {
        self.0.now_ms()
    }
}

impl App for Bridging {
    fn clock(&mut self, now_ms: i64) {
        self.0.clock(now_ms)
    }
    fn auto_mix(&self) -> bool {
        false
    }
    fn window(&mut self, window: Vec<WindowSong>, shuffling: bool) {
        self.0.window(window, shuffling)
    }
    fn measure_ahead<S: nori_player::pipeline::Songs>(&mut self, _: &mut S, _: &[String]) {}
    fn on_error(&mut self, kind: PlaybackError, _: bool) -> Option<OnError> {
        Some(if kind == PlaybackError::Network { OnError::Bridge } else { OnError::Skip })
    }
}

#[test]
fn unreachable_song_goes_to_bridge() {
    let a = ramp(44_100, 16, 3);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &wav(44_100, 16, &a))]);
    server.down.lock().push("b".into());
    let songs = vec![("a".into(), "wav".into(), 1_000), ("b".into(), "wav".into(), 1_000)];
    let rig = Rig::new(server, songs, Bridging(app()), None, Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(30, |r| r.events.lock().iter().any(|e| matches!(e, Event::Bridge { .. }))), "{:?}", rig.events.lock());
    let events = rig.events.lock().clone();
    assert!(!events.iter().any(|e| matches!(e, Event::Stopped { .. })), "the bridge takes over: not a stop: {events:?}");
    assert!(rig.wait(5, |r| r.engine.status().state == State::Paused));
    // The bridge's jump plays at once.
    rig.queue.0.lock().set(vec!["a".into(), "b".into(), "a".into()], Some(1), false, 0);
    rig.engine.queue_changed();
    rig.engine.play_at(2, 0);
    assert!(rig.wait(5, |r| r.engine.status().state == State::Playing));
    rig.engine.stop();
}

/// A server error status is the song's failure: skipped, not handed to the bridge.
#[test]
fn refused_song_is_not_network_failure() {
    let a = ramp(44_100, 16, 3);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &wav(44_100, 16, &a)), ("c", &wav(44_100, 16, &a))]);
    server.refused.lock().push(("b".into(), 404));
    let songs = vec![("a".into(), "wav".into(), 1_000), ("b".into(), "wav".into(), 1_000), ("c".into(), "wav".into(), 1_000)];
    let rig = Rig::new(server, songs, Bridging(app()), None, Settings::default());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(30, |r| r.heard_song("c")), "c after b is skipped: {:?}", rig.events.lock());
    let events = rig.events.lock().clone();
    assert!(!events.iter().any(|e| matches!(e, Event::Bridge { .. })), "not the bridge's: {events:?}");
    assert!(events.iter().any(|e| matches!(e, Event::Error { id, message } if id == "b" && message.contains("404"))), "b failed with the server's answer: {events:?}");
    rig.engine.stop();
}

// ---- repeat one on the CPU ----

#[test]
fn repeat_one_reports_loops() {
    let a = ramp(22_050, 16, 4);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &wav(44_100, 16, &a))]);
    let rig = Rig::new(server, vec![("a".into(), "wav".into(), 500)], app(), None, Settings::default());
    rig.engine.set_repeat(REPEAT_ONE);
    rig.engine.play_at(0, 0);
    assert!(rig.wait(20, |r| r.events.lock().iter().filter(|e| matches!(e, Event::Looped { index: 0, .. })).count() >= 3), "{:?}", rig.events.lock());
    let songs = rig.events.lock().iter().filter(|e| matches!(e, Event::Song { .. })).count();
    assert_eq!(songs, 1, "the song itself is said once; each time round is a loop");
    rig.engine.stop();
}

// ---- why the CPU plays ----

#[test]
fn cpu_song_reports_why_not_offloaded() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (lame, fl) = (mp3(&d, "lame", 10, 440), flac(&d, "fl", 10, 550));
    let cases: [(&[u8], &str, &[(Coding, Support)], Settings, &str); 3] = [
        (&fl, "flac", MP3_ONLY, offload(), "FLAC is not a compression the output decodes"),
        (&lame, "mp3", &[], offload(), "the output does not decode MP3 at 44100 Hz x2"),
        (&lame, "mp3", MP3_ONLY, Settings::default(), "offload is off in the settings"),
    ];
    for (song, ext, support, settings, why) in cases {
        let server = Arc::new(Server::default());
        serve(&server, &[("a", song)]);
        let fake = Fake::new(support);
        let rig = Rig::new(server, vec![("a".into(), ext.into(), 10_000)], app(), Some(fake.clone()), settings);
        rig.engine.play_at(0, 0);
        assert!(rig.wait(10, |r| r.engine.status().pcm_why.is_some_and(|w| w.contains(why))), "{why}: {:?}", rig.engine.status().pcm_why);
        assert!(!rig.engine.status().offloaded);
        assert!(fake.calls().iter().all(|c| !matches!(c, Call::Open(_))), "no track for the chip: {:?}", fake.calls());
        rig.engine.stop();
    }
}

#[test]
fn plain_offload_takes_gapless_song() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    // No LAME tag: no gap to cut.
    let a = made(&d, "a", 10, 440, &["-c:a", "libmp3lame", "-b:a", "128k", "-write_xing", "0"], "mp3");
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(&[(Coding::Mp3, Support::Plain)]);
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 10_000)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.engine.status().offloaded), "{:?}", rig.engine.status().pcm_why);
    assert!(fake.calls().contains(&Call::DelayPadding(0, 0)), "{:?}", fake.calls());
    assert_eq!(rig.engine.status().pcm_why, None);
    rig.engine.stop();
}

// ---- offload without gapless support ----

const PLAIN_MP3: &[(Coding, Support)] = &[(Coding::Mp3, Support::Plain)];

/// Half-minute LAME MP3s (tagged with delay and padding) served under `ids`.
fn lame_songs(d: &Path, server: &Server, ids: &[&str]) -> Vec<(String, String, i64)> {
    for (k, id) in ids.iter().enumerate() {
        let f = mp3(d, id, 30, 440 + 110 * k as u32);
        serve(server, &[(id, &f)]);
    }
    ids.iter().map(|id| (id.to_string(), "mp3".to_string(), 30_000)).collect()
}

fn on(albums: &[(&str, &str, i32)]) -> Vec<(String, String, i32)> {
    albums.iter().map(|(id, album, track)| (id.to_string(), album.to_string(), *track)).collect()
}

#[test]
fn plain_offload_takes_unrelated_songs() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let server = Arc::new(Server::default());
    let songs = lame_songs(&d, &server, &["x", "y", "z"]);
    let fake = Fake::new(PLAIN_MP3);
    // Shuffled albums: no album neighbours adjacent.
    let rig = Rig::albums(server, songs, on(&[("x", "X", 3), ("y", "Y", 7), ("z", "Z", 1)]), app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.engine.status().offloaded), "{:?}", rig.engine.status().pcm_why);
    let why = rig.engine.status().pcm_why.unwrap_or_default();
    assert!(why.contains("near silence at its ends") && why.contains("does not do gapless offload"), "{why}");
    // Each next song on the same track from its start.
    for next in ["y", "z"] {
        assert!(rig.wait(10, |_| fake.written() > 0));
        fake.advance(1 << 40);
        assert!(rig.wait(10, |r| r.heard_song(next) && r.engine.status().offloaded), "{next}: {:?}", rig.events.lock());
    }
    let calls = fake.calls();
    assert_eq!(calls.iter().filter(|c| matches!(c, Call::Open(_))).count(), 1, "one track: {calls:?}");
    assert_eq!(calls.iter().filter(|c| matches!(c, Call::DelayPadding(576, _))).count(), 3, "every song from its start: {calls:?}");
    assert!(rig.card.opened.lock().is_empty(), "the CPU's output was never opened");
    rig.engine.stop();
}

#[test]
fn plain_offload_keeps_album_on_cpu() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let server = Arc::new(Server::default());
    let songs = lame_songs(&d, &server, &["a1", "a2"]);
    let fake = Fake::new(PLAIN_MP3);
    let rig = Rig::albums(server, songs, on(&[("a1", "A", 1), ("a2", "A", 2)]), app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    let why = "joins a song of its album without a gap, which needs gapless offload, and the output does not do it";
    assert!(rig.wait(10, |r| r.engine.status().pcm_why.is_some_and(|w| w.starts_with("MP3 with an encoder delay of 576") && w.contains(why))), "{:?}", rig.engine.status().pcm_why);
    assert!(rig.wait(30, |r| r.heard_song("a2")));
    assert!(!rig.engine.status().offloaded);
    assert!(fake.calls().iter().all(|c| !matches!(c, Call::Open(_))), "no track for the chip: {:?}", fake.calls());
    rig.engine.stop();
}

#[test]
fn plain_offload_album_handover_both_ways() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let server = Arc::new(Server::default());
    let songs = lame_songs(&d, &server, &["x", "a1", "a2", "y"]);
    let fake = Fake::new(PLAIN_MP3);
    let albums = on(&[("x", "X", 5), ("a1", "A", 1), ("a2", "A", 2), ("y", "Y", 9)]);
    let rig = Rig::albums(server, songs, albums, app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.engine.status().offloaded && fake.written() > 0), "x on the chip: {:?}", rig.engine.status().pcm_why);
    fake.advance(1 << 40);
    // a1 joins a2 gaplessly: the CPU from a1's start.
    assert!(rig.wait(10, |r| r.heard_song("a1") && !r.engine.status().offloaded && !r.card.heard.lock().is_empty()), "{:?}", rig.events.lock());
    assert_eq!(fake.calls().iter().filter(|c| matches!(c, Call::DelayPadding(..))).count(), 1, "only x went to the chip: {:?}", fake.calls());
    assert_eq!(fake.written(), 0, "and nothing of a1 after x");
    // After a2, y is offloaded again from its start.
    assert!(rig.wait(20, |r| r.heard_song("y") && r.engine.status().offloaded), "{:?}", rig.events.lock());
    let heard = rig.card.heard.lock().len() / 2;
    assert!(heard + 44_100 / 2 >= 2 * 30 * 44_100, "a1 and a2 whole on the CPU: {heard} frames");
    assert_eq!(fake.calls().iter().filter(|c| matches!(c, Call::DelayPadding(576, _))).count(), 2, "x and y from their starts: {:?}", fake.calls());
    rig.engine.stop();
}

// ---- a play head that makes no sense ----

/// Two songs of `secs` on one gapless track, both written, a second played; head moved at clock pace.
fn two_on_the_chip(d: &Path, secs: u32) -> Option<(Rig, Fake)> {
    two_on_the_chip_with(d, secs, app())
}

fn two_on_the_chip_with(d: &Path, secs: u32, app: impl App + Send + 'static) -> Option<(Rig, Fake)> {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return None;
    }
    let (a, b) = (mp3(d, "a", secs, 440), mp3(d, "b", secs, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let fake = Fake::new(MP3_ONLY);
    fake.pace(Some(1.0));
    let ms = secs as i64 * 1000;
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), ms), ("b".into(), "mp3".into(), ms)], app, Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    let both = 2 * secs as u64 * 44_100;
    assert!(written_up_to(&rig, &fake, both), "both written: {}", fake.written());
    assert_eq!(fake.written(), both, "both written, not a frame more");
    rig.run(1_200);
    fake.advance(44_100);
    assert!(rig.wait(5, |r| r.engine.status().position_ms >= 990), "{:?}", rig.engine.status());
    Some((rig, fake))
}

/// Waits until `frames` were written to the offload track, giving the loader real time first (the
/// clock's byte wait is capped, so on a busy machine it would otherwise run on).
fn written_up_to(rig: &Rig, fake: &Fake, frames: u64) -> bool {
    let started = std::time::Instant::now();
    let mut last = (fake.written(), std::time::Instant::now());
    while fake.written() < frames {
        if started.elapsed() > Duration::from_secs(120) || rig.now_ms() > 60_000 {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
        rig.time.clock.settle();
        let now = fake.written();
        if now != last.0 {
            last = (now, std::time::Instant::now());
        } else if last.1.elapsed() > Duration::from_millis(50) {
            rig.run(10);
        }
    }
    rig.time.clock.settle();
    true
}

/// Waits until the engine read the queued head values.
fn read_all(rig: &Rig, fake: &Fake) {
    assert!(rig.wait(5, |_| fake.0.lock().readings.is_empty()), "the engine read the play head");
    rig.run(400);
}

#[test]
fn unreadable_head_skips_no_song() {
    let d = dir();
    let Some((rig, fake)) = two_on_the_chip(&d, 20) else { return };
    // A failed call, zero, another failure: a second into a, b after it on the track.
    fake.read_as(&[None, Some(0), None]);
    read_all(&rig, &fake);
    let s = rig.engine.status();
    assert!(!rig.heard_song("b") && s.index == Some(0) && s.offloaded, "still a, on the chip: {s:?} {:?}", rig.events.lock());
    assert!((990..=1_100).contains(&s.position_ms), "where the ear was: {s:?}");
    let notes = fake.notes();
    assert!(notes.iter().any(|n| n.contains("could not be read")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("read 0 after 44100, not at a join")), "{notes:?}");
    // With the true count back, b starts where a ends.
    fake.pace(None);
    fake.advance(19 * 44_100 + 100);
    assert!(rig.wait(5, |r| r.heard_song("b")), "{:?}", rig.events.lock());
    let at = rig.engine.status().position_ms;
    assert!((0..100).contains(&at), "at the start of b: {at}");
    rig.engine.stop();
}

#[test]
fn head_restart_after_pause_skips_no_song() {
    let d = dir();
    let Some((rig, fake)) = two_on_the_chip(&d, 20) else { return };
    rig.engine.pause();
    assert!(rig.wait(5, |r| r.engine.status().state == State::Paused));
    // Standby while paused restarts the count.
    fake.count_again();
    rig.engine.play();
    assert!(rig.wait(5, |r| r.engine.status().state == State::Playing));
    rig.run(400);
    fake.advance(13_230);
    rig.run(400);
    fake.read_as(&[]);
    rig.run(100);
    let s = rig.engine.status();
    assert!(!rig.heard_song("b") && s.index == Some(0) && s.offloaded, "still a: {s:?} {:?}", rig.events.lock());
    assert!((1_250..=1_450).contains(&s.position_ms), "a second and 300 ms into a: {s:?}");
    assert!(fake.notes().iter().any(|n| n.contains("counts again from nought")), "{:?}", fake.notes());
    fake.pace(None);
    fake.advance(19 * 44_100 - 13_230 + 100);
    assert!(rig.wait(5, |r| r.heard_song("b")), "{:?}", rig.events.lock());
    let at = rig.engine.status().position_ms;
    assert!((0..100).contains(&at), "at the start of b: {at}");
    rig.engine.stop();
}

#[test]
fn head_ahead_of_clock_hands_to_cpu() {
    let d = dir();
    let app = Logged::new();
    let Some((rig, fake)) = two_on_the_chip_with(&d, 20, app.clone()) else { return };
    // A 10 s jump, a failed call, the jump again.
    fake.read_as(&[Some(11 * 44_100), None, Some(11 * 44_100)]);
    assert!(rig.wait(10, |r| !r.engine.status().offloaded && r.card.heard.lock().len() > 44_100), "the CPU took over: {:?}", rig.engine.status());
    let s = rig.engine.status();
    assert!(!rig.heard_song("b") && s.index == Some(0), "a, on the CPU: {s:?} {:?}", rig.events.lock());
    assert!(s.position_ms >= 990 && s.position_ms < 11_000, "from where the ear was, not where the head said: {s:?}");
    // Why the CPU took over.
    let log = app.log();
    let why = log.iter().filter_map(|l| l.strip_prefix("playing on the CPU: ")).next().unwrap_or_default();
    assert!(why.contains("could not be followed") && why.contains("ahead of the clock"), "{why}: {log:?}");
    assert!(fake.notes().iter().any(|n| n.starts_with("offload given up")), "{:?}", fake.notes());
    rig.engine.stop();
}

#[test]
fn late_presented_ends_no_song() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 20, 440);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(MP3_ONLY);
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 20_000)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.calls().contains(&Call::EndOfStream)), "{:?}", fake.calls());
    // A seek near the end: the new track accepts its end of stream on the third try, and meanwhile
    // "presented" arrives about the previous track.
    fake.0.lock().refuse_eos = 2;
    rig.engine.seek(18_000);
    assert!(rig.wait(5, |_| opens(&fake) == 2 && fake.written() > 0), "{:?}", fake.calls());
    fake.0.lock().stale_presented = true;
    fake.read_as(&[]);
    rig.run(300);
    assert!(!rig.events.lock().contains(&Event::State(State::Ended)), "not ended by a word about another end of stream");
    assert!(rig.wait(5, |_| fake.calls().iter().filter(|c| **c == Call::EndOfStream).count() == 2), "said again until taken: {:?}", fake.calls());
    rig.run(200);
    let s = rig.engine.status();
    assert!(s.state == State::Playing && s.offloaded, "{s:?}");
    // Ended, with a perf note saying how.
    fake.advance(3 * 44_100);
    assert!(rig.wait(5, |r| r.events.lock().contains(&Event::State(State::Ended))), "{:?}", rig.events.lock());
    let notes = fake.notes();
    assert!(notes.iter().any(|n| n.contains("would not take the end of stream while its track played (2 of 3)")), "{notes:?}");
    assert!(notes.iter().any(|n| n.starts_with("a ended by the play head")), "{notes:?}");
    rig.engine.stop();
}

#[test]
fn refused_end_of_stream_hands_to_cpu() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 10, 440);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(MP3_ONLY);
    fake.0.lock().refuse_eos = 100;
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 10_000)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| !r.engine.status().offloaded && r.card.heard.lock().len() > 44_100), "the CPU took over: {:?}", rig.engine.status());
    let why = rig.engine.status().pcm_why.unwrap_or_default();
    assert!(why.contains("would not take the end of stream"), "{why}");
    assert!(!fake.calls().contains(&Call::EndOfStream));
    rig.engine.stop();
}

#[test]
fn track_holds_at_most_four_minutes() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 300, 440);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(MP3_ONLY);
    // A track granting four times the request: a whole five-minute song at once.
    fake.0.lock().takes = 4;
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 300_000)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.written() >= 240 * 44_100), "{}", fake.written());
    rig.run(300);
    let written = fake.written();
    // Four minutes plus the last 256 KB write.
    assert!(written <= 257 * 44_100, "four minutes ahead at most: {} s", written / 44_100);
    assert!(!fake.calls().contains(&Call::EndOfStream), "the song is not written to its end yet");
    // Under 30 s left: the rest is written.
    fake.advance(written - 20 * 44_100);
    assert!(rig.wait(5, |_| fake.written() == 300 * 44_100), "{}", fake.written());
    assert!(rig.wait(5, |_| fake.calls().contains(&Call::EndOfStream)));
    rig.engine.stop();
}

/// Offloaded, the engine sleeps for minutes; `look` reads the offload track's position at once.
#[test]
fn look_reads_offload_place() {
    let d = dir();
    let Some((rig, fake)) = two_on_the_chip(&d, 20) else { return };
    // Ten seconds play without waking the engine.
    rig.run(10_000);
    fake.advance_quietly(10 * 44_100);
    let stale = rig.engine.status();
    assert!(stale.offloaded && stale.position_ms < 2_000, "the engine has not looked: {stale:?}");
    rig.engine.look();
    // Within 20 ms, not at the next wake.
    assert!(rig.time.until(Duration::from_millis(20), || rig.engine.status().position_ms >= 10_990), "{:?}", rig.engine.status());
    let s = rig.engine.status();
    assert!(s.offloaded && s.index == Some(0) && s.position_ms <= 11_400, "eleven seconds into a, on the chip: {s:?}");
    assert!(!fake.notes().iter().any(|n| n.contains("ahead of the clock")), "{:?}", fake.notes());
    rig.engine.stop();
}

#[test]
fn pause_after_sleep_keeps_place() {
    let d = dir();
    let Some((rig, fake)) = two_on_the_chip(&d, 20) else { return };
    // Paused before the engine looked again.
    rig.run(1_500);
    fake.advance_quietly(44_100);
    rig.engine.pause();
    assert!(rig.wait(5, |r| r.engine.status().state == State::Paused));
    rig.run(300);
    rig.engine.play();
    fake.read_as(&[]);
    read_all(&rig, &fake);
    let s = rig.engine.status();
    assert!(s.offloaded && s.index == Some(0) && s.position_ms >= 1_990, "two seconds into a, still on the chip: {s:?}");
    assert!(!fake.notes().iter().any(|n| n.contains("ahead of the clock")), "{:?}", fake.notes());
    rig.engine.stop();
}

/// Two songs of `secs` on a Galaxy S22 offload track ([`Fake::phone`]).
fn two_on_a_phone(d: &Path, secs: u32, stamps: bool, head_stuck: bool) -> Option<(Rig, Fake)> {
    two_on_a_phone_with(d, secs, stamps, head_stuck, false)
}

/// [`two_on_a_phone`], with [`Fake::jittery`] timestamps if `jittery`.
fn two_on_a_phone_with(d: &Path, secs: u32, stamps: bool, head_stuck: bool, jittery: bool) -> Option<(Rig, Fake)> {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return None;
    }
    let (a, b) = (mp3(d, "a", secs, 440), mp3(d, "b", secs, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let fake = Fake::new(MP3_ONLY);
    fake.phone(stamps, head_stuck);
    if jittery {
        fake.jittery();
    }
    let ms = secs as i64 * 1000;
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), ms), ("b".into(), "mp3".into(), ms)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    Some((rig, fake))
}

/// Both songs play through on the small track, offloaded, with no silence.
fn plays_through_on_the_phone(rig: &Rig, fake: &Fake, secs: u32) {
    let ended = rig.time.until(Duration::from_secs(2 * secs as u64 + 10), || rig.events.lock().contains(&Event::State(State::Ended)));
    assert!(ended, "{:?} {:?} {:?}", rig.engine.status(), fake.notes(), rig.events.lock());
    assert!(rig.heard_song("b"), "{:?}", rig.events.lock());
    assert_eq!(fake.written(), 2 * secs as u64 * 44_100, "every frame of both songs: {:?} {:?}", fake.notes(), fake.calls().iter().filter(|c| !matches!(c, Call::Write(..))).collect::<Vec<_>>());
    let head = fake.0.lock().head;
    assert_eq!(head, fake.written(), "all of it played");
    assert_eq!(fake.starved_ms(), 0, "never out of music: {:?}", fake.notes());
    assert_eq!(opens(fake), 1, "one track for both");
    assert!(rig.card.opened.lock().is_empty(), "the CPU's output was never opened");
    let notes = fake.notes();
    assert!(!notes.iter().any(|n| n.contains("given up")), "{notes:?}");
    // The grant note for the perf report.
    assert!(notes.iter().any(|n| n.contains("granted a track of 64 KB of the") && n.contains("topped up when the platform asks")), "{notes:?}");
}

#[test]
fn small_track_dead_head_uses_timestamps() {
    let d = dir();
    let secs = 30;
    let Some((rig, fake)) = two_on_a_phone(&d, secs, true, true) else { return };
    // At 10 s the position follows the timestamp, not the stuck head.
    assert!(rig.time.until(Duration::from_secs(20), || fake.0.lock().head >= 10 * 44_100));
    rig.run(100);
    let s = rig.engine.status();
    // As of the last wake (about two per track fill).
    assert!(s.offloaded && s.index == Some(0) && (7_000..=10_200).contains(&s.position_ms), "{s:?}");
    plays_through_on_the_phone(&rig, &fake, secs);
    rig.engine.stop();
}

#[test]
fn small_track_without_timestamps_uses_head() {
    let d = dir();
    let secs = 30;
    let Some((rig, fake)) = two_on_a_phone(&d, secs, false, false) else { return };
    plays_through_on_the_phone(&rig, &fake, secs);
    rig.engine.stop();
}

/// Counts stuck at zero while the platform keeps asking: after the slack, the CPU takes over at the
/// clock's position.
#[test]
fn dead_counts_hand_to_cpu_at_clock() {
    let d = dir();
    let Some((rig, fake)) = two_on_a_phone(&d, 30, false, true) else { return };
    let took = rig.time.until(Duration::from_secs(30), || !rig.engine.status().offloaded && rig.card.heard.lock().len() > 2 * 44_100);
    assert!(took, "the CPU took over: {:?} {:?}", rig.engine.status(), fake.notes());
    let s = rig.engine.status();
    assert!(s.index == Some(0) && !rig.heard_song("b"), "a, on the CPU: {s:?}");
    // About 12 s in: the slack plus what the CPU played since.
    assert!((10_000..=14_000).contains(&s.position_ms), "where the clock puts the ear, not at nought: {s:?}");
    let notes = fake.notes();
    let given_up = notes.iter().find(|n| n.starts_with("offload given up, the CPU plays on from")).cloned().unwrap_or_default();
    assert!(given_up.contains("play head stood at 0") && given_up.contains("asked for more") && given_up.contains("where the clock puts the ear"), "{notes:?}");
    // No silence before the takeover.
    assert_eq!(fake.starved_ms(), 0, "{notes:?}");
    rig.engine.stop();
}

#[test]
fn still_timestamp_gives_way_to_head() {
    let d = dir();
    let secs = 30;
    let Some((rig, fake)) = two_on_a_phone(&d, secs, true, false) else { return };
    assert!(rig.time.until(Duration::from_secs(10), || fake.0.lock().head >= 3 * 44_100));
    // The timestamp freezes; the play head moves on.
    let at = fake.0.lock().head;
    fake.0.lock().frozen_stamp = Some(at);
    plays_through_on_the_phone(&rig, &fake, secs);
    assert!(fake.notes().iter().any(|n| n.contains("the play head is followed instead")), "{:?}", fake.notes());
    rig.engine.stop();
}

#[test]
fn jittery_timestamp_keeps_place() {
    let d = dir();
    let secs = 20;
    let Some((rig, fake)) = two_on_a_phone_with(&d, secs, true, true, true) else { return };
    // The reported position never runs ahead of what was presented. Regression: jitter taken as a
    // restart put it 160-320 ms ahead.
    let mut ahead_ms = 0i64;
    let mut looked = 0;
    let mut watch = |rig: &Rig| {
        // The engine reads the count every step.
        fake.0.lock().wake();
        let s = rig.engine.status();
        let (head, flushes) = {
            let c = fake.0.lock();
            (c.head, c.flushes)
        };
        // a before the skip, b from zero after the flush.
        if s.offloaded && !s.switching && s.state == State::Playing && s.index == Some(flushes as usize) {
            let truth = (head * 1000 / 44_100) as i64;
            ahead_ms = ahead_ms.max(s.position_ms - truth);
            looked += 1;
        }
    };
    assert!(rig.time.until(Duration::from_secs(20), || {
        watch(&rig);
        fake.0.lock().head >= 5 * 44_100
    }));
    let at = rig.engine.status().position_ms;
    assert!((4_000..=5_100).contains(&at), "five seconds into a, not further: {at} {:?} {:?}", fake.notes(), rig.engine.status());
    // A skip: after the flush the first timestamps are a's stale count.
    rig.engine.go_to(1, 0);
    let ended = rig.time.until(Duration::from_secs(secs as u64 + 10), || {
        watch(&rig);
        rig.events.lock().contains(&Event::State(State::Ended))
    });
    let notes = fake.notes();
    assert!(ended, "{:?} {notes:?}", rig.engine.status());
    assert!(looked > 20, "the ear was looked at often: {looked}");
    assert!(ahead_ms <= 20, "the ear {ahead_ms} ms ahead of the chip: {notes:?}");
    assert!(rig.heard_song("b"));
    let c = fake.0.lock();
    // b ends within the 100 ms end slack, not earlier.
    assert!(c.flushes == 1 && c.head + 4_410 >= secs as u64 * 44_100, "all of b played on the flushed track: {}", c.head);
    drop(c);
    assert_eq!(fake.starved_ms(), 0, "never out of music: {notes:?}");
    assert!(rig.card.opened.lock().is_empty(), "the CPU's output was never opened");
    assert!(!notes.iter().any(|n| n.contains("given up") || n.contains("counts again from nought")), "no count taken as started again: {notes:?}");
    // The stale readings are noted; the steps back only once, at the song's end.
    let of_count: Vec<&String> = notes.iter().filter(|n| !n.contains("granted a track") && !n.contains(" ended ")).collect();
    assert!(of_count.len() <= 2 && of_count.iter().all(|n| n.contains("ahead of the clock")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains(" ended ") && n.contains("a moment back")), "{notes:?}");
    rig.engine.stop();
}

#[test]
fn offload_fast_next_moves_one_song_per_press() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let ids = ["a", "b", "c", "d", "e"];
    let files: Vec<Vec<u8>> = ids.iter().enumerate().map(|(k, id)| mp3(&d, id, 10, 440 + 110 * k as u32)).collect();
    for gap in [0u64, 150] {
        let server = Arc::new(Server::default());
        serve(&server, &ids.iter().zip(&files).map(|(id, f)| (*id, f.as_slice())).collect::<Vec<_>>());
        let fake = Fake::new(MP3_ONLY);
        let songs = ids.iter().map(|id| (id.to_string(), "mp3".to_string(), 10_000)).collect();
        let rig = Rig::new(server, songs, app(), Some(fake.clone()), offload());
        rig.engine.play_at(0, 0);
        assert!(rig.wait(10, |_| fake.written() > 0));
        fake.advance(2 * 44_100);
        assert!(rig.wait(5, |r| r.engine.status().position_ms >= 1_900));
        let mut last = 0;
        for to in 1..=3 {
            last = rig.engine.go_to(to, 0);
            rig.run(gap);
        }
        assert!(rig.wait(5, |r| r.engine.status().index == Some(3)), "gap {gap}: {:?}", rig.events.lock());
        // A second into d.
        assert!(rig.wait(5, |_| fake.written() > 0));
        fake.advance(44_100);
        rig.run(300);
        let events = rig.events.lock().clone();
        let said: Vec<(usize, u64)> = events.iter().filter_map(|e| if let Event::Song { index, jumps, .. } = e { Some((*index, *jumps)) } else { None }).collect();
        let after: Vec<usize> = said.iter().filter(|s| s.1 >= last).map(|s| s.0).collect();
        assert_eq!(after, vec![3], "gap {gap}: d once after the last press: {said:?}");
        assert!(said.windows(2).all(|w| w[0].0 < w[1].0), "gap {gap}: forwards only: {said:?}");
        let s = rig.engine.status();
        assert_eq!(s.index, Some(3), "gap {gap}");
        assert!((900..2_000).contains(&s.position_ms), "gap {gap}: a second into d: {}", s.position_ms);
        rig.engine.stop();
    }
}

// ---- the transition settings and offload, changed while music plays ----

/// A shared `sim::App` the test changes and reads while playing.
#[derive(Clone)]
/// The second field, above 0, is where every plan enters its incoming song, µs.
struct Watched(Arc<Mutex<sim::App>>, Arc<Mutex<i64>>);

impl Watched {
    fn automix() -> Watched {
        let mut a = sim::App::new();
        a.prefs = nori_player::transitions::TransitionPrefs { auto_mix: true, auto_mix_max_s: 6, echo_out: false, ..sim::prefs_off() };
        Watched(Arc::new(Mutex::new(a)), Arc::default())
    }

    fn log(&self) -> Vec<String> {
        self.0.lock().log.clone()
    }
}

impl Host for Watched {
    fn plan_for(&mut self, id: &str) -> Option<Plan> {
        let skip = *self.1.lock();
        self.0.lock().plan_for(id).map(|p| if skip > 0 { Plan { in_skip_us: skip, ..p } } else { p })
    }
    fn wants_analysis(&mut self, id: &str) -> Option<u64> {
        self.0.lock().wants_analysis(id)
    }
    fn analysed(&mut self, id: &str, a: Analyzer, channels: usize, frames: u64, rate: u32) {
        self.0.lock().analysed(id, a, channels, frames, rate)
    }
    fn log(&mut self, message: &str) {
        self.0.lock().log(message)
    }
    fn now_ms(&self) -> i64 {
        self.0.lock().now_ms()
    }
}

impl App for Watched {
    fn clock(&mut self, now_ms: i64) {
        self.0.lock().clock(now_ms)
    }
    fn auto_mix(&self) -> bool {
        false
    }
    fn window(&mut self, window: Vec<WindowSong>, shuffling: bool) {
        self.0.lock().window(window, shuffling)
    }
    fn measure_ahead<S: nori_player::pipeline::Songs>(&mut self, _: &mut S, _: &[String]) {}
}

fn automix() -> Settings {
    Settings { auto_mix: true, ..offload() }
}

#[test]
fn automix_on_moves_offload_to_cpu() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b) = (mp3(&d, "a", 40, 440), mp3(&d, "b", 30, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let fake = Fake::new(MP3_ONLY);
    let app = Watched::automix();
    let songs = vec![("a".into(), "mp3".into(), 40_000), ("b".into(), "mp3".into(), 30_000)];
    let rig = Rig::new(server, songs, app.clone(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.engine.status().offloaded && fake.written() > 0), "{:?}", rig.engine.status().pcm_why);
    fake.advance(5 * 44_100);
    assert!(rig.wait(5, |r| r.engine.status().position_ms >= 4_900), "{:?}", rig.engine.status());
    // The offload track cannot mix: the CPU takes over at once, not at the next song.
    rig.engine.set_settings(automix());
    rig.engine.replan();
    assert!(rig.wait(10, |r| !r.engine.status().offloaded && r.card.heard.lock().len() > 2 * 44_100), "the CPU took over: {:?}", rig.engine.status());
    assert!(fake.calls().contains(&Call::Close), "the chip's track was let go");
    assert_eq!(rig.engine.status().pcm_why.as_deref(), Some("AutoMix is on"));
    let s = rig.engine.status();
    assert!(s.index == Some(0) && s.position_ms >= 5_000, "a from where the ear was: {s:?}");
    assert!(rig.wait(20, |r| r.events.lock().contains(&Event::State(State::Ended))), "{:?}", app.log());
    let log = app.log();
    assert!(log.iter().any(|l| l.contains("transition a -> b")), "{log:?}");
    assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{log:?}");
    rig.engine.stop();
}

/// Offloaded with AutoMix off; the equalizer goes on (CPU takeover), then AutoMix a few seconds later.
#[test]
fn automix_on_after_equalizer_mixes() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b) = (mp3(&d, "a", 40, 440), mp3(&d, "b", 30, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let fake = Fake::new(MP3_ONLY);
    let app = Watched::automix();
    app.0.lock().prefs = sim::prefs_off();
    let songs = vec![("a".into(), "mp3".into(), 40_000), ("b".into(), "mp3".into(), 30_000)];
    let rig = Rig::new(server, songs, app.clone(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.engine.status().offloaded && fake.written() > 0), "{:?}", rig.engine.status().pcm_why);
    fake.advance(5 * 44_100);
    assert!(rig.wait(5, |r| r.engine.status().position_ms >= 4_900), "{:?}", rig.engine.status());
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    rig.engine.set_settings(Settings { sound: eq.clone(), ..offload() });
    assert!(rig.wait(10, |r| !r.engine.status().offloaded && r.card.heard.lock().len() > 2 * 44_100), "the CPU took over: {:?}", rig.engine.status());
    rig.run(1_000);
    app.0.lock().prefs = Watched::automix().0.lock().prefs;
    rig.engine.set_settings(Settings { sound: eq, ..automix() });
    rig.engine.replan();
    assert!(rig.wait(40, |r| r.events.lock().contains(&Event::State(State::Ended))), "{:?}", app.log());
    let log = app.log();
    assert!(log.iter().any(|l| l.contains("transition a -> b")), "{log:?}");
    assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{log:?}");
    rig.engine.stop();
}

#[test]
fn automix_off_returns_to_offload() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let (a, b) = (mp3(&d, "a", 60, 440), mp3(&d, "b", 30, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let fake = Fake::new(MP3_ONLY);
    let app = Watched::automix();
    let songs = vec![("a".into(), "mp3".into(), 60_000), ("b".into(), "mp3".into(), 30_000)];
    let rig = Rig::new(server, songs, app.clone(), Some(fake.clone()), automix());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.card.heard.lock().len() > 2 * 44_100), "the CPU plays a");
    assert!(!rig.engine.status().offloaded);
    app.0.lock().prefs = sim::prefs_off();
    let before = rig.engine.status().position_ms;
    rig.engine.set_settings(offload());
    rig.engine.replan();
    assert!(rig.wait(10, |r| r.engine.status().offloaded), "{:?}", rig.engine.status().pcm_why);
    let s = rig.engine.status();
    assert!(s.index == Some(0) && s.position_ms >= before, "a from where the ear was ({before} ms): {s:?}");
    rig.engine.stop();
}

#[test]
fn automix_off_keeps_album_on_cpu() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let server = Arc::new(Server::default());
    let songs = lame_songs(&d, &server, &["a1", "a2"]);
    let fake = Fake::new(PLAIN_MP3);
    let app = Watched::automix();
    let rig = Rig::albums(server, songs, on(&[("a1", "A", 1), ("a2", "A", 2)]), app.clone(), Some(fake.clone()), automix());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.card.heard.lock().len() > 2 * 44_100), "the CPU plays a1");
    app.0.lock().prefs = sim::prefs_off();
    rig.engine.set_settings(offload());
    rig.engine.replan();
    // Offload wanted again, but a1 joins a2 gaplessly, which this track cannot: the CPU plays on.
    let why = "joins a song of its album without a gap";
    assert!(rig.wait(10, |r| r.engine.status().pcm_why.is_some_and(|w| w.contains(why))), "{:?}", rig.engine.status().pcm_why);
    assert!(rig.engine.status().offload_wanted && !rig.engine.status().offloaded);
    assert!(fake.calls().iter().all(|c| !matches!(c, Call::Open(_))), "no track for the chip: {:?}", fake.calls());
    rig.engine.stop();
}

#[test]
fn offload_setting_moves_song_both_ways() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 60, 440);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(MP3_ONLY);
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 60_000)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.engine.status().offloaded && fake.written() > 0), "{:?}", rig.engine.status().pcm_why);
    assert!(rig.wait(5, |r| { fake.advance(4_410); r.engine.status().position_ms >= 4_900 }), "{:?}", rig.engine.status());
    rig.engine.set_settings(Settings::default());
    assert!(rig.wait(10, |r| !r.engine.status().offloaded && r.card.heard.lock().len() > 44_100), "the CPU took over");
    assert_eq!(rig.engine.status().pcm_why.as_deref(), Some("offload is off in the settings"));
    assert!(rig.engine.status().position_ms >= 4_900, "{:?}", rig.engine.status());
    rig.engine.set_settings(offload());
    assert!(rig.wait(10, |r| r.engine.status().offloaded), "back on the chip: {:?}", rig.engine.status().pcm_why);
    rig.engine.stop();
}

// ---- CPU takeover at the offload track's real position ----

/// What happened offloaded before the equalizer went on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Before {
    Played,
    /// 5 s, 2 s paused, 5 s.
    PausedAndResumed,
    /// 5 s, a skip to b, the equalizer on 0.5 s into b.
    Skipped,
}

/// On a Galaxy S22 offload track (jittery timestamps if `jittery`) the equalizer goes on: the CPU takes
/// over at the track's position and plays on, status and events following.
fn equalizer_on_a_phone(jittery: bool, before: Before) {
    let d = dir();
    let secs = 30;
    let Some((rig, fake)) = two_on_a_phone_with(&d, secs, true, true, jittery) else { return };
    rig.engine.position_updates(Some(Duration::from_millis(250)));
    let head = || fake.0.lock().head;
    let played = |frames: u64| rig.time.until(Duration::from_secs(40), || head() >= frames);
    let song = match before {
        Before::Played => {
            assert!(played(10 * 44_100));
            0
        }
        Before::PausedAndResumed => {
            assert!(played(5 * 44_100));
            rig.engine.pause();
            rig.run(2_000);
            assert!(!fake.0.lock().playing, "paused");
            rig.engine.play();
            assert!(played(10 * 44_100));
            0
        }
        Before::Skipped => {
            assert!(played(5 * 44_100));
            rig.engine.go_to(1, 0);
            // Past the first 300 ms of start glitches.
            assert!(rig.time.until(Duration::from_secs(10), || fake.0.lock().flushes == 1 && head() >= 44_100 / 2), "b on the chip");
            1
        }
    };
    let s = rig.engine.status();
    assert!(s.offloaded && s.index == Some(song), "{s:?} {:?}", fake.notes());
    let events_before = rig.events.lock().len();
    // The track moves with the clock: here is the position when the settings arrive.
    let chip_ms = (head() * 1000 / 44_100) as i64;
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let asked_at = rig.now_ms();
    rig.engine.set_settings(Settings { sound: eq, ..offload() });
    // The CPU plays at once, not at the next top-up.
    assert!(rig.time.until(Duration::from_secs(10), || !rig.engine.status().offloaded && !rig.card.heard.lock().is_empty()));
    assert!(rig.now_ms() - asked_at <= 300, "the CPU played {} ms after the equalizer went on: {:?}", rig.now_ms() - asked_at, fake.notes());
    let took = rig.time.until(Duration::from_secs(10), || !rig.engine.status().offloaded && rig.card.heard.lock().len() >= 2 * 44_100 / 2);
    assert!(took, "the CPU took over: {:?} {:?}", rig.engine.status(), fake.notes());
    assert!(fake.calls().contains(&Call::Close) && !fake.0.lock().playing);
    let s = rig.engine.status();
    let cpu_ms = (rig.card.heard.lock().len() / 2 * 1000 / 44_100) as i64;
    let notes = fake.notes();
    assert!(s.index == Some(song) && s.state == State::Playing, "{s:?}");
    // The takeover point in the perf note is where the track was (not the last read, nor the end of
    // what was written).
    let left = notes.iter().find(|n| n.starts_with("offload: left at ")).cloned().unwrap_or_default();
    assert!(left.contains("chip said") && left.contains("written"), "the handoff noted: {notes:?}");
    let left_ms: i64 = left["offload: left at ".len()..].split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(-1);
    let lead = nori_engine::REMAKE_LEAD_MS;
    assert!((left_ms - chip_ms - lead).abs() <= 50, "the CPU took {song} over from {left_ms} ms, the chip was at {chip_ms} ms {lead} ms before: {notes:?}");
    // The status runs on from there.
    assert!(s.position_ms <= left_ms + cpu_ms + 50 && s.position_ms >= left_ms + cpu_ms - 300, "from {left_ms} ms, {cpu_ms} ms played: {s:?}");
    rig.run(3_000);
    let later = rig.engine.status();
    assert!(later.index == Some(song) && later.state == State::Playing && !later.offloaded, "{later:?}");
    assert!((later.position_ms - s.position_ms - 3_000).abs() <= 100, "three seconds on: {s:?} {later:?}");
    // No song change nor end; positions continue from the track's.
    let events: Vec<Event> = rig.events.lock()[events_before..].to_vec();
    assert!(!events.iter().any(|e| matches!(e, Event::Song { index, .. } if *index != song) || *e == Event::State(State::Ended)), "{events:?}");
    // The takeover position is said once, so clients running their own clock re-anchor.
    let placed: Vec<(usize, i64)> = events.iter().filter_map(|e| if let Event::Placed { index, ms } = e { Some((*index, *ms)) } else { None }).collect();
    assert!(placed.len() == 1 && placed[0].0 == song && (placed[0].1 - left_ms).abs() <= 50, "placed at {left_ms} ms: {placed:?}");
    let positions: Vec<i64> = events.iter().filter_map(|e| if let Event::Position { index, ms } = e { assert_eq!(*index, song); Some(*ms) } else { None }).collect();
    assert!(positions.len() >= 8, "{events:?}");
    assert!(positions.windows(2).all(|w| w[1] >= w[0] - 20), "onwards: {positions:?}");
    assert!(positions.iter().all(|&ms| ms >= chip_ms - 300 && ms <= later.position_ms + 50), "from where the chip was ({chip_ms} ms): {positions:?}");
    rig.engine.stop();
}

#[test]
fn equalizer_on_hands_offload_to_cpu() {
    equalizer_on_a_phone(false, Before::Played);
}

#[test]
fn equalizer_on_hands_jittery_offload_to_cpu() {
    equalizer_on_a_phone(true, Before::Played);
}

#[test]
fn equalizer_on_after_pause_hands_to_cpu() {
    equalizer_on_a_phone(true, Before::PausedAndResumed);
    equalizer_on_a_phone(false, Before::PausedAndResumed);
}

#[test]
fn equalizer_on_after_skip_hands_to_cpu() {
    equalizer_on_a_phone(true, Before::Skipped);
    equalizer_on_a_phone(false, Before::Skipped);
}

/// Repeated offload handovers (equalizer on and off): each fades out on one side and in on the other,
/// no gap longer than a track start, position continuous.
#[test]
fn leaving_offload_has_no_gap() {
    let d = dir();
    let Some((rig, fake)) = two_on_a_phone(&d, 30, true, true) else { return };
    rig.engine.position_updates(Some(Duration::from_millis(100)));
    let head = || fake.0.lock().played;
    assert!(rig.time.until(Duration::from_secs(40), || head() >= 3 * 44_100), "on the chip");
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    for round in 0..3 {
        let (t0, chip0, cpu0, calls0, seen) = (rig.time.clock.now_ns(), head(), rig.card.heard.lock().len(), fake.0.lock().calls.len(), rig.events.lock().len());
        assert!(rig.engine.status().offloaded, "round {round}: on the chip");
        rig.engine.set_settings(Settings { sound: eq.clone(), ..offload() });
        rig.run(1_000);
        let s = rig.engine.status();
        assert!(!s.offloaded && s.index == Some(0), "round {round}: the CPU took over: {s:?}");
        // Every frame of the time was played, offloaded then by the CPU.
        let (chip, cpu) = (head() - chip0, (rig.card.heard.lock().len() - cpu0) as u64 / 2);
        let due = ((rig.time.clock.now_ns() - t0) as u128 * 44_100 / 1_000_000_000) as u64;
        let gap_ms = due.saturating_sub(chip + cpu) * 1000 / 44_100;
        assert!(gap_ms <= 5, "round {round}: {gap_ms} ms of silence between the chip ({chip} frames) and the CPU ({cpu})");
        // The offload track faded out before release.
        let calls = fake.0.lock().calls[calls0..].to_vec();
        let close = calls.iter().position(|c| *c == Call::Close).expect("the chip's track let go");
        let fade: Vec<f32> = calls[..close].iter().filter_map(|c| if let Call::Volume(v) = c { Some(*v) } else { None }).collect();
        assert!(fade.len() >= 2 && fade.windows(2).all(|w| w[1] <= w[0]) && fade.last() == Some(&0.0), "round {round}: faded out: {fade:?}");
        // The CPU faded in from silence.
        let heard = rig.card.heard.lock()[cpu0..].to_vec();
        let loud = |s: &[f32]| s.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!(loud(&heard[..88]) * 4.0 < loud(&heard[4_410..4_498]), "round {round}: faded in");
        let places: Vec<i64> = rig.events.lock()[seen..].iter().filter_map(|e| if let Event::Position { ms, .. } = e { Some(*ms) } else { None }).collect();
        assert!(places.windows(2).all(|w| w[1] >= w[0] - 20 && w[1] - w[0] <= 250), "round {round}: the place runs on: {places:?}");
        rig.engine.set_settings(offload());
        assert!(rig.time.until(Duration::from_secs(10), || rig.engine.status().offloaded), "round {round}: back on the chip");
        rig.run(1_000);
    }
    rig.engine.stop();
}

/// A server sending no length (chunked transcode): the first `hold` bytes at once, the rest once `gate`
/// opens.
struct Unsized {
    files: Vec<(String, Arc<Vec<u8>>)>,
    hold: usize,
    gate: Arc<common::Gate>,
}

struct Trickle {
    file: Arc<Vec<u8>>,
    at: usize,
    hold: usize,
    gate: Arc<common::Gate>,
}

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at >= self.hold {
            self.gate.wait();
        }
        let n = buf.len().min(self.file.len() - self.at);
        buf[..n].copy_from_slice(&self.file[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl ByteSource for Unsized {
    fn open(&self, url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        let f = self.files.iter().find(|(u, _)| u == url).map(|(_, f)| f.clone()).ok_or("404")?;
        Ok(Body { start: from, len: None, reader: Box::new(Trickle { file: f, at: from as usize, hold: self.hold, gate: self.gate.clone() }) })
    }
}

struct UnsizedSongs(Arc<Unsized>, Vec<(String, String, i64)>);

impl Library for UnsizedSongs {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let (_, hint, ms) = self.1.iter().find(|(i, _, _)| i == id).cloned().ok_or("no such song")?;
        Ok(Located { source: Source::Url { url: id.to_string(), bytes: self.0.clone() }, hint: Some(hint), duration_ms: Some(ms), estimated: false })
    }

    fn about(&self, id: &str) -> WindowSong {
        let ms = self.1.iter().find(|(i, _, _)| i == id).map_or(0, |s| s.2);
        WindowSong { id: id.into(), title: id.into(), duration_ms: ms, ..Default::default() }
    }
}

/// Two songs of `secs` from [`Unsized`], nine tenths sent before the gate. None without ffmpeg.
fn unsized_rig(d: &Path, secs: u32, fake: Option<Fake>, settings: Settings) -> Option<(Rig, Arc<common::Gate>)> {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed");
        return None;
    }
    let (a, b) = (mp3(d, "a", secs, 440), mp3(d, "b", secs, 660));
    let gate = Arc::new(common::Gate::default());
    let hold = a.len() * 9 / 10;
    let server = Arc::new(Unsized { files: vec![("a".into(), Arc::new(a)), ("b".into(), Arc::new(b))], hold, gate: gate.clone() });
    let songs = vec![("a".to_string(), "mp3".to_string(), secs as i64 * 1000), ("b".to_string(), "mp3".to_string(), secs as i64 * 1000)];
    let queue = SharedQueue::default();
    queue.0.lock().set(songs.iter().map(|s| s.0.clone()).collect(), Some(0), false, 0);
    let card = Card::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let clock = Virtual::default();
    if let Some(f) = &fake {
        f.0.lock().clock = Some(clock.clone());
        card.pull.lock().chip = Some(f.clone());
    }
    let config = Config { memory_mb: 256, settings, ..Config::default() };
    let offload = fake.map(|f| Box::new(f) as Box<dyn OffloadOutput>);
    let engine = Engine::start_on(UnsizedSongs(server, songs), app(), queue.clone(), Box::new(card.clone()), offload, config, clock.clone(), move |e| seen.lock().push(e));
    engine.queue_changed();
    Some((Rig { engine, time: Stepper::new(clock, card.pull.clone()), card, queue, events }, gate))
}

/// An unsized, still-arriving song offloaded; the equalizer goes on: the CPU takes over at the track's
/// position (seeked, not treated as an unseekable station).
#[test]
fn equalizer_on_over_unsized_offload_hands_to_cpu() {
    let d = dir();
    let secs = 60;
    let fake = Fake::new(MP3_ONLY);
    fake.phone(true, true);
    let Some((rig, gate)) = unsized_rig(&d, secs, Some(fake.clone()), offload()) else { return };
    // Status is at most 100 ms old.
    rig.engine.position_updates(Some(Duration::from_millis(100)));
    rig.engine.play_at(0, 0);
    assert!(rig.time.until(Duration::from_secs(40), || fake.0.lock().head >= 8 * 44_100), "a on the chip: {:?} {:?}", rig.engine.status(), fake.notes());
    let chip_ms = (fake.0.lock().head * 1000 / 44_100) as i64;
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    rig.engine.set_settings(Settings { sound: eq, ..offload() });
    let took = rig.time.until(Duration::from_secs(10), || !rig.engine.status().offloaded && rig.card.heard.lock().len() >= 44_100);
    let events = rig.events.lock().clone();
    assert!(took, "the CPU took over: {:?} {:?} {events:?}", rig.engine.status(), fake.notes());
    assert!(!events.iter().any(|e| matches!(e, Event::Error { .. })), "{events:?}");
    rig.run(2_000);
    let s = rig.engine.status();
    let cpu_ms = (rig.card.heard.lock().len() / 2 * 1000 / 44_100) as i64;
    assert!(s.index == Some(0) && s.state == State::Playing, "a plays on: {s:?} {:?}", rig.events.lock());
    let lead = { let h = rig.card.heard.lock(); h.iter().position(|x| x.abs() > 1e-3).unwrap_or(h.len()) / 2 };
    assert!((s.position_ms - chip_ms - cpu_ms).abs() <= 150, "from where the chip was ({chip_ms} ms), {cpu_ms} ms played, {lead} silent: {s:?} {:?}", fake.notes());
    gate.open();
    rig.engine.stop();
}

/// The equalizer toggled rapidly while offloaded: no error, silence or position jump, ending offloaded.
#[test]
fn equalizer_toggle_on_offload_without_glitch() {
    let d = dir();
    let secs = 30;
    let Some((rig, fake)) = two_on_a_phone_with(&d, secs, true, true, true) else { return };
    rig.engine.position_updates(Some(Duration::from_millis(100)));
    assert!(rig.time.until(Duration::from_secs(40), || fake.0.lock().head >= 5 * 44_100));
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let events_before = rig.events.lock().len();
    let started = rig.engine.status().position_ms;
    let t0 = rig.now_ms();
    for k in 0..8 {
        let on = k % 2 == 0;
        rig.engine.set_settings(if on { Settings { sound: eq.clone(), ..offload() } } else { offload() });
        rig.run(if k == 3 { 1_000 } else { 150 });
    }
    // Last switched off: offloaded and playing.
    assert!(rig.time.until(Duration::from_secs(10), || rig.engine.status().offloaded && fake.0.lock().playing), "{:?} {:?}", rig.engine.status(), fake.notes());
    rig.run(2_000);
    let s = rig.engine.status();
    let elapsed = rig.now_ms() - t0;
    let events: Vec<Event> = rig.events.lock()[events_before..].to_vec();
    let notes = fake.notes();
    // No loop reported.
    assert!(!events.iter().any(|e| matches!(e, Event::Error { .. } | Event::Stopped { .. } | Event::Buffering(true) | Event::Looped { .. }) || matches!(e, Event::Song { index, .. } if *index != 0)), "{events:?}");
    assert!(s.index == Some(0) && s.state == State::Playing, "{s:?}");
    assert_eq!(s.underruns, 0, "the CPU's output never ran dry: {s:?}");
    assert_eq!(fake.starved_ms(), 0, "the chip never ran dry: {notes:?}");
    // Dips cost a little each, never a jump.
    let moved = s.position_ms - started;
    assert!(moved <= elapsed + 150 && moved >= elapsed - 8 * 150, "{moved} ms on in {elapsed} ms: {s:?} {notes:?}");
    let positions: Vec<i64> = events.iter().filter_map(|e| if let Event::Position { ms, .. } = e { Some(*ms) } else { None }).collect();
    assert!(positions.windows(2).all(|w| w[1] >= w[0] - 150 && w[1] <= w[0] + 400), "no jump: {positions:?}");
    // Each move is said with its position.
    let placed = events.iter().filter(|e| matches!(e, Event::Placed { index: 0, .. })).count();
    assert!((2..=8).contains(&placed), "{events:?}");
    rig.engine.stop();
}

/// On the CPU with AutoMix: the equalizer takes effect at once, and rapid toggling never runs dry.
#[test]
fn equalizer_toggle_on_cpu_without_underrun() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 60, 440);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 60_000)], app(), None, Settings::default());
    rig.engine.position_updates(Some(Duration::from_millis(100)));
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.card.heard.lock().len() > 2 * 5 * 44_100), "the CPU plays a");
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let asked_at = rig.now_ms();
    rig.engine.set_settings(Settings { sound: eq.clone(), ..Settings::default() });
    assert!(rig.time.until(Duration::from_secs(10), || rig.engine.status().chain), "{:?}", rig.engine.status());
    assert!(rig.now_ms() - asked_at <= 300, "the chain came in {} ms after the equalizer went on", rig.now_ms() - asked_at);
    let events_before = rig.events.lock().len();
    for k in 0..8 {
        rig.engine.set_settings(if k % 2 == 0 { Settings::default() } else { Settings { sound: eq.clone(), ..Settings::default() } });
        rig.run(if k == 3 { 1_000 } else { 150 });
    }
    rig.run(2_000);
    let s = rig.engine.status();
    let events: Vec<Event> = rig.events.lock()[events_before..].to_vec();
    assert!(!events.iter().any(|e| matches!(e, Event::Error { .. } | Event::Buffering(true))), "{events:?}");
    assert!(s.index == Some(0) && s.state == State::Playing && s.chain, "{s:?}");
    assert_eq!(s.underruns, 0, "the output never ran dry: {s:?}");
    rig.engine.stop();
}

/// On the CPU, an unsized arriving song: the equalizer remakes it by a seek, which plays on.
#[test]
fn equalizer_on_over_unsized_cpu_song() {
    let d = dir();
    let Some((rig, gate)) = unsized_rig(&d, 60, None, Settings::default()) else { return };
    rig.engine.position_updates(Some(Duration::from_millis(100)));
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.engine.status().position_ms >= 5_000), "{:?} {:?}", rig.engine.status(), rig.events.lock());
    let before = rig.engine.status().position_ms;
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let asked_at = rig.now_ms();
    rig.engine.set_settings(Settings { sound: eq, ..Settings::default() });
    assert!(rig.time.until(Duration::from_secs(10), || rig.engine.status().chain), "{:?}", rig.engine.status());
    assert!(rig.now_ms() - asked_at <= 300, "the chain came in {} ms after", rig.now_ms() - asked_at);
    rig.run(2_000);
    let s = rig.engine.status();
    let events = rig.events.lock().clone();
    assert!(!events.iter().any(|e| matches!(e, Event::Error { .. }) || matches!(e, Event::Song { index, .. } if *index != 0)), "{events:?}");
    assert!(s.index == Some(0) && s.state == State::Playing && (s.position_ms - before - 2_000).abs() <= 400, "a plays on from {before} ms: {s:?}");
    gate.open();
    rig.engine.stop();
}

/// A seek while offloaded is not reported as a repeat-one loop (clients count loops as plays).
#[test]
fn offload_seek_is_not_a_loop() {
    let d = dir();
    let Some((rig, fake)) = two_on_a_phone(&d, 30, true, true) else { return };
    rig.engine.position_updates(Some(Duration::from_millis(100)));
    assert!(rig.time.until(Duration::from_secs(40), || fake.0.lock().head >= 3 * 44_100));
    rig.engine.seek(12_000);
    assert!(rig.time.until(Duration::from_secs(10), || fake.0.lock().flushes >= 1 && fake.0.lock().head >= 44_100));
    rig.run(500);
    let s = rig.engine.status();
    assert!(s.offloaded && !s.on_cpu && s.index == Some(0) && (13_000..14_000).contains(&s.position_ms), "{s:?}");
    let events = rig.events.lock().clone();
    assert!(!events.iter().any(|e| matches!(e, Event::Looped { .. })), "{events:?}");
    rig.engine.stop();
}

/// Through a mix, every read gives the position of the song reported, never the other song's.
#[test]
fn place_matches_song_through_mix() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed");
        return;
    }
    let d = dir();
    let (a, b) = (mp3(&d, "a", 40, 440), mp3(&d, "b", 30, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let app = Watched::automix();
    let songs = vec![("a".into(), "mp3".into(), 40_000), ("b".into(), "mp3".into(), 30_000)];
    let rig = Rig::new(server, songs, app.clone(), None, automix());
    rig.engine.position_updates(Some(Duration::from_millis(100)));
    rig.engine.play_at(0, 0);
    rig.engine.replan();
    let mut last: Option<(Option<usize>, i64)> = None;
    let mut bad = Vec::new();
    let mut mixed = false;
    let ended = rig.time.until(Duration::from_secs(90), || {
        let s = rig.engine.status();
        mixed |= s.mixing;
        if s.state == State::Playing {
            // a's position within a; b's no further than the mix.
            match s.index {
                Some(0) if s.position_ms > 40_100 => bad.push((s.index, s.position_ms)),
                Some(1) if last.is_some_and(|l| l.0 == Some(0)) && s.position_ms > 8_000 => bad.push((s.index, s.position_ms)),
                _ => {}
            }
            if let Some((i, ms)) = last {
                // Within a song the position only advances, by at most a step.
                if i == s.index && (s.position_ms < ms - 50 || s.position_ms > ms + 2_000) {
                    bad.push((s.index, s.position_ms));
                }
            }
            last = Some((s.index, s.position_ms));
        }
        rig.events.lock().contains(&Event::State(State::Ended))
    });
    assert!(ended, "{:?}", app.log());
    assert!(mixed, "a mix was heard: {:?}", app.log());
    assert!(bad.is_empty(), "places read with the wrong song: {bad:?}");
    // Position events too.
    let events = rig.events.lock().clone();
    let mut song = None;
    for e in &events {
        match e {
            Event::Song { index, .. } => song = Some(*index),
            Event::Position { index, ms } => {
                assert_eq!(Some(*index), song, "a position said for the song said heard: {events:?}");
                assert!(*index != 0 || *ms <= 40_100, "{events:?}");
            }
            _ => {}
        }
    }
    rig.engine.stop();
}

/// After a mix the incoming song's position is what was really played of it: a pause agrees, and the
/// remaining time equals what the card then plays.
#[test]
fn place_after_mix() {
    mixed_into_b_its_place_is_what_was_played(0);
}

/// [`place_after_mix`], b opening on 5 s of silence the mix may skip.
#[test]
fn place_after_mix_entered_late() {
    mixed_into_b_its_place_is_what_was_played(5);
}

fn mixed_into_b_its_place_is_what_was_played(silent_s: u32) {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 40, 440);
    let b = if silent_s == 0 { mp3(&d, "b", 30, 660) } else { quiet_start(&d, "b", silent_s, 30, 660) };
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let app = Watched::automix();
    let songs = vec![("a".into(), "mp3".into(), 40_000), ("b".into(), "mp3".into(), 30_000)];
    *app.1.lock() = silent_s as i64 * 1_000_000;
    let rig = Rig::new(server, songs, app.clone(), None, automix());
    rig.engine.position_updates(Some(Duration::from_millis(100)));
    rig.engine.play_at(0, 25_000);
    rig.engine.replan();
    assert!(rig.wait(40, |r| r.heard_song("b")), "{:?}", app.log());
    // Along b the position follows the clock.
    let (t0, p0) = (rig.now_ms(), rig.engine.status().position_ms);
    rig.run(4_000);
    let said = |r: &Rig| r.events.lock().iter().rev().find_map(|e| match e { Event::Position { index: 1, ms } => Some(*ms), _ => None });
    let before = said(&rig).expect("b's place said");
    assert!((before - p0 - (rig.now_ms() - t0)).abs() <= 300, "b ran on from {p0} to {before} in {} ms", rig.now_ms() - t0);
    rig.engine.pause();
    assert!(rig.wait(5, |r| r.engine.status().state == State::Paused), "{:?}", rig.engine.status());
    let paused = rig.engine.status();
    assert_eq!(paused.index, Some(1));
    assert!((paused.position_ms - before).abs() <= 300, "a pause put b's place back or on: {before} ms before it, {} ms paused", paused.position_ms);
    // The rest of b matches the remaining time.
    let rate = rig.card.opened.lock().last().map_or(44_100, |f| f.rate as i64);
    let channels = rig.card.opened.lock().last().map_or(2, |f| f.channels as i64);
    let from = rig.card.heard.lock().len() as i64;
    rig.engine.play();
    assert!(rig.wait(40, |r| r.events.lock().contains(&Event::State(State::Ended))), "{:?}", app.log());
    let rest_ms = (rig.card.heard.lock().len() as i64 - from) / channels * 1000 / rate;
    assert!((paused.position_ms + rest_ms - 30_000).abs() <= 400, "b paused at {} ms, then {rest_ms} ms of it played to its end", paused.position_ms);
    rig.engine.stop();
}

/// MP3: `silent` s of silence, then a tone of `hz`, `secs` in all.
fn quiet_start(dir: &Path, name: &str, silent: u32, secs: u32, hz: u32) -> Vec<u8> {
    let out = dir.join(format!("{name}.mp3"));
    let filter = format!("sine=frequency={hz}:sample_rate=44100:duration={},adelay={}:all=1", secs - silent, silent * 1000);
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &filter, "-ac", "2", "-t", &secs.to_string()])
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg made {name}");
    std::fs::read(out).unwrap()
}

// ---- small track grants ----

/// 32 KB (Galaxy S21 FE) and 64 KB (Galaxy S22) tracks.
const KB32: usize = 32 * 1024;
const KB64: usize = 64 * 1024;
/// A DSP buffering about 6 s of 320 kbps beyond the track.
const DSP: usize = 256 * 1024;

/// Two 320 kbps songs of `secs` on a [`Fake::phone`] granting `grant` bytes plus `dsp`, with working
/// timestamps and head.
fn two_on_a_small_grant(d: &Path, secs: u32, grant: usize, dsp: usize) -> Option<(Rig, Fake)> {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return None;
    }
    let (a, b) = (mp3_320(d, "a", secs, 440), mp3_320(d, "b", secs, 660));
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a), ("b", &b)]);
    let fake = Fake::new(MP3_ONLY);
    fake.phone(true, false);
    {
        let mut c = fake.0.lock();
        c.grant = Some(grant);
        c.dsp = dsp;
    }
    let ms = secs as i64 * 1000;
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), ms), ("b".into(), "mp3".into(), ms)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    Some((rig, fake))
}

/// Engine wakes/s and platform requests/s over 40 s of steady playing.
fn wakes_on_a_small_grant(grant: usize, dsp: usize) -> Option<(f64, f64)> {
    let d = dir();
    let (rig, fake) = two_on_a_small_grant(&d, 60, grant, dsp)?;
    assert!(rig.time.until(Duration::from_secs(20), || fake.0.lock().head >= 5 * 44_100), "on the chip: {:?}", fake.notes());
    let (s0, r0, t0) = (rig.time.clock.sleeps(), fake.0.lock().requests, rig.now_ms());
    rig.run(40_000);
    let (s1, r1, t1) = (rig.time.clock.sleeps(), fake.0.lock().requests, rig.now_ms());
    let secs = (t1 - t0) as f64 / 1000.0;
    let (wakes, requests) = ((s1 - s0) as f64 / secs, (r1 - r0) as f64 / secs);
    let notes = fake.notes();
    eprintln!("grant {} KB, dsp {} KB: the engine woke {wakes:.2}/s, the platform asked {requests:.2}/s", grant / 1024, dsp / 1024);
    assert_eq!(fake.starved_ms(), 0, "never out of music: {notes:?}");
    assert!(!notes.iter().any(|n| n.contains("given up")), "{notes:?}");
    assert!(rig.engine.status().offloaded, "{:?}", rig.engine.status());
    rig.engine.stop();
    Some((wakes, requests))
}

/// Offloaded, the engine wakes on the platform's requests, not a timer of its own.
#[test]
fn small_track_wakes_only_on_asks() {
    let mut measured = Vec::new();
    for (grant, dsp) in [(KB32, 0), (KB32, DSP), (KB64, 0), (KB64, DSP)] {
        let Some((wakes, requests)) = wakes_on_a_small_grant(grant, dsp) else { return };
        measured.push((grant, dsp, wakes, requests));
    }
    for (grant, dsp, wakes, requests) in measured {
        assert!(wakes <= requests * 1.1 + 0.05, "grant {grant}, dsp {dsp}: {wakes:.2} wakes/s for {requests:.2} requests/s");
        if dsp > 0 {
            assert!(wakes < 0.5, "grant {grant} with a DSP: {wakes:.2} wakes/s");
        }
    }
}

/// A count standing still for 5 s without requests (a Galaxy S21 FE, screen off, stood 2.8 s) is not
/// a stall. Regression: the engine gave up offload.
fn screen_off_on_a_32_kb_track(head_too: bool) {
    let d = dir();
    let secs = 40;
    let Some((rig, fake)) = two_on_a_small_grant(&d, secs, KB32, DSP) else { return };
    assert!(rig.time.until(Duration::from_secs(40), || fake.0.lock().head >= 20 * 44_100), "on the chip: {:?}", fake.notes());
    {
        let mut c = fake.0.lock();
        let at = c.head;
        c.frozen_stamp = Some(at);
        if head_too {
            c.frozen_head = Some(at);
        }
    }
    let requests = fake.0.lock().requests;
    rig.run(5_000);
    {
        let mut c = fake.0.lock();
        c.frozen_stamp = None;
        c.frozen_head = None;
    }
    let asked = fake.0.lock().requests - requests;
    let s = rig.engine.status();
    assert!(s.offloaded, "still on the chip after the count stood 5 s ({asked} requests meanwhile): {s:?} {:?}", fake.notes());
    plays_through_on_a_small_grant(&rig, &fake, secs);
    rig.engine.stop();
}

#[test]
fn standing_timestamp_is_no_stall() {
    screen_off_on_a_32_kb_track(false);
}

#[test]
fn standing_counts_are_no_stall() {
    screen_off_on_a_32_kb_track(true);
}

/// Both songs play through a small track, offloaded all along.
fn plays_through_on_a_small_grant(rig: &Rig, fake: &Fake, secs: u32) {
    let ended = rig.time.until(Duration::from_secs(2 * secs as u64 + 20), || rig.events.lock().contains(&Event::State(State::Ended)));
    let notes = fake.notes();
    assert!(ended, "{:?} {notes:?}", rig.engine.status());
    assert!(rig.heard_song("b"), "{:?}", rig.events.lock());
    assert_eq!(fake.0.lock().head, 2 * secs as u64 * 44_100, "every frame of both songs played: {notes:?}");
    assert_eq!(fake.starved_ms(), 0, "never out of music: {notes:?}");
    assert_eq!(opens(fake), 1, "one track for both");
    assert!(rig.card.opened.lock().is_empty(), "the CPU's output was never opened");
    assert!(!notes.iter().any(|n| n.contains("given up")), "{notes:?}");
}

/// A real stall is given up; the CPU takes over at the last count, never ahead of it.
#[test]
fn real_stall_hands_to_cpu() {
    let d = dir();
    let Some((rig, fake)) = two_on_a_small_grant(&d, 60, KB32, DSP) else { return };
    assert!(rig.time.until(Duration::from_secs(40), || fake.0.lock().head >= 20 * 44_100), "on the chip: {:?}", fake.notes());
    rig.run(300);
    let stopped_ms = {
        let mut c = fake.0.lock();
        c.stalled = true;
        (c.head * 1000 / 44_100) as i64
    };
    let took = rig.time.until(Duration::from_secs(60), || !rig.engine.status().offloaded);
    let notes = fake.notes();
    assert!(took, "the CPU took over: {:?} {notes:?}", rig.engine.status());
    let s = rig.engine.status();
    assert_eq!(s.index, Some(0), "{s:?}");
    // At the track's position or just before, not the clock's.
    assert!(s.position_ms <= stopped_ms + 50 && s.position_ms >= stopped_ms - 1_000, "the CPU at {} ms, the chip stopped at {stopped_ms} ms: {notes:?}", s.position_ms);
    let given_up = notes.iter().find(|n| n.starts_with("offload given up")).cloned().unwrap_or_default();
    assert!(given_up.contains("asked for nothing") && given_up.contains("where the chip"), "{notes:?}");
    rig.engine.stop();
}

/// A track holding back its last frames until more comes (as an S21 FE seemed to): the next song is
/// written long before the end.
#[test]
fn next_song_written_before_needed() {
    let d = dir();
    let secs = 30;
    let Some((rig, fake)) = two_on_a_small_grant(&d, secs, KB32, DSP) else { return };
    fake.0.lock().holds_back = 38_235;
    plays_through_on_a_small_grant(&rig, &fake, secs);
    rig.engine.stop();
}

/// Offloaded, the CPU may sleep except at the start, a few seconds before the next song (whose event
/// comes on time), and the end.
#[test]
fn cpu_sleeps_while_offloaded() {
    let d = dir();
    let secs = 60;
    let Some((rig, fake)) = two_on_a_small_grant(&d, secs, KB32, DSP) else { return };
    // Awake spans, ms, and b's event time.
    let (mut awake_ms, mut last) = (0i64, (rig.now_ms(), rig.engine.status().awake));
    let mut b_said = None;
    let mut awake_at = Vec::new();
    let ended = rig.time.until(Duration::from_secs(2 * secs as u64 + 20), || {
        let now = rig.now_ms();
        let awake = rig.engine.status().awake;
        if last.1 {
            awake_ms += now - last.0;
        }
        if awake && !last.1 {
            awake_at.push(fake.0.lock().head * 1000 / 44_100);
        }
        last = (now, awake);
        if b_said.is_none() && rig.heard_song("b") {
            b_said = Some(fake.0.lock().head * 1000 / 44_100);
        }
        rig.events.lock().contains(&Event::State(State::Ended))
    });
    let notes = fake.notes();
    assert!(ended, "{:?} {notes:?}", rig.engine.status());
    assert_eq!(fake.starved_ms(), 0, "{notes:?}");
    assert!(!notes.iter().any(|n| n.contains("given up")), "{notes:?}");
    let total = rig.now_ms();
    eprintln!("the CPU kept awake {awake_ms} ms of {total} ms, from {awake_at:?} ms of the chip's music");
    // Awake only at the start, before b, and before the end.
    assert!(awake_ms * 100 / total <= 15, "awake {awake_ms} ms of {total} ms, from {awake_at:?} (ms of the chip's music)");
    assert!(awake_at.iter().any(|&ms| (secs as u64 * 1000 - 8_000..secs as u64 * 1000).contains(&ms)), "awake before b: {awake_at:?}");
    // b's event came on time.
    let b = b_said.expect("b said");
    assert!((secs as u64 * 1000..=secs as u64 * 1000 + 50).contains(&b), "b said at {b} ms of the chip's music");
    // Also as events, for platform wake locks.
    let said: Vec<bool> = rig.events.lock().iter().filter_map(|e| if let Event::Awake(a) = e { Some(*a) } else { None }).collect();
    assert!(said.len() >= 4 && said.windows(2).all(|w| w[0] != w[1]) && !said[0], "{said:?}");
    rig.engine.stop();
}

/// On the CPU the engine never allows sleep.
#[test]
fn cpu_awake_while_on_cpu() {
    let d = dir();
    let Some((rig, _fake)) = two_on_a_small_grant(&d, 20, KB32, DSP) else { return };
    rig.engine.set_settings(Settings::default());
    assert!(rig.wait(10, |r| !r.engine.status().offloaded && r.card.heard.lock().len() > 44_100 * 2), "{:?}", rig.engine.status());
    rig.events.lock().clear();
    rig.run(5_000);
    assert!(rig.engine.status().awake);
    assert!(!rig.events.lock().iter().any(|e| matches!(e, Event::Awake(false))), "{:?}", rig.events.lock());
    rig.engine.stop();
}

// ---- a new queue around the current song ----

/// A new queue around the playing song (Android's `keepPlaying`): it plays on without stop or end
/// events, though the read-ahead next song is gone.
#[test]
fn new_queue_around_current_song_plays_on() {
    let tone = ramp(3 * 44_100, 16, 5);
    let server = Arc::new(Server::default());
    let file = wav(44_100, 16, &tone);
    serve(&server, &[("p1", &file), ("a", &file), ("p2", &file), ("c", &file), ("d", &file)]);
    let songs = ["p1", "a", "p2", "c", "d"].iter().map(|id| (id.to_string(), "wav".to_string(), 3_000)).collect();
    // The equalizer on, as where it was seen.
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let rig = Rig::new(server, songs, app(), None, Settings { sound: eq, ..Settings::default() });
    rig.queue.0.lock().set(vec!["p1".into(), "a".into(), "p2".into()], Some(1), false, 0);
    rig.engine.queue_changed();
    rig.engine.play_at(1, 0);
    assert!(rig.wait(10, |r| r.heard_song("a")), "{:?}", rig.events.lock());
    // 1 s in: a's rest and p2's start are buffered.
    rig.run(1_000);
    let before = rig.events.lock().len();
    rig.queue.0.lock().set(vec!["c".into(), "a".into(), "d".into()], Some(1), false, 0);
    rig.engine.queue_changed();
    assert!(rig.wait(10, |r| r.heard_song("d")), "d follows a: {:?}", rig.events.lock());
    let after: Vec<Event> = rig.events.lock()[before..].to_vec();
    assert!(!after.iter().any(|e| matches!(e, Event::Stopped { .. } | Event::State(State::Paused | State::Ended | State::Idle))), "{after:?}");
    assert!(!after.iter().any(|e| matches!(e, Event::Song { id, .. } if id == "p2")), "the old queue's next song is not heard: {after:?}");
    rig.engine.stop();
}

/// [`new_queue_around_current_song_plays_on`], the old next song failing in read-ahead: stops nothing.
#[test]
fn new_queue_ignores_old_next_failing() {
    let tone = ramp(3 * 44_100, 16, 5);
    let server = Arc::new(Server::default());
    let file = wav(44_100, 16, &tone);
    serve(&server, &[("p1", &file), ("a", &file), ("c", &file), ("d", &file)]);
    server.down.lock().push("p2".into());
    let songs = ["p1", "a", "p2", "c", "d"].iter().map(|id| (id.to_string(), "wav".to_string(), 3_000)).collect();
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let rig = Rig::new(server, songs, app(), None, Settings { sound: eq, ..Settings::default() });
    rig.queue.0.lock().set(vec!["p1".into(), "a".into(), "p2".into()], Some(1), false, 0);
    rig.engine.queue_changed();
    rig.engine.play_at(1, 0);
    assert!(rig.wait(10, |r| r.heard_song("a")), "{:?}", rig.events.lock());
    rig.run(1_000);
    let before = rig.events.lock().len();
    rig.queue.0.lock().set(vec!["c".into(), "a".into(), "d".into()], Some(1), false, 0);
    rig.engine.queue_changed();
    assert!(rig.wait(10, |r| r.heard_song("d")), "d follows a: {:?}", rig.events.lock());
    let after: Vec<Event> = rig.events.lock()[before..].to_vec();
    assert!(!after.iter().any(|e| matches!(e, Event::Stopped { .. } | Event::State(State::Paused | State::Ended | State::Idle))), "{after:?}");
    rig.engine.stop();
}
