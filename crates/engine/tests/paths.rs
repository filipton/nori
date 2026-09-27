//! The engine's other paths end to end on real threads: songs handed as packets to an output that
//! decodes them itself (a simulated offloaded AudioTrack whose play head the test moves), bit-perfect
//! output (every sample reaching the device as the file stores it, at its own rate and depth), a live
//! stream with the station's announcements in it, the offline bridge taking a song the network would not
//! bring, and repeat one reported as loops.
//!
//! The compressed songs are made by ffmpeg on the machine running the tests; without it the tests that
//! need them say so and pass.
//!
//! The engine runs on a clock the test moves (`common::Virtual`), as in tests/engine.rs: the CPU's card
//! pulls on it, and the chip's play head wakes the engine through it.

mod common;

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

fn ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A tone of `secs` encoded by ffmpeg with `codec` into a file of extension `ext`, its bytes. Encoded
/// once per tone and codec for the whole binary: the same few tones are asked for again and again.
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

/// A directory of the test's own, gone when the test is.
fn dir() -> nori_testdir::TempDir {
    nori_testdir::TempDir::new("paths")
}

fn mp3(dir: &Path, name: &str, secs: u32, hz: u32) -> Vec<u8> {
    made(dir, name, secs, hz, &["-c:a", "libmp3lame", "-b:a", "128k"], "mp3")
}

/// [`mp3`] at 320 kbps, as the tester's songs were: 32 KB of it is 819 ms.
fn mp3_320(dir: &Path, name: &str, secs: u32, hz: u32) -> Vec<u8> {
    made(dir, name, secs, hz, &["-c:a", "libmp3lame", "-b:a", "320k"], "mp3")
}

fn flac(dir: &Path, name: &str, secs: u32, hz: u32) -> Vec<u8> {
    made(dir, name, secs, hz, &["-c:a", "flac"], "flac")
}

/// Samples as a WAV file stores them: `bits` of 16 or 24, interleaved stereo.
fn wav(rate: u32, bits: u16, samples: &[i32]) -> Vec<u8> {
    let width = bits as u32 / 8;
    let data = samples.len() as u32 * width;
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2 * width).to_le_bytes());
    w.extend_from_slice(&(2 * width as u16).to_le_bytes());
    w.extend_from_slice(&bits.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for v in samples {
        w.extend_from_slice(&v.to_le_bytes()[..width as usize]);
    }
    w
}

/// Every sample a different value, spread over the whole range of `bits`.
fn ramp(frames: usize, bits: u32, seed: i64) -> Vec<i32> {
    let max = (1i64 << (bits - 1)) - 1;
    (0..frames * 2).map(|i| (((i as i64 * 7919 + seed * 104_729) % (2 * max)) - max) as i32).collect()
}

// ---- where songs come from ----

#[derive(Default)]
struct Server {
    files: Mutex<Vec<(String, Arc<Vec<u8>>)>>,
    /// Songs the network will not bring.
    down: Mutex<Vec<String>>,
    /// Songs the server answers with an error status instead.
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

/// The songs (id, container, length ms), and the albums some are on (id, album, track number).
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

/// What the CPU's card pulls from, on the test's clock.
#[derive(Default)]
struct Pull {
    feed: Option<Feed>,
    playing: bool,
    due_ns: i64,
    heard: Arc<Mutex<Vec<f32>>>,
    block: Vec<f32>,
    /// The offloaded track, which plays on the same clock when it plays on its own ([`Fake::runs`]).
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
        // The chip's request for more wakes the engine as the card's pull does.
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

/// A sound card that keeps everything it plays, as float, and the formats it was opened in.
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

// ---- the output that decodes songs itself ----

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

/// An offloaded AudioTrack on a clock the test moves: it takes what fits in its buffer, plays what it
/// was given as far as the test says, and can be torn down.
#[derive(Default)]
struct Chip {
    support: Vec<(Coding, Support)>,
    calls: Vec<Call>,
    capacity: usize,
    /// What it holds and has not played: bytes and the frames they stand for.
    held: VecDeque<(usize, u64)>,
    written: u64,
    /// Every byte it took.
    bytes: Vec<u8>,
    head: u64,
    /// The frames written up to the last end of stream.
    ended_at: Option<u64>,
    torn: bool,
    open: bool,
    engine: Option<Thread>,
    /// The test's clock: the engine is woken through it, so the test knows to wait for it.
    clock: Option<Virtual>,
    /// Playing, as Android's track would say: an end of stream is taken only then.
    playing: bool,
    /// Readings the platform gives before its true count, one each time it is asked: none for a call
    /// that failed, or a number that makes no sense.
    readings: VecDeque<Option<u64>>,
    /// Where the platform's count started again from nought (a standby), in the frames it played.
    offset: u64,
    /// The platform says it presented everything, whatever it played (a late word about another song).
    stale_presented: bool,
    /// How many times faster than the music the test moves the play head (none: as fast as it likes).
    pace: Option<f64>,
    /// How many times what it was asked for it takes, as a phone that takes whole songs at once.
    takes: usize,
    /// What each note said.
    notes: Vec<String>,
    /// Ends of stream it refuses though it plays (Android's track still stopping from the one before).
    refuse_eos: u32,
    /// It plays on its own, at the music's pace on the test's clock, while it is told to play (none: the
    /// test moves its play head).
    runs: bool,
    /// When it was last moved on, ns, and the part of a frame owed since.
    synced_ns: Option<i64>,
    owed: f64,
    /// The bytes the platform grants a track, whatever it was asked for (64 KB on a Galaxy S22).
    grant: Option<usize>,
    /// Its play head reads nought for ever (`getRenderPosition` failing, as on a Galaxy S22).
    head_stuck: bool,
    /// It gives timestamps: the true count of what it presented, or this one for ever.
    stamps: bool,
    frozen_stamp: Option<u64>,
    /// It asked for more (`onDataRequest`) since the engine last looked, and whether it asks again the
    /// next time it holds under half its buffer.
    requested: bool,
    armed: bool,
    /// Frames of music it had nothing to play for while playing, before the end of what it was given.
    starved: u64,
    /// Its timestamps misbehave as a Galaxy S22's do ([`Fake::jittery`]).
    jittery: bool,
    /// What the timestamps read next while it plays, before the true count: a track's start, or the
    /// count from before a flush.
    stamp_script: VecDeque<u64>,
    /// The least the timestamp reads while its start's glitch lasts (the platform holds its own count
    /// from going back).
    stamp_floor: u64,
    /// The last timestamp given, and the random numbers its steps back come from.
    last_stamp: u64,
    seed: u64,
    /// Times the track was flushed.
    flushes: u32,
    /// Every frame it played, whichever track.
    played: u64,
    /// Bytes its own decoder buffers beyond the track the platform granted (a DSP's own buffer): a track
    /// takes this much more, and asks for more once what it holds falls under half of the whole.
    dsp: usize,
    /// Times it asked for more (`onDataRequest`).
    requests: u64,
    /// It plays nothing and asks for nothing: a stall.
    stalled: bool,
    /// Frames at the end of what it was given that it presents only once more is written or an end of
    /// stream is said after them (a decoder holding back a partial buffer).
    holds_back: u64,
    /// Its play head reads this for ever (a count the platform stopped updating).
    frozen_head: Option<u64>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Chip>>);

impl Chip {
    /// Plays `frames` more of what it holds, as far as it holds: the frames played.
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

    /// What its timestamps read as a track starts, after `stale` (the count before a flush) twice.
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

    /// The timestamp now, as a jittery phone gives it.
    fn jittery_stamp(&mut self) -> u64 {
        let truth = self.head - self.offset;
        // The start's glitches last its first 300 ms.
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
        // A step back from the reading before when the two are a moment apart; behind the true count by
        // as much otherwise.
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

    /// A chip that plays on its own is moved on to `now_ns`, counting what it had nothing to play for:
    /// true when that took it under half its buffer, where it asks for more.
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
        // Out of music before the end of stream said last: silence where there should be none.
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

    /// [`Chip::sync`] to the clock's time, from the engine's own calls: a request made then wakes it.
    fn sync_now(&mut self) {
        let Some(now) = self.clock.as_ref().map(Virtual::now_ns) else { return };
        if self.sync(now) {
            self.wake();
        }
    }

    /// The engine is told, as the platform tells it.
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

    /// The chip plays `frames` more of what it holds, and wakes the engine as the platform does.
    fn advance(&self, frames: u64) {
        self.play_on(frames, true);
    }

    /// The chip plays `frames` more without the engine hearing of it: it reads the head when it next
    /// wakes for something else.
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

    /// A Galaxy S22's offloaded track: 64 KB granted whatever was asked, playing on its own at the
    /// music's pace and asking for more under half its buffer; its play head reads nought for ever
    /// (`head_stuck`), its timestamps are true (`stamps`).
    fn phone(&self, stamps: bool, head_stuck: bool) {
        let mut c = self.0.lock();
        c.runs = true;
        c.pace = Some(1.0);
        c.grant = Some(64 * 1024);
        c.stamps = stamps;
        c.head_stuck = head_stuck;
    }

    /// Its timestamps as a Galaxy S22's are (perf10): in a track's first 300 ms they read 10, 6, 5, 4,
    /// 4, 6 frames, then 160 ms ahead of what it presented, held there by the platform until the true
    /// count passes it; as it plays, about one reading in three steps back 1 to 80 frames from the one
    /// before; and the first two after a flush are the count from before it.
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

    /// The next readings of the play head, before its true count again.
    fn read_as(&self, readings: &[Option<u64>]) {
        let mut c = self.0.lock();
        c.readings.extend(readings.iter().copied());
        c.wake();
    }

    /// The platform starts counting again from nought, where the head is.
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
        // What it said of the end of stream before is not about this one.
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

    /// [`Rig::new`], with some of the songs on albums: (id, album, track number).
    fn albums(server: Arc<Server>, songs: Vec<(String, String, i64)>, albums: Vec<(String, String, i32)>, app: impl App + Send + 'static, fake: Option<Fake>, settings: Settings) -> Rig {
        let queue = SharedQueue::default();
        queue.0.lock().set(songs.iter().map(|s| s.0.clone()).collect(), Some(0), false, 0);
        // Each album's songs next to each other were queued as the album: one album run each.
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

    /// Runs the music on until `done`, for `secs` of the old card's time at most: it played on a real
    /// clock twenty times faster than the music, and the limits the tests wait with are in its seconds.
    fn wait(&self, secs: u64, mut done: impl FnMut(&Rig) -> bool) -> bool {
        self.time.until(Duration::from_secs(secs * 20), || done(self))
    }

    /// Runs the engine's clock on by `ms`.
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

/// [`app`], shared, so a test can read the engine's log: every reason it gave for playing on the CPU is
/// there, in the order given, however quickly the next one followed.
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

/// The tracks opened for the chip.
fn opens(fake: &Fake) -> usize {
    fake.calls().iter().filter(|c| matches!(c, Call::Open(_))).count()
}

/// What was done with the track opened last.
fn after_last_open(calls: &[Call]) -> Vec<&Call> {
    let from = calls.iter().rposition(|c| matches!(c, Call::Open(_))).unwrap_or(0);
    calls[from..].iter().collect()
}

// ---- offload ----

#[test]
fn songs_of_one_format_join_on_one_offloaded_track_each_with_its_delay_and_padding() {
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
    // Both songs are short beside the track: all of them goes in at once, the end of the queue closing it.
    assert!(rig.wait(10, |_| fake.calls().iter().filter(|c| **c == Call::EndOfStream).count() == 2), "{:?}", fake.calls());
    let calls = fake.calls();
    let opens: Vec<&Call> = calls.iter().filter(|c| matches!(c, Call::Open(_))).collect();
    assert_eq!(opens.len(), 1, "one track for both: {calls:?}");
    assert!(matches!(opens[0], Call::Open(Coded { coding: Coding::Mp3, rate: 44_100, channels: 2, .. })));
    // Each song's delay and padding before its first packet, and the end of stream between them.
    let marks: Vec<&Call> = calls.iter().filter(|c| matches!(c, Call::DelayPadding(..) | Call::EndOfStream)).collect();
    assert_eq!(marks.len(), 4, "{marks:?}");
    let (Call::DelayPadding(d1, p1), Call::EndOfStream, Call::DelayPadding(d2, p2), Call::EndOfStream) = (marks[0], marks[1], marks[2], marks[3]) else { panic!("{marks:?}") };
    // LAME's numbers as media3 hands them over: the encoder's delay (576), and padding past it.
    assert_eq!((*d1, *d2), (576, 576), "the LAME tag's delay");
    assert!(*p1 > 0 && *p2 > 0);
    // Every frame of both songs, and nothing else.
    let frames = fake.written();
    assert_eq!(frames, 2 * 20 * 44_100, "the music, delay and padding cut: {frames}");
    // The ear reaches the second song: said, at its own volume.
    fake.advance(20 * 44_100 + 100);
    assert!(rig.wait(5, |r| r.heard_song("b")), "{:?}", rig.events.lock());
    assert!(rig.wait(5, |_| fake.0.lock().calls.contains(&Call::Volume(0.5))), "b plays at its ReplayGain volume");
    assert!(rig.engine.status().offloaded);
    let at = rig.engine.status().position_ms;
    assert!((0..100).contains(&at), "at the start of b: {at}");
    // Played out: the end of the queue.
    fake.advance(20 * 44_100);
    assert!(rig.wait(5, |r| r.events.lock().contains(&Event::State(State::Ended))), "{:?}", rig.events.lock());
    assert!(rig.card.opened.lock().is_empty(), "the CPU's output was never opened");
    rig.engine.stop();
}

#[test]
fn a_track_that_is_torn_down_hands_the_music_to_the_cpu_where_the_ear_is() {
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
    // The CPU plays on from five seconds in.
    assert!(rig.wait(10, |r| r.card.heard.lock().len() > 44_100), "the CPU took over");
    assert!(fake.calls().contains(&Call::Close), "the torn track was let go");
    let s = rig.engine.status();
    assert!(!s.offloaded && s.position_ms >= 5_000, "{s:?}");
    rig.engine.stop();
}

#[test]
fn offload_is_taken_up_where_the_ear_is_once_nothing_touches_the_samples_and_given_up_at_once() {
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
    // Offload on, and the equalizer with it: the CPU plays, the samples being touched.
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let rig = Rig::new(server, songs, app(), Some(fake.clone()), Settings { sound: eq.clone(), ..offload() });
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.card.heard.lock().len() > 2 * 44_100), "the CPU plays a");
    // Where the ear is: what the card played (the status has it as of the engine's last wake).
    let (before, asked) = ((rig.card.heard.lock().len() / 2) as i64 * 1000 / 44_100, rig.now_ms());
    // The equalizer off: nothing touches the samples any more.
    rig.engine.set_settings(offload());
    // Not at the next song: at once, where the ear is, behind a dip.
    assert!(rig.wait(10, |r| r.engine.status().offloaded), "{:?}", rig.engine.status().pcm_why);
    let s = rig.engine.status();
    // What the music moved on while the switch was made; and the chip starts at the packet the place
    // lands in, a few frames back for the decoder's reservoir.
    let moved = rig.now_ms() - asked + 500;
    assert!(s.index == Some(0) && s.position_ms >= before - 200 && s.position_ms <= before + moved, "a from where it was ({before} ms): {s:?}");
    let calls = fake.calls();
    assert!(matches!(calls.iter().find(|c| matches!(c, Call::DelayPadding(..))), Some(Call::DelayPadding(0, _))), "a from part way in, no delay to cut: {calls:?}");
    // The equalizer on: off offload at once, where the ear is.
    fake.advance(10 * 44_100);
    let at = rig.engine.status().position_ms;
    assert!(rig.wait(5, |r| r.engine.status().position_ms >= at + 9_900));
    let heard = rig.card.heard.lock().len();
    rig.engine.set_settings(Settings { sound: eq, ..offload() });
    assert!(rig.wait(10, |r| r.card.heard.lock().len() > heard + 44_100), "the CPU plays a on");
    assert!(fake.calls().contains(&Call::Close));
    assert!(rig.wait(5, |r| !r.engine.status().offloaded), "the status follows once the burst is in");
    // From where the chip's ear was: the status has the place the CPU took it up at (the engine has not
    // woken since), each reading cut to the millisecond.
    let s = rig.engine.status();
    assert!(s.index == Some(0) && s.position_ms >= at + 9_990, "{s:?}");
    rig.engine.stop();
}

#[test]
fn a_song_the_chip_does_not_decode_plays_on_the_cpu_between_songs_it_does() {
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
    // a played out: the FLAC song on the CPU.
    fake.advance(10 * 44_100);
    assert!(rig.wait(10, |r| r.heard_song("b") && !r.card.heard.lock().is_empty()), "{:?}", rig.events.lock());
    assert!(fake.calls().contains(&Call::Close));
    // And back to the chip for c, once the CPU has played b to its end.
    assert!(rig.wait(20, |r| r.heard_song("c")), "{:?}", rig.events.lock());
    assert!(rig.wait(5, |r| r.engine.status().offloaded), "c is the chip's");
    let heard = rig.card.heard.lock().len() / 2;
    assert!(heard + 44_100 / 2 >= 30 * 44_100, "b whole on the CPU: {heard}");
    rig.engine.stop();
}

#[test]
fn a_song_replay_gain_turns_up_plays_on_the_cpu_between_songs_the_chip_plays_at_their_volume() {
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
    // b is quiet and turned up 6 dB: the output's volume cannot do that. c is turned down, which it can.
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
    // Back to the chip for c, at its volume, once the CPU has played b to its end.
    assert!(rig.wait(30, |r| r.heard_song("c")), "{:?}", rig.events.lock());
    assert!(rig.wait(5, |r| r.engine.status().offloaded), "c is the chip's");
    assert!(rig.wait(5, |_| fake.0.lock().calls.contains(&Call::Volume(0.5))), "c at its ReplayGain volume");
    // The CPU played b turned up: louder than the file, never over the limiter's ceiling.
    let heard = rig.card.heard.lock().clone();
    let peak = heard.iter().map(|v| (*v as f64).abs()).fold(0.0, f64::max);
    assert!(peak <= 10f64.powf(-1.0 / 20.0) * 32768.0 + 1.0, "{peak}");
    assert!(heard.len() / 2 + 44_100 / 2 >= 20 * 44_100, "b whole on the CPU: {}", heard.len() / 2);
    rig.engine.stop();
}

#[test]
fn a_song_on_the_chip_turned_up_by_a_settings_change_goes_to_the_cpu_where_the_ear_is() {
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
    // A louder target, say: a now wants +6 dB.
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
fn a_seek_on_the_chip_empties_its_track_and_starts_again_at_a_packet() {
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
    // All of it is written, and its end of stream said, which stops Android's track until it gets there.
    assert!(rig.wait(10, |_| fake.calls().contains(&Call::EndOfStream)), "{:?}", fake.calls());
    rig.engine.seek(12_000);
    // So the seek lets that track go and opens another, as media3 does, rather than flushing it.
    assert!(rig.wait(5, |_| opens(&fake) == 2 && fake.written() > 0), "{:?}", fake.calls());
    let calls = fake.calls();
    assert!(!calls.contains(&Call::Flush), "a stopped track is not flushed: {calls:?}");
    assert!(calls.iter().position(|c| *c == Call::Close) < calls.iter().rposition(|c| matches!(c, Call::Open(_))), "{calls:?}");
    let after = after_last_open(&calls);
    assert!(matches!(after.iter().find(|c| matches!(c, Call::DelayPadding(..))), Some(Call::DelayPadding(0, _))), "no delay to cut part way in: {after:?}");
    // The place is the packet's the seek landed in (a few frames back, for the decoder's reservoir), and
    // what is written is the song from there.
    let at = rig.engine.status().position_ms;
    assert!((11_800..=12_000).contains(&at), "{at}");
    let frames = fake.written() as i64;
    assert!((frames + at * 44_100 / 1000 - 30 * 44_100).abs() <= 1152, "{frames} frames from {at} ms");
    rig.engine.stop();
}

#[test]
fn repeat_one_on_the_chip_joins_the_song_to_itself_and_says_each_loop() {
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
fn the_sleep_timer_s_end_of_song_on_the_chip_takes_back_the_song_written_after_it() {
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
    // b cannot be taken back out of the track: it starts again from where the ear is, a alone, on a new
    // track (the join's end of stream stopped the one before).
    assert!(rig.wait(5, |_| opens(&fake) == 2 && fake.written() > 0), "{:?}", fake.calls());
    let after_flush = |f: &Fake| after_last_open(&f.calls()).into_iter().filter(|c| matches!(c, Call::EndOfStream | Call::DelayPadding(..))).cloned().collect::<Vec<_>>();
    assert!(rig.wait(5, |_| after_flush(&fake).contains(&Call::EndOfStream)), "{:?}", fake.calls());
    assert_eq!(after_flush(&fake).len(), 2, "a's rest, closed: {:?}", after_flush(&fake));
    let frames = fake.written() as i64;
    // From the packet the place lands in, a few frames back for the decoder's reservoir.
    assert!((frames - 7 * 44_100).abs() <= 4 * 1152, "the rest of a only: {frames}");
    fake.advance(frames as u64);
    assert!(rig.wait(5, |r| r.events.lock().iter().any(|e| matches!(e, Event::Stopped { .. }))), "{:?}", rig.events.lock());
    assert!(rig.wait(5, |r| { let s = r.engine.status(); s.state == State::Paused && s.index == Some(1) && s.position_ms == 0 }), "{:?}", rig.engine.status());
    rig.engine.stop();
}

#[test]
fn opus_goes_to_the_chip_in_ogg_pages_its_header_first() {
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
    // Page by page: the stream's header, the comment header, then one packet a page, each stamped with
    // the samples decoded to its end.
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
    // Three seconds of music, and the encoder's pre-skip before it (which the header tells the chip).
    let last = pages.last().unwrap().0;
    let skip = u16::from_le_bytes([pages[0].1[10], pages[0].1[11]]) as u64;
    assert!(last >= 3 * 48_000 + skip && last < 3 * 48_000 + skip + 960 * 3, "{last}");
    assert_eq!(fake.written(), 3 * 48_000, "the frames heard, pre-skip and padding cut");
    rig.engine.stop();
}

#[test]
fn aac_in_mp4_goes_to_the_chip_with_its_edit_list_as_delay_and_padding() {
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
    // ffmpeg's encoder primes 1024 frames (the edit list's start), and cuts the last block short in the
    // sample table itself, so there is no padding past the edit: media3 reads the same.
    assert_eq!((*delay, *padding), (1024, 0));
    assert_eq!(fake.written(), 3 * 44_100, "the frames heard");
    rig.engine.stop();
}

// ---- bit-perfect ----

#[test]
fn bit_perfect_hands_every_sample_over_as_the_file_stores_it_at_its_own_rate_and_depth() {
    let a = ramp(44_100, 24, 1);
    let b = ramp(48_000, 16, 2);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &wav(44_100, 24, &a)), ("b", &wav(48_000, 16, &b))]);
    let mut app = app();
    // ReplayGain and the equalizer both stand down.
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

/// An endless stream of 16-bit stereo WAV with ICY announcements every 4 KB: the title changes at
/// 64 KB of music.
struct Station;

struct Live {
    at: u64,
    since: usize,
    header: Vec<u8>,
    /// An announcement still to be handed over, however small the reads.
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
        std::thread::sleep(Duration::from_micros(200));
        Ok(n)
    }
}

impl ByteSource for Station {
    fn open(&self, _: &str, _: u64) -> Result<Body, nori_engine::OpenError> {
        Err("a live stream is opened live".into())
    }

    fn open_live(&self, _: &str) -> Result<(Body, Option<usize>), String> {
        let mut header = wav(44_100, 16, &[]);
        // A stream with no end: the largest length a WAV header holds.
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
fn a_live_stream_plays_as_it_comes_with_its_announcements_taken_out() {
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
    // The music is the station's bytes with not one byte of announcement in it.
    for (k, v) in heard.iter().enumerate().take(200_000) {
        let at = k as u64 * 2;
        let byte = |i: u64| (i / 4) as u8;
        let want = i16::from_le_bytes([byte(at), byte(at + 1)]) as f32 / 32768.0;
        assert_eq!(*v, want, "sample {k}");
    }
    // Said when the ear reaches it: after what the output held when the reader passed it.
    time.until(Duration::from_secs(400), || events.lock().contains(&Event::Title("Artist - Song".into())));
    assert!(events.lock().contains(&Event::Title("Artist - Song".into())), "{:?}", events.lock());
    assert_eq!(engine.status().state, State::Playing, "it goes on");
    engine.stop();
}

#[test]
fn an_announcement_reads_its_title() {
    assert_eq!(nori_engine::source::stream_title(b"StreamTitle='Muse - Uprising';StreamUrl='';\0\0"), Some("Muse - Uprising".into()));
    assert_eq!(nori_engine::source::stream_title(b"StreamTitle='';\0"), None);
    assert_eq!(nori_engine::source::stream_title(b"StreamTitle='Sigur R\xf3s - Hopp\xedpolla';"), Some("Sigur Rós - Hoppípolla".into()), "Latin-1");
    assert_eq!(nori_engine::source::stream_title(b"StreamTitle='Guns N' Roses - Patience';"), Some("Guns N' Roses - Patience".into()));
}

// ---- the offline bridge ----

/// The simulated app, with the core's word on a song the network would not bring: the bridge takes it.
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
fn a_song_the_network_will_not_bring_is_handed_to_the_offline_bridge() {
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
    // The bridge's jump (a downloaded song, in the app) plays at once.
    rig.queue.0.lock().set(vec!["a".into(), "b".into(), "a".into()], Some(1), false, 0);
    rig.engine.queue_changed();
    rig.engine.play_at(2, 0);
    assert!(rig.wait(5, |r| r.engine.status().state == State::Playing));
    rig.engine.stop();
}

/// A server that answers with an error status was reached: the song fails for its own reasons, and is
/// skipped rather than handed to the offline bridge as the network's failure.
#[test]
fn a_song_the_server_refuses_is_not_the_network_s_failure() {
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
fn repeat_one_on_the_cpu_says_each_loop() {
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
fn a_song_on_the_cpu_says_why_the_chip_did_not_take_it() {
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
fn plain_offload_takes_a_song_with_no_gap_to_cut_as_media3_does() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    // No LAME tag: nothing says the song has a delay or padding, so it needs no gapless offload.
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

/// Half-minute MP3s by LAME, which tags each with its encoder delay and padding, served under `ids`: long
/// enough that the next song is not read while the one before has just begun.
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
fn without_gapless_offload_songs_of_different_albums_are_offloaded_their_delay_left_in() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let server = Arc::new(Server::default());
    let songs = lame_songs(&d, &server, &["x", "y", "z"]);
    let fake = Fake::new(PLAIN_MP3);
    // A shuffle of three albums: no song follows another on its album.
    let rig = Rig::albums(server, songs, on(&[("x", "X", 3), ("y", "Y", 7), ("z", "Z", 1)]), app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |r| r.engine.status().offloaded), "{:?}", rig.engine.status().pcm_why);
    let why = rig.engine.status().pcm_why.unwrap_or_default();
    assert!(why.contains("near silence at its ends") && why.contains("does not do gapless offload"), "{why}");
    // Each played out, the next goes on the same track from its start: no join to make gapless.
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
fn without_gapless_offload_an_album_in_order_stays_on_the_cpu() {
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
fn without_gapless_offload_the_cpu_takes_an_album_over_at_its_first_song_and_hands_back_after_its_last() {
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
    // a1 joins a2 without a gap: the CPU from a1's start.
    assert!(rig.wait(10, |r| r.heard_song("a1") && !r.engine.status().offloaded && !r.card.heard.lock().is_empty()), "{:?}", rig.events.lock());
    assert_eq!(fake.calls().iter().filter(|c| matches!(c, Call::DelayPadding(..))).count(), 1, "only x went to the chip: {:?}", fake.calls());
    assert_eq!(fake.written(), 0, "and nothing of a1 after x");
    // After a2, y needs no gap cut: the chip again, from y's start, a2 heard to its end on the CPU.
    assert!(rig.wait(20, |r| r.heard_song("y") && r.engine.status().offloaded), "{:?}", rig.events.lock());
    let heard = rig.card.heard.lock().len() / 2;
    assert!(heard + 44_100 / 2 >= 2 * 30 * 44_100, "a1 and a2 whole on the CPU: {heard} frames");
    assert_eq!(fake.calls().iter().filter(|c| matches!(c, Call::DelayPadding(576, _))).count(), 2, "x and y from their starts: {:?}", fake.calls());
    rig.engine.stop();
}

// ---- a play head that makes no sense ----

/// Two songs of `secs` on one gapless track, playing, both written, a second heard; the play head moved
/// by the test only as fast as the clock (`pace` 1), until a test lets it run.
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
    // A second of music, as a second goes by.
    rig.run(1_200);
    fake.advance(44_100);
    assert!(rig.wait(5, |r| r.engine.status().position_ms >= 990), "{:?}", rig.engine.status());
    Some((rig, fake))
}

/// Waits until the engine has written `frames` to the chip, however slowly the loader brings the bytes on
/// a busy machine. The time waiting for them in [`common::Virtual::settle`] is capped in real time, and
/// past that cap the test's clock would run on for as long as the loader is starved, through any limit
/// on it. So the bytes are given real time first, and the clock is moved only when nothing came for a
/// while: then the engine waits for its own timer, not for bytes.
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

/// Waits for the engine to have read what the play head was set to say, and a little more.
fn read_all(rig: &Rig, fake: &Fake) {
    assert!(rig.wait(5, |_| fake.0.lock().readings.is_empty()), "the engine read the play head");
    rig.run(400);
}

#[test]
fn a_play_head_that_could_not_be_read_or_read_nought_skips_no_song() {
    let d = dir();
    let Some((rig, fake)) = two_on_the_chip(&d, 20) else { return };
    // A call that failed, nought (what a failed call was read as, and what a gapless join starts the
    // count again from), another failure: a second into a, with b placed after it on the track.
    fake.read_as(&[None, Some(0), None]);
    read_all(&rig, &fake);
    let s = rig.engine.status();
    assert!(!rig.heard_song("b") && s.index == Some(0) && s.offloaded, "still a, on the chip: {s:?} {:?}", rig.events.lock());
    assert!((990..=1_100).contains(&s.position_ms), "where the ear was: {s:?}");
    let notes = fake.notes();
    assert!(notes.iter().any(|n| n.contains("could not be read")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("read 0 after 44100, not at a join")), "{notes:?}");
    // The true count back, b comes where a ends.
    fake.pace(None);
    fake.advance(19 * 44_100 + 100);
    assert!(rig.wait(5, |r| r.heard_song("b")), "{:?}", rig.events.lock());
    let at = rig.engine.status().position_ms;
    assert!((0..100).contains(&at), "at the start of b: {at}");
    rig.engine.stop();
}

#[test]
fn a_play_head_counting_again_from_nought_after_a_pause_skips_no_song() {
    let d = dir();
    let Some((rig, fake)) = two_on_the_chip(&d, 20) else { return };
    rig.engine.pause();
    assert!(rig.wait(5, |r| r.engine.status().state == State::Paused));
    // The platform went to standby while paused: its count starts again from nought.
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
    // And a ends where it ends.
    fake.pace(None);
    fake.advance(19 * 44_100 - 13_230 + 100);
    assert!(rig.wait(5, |r| r.heard_song("b")), "{:?}", rig.events.lock());
    let at = rig.engine.status().position_ms;
    assert!((0..100).contains(&at), "at the start of b: {at}");
    rig.engine.stop();
}

#[test]
fn a_play_head_ahead_of_the_clock_hands_the_song_to_the_cpu_where_the_ear_is() {
    let d = dir();
    let app = Logged::new();
    let Some((rig, fake)) = two_on_the_chip_with(&d, 20, app.clone()) else { return };
    // Ten seconds on in a moment (a count in other units, a jump), a failed call, the jump again.
    fake.read_as(&[Some(11 * 44_100), None, Some(11 * 44_100)]);
    assert!(rig.wait(10, |r| !r.engine.status().offloaded && r.card.heard.lock().len() > 44_100), "the CPU took over: {:?}", rig.engine.status());
    let s = rig.engine.status();
    assert!(!rig.heard_song("b") && s.index == Some(0), "a, on the CPU: {s:?} {:?}", rig.events.lock());
    assert!(s.position_ms >= 990 && s.position_ms < 11_000, "from where the ear was, not where the head said: {s:?}");
    // Why, as the CPU took over (by now it may say the chip takes the next song, as it does).
    let log = app.log();
    let why = log.iter().filter_map(|l| l.strip_prefix("playing on the CPU: ")).next().unwrap_or_default();
    assert!(why.contains("could not be followed") && why.contains("ahead of the clock"), "{why}: {log:?}");
    assert!(fake.notes().iter().any(|n| n.starts_with("offload given up")), "{:?}", fake.notes());
    rig.engine.stop();
}

#[test]
fn a_late_word_that_everything_was_presented_ends_no_song() {
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
    // A seek near the end: a new track, which takes its end of stream only at the third time of asking
    // (still stopping, as Android's is after one), and meanwhile the platform says it presented
    // everything: about the track before.
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
    // Played to its end: ended, and the perf notes say how.
    fake.advance(3 * 44_100);
    assert!(rig.wait(5, |r| r.events.lock().contains(&Event::State(State::Ended))), "{:?}", rig.events.lock());
    let notes = fake.notes();
    assert!(notes.iter().any(|n| n.contains("would not take the end of stream while its track played (2 of 3)")), "{notes:?}");
    assert!(notes.iter().any(|n| n.starts_with("a ended by the play head")), "{notes:?}");
    rig.engine.stop();
}

#[test]
fn an_end_of_stream_the_platform_keeps_refusing_hands_the_song_to_the_cpu() {
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
fn the_track_holds_four_minutes_at_most_whatever_the_platform_would_take() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: nothing to offload");
        return;
    }
    let d = dir();
    let a = mp3(&d, "a", 300, 440);
    let server = Arc::new(Server::default());
    serve(&server, &[("a", &a)]);
    let fake = Fake::new(MP3_ONLY);
    // A phone that takes four times what it was asked for: all of a five-minute song at once.
    fake.0.lock().takes = 4;
    let rig = Rig::new(server, vec![("a".into(), "mp3".into(), 300_000)], app(), Some(fake.clone()), offload());
    rig.engine.play_at(0, 0);
    assert!(rig.wait(10, |_| fake.written() >= 240 * 44_100), "{}", fake.written());
    rig.run(300);
    let written = fake.written();
    // Four minutes, and the last write's quarter megabyte past them (16 s at 128 kbps).
    assert!(written <= 257 * 44_100, "four minutes ahead at most: {} s", written / 44_100);
    assert!(!fake.calls().contains(&Call::EndOfStream), "the song is not written to its end yet");
    // Under half a minute left in the track: the rest is written.
    fake.advance(written - 20 * 44_100);
    assert!(rig.wait(5, |_| fake.written() == 300 * 44_100), "{}", fake.written());
    assert!(rig.wait(5, |_| fake.calls().contains(&Call::EndOfStream)));
    rig.engine.stop();
}

/// Offloaded, the engine sleeps for minutes while the chip plays on (the app hidden): its status is the
/// place at its last wake. A look - the screen coming back, or a seek bar whose reading is old - reads the
/// chip at once, and the status is the ear's place.
#[test]
fn a_look_reads_the_place_the_chip_played_to_while_the_engine_slept() {
    let d = dir();
    let Some((rig, fake)) = two_on_the_chip(&d, 20) else { return };
    // Ten seconds go by with the chip playing them, and the engine is not told.
    rig.run(10_000);
    fake.advance_quietly(10 * 44_100);
    let stale = rig.engine.status();
    assert!(stale.offloaded && stale.position_ms < 2_000, "the engine has not looked: {stale:?}");
    rig.engine.look();
    // At once: within 20 ms of the clock, not at the engine's next wake.
    assert!(rig.time.until(Duration::from_millis(20), || rig.engine.status().position_ms >= 10_990), "{:?}", rig.engine.status());
    let s = rig.engine.status();
    assert!(s.offloaded && s.index == Some(0) && s.position_ms <= 11_400, "eleven seconds into a, on the chip: {s:?}");
    assert!(!fake.notes().iter().any(|n| n.contains("ahead of the clock")), "{:?}", fake.notes());
    rig.engine.stop();
}

#[test]
fn a_pause_after_the_engine_slept_through_the_music_keeps_the_ear_where_it_is() {
    let d = dir();
    let Some((rig, fake)) = two_on_the_chip(&d, 20) else { return };
    // The engine sleeps while the chip plays on; the pause comes before it looked again.
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

/// Two songs of `secs` on a Galaxy S22's offloaded track ([`Fake::phone`]).
fn two_on_a_phone(d: &Path, secs: u32, stamps: bool, head_stuck: bool) -> Option<(Rig, Fake)> {
    two_on_a_phone_with(d, secs, stamps, head_stuck, false)
}

/// [`two_on_a_phone`], its timestamps jittery from the start ([`Fake::jittery`]) if `jittery`.
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

/// Both songs play through on the phone's small track, to the end of the queue, without a moment of
/// silence and on the chip all along.
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
    // What the platform granted, and how often the thread wakes for it, for the perf report.
    assert!(notes.iter().any(|n| n.contains("granted a track of 64 KB of the") && n.contains("topped up when the platform asks")), "{notes:?}");
}

#[test]
fn a_phone_that_grants_64_kb_and_whose_play_head_never_moves_plays_by_its_timestamps() {
    let d = dir();
    let secs = 30;
    let Some((rig, fake)) = two_on_a_phone(&d, secs, true, true) else { return };
    // Ten seconds into a, the ear is where the timestamp says, not at nought.
    assert!(rig.time.until(Duration::from_secs(20), || fake.0.lock().head >= 10 * 44_100));
    rig.run(100);
    let s = rig.engine.status();
    // As of the engine's last wake: it wakes about twice in what the track holds.
    assert!(s.offloaded && s.index == Some(0) && (7_000..=10_200).contains(&s.position_ms), "{s:?}");
    plays_through_on_the_phone(&rig, &fake, secs);
    rig.engine.stop();
}

#[test]
fn a_phone_that_grants_64_kb_without_timestamps_plays_by_its_play_head() {
    let d = dir();
    let secs = 30;
    let Some((rig, fake)) = two_on_a_phone(&d, secs, false, false) else { return };
    plays_through_on_the_phone(&rig, &fake, secs);
    rig.engine.stop();
}

/// A count that stands at nought while the platform keeps asking for more is one it does not keep: the
/// chip plays (it asks for what it played), so once the slack has run out the CPU takes over where the
/// clock puts the ear.
#[test]
fn a_track_whose_play_head_and_timestamp_never_move_hands_the_song_to_the_cpu_where_the_clock_puts_the_ear() {
    let d = dir();
    let Some((rig, fake)) = two_on_a_phone(&d, 30, false, true) else { return };
    // Its count stands at nought for the watchdog's ten seconds while the platform asks for more, and the
    // CPU takes over where the clock says the ear is.
    let took = rig.time.until(Duration::from_secs(30), || !rig.engine.status().offloaded && rig.card.heard.lock().len() > 2 * 44_100);
    assert!(took, "the CPU took over: {:?} {:?}", rig.engine.status(), fake.notes());
    let s = rig.engine.status();
    assert!(s.index == Some(0) && !rig.heard_song("b"), "a, on the CPU: {s:?}");
    // About twelve seconds in: the slack and what the CPU played since, by the clock the chip played by.
    assert!((10_000..=14_000).contains(&s.position_ms), "where the clock puts the ear, not at nought: {s:?}");
    let notes = fake.notes();
    let given_up = notes.iter().find(|n| n.starts_with("offload given up, the CPU plays on from")).cloned().unwrap_or_default();
    assert!(given_up.contains("play head stood at 0") && given_up.contains("asked for more") && given_up.contains("where the clock puts the ear"), "{notes:?}");
    // Its requests kept it fed until then: no silence before the CPU took over.
    assert_eq!(fake.starved_ms(), 0, "{notes:?}");
    rig.engine.stop();
}

#[test]
fn a_timestamp_that_stands_still_gives_way_to_a_play_head_that_moves() {
    let d = dir();
    let secs = 30;
    let Some((rig, fake)) = two_on_a_phone(&d, secs, true, false) else { return };
    assert!(rig.time.until(Duration::from_secs(10), || fake.0.lock().head >= 3 * 44_100));
    // The timestamp stops where it is (the platform no longer updates it); the play head goes on.
    let at = fake.0.lock().head;
    fake.0.lock().frozen_stamp = Some(at);
    plays_through_on_the_phone(&rig, &fake, secs);
    assert!(fake.notes().iter().any(|n| n.contains("the play head is followed instead")), "{:?}", fake.notes());
    rig.engine.stop();
}

#[test]
fn a_phone_whose_timestamp_jitters_keeps_the_ear_where_the_chip_is() {
    let d = dir();
    let secs = 20;
    let Some((rig, fake)) = two_on_a_phone_with(&d, secs, true, true, true) else { return };
    // Where the status puts the ear against where the chip is, after every wake: the ear is never
    // further on than the chip presented (a restart taken from a moment's jitter put it 160 to 320 ms on).
    let mut ahead_ms = 0i64;
    let mut looked = 0;
    let mut watch = |rig: &Rig| {
        // The engine looks at the count at every step, as often as fades and the screen have it look.
        fake.0.lock().wake();
        let s = rig.engine.status();
        let (head, flushes) = {
            let c = fake.0.lock();
            (c.head, c.flushes)
        };
        // a on the track before the skip, b from nought on the track flushed for it.
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
    // A skip: the track flushed, and its first timestamps are a's count from before.
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
    // b ends where the chip is within the engine's 100 ms of slack at the end, not earlier.
    assert!(c.flushes == 1 && c.head + 4_410 >= secs as u64 * 44_100, "all of b played on the flushed track: {}", c.head);
    drop(c);
    assert_eq!(fake.starved_ms(), 0, "never out of music: {notes:?}");
    assert!(rig.card.opened.lock().is_empty(), "the CPU's output was never opened");
    assert!(!notes.iter().any(|n| n.contains("given up") || n.contains("counts again from nought")), "no count taken as started again: {notes:?}");
    // The two stale readings after the flush are said (the second may be left out), and nothing else of
    // the count: the steps back are said once, with how the song ended.
    let of_count: Vec<&String> = notes.iter().filter(|n| !n.contains("granted a track") && !n.contains(" ended ")).collect();
    assert!(of_count.len() <= 2 && of_count.iter().all(|n| n.contains("ahead of the clock")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains(" ended ") && n.contains("a moment back")), "{notes:?}");
    rig.engine.stop();
}

#[test]
fn next_pressed_quickly_on_the_chip_moves_one_song_per_press() {
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
        // The chip plays on into d a second.
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

/// The simulated app, shared, so a test can change the planner's settings (the core's, which change the
/// moment the user touches them) and read its log while the engine plays.
#[derive(Clone)]
/// The second field, when above 0, is where every plan enters its incoming song, µs (a planner that
/// skips the song's quiet opening).
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
fn automix_switched_on_while_the_chip_plays_takes_the_music_to_the_cpu_at_once_and_mixes() {
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
    // Switched on in the settings: the chip cannot mix, so the CPU takes the song over where the ear is,
    // not at the next song - whose join with this one is the mix.
    rig.engine.set_settings(automix());
    rig.engine.replan();
    assert!(rig.wait(10, |r| !r.engine.status().offloaded && r.card.heard.lock().len() > 2 * 44_100), "the CPU took over: {:?}", rig.engine.status());
    assert!(fake.calls().contains(&Call::Close), "the chip's track was let go");
    assert_eq!(rig.engine.status().pcm_why.as_deref(), Some("AutoMix is on"));
    let s = rig.engine.status();
    assert!(s.index == Some(0) && s.position_ms >= 5_000, "a from where the ear was: {s:?}");
    // And the song's ending is mixed into the next one.
    assert!(rig.wait(20, |r| r.events.lock().contains(&Event::State(State::Ended))), "{:?}", app.log());
    let log = app.log();
    assert!(log.iter().any(|l| l.contains("transition a -> b")), "{log:?}");
    assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{log:?}");
    rig.engine.stop();
}

/// As a phone had it: the chip plays with AutoMix off, the equalizer is switched on (the CPU takes the song
/// over), and AutoMix a few seconds later, the planner's own settings flipping only then.
#[test]
fn automix_switched_on_after_the_equalizer_took_the_song_off_the_chip_mixes() {
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
    // A second later, on the test's clock.
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
fn automix_switched_off_hands_the_song_back_to_the_chip_where_the_ear_is() {
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
fn automix_switched_off_leaves_an_album_in_order_on_the_cpu_without_gapless_offload() {
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
    // Offload is wanted again, but a1 joins a2 without a gap, which this chip cannot do: the CPU plays on.
    let why = "joins a song of its album without a gap";
    assert!(rig.wait(10, |r| r.engine.status().pcm_why.is_some_and(|w| w.contains(why))), "{:?}", rig.engine.status().pcm_why);
    assert!(rig.engine.status().offload_wanted && !rig.engine.status().offloaded);
    assert!(fake.calls().iter().all(|c| !matches!(c, Call::Open(_))), "no track for the chip: {:?}", fake.calls());
    rig.engine.stop();
}

#[test]
fn offload_switched_off_and_on_while_playing_moves_the_song_at_once_both_ways() {
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

// ---- the chip's song handed to the CPU on a phone, where the chip really was ----

/// What happened on the phone's chip before the equalizer was switched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Before {
    /// Ten seconds of a.
    Played,
    /// Five seconds of a, two paused, then five more.
    PausedAndResumed,
    /// Five seconds of a, then a skip to b, the equalizer switched on half a second into it.
    Skipped,
}

/// A song plays on a Galaxy S22's chip (64 KB granted, a play head stuck at nought, its timestamps
/// jittery if `jittery`), and the equalizer is switched on: the CPU takes the song over within a moment
/// of where the chip was, and the song plays on from there, the status and the events following it.
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
            // Half a second in: past the glitches of a jittery track's first 300 ms, whose timestamps
            // say less than it presented.
            assert!(rig.time.until(Duration::from_secs(10), || fake.0.lock().flushes == 1 && head() >= 44_100 / 2), "b on the chip");
            1
        }
    };
    let s = rig.engine.status();
    assert!(s.offloaded && s.index == Some(song), "{s:?} {:?}", fake.notes());
    let events_before = rig.events.lock().len();
    // The chip moves only as the test's clock does: where it is now is where the ear is when the
    // settings arrive.
    let chip_ms = (head() * 1000 / 44_100) as i64;
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let asked_at = rig.now_ms();
    rig.engine.set_settings(Settings { sound: eq, ..offload() });
    // At once: the CPU plays within a moment of the settings changing, not at the chip's next top-up.
    assert!(rig.time.until(Duration::from_secs(10), || !rig.engine.status().offloaded && !rig.card.heard.lock().is_empty()));
    assert!(rig.now_ms() - asked_at <= 300, "the CPU played {} ms after the equalizer went on: {:?}", rig.now_ms() - asked_at, fake.notes());
    let took = rig.time.until(Duration::from_secs(10), || !rig.engine.status().offloaded && rig.card.heard.lock().len() >= 2 * 44_100 / 2);
    assert!(took, "the CPU took over: {:?} {:?}", rig.engine.status(), fake.notes());
    assert!(fake.calls().contains(&Call::Close) && !fake.0.lock().playing);
    let s = rig.engine.status();
    let cpu_ms = (rig.card.heard.lock().len() / 2 * 1000 / 44_100) as i64;
    let notes = fake.notes();
    assert!(s.index == Some(song) && s.state == State::Playing, "{s:?}");
    // Where the CPU took over, as the perf report has it: where the chip was, not where the engine last
    // looked at it, nor the end of what was written. The chip played on while the CPU opened the song
    // ahead of it, and handed over there.
    let left = notes.iter().find(|n| n.starts_with("offload: left at ")).cloned().unwrap_or_default();
    assert!(left.contains("chip said") && left.contains("written"), "the handoff noted: {notes:?}");
    let left_ms: i64 = left["offload: left at ".len()..].split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(-1);
    let lead = nori_engine::REMAKE_LEAD_MS;
    assert!((left_ms - chip_ms - lead).abs() <= 50, "the CPU took {song} over from {left_ms} ms, the chip was at {chip_ms} ms {lead} ms before: {notes:?}");
    // The status runs on from there with what the CPU played (as of the engine's last wake, a moment
    // behind the card).
    assert!(s.position_ms <= left_ms + cpu_ms + 50 && s.position_ms >= left_ms + cpu_ms - 300, "from {left_ms} ms, {cpu_ms} ms played: {s:?}");
    // It plays on from there, the status with it.
    rig.run(3_000);
    let later = rig.engine.status();
    assert!(later.index == Some(song) && later.state == State::Playing && !later.offloaded, "{later:?}");
    assert!((later.position_ms - s.position_ms - 3_000).abs() <= 100, "three seconds on: {s:?} {later:?}");
    // Nothing said another song, nor the end; the positions said go on from where the chip was.
    let events: Vec<Event> = rig.events.lock()[events_before..].to_vec();
    assert!(!events.iter().any(|e| matches!(e, Event::Song { index, .. } if *index != song) || *e == Event::State(State::Ended)), "{events:?}");
    // The new place is said once, where the CPU took over, for a client running its own clock to take
    // it again without a command: the player's own bar anchors there, not at the chip's last word.
    let placed: Vec<(usize, i64)> = events.iter().filter_map(|e| if let Event::Placed { index, ms } = e { Some((*index, *ms)) } else { None }).collect();
    assert!(placed.len() == 1 && placed[0].0 == song && (placed[0].1 - left_ms).abs() <= 50, "placed at {left_ms} ms: {placed:?}");
    let positions: Vec<i64> = events.iter().filter_map(|e| if let Event::Position { index, ms } = e { assert_eq!(*index, song); Some(*ms) } else { None }).collect();
    assert!(positions.len() >= 8, "{events:?}");
    assert!(positions.windows(2).all(|w| w[1] >= w[0] - 20), "onwards: {positions:?}");
    assert!(positions.iter().all(|&ms| ms >= chip_ms - 300 && ms <= later.position_ms + 50), "from where the chip was ({chip_ms} ms): {positions:?}");
    rig.engine.stop();
}

#[test]
fn the_equalizer_switched_on_while_a_phone_s_chip_plays_hands_the_song_to_the_cpu_where_the_chip_was() {
    equalizer_on_a_phone(false, Before::Played);
}

#[test]
fn the_equalizer_switched_on_while_a_jittery_phone_s_chip_plays_hands_the_song_to_the_cpu_where_the_chip_was() {
    equalizer_on_a_phone(true, Before::Played);
}

#[test]
fn the_equalizer_switched_on_after_a_pause_on_a_phone_s_chip_hands_the_song_to_the_cpu_where_the_chip_was() {
    equalizer_on_a_phone(true, Before::PausedAndResumed);
    equalizer_on_a_phone(false, Before::PausedAndResumed);
}

#[test]
fn the_equalizer_switched_on_right_after_a_skip_on_a_phone_s_chip_hands_the_song_to_the_cpu_where_the_chip_was() {
    equalizer_on_a_phone(true, Before::Skipped);
    equalizer_on_a_phone(false, Before::Skipped);
}

/// Offload given up and taken up again, again and again, while a phone's chip plays (the equalizer on and
/// off): each time the chip plays on while the CPU opens the song where the ear will be, fades out, and
/// the CPU comes in from silence where it stopped. Between the two the music never stops for longer
/// than the platform takes to start a track, and the place runs on.
#[test]
fn offload_given_up_on_a_phone_hands_the_song_to_the_cpu_behind_a_dip_with_no_gap() {
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
        // Every frame of the time went to the ear, from the chip and then the CPU.
        let (chip, cpu) = (head() - chip0, (rig.card.heard.lock().len() - cpu0) as u64 / 2);
        let due = ((rig.time.clock.now_ns() - t0) as u128 * 44_100 / 1_000_000_000) as u64;
        let gap_ms = due.saturating_sub(chip + cpu) * 1000 / 44_100;
        assert!(gap_ms <= 5, "round {round}: {gap_ms} ms of silence between the chip ({chip} frames) and the CPU ({cpu})");
        // The chip faded out before it was let go, over the dip.
        let calls = fake.0.lock().calls[calls0..].to_vec();
        let close = calls.iter().position(|c| *c == Call::Close).expect("the chip's track let go");
        let fade: Vec<f32> = calls[..close].iter().filter_map(|c| if let Call::Volume(v) = c { Some(*v) } else { None }).collect();
        assert!(fade.len() >= 2 && fade.windows(2).all(|w| w[1] <= w[0]) && fade.last() == Some(&0.0), "round {round}: faded out: {fade:?}");
        // And the CPU came in from silence: its first millisecond far quieter than what follows.
        let heard = rig.card.heard.lock()[cpu0..].to_vec();
        let loud = |s: &[f32]| s.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!(loud(&heard[..88]) * 4.0 < loud(&heard[4_410..4_498]), "round {round}: faded in");
        let places: Vec<i64> = rig.events.lock()[seen..].iter().filter_map(|e| if let Event::Position { ms, .. } = e { Some(*ms) } else { None }).collect();
        assert!(places.windows(2).all(|w| w[1] >= w[0] - 20 && w[1] - w[0] <= 250), "round {round}: the place runs on: {places:?}");
        // Back to the chip for the next round.
        rig.engine.set_settings(offload());
        assert!(rig.time.until(Duration::from_secs(10), || rig.engine.status().offloaded), "round {round}: back on the chip");
        rig.run(1_000);
    }
    rig.engine.stop();
}

/// A server that says no length (a song transcoded as it is sent, in chunks) and sends the first
/// `hold` bytes of each song at once, the rest only once `gate` opens.
struct Unsized {
    files: Vec<(String, Arc<Vec<u8>>)>,
    hold: usize,
    gate: Arc<std::sync::atomic::AtomicBool>,
}

struct Trickle {
    file: Arc<Vec<u8>>,
    at: usize,
    hold: usize,
    gate: Arc<std::sync::atomic::AtomicBool>,
}

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.hold && !self.gate.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
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

/// Two songs of `secs` from a server that does not say their length ([`Unsized`]), nine tenths of each
/// sent and the rest held back until the gate opens (long songs, so that what the CPU reads ahead stays
/// well short of it). None without ffmpeg.
fn unsized_rig(d: &Path, secs: u32, fake: Option<Fake>, settings: Settings) -> Option<(Rig, Arc<std::sync::atomic::AtomicBool>)> {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed");
        return None;
    }
    let (a, b) = (mp3(d, "a", secs, 440), mp3(d, "b", secs, 660));
    let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
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

/// A song whose length the server does not say, still on its way, plays on the phone's chip; the
/// equalizer goes on: the CPU takes it over where the chip was (a song of the library is seeked, not
/// taken for a station that cannot be), and it plays on from there.
#[test]
fn the_equalizer_switched_on_over_a_song_of_no_known_length_still_coming_hands_it_to_the_cpu_where_the_chip_was() {
    let d = dir();
    let secs = 60;
    let fake = Fake::new(MP3_ONLY);
    fake.phone(true, true);
    let Some((rig, gate)) = unsized_rig(&d, secs, Some(fake.clone()), offload()) else { return };
    // The engine wakes every 100 ms to say where the ear is, so its status is never older than that.
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
    gate.store(true, std::sync::atomic::Ordering::Release);
    rig.engine.stop();
}

/// The equalizer switched on and off quickly, again and again, while a song plays on a phone's chip:
/// the song goes to the CPU and back without an error, a moment of silence on either path or a jump of
/// its place, and ends where the last switch left it.
#[test]
fn the_equalizer_switched_on_and_off_quickly_on_a_phone_plays_on_without_a_glitch() {
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
    // Last switched off: back on the chip, and playing there.
    assert!(rig.time.until(Duration::from_secs(10), || rig.engine.status().offloaded && fake.0.lock().playing), "{:?} {:?}", rig.engine.status(), fake.notes());
    rig.run(2_000);
    let s = rig.engine.status();
    let elapsed = rig.now_ms() - t0;
    let events: Vec<Event> = rig.events.lock()[events_before..].to_vec();
    let notes = fake.notes();
    // Nor a loop: the song placed on the chip again is not repeat one starting it again.
    assert!(!events.iter().any(|e| matches!(e, Event::Error { .. } | Event::Stopped { .. } | Event::Buffering(true) | Event::Looped { .. }) || matches!(e, Event::Song { index, .. } if *index != 0)), "{events:?}");
    assert!(s.index == Some(0) && s.state == State::Playing, "{s:?}");
    assert_eq!(s.underruns, 0, "the CPU's output never ran dry: {s:?}");
    assert_eq!(fake.starved_ms(), 0, "the chip never ran dry: {notes:?}");
    // The dips of the switches cost a little each, never a jump.
    let moved = s.position_ms - started;
    assert!(moved <= elapsed + 150 && moved >= elapsed - 8 * 150, "{moved} ms on in {elapsed} ms: {s:?} {notes:?}");
    let positions: Vec<i64> = events.iter().filter_map(|e| if let Event::Position { ms, .. } = e { Some(*ms) } else { None }).collect();
    assert!(positions.windows(2).all(|w| w[1] >= w[0] - 150 && w[1] <= w[0] + 400), "no jump: {positions:?}");
    // Each move between the chip and the CPU said with its place.
    let placed = events.iter().filter(|e| matches!(e, Event::Placed { index: 0, .. })).count();
    assert!((2..=8).contains(&placed), "{events:?}");
    rig.engine.stop();
}

/// As a phone had it with AutoMix on (the CPU plays): the equalizer switched on puts the sound chain in
/// the path within a moment, and switched on and off quickly again and again never runs the output dry.
#[test]
fn the_equalizer_switched_on_and_off_quickly_on_the_cpu_is_heard_at_once_without_an_underrun() {
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

/// On the CPU, a song whose length the server does not say, still on its way: the equalizer switched on
/// makes the music again from where the ear is (a seek), which plays on, the chain in the path at once.
#[test]
fn the_equalizer_switched_on_over_a_song_of_no_known_length_on_the_cpu_is_heard_at_once_where_the_ear_is() {
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
    gate.store(true, std::sync::atomic::Ordering::Release);
    rig.engine.stop();
}

/// A seek in a song on a phone's chip places it on the track again: that is no loop of repeat one, and
/// none is said (a client counts a loop as a play of its own, and moves its place to the song's end
/// and back).
#[test]
fn a_seek_on_a_phone_s_chip_says_no_loop() {
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

/// Through an AutoMix, whenever a screen reads the engine (one come back mid-mix does so once), the
/// place it gets belongs to the song it is told is heard: a's place while a is, b's from the moment b is,
/// never a's place under b or b's under a.
#[test]
fn through_a_mix_the_place_read_belongs_to_the_song_read_with_it() {
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
            // a's place never past a's end; b's place, the moment b is said, no further in than the mix.
            match s.index {
                Some(0) if s.position_ms > 40_100 => bad.push((s.index, s.position_ms)),
                Some(1) if last.is_some_and(|l| l.0 == Some(0)) && s.position_ms > 8_000 => bad.push((s.index, s.position_ms)),
                _ => {}
            }
            if let Some((i, ms)) = last {
                // Within one song the place only moves on, by no more than a step.
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
    // The positions said follow the same rule.
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

/// After a mix, the place said for the song mixed into is the music of it really played, from where the
/// mix entered it: a pause (which says the place again) finds it where the running on had it, and what is
/// left of the song to its end is exactly what the card then plays.
#[test]
fn after_a_mix_the_new_song_s_place_is_what_was_played_of_it() {
    mixed_into_b_its_place_is_what_was_played(0);
}

/// [`after_a_mix_the_new_song_s_place_is_what_was_played_of_it`], b opening on 5 s of silence the mix
/// may enter past.
#[test]
fn after_a_mix_into_a_song_entered_past_its_start_its_place_is_what_was_played_of_it() {
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
    // Along b, the place moves with the clock, whatever the mix did around it.
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
    // The rest of b, played from there to its end, is what the place said was left of it.
    let rate = rig.card.opened.lock().last().map_or(44_100, |f| f.rate as i64);
    let channels = rig.card.opened.lock().last().map_or(2, |f| f.channels as i64);
    let from = rig.card.heard.lock().len() as i64;
    rig.engine.play();
    assert!(rig.wait(40, |r| r.events.lock().contains(&Event::State(State::Ended))), "{:?}", app.log());
    let rest_ms = (rig.card.heard.lock().len() as i64 - from) / channels * 1000 / rate;
    assert!((paused.position_ms + rest_ms - 30_000).abs() <= 400, "b paused at {} ms, then {rest_ms} ms of it played to its end", paused.position_ms);
    rig.engine.stop();
}

/// `secs` of MP3 opening on `silent` seconds of silence, then a tone of `hz`.
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

// ---- small grants: a phone that gives the track far less than asked ----

/// A 32 KB track (a Galaxy S21 FE's) and a 64 KB one (a Galaxy S22's).
const KB32: usize = 32 * 1024;
const KB64: usize = 64 * 1024;
/// A DSP that buffers about six seconds of a 320 kbps song beyond the track.
const DSP: usize = 256 * 1024;

/// Two songs of `secs` at 320 kbps on a phone's chip ([`Fake::phone`]) that grants `grant` bytes and
/// buffers `dsp` more in its own decoder: true timestamps, a play head that moves.
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

/// The engine's wakes a second against the platform's requests for more a second, over 40 s of steady
/// playing (5 s to 45 s into a song of a minute) on a track of `grant` bytes with `dsp` more behind it.
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

/// The engine wakes for the platform's word that the track has room, not on a timer of its own guessed
/// from the bytes the track holds: on a track of 32 or 64 KB it wakes as often as the platform asks, and
/// with a DSP that buffers seconds on its own, as rarely as that lets it.
#[test]
fn on_a_small_grant_the_engine_wakes_only_when_the_platform_asks() {
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

/// The tester's Galaxy S21 FE with the screen off: its timestamp stood for 2.8 s (and its play head with
/// it) while the chip played from its own buffer, without asking for more, and the engine took that for
/// a stall. It is not one: the music plays on the chip, and the ear follows it again once the count
/// moves. Here the count stands for five seconds, longer than the tester saw.
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
    // The chip played on through it (and whether it asked for more meanwhile is the platform's).
    let asked = fake.0.lock().requests - requests;
    let s = rig.engine.status();
    assert!(s.offloaded, "still on the chip after the count stood 5 s ({asked} requests meanwhile): {s:?} {:?}", fake.notes());
    plays_through_on_a_small_grant(&rig, &fake, secs);
    rig.engine.stop();
}

#[test]
fn a_timestamp_standing_for_seconds_with_the_screen_off_on_a_32_kb_track_is_no_stall() {
    screen_off_on_a_32_kb_track(false);
}

#[test]
fn a_timestamp_and_play_head_standing_for_seconds_with_the_screen_off_on_a_32_kb_track_is_no_stall() {
    screen_off_on_a_32_kb_track(true);
}

/// Both songs play through on a small track, to the end of the queue, on the chip all along.
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

/// A chip that really stops (it plays nothing and asks for nothing) is given up, and the CPU takes the
/// song over where the chip's count last put the ear: never ahead of it, whatever the clock says.
#[test]
fn a_chip_that_really_stalls_hands_the_song_to_the_cpu_where_the_chip_stopped() {
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
    // At the chip's place, a moment back at most: never the clock's (which ran on by the whole wait).
    assert!(s.position_ms <= stopped_ms + 50 && s.position_ms >= stopped_ms - 1_000, "the CPU at {} ms, the chip stopped at {stopped_ms} ms: {notes:?}", s.position_ms);
    let given_up = notes.iter().find(|n| n.starts_with("offload given up")).cloned().unwrap_or_default();
    assert!(given_up.contains("asked for nothing") && given_up.contains("where the chip"), "{notes:?}");
    rig.engine.stop();
}

/// A chip that presents the last moments of what it holds only once more comes (or the end of stream is
/// said after them), as the tester's S21 FE seemed to at the end of a song: the next song is written
/// long before the one playing runs out, so the chip is never left waiting for it.
#[test]
fn a_chip_that_holds_back_the_last_moments_gets_the_next_song_before_it_needs_it() {
    let d = dir();
    let secs = 30;
    let Some((rig, fake)) = two_on_a_small_grant(&d, secs, KB32, DSP) else { return };
    fake.0.lock().holds_back = 38_235;
    plays_through_on_a_small_grant(&rig, &fake, secs);
    rig.engine.stop();
}

/// The engine lets the CPU sleep while the chip plays from a small track fed on the platform's word,
/// and keeps it awake where its own work is: the start, the few seconds before the ear reaches the next
/// song (its event comes on time, with the chip asleep in between), and the end of the music.
#[test]
fn the_engine_lets_the_cpu_sleep_while_the_chip_plays_and_keeps_it_awake_for_its_own_work() {
    let d = dir();
    let secs = 60;
    let Some((rig, fake)) = two_on_a_small_grant(&d, secs, KB32, DSP) else { return };
    // Where the CPU was kept awake, in ms of the test's clock, and where b's song event came.
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
    // Two minutes of music: awake for the start, a few seconds before b and before the end, no more.
    assert!(awake_ms * 100 / total <= 15, "awake {awake_ms} ms of {total} ms, from {awake_at:?} (ms of the chip's music)");
    assert!(awake_at.iter().any(|&ms| (secs as u64 * 1000 - 8_000..secs as u64 * 1000).contains(&ms)), "awake before b: {awake_at:?}");
    // b's event came as the chip reached it, not at some later wake.
    let b = b_said.expect("b said");
    assert!((secs as u64 * 1000..=secs as u64 * 1000 + 50).contains(&b), "b said at {b} ms of the chip's music");
    // Said as events too, for a platform to take its lock by.
    let said: Vec<bool> = rig.events.lock().iter().filter_map(|e| if let Event::Awake(a) = e { Some(*a) } else { None }).collect();
    assert!(said.len() >= 4 && said.windows(2).all(|w| w[0] != w[1]) && said[0] == false, "{said:?}");
    rig.engine.stop();
}

/// On the CPU the engine never lets it sleep: its own bursts feed the output.
#[test]
fn on_the_cpu_the_engine_keeps_the_cpu_awake() {
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

// ---- a new queue made around the song playing ----

/// A tap on the song playing in its album's list makes the album the queue around it (Android's
/// `keepPlaying`): the song goes on, and nothing says the music stopped or ended - the song after it in
/// the old queue, read ahead already, is no longer there.
#[test]
fn a_new_queue_made_around_the_song_playing_plays_on_without_a_stop() {
    let tone = ramp(3 * 44_100, 16, 5);
    let server = Arc::new(Server::default());
    let file = wav(44_100, 16, &tone);
    serve(&server, &[("p1", &file), ("a", &file), ("p2", &file), ("c", &file), ("d", &file)]);
    let songs = ["p1", "a", "p2", "c", "d"].iter().map(|id| (id.to_string(), "wav".to_string(), 3_000)).collect();
    // The equalizer on, as on the phone it was seen on: the samples go through the sound chain.
    let eq = Sound { bands: vec![Band { kind: 0, freq: 1000.0, gain_db: 3.0, q: 1.0, channel: 0 }], ..Sound::default() };
    let rig = Rig::new(server, songs, app(), None, Settings { sound: eq, ..Settings::default() });
    rig.queue.0.lock().set(vec!["p1".into(), "a".into(), "p2".into()], Some(1), false, 0);
    rig.engine.queue_changed();
    rig.engine.play_at(1, 0);
    assert!(rig.wait(10, |r| r.heard_song("a")), "{:?}", rig.events.lock());
    // A second in: the rest of a and the start of p2 are already read, deep in the buffer.
    rig.run(1_000);
    let before = rig.events.lock().len();
    // The album around it: c, a, d.
    rig.queue.0.lock().set(vec!["c".into(), "a".into(), "d".into()], Some(1), false, 0);
    rig.engine.queue_changed();
    assert!(rig.wait(10, |r| r.heard_song("d")), "d follows a: {:?}", rig.events.lock());
    let after: Vec<Event> = rig.events.lock()[before..].to_vec();
    assert!(!after.iter().any(|e| matches!(e, Event::Stopped { .. } | Event::State(State::Paused | State::Ended | State::Idle))), "{after:?}");
    assert!(!after.iter().any(|e| matches!(e, Event::Song { id, .. } if id == "p2")), "the old queue's next song is not heard: {after:?}");
    rig.engine.stop();
}

/// [`a_new_queue_made_around_the_song_playing_plays_on_without_a_stop`], the old queue's next song
/// failing as it is read ahead: its failure is not the new queue's, and stops nothing.
#[test]
fn a_new_queue_made_around_the_song_playing_is_not_stopped_by_the_old_next_song_failing() {
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
