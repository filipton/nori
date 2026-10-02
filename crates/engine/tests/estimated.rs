//! A transcoding server that promises a longer length than it sends (Navidrome's
//! `estimateContentLength`) and answers 416 past the real end: songs play whole with their real length
//! and seeks, and neither the 416 nor a clean early end is a failure. Ogg Opus songs come from ffmpeg;
//! without it the tests pass trivially.

use crate::common;

use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_engine::{Body, ByteSource, Config, Engine, Event, Library, Located, OpenError, Recent, SharedQueue, Source, State, Store};
use nori_player::sim;
use nori_player::transitions::WindowSong;
use parking_lot::Mutex;

use common::ffmpeg;

/// `a`, `b` and `c`, made once per binary; None without ffmpeg.
pub(crate) fn songs() -> Option<&'static [Arc<Vec<u8>>; 3]> {
    static MADE: std::sync::OnceLock<Option<[Arc<Vec<u8>>; 3]>> = std::sync::OnceLock::new();
    MADE.get_or_init(|| {
        if !ffmpeg() {
            return None;
        }
        let dir = nori_testdir::TempDir::new("estimated-songs");
        Some([opus(&dir, "a", A_SECS, 440), opus(&dir, "b", B_SECS, 660), opus(&dir, "c", B_SECS, 880)].map(Arc::new))
    })
    .as_ref()
}

/// A tone of `secs` as 192 kbps Ogg Opus.
fn opus(dir: &Path, name: &str, secs: u32, hz: u32) -> Vec<u8> {
    let out = dir.join(format!("{name}.opus"));
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={hz}:sample_rate=48000:duration={secs}"), "-ac", "2"])
        .args(["-c:a", "libopus", "-b:a", "192k"])
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg made {name}");
    std::fs::read(out).unwrap()
}

/// How a body from the server ends.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ends {
    /// Cleanly at the real end.
    Clean,
    /// With an error at the real end, as OkHttp reads a short body.
    Broken,
}

/// The transcoding server. Answers promise `extra` bytes too many; a range past the real end is a 416
/// (with the real length when `says`). A first body sends `hold` bytes, then the rest a moment later;
/// `cut` breaks the named song's first body once at that byte. A range not from the start, asked before
/// the transcode is complete, costs `charge` of clock time; with `arrives`, a body from the start
/// streams at a set rate of the test's clock.
struct Transcoder {
    files: Vec<(String, Arc<Vec<u8>>)>,
    extra: u64,
    says: bool,
    ends: Ends,
    hold: usize,
    cut: Mutex<Option<(String, usize)>>,
    requests: Mutex<Vec<(String, u64)>>,
    clock: Virtual,
    charge: Duration,
    arrives: Option<(usize, u64)>,
    /// Files fully transcoded: ranges are answered at once.
    made: Vec<Arc<AtomicBool>>,
    /// Ranges that waited for a full transcode.
    charged: Mutex<Vec<(String, u64)>>,
    /// Files asked for from past their start.
    probed: Vec<Arc<AtomicBool>>,
}

struct Sent {
    file: Arc<Vec<u8>>,
    at: usize,
    hold: Option<usize>,
    /// Ends the hold once the file is asked for further on.
    probed: Arc<AtomicBool>,
    cut: Option<usize>,
    ends: Ends,
    /// First bytes at once, the rest at a rate from this clock time.
    arrives: Option<(usize, u64, i64, Virtual)>,
    /// Set once the whole transcode was sent.
    made: Arc<AtomicBool>,
    clock: Virtual,
}

/// Real time a held body waits for the reader to look past it.
const HOLD: Duration = Duration::from_millis(300);

impl Read for Sent {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.hold.is_some_and(|h| self.at >= h) {
            // Hold until the reader looks further on, or HOLD passes.
            let probed = self.probed.clone();
            self.clock.hang_while(|| !probed.load(Ordering::Acquire), HOLD);
            self.hold = None;
        }
        // Wait until the transcode has come this far.
        let made = self.arrives.as_ref().filter(|_| self.at < self.file.len()).map_or(usize::MAX, |(first, rate, t0, clock)| {
            let by = |ns: i64| first + ((ns - t0).max(0) as u128 * *rate as u128 / 1_000_000_000) as usize;
            if by(clock.now_ns()) <= self.at {
                clock.wait_until(t0 +((self.at + 1 - first) as u128 * 1_000_000_000 / *rate as u128) as i64 + 1);
            }
            by(clock.now_ns())
        });
        let stop = self.cut.unwrap_or(self.file.len()).min(self.hold.unwrap_or(usize::MAX)).min(made);
        if self.at >= self.file.len() {
            self.made.store(true, Ordering::Release);
        }
        if self.at >= stop {
            if self.cut.is_some() || self.ends == Ends::Broken {
                return Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "unexpected end of stream"));
            }
            return Ok(0);
        }
        let n = buf.len().min(stop - self.at);
        buf[..n].copy_from_slice(&self.file[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl ByteSource for Transcoder {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        self.requests.lock().push((url.to_string(), from));
        let k = self.files.iter().position(|(u, _)| u == url).ok_or("404")?;
        let (file, made, probed) = (self.files[k].1.clone(), self.made[k].clone(), self.probed[k].clone());
        if from > 0 {
            probed.store(true, Ordering::Release);
        }
        if from > 0 && !self.charge.is_zero() && !made.load(Ordering::Acquire) {
            // Past the start only once fully transcoded.
            self.charged.lock().push((url.to_string(), from));
            self.clock.wait_until(self.clock.now_ns() + self.charge.as_nanos() as i64);
            made.store(true, Ordering::Release);
        }
        let real = file.len() as u64;
        if from >= real {
            return Err(OpenError::PastEnd { len: self.says.then_some(real) });
        }
        let hold = (from == 0).then_some(self.hold);
        let arrives = self.arrives.filter(|_| from == 0 && !made.load(Ordering::Acquire)).map(|(first, rate)| (first, rate, self.clock.now_ns(), self.clock.clone()));
        let mut cut = self.cut.lock();
        let cut = if from == 0 && cut.as_ref().is_some_and(|c| c.0 == url) { cut.take().map(|c| c.1) } else { None };
        let sent = Sent { file, at: from as usize, hold, probed, cut, ends: self.ends, arrives, made, clock: self.clock.clone() };
        Ok(Body { start: from, len: Some(real + self.extra), reader: Box::new(sent) })
    }
}

impl Transcoder {
    fn asked(&self, id: &str) -> Vec<u64> {
        self.requests.lock().iter().filter(|(u, _)| u == id).map(|r| r.1).collect()
    }

    fn charged(&self) -> Vec<(String, u64)> {
        self.charged.lock().clone()
    }
}

/// The songs with their tagged lengths (0: none), cached in `2` when given.
struct Songs(Arc<Transcoder>, Vec<(String, i64)>, Option<Arc<Store>>);

impl Library for Songs {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let ms = self.1.iter().find(|(i, _)| i == id).map(|s| s.1).ok_or("no such song")?;
        let (url, bytes) = (id.to_string(), self.0.clone() as Arc<dyn ByteSource>);
        let source = match &self.2 {
            Some(store) => Source::Cached { url, bytes, store: store.clone(), key: format!("{id}:192opus") },
            None => Source::Url { url, bytes },
        };
        Ok(Located { source, hint: Some("opus".into()), duration_ms: Some(ms).filter(|&d| d > 0), estimated: true })
    }

    fn about(&self, id: &str) -> WindowSong {
        let ms = self.1.iter().find(|(i, _)| i == id).map_or(0, |s| s.1);
        WindowSong { id: id.into(), title: id.into(), duration_ms: ms, ..Default::default() }
    }
}

struct Rig {
    engine: Engine,
    time: Stepper<Pull>,
    card: Card,
    events: Arc<Mutex<Vec<Event>>>,
    server: Arc<Transcoder>,
    _dir: nori_testdir::TempDir,
}

/// Song lengths: `a` long enough that the last-page probe lands past what the loader reads on to (as a
/// 6 MB transcode on a phone); `b` short enough that the probe waits for bytes.
pub(crate) const A_SECS: u32 = 60;
const B_SECS: u32 = 5;

/// `a`, `b` and `c` from a [`Transcoder`] promising `extra` bytes too many (past one Ogg page). None
/// without ffmpeg.
fn rig(extra: u64, says: bool, ends: Ends, cut: Option<(&str, usize)>) -> Option<Rig> {
    rig_making(extra, says, ends, cut, Making::default())
}

/// How a [`Transcoder`] produces songs, and `a`'s tagged length.
#[derive(Clone, Copy)]
struct Making {
    charge: Duration,
    arrives: Option<(usize, u64)>,
    hold: usize,
    a_ms: i64,
    /// Songs go through an empty stream cache.
    cache: bool,
}

impl Default for Making {
    /// Immediate ranges, 48 kB held, and `a` tagged slightly long (rounded).
    fn default() -> Making {
        Making { charge: Duration::ZERO, arrives: None, hold: 48 * 1024, a_ms: A_SECS as i64 * 1000 + 400, cache: false }
    }
}

/// A fresh transcode: ~12 s of `a` at once, the rest at 8x real time, a range past the start costing
/// 20 s. `a` is tagged `a_ms`.
fn uncached(a_ms: i64) -> Making {
    Making { charge: Duration::from_secs(20), arrives: Some((320 * 1024, 192 * 1024)), hold: usize::MAX, a_ms, cache: false }
}

fn rig_making(extra: u64, says: bool, ends: Ends, cut: Option<(&str, usize)>, making: Making) -> Option<Rig> {
    let Some([a, b, c]) = songs() else {
        eprintln!("ffmpeg is not installed");
        return None;
    };
    let dir = nori_testdir::TempDir::new("estimated");
    let clock = Virtual::default();
    let server = Arc::new(Transcoder {
        files: vec![("a".into(), a.clone()), ("b".into(), b.clone()), ("c".into(), c.clone())],
        extra,
        says,
        ends,
        hold: making.hold,
        cut: Mutex::new(cut.map(|(id, at)| (id.to_string(), at))),
        requests: Mutex::new(Vec::new()),
        clock: clock.clone(),
        charge: making.charge,
        arrives: making.arrives,
        made: (0..3).map(|_| Arc::default()).collect(),
        charged: Mutex::new(Vec::new()),
        probed: (0..3).map(|_| Arc::default()).collect(),
    });
    let songs = vec![("a".to_string(), making.a_ms), ("b".to_string(), B_SECS as i64 * 1000), ("c".to_string(), B_SECS as i64 * 1000)];
    let queue = SharedQueue::default();
    queue.0.lock().set(songs.iter().map(|s| s.0.clone()).collect(), Some(0), false, 0);
    let card = Card::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut app = sim::App::new();
    app.prefs = sim::prefs_off();
    let config = Config { memory_mb: 256, ..Config::default() };
    let store = making.cache.then(|| Store::open(dir.join("cache"), 64 << 20, Box::new(Recent::default())).unwrap());
    let engine = Engine::start_on(Songs(server.clone(), songs, store), app, queue, Box::new(card.clone()), None, config, clock.clone(), move |e| seen.lock().push(e));
    engine.queue_changed();
    Some(Rig { engine, time: Stepper::new(clock, card.pull.clone()), card, events, server, _dir: dir })
}

impl Rig {
    fn errors(&self) -> Vec<Event> {
        self.events.lock().iter().filter(|e| matches!(e, Event::Error { .. } | Event::Bridge { .. } | Event::Stopped { .. })).cloned().collect()
    }

    fn heard_song_since(&self, from: usize, id: &str) -> bool {
        self.events.lock().iter().skip(from).any(|e| matches!(e, Event::Song { id: i, .. } if i == id))
    }

    /// Plays `a`; returns clock time until it is heard.
    fn first_sound(&self) -> Duration {
        let t0 = self.time.clock.now_ns();
        self.engine.play_at(0, 0);
        assert!(self.time.until(Duration::from_secs(60), || !self.card.heard.lock().is_empty() || !self.errors().is_empty()), "a heard");
        assert!(self.errors().is_empty(), "a opened: {:?}", self.errors());
        Duration::from_nanos((self.time.clock.now_ns() - t0) as u64)
    }

    /// Seeks to `ms`; returns clock time until playback passes it.
    fn seek_heard(&self, ms: i64) -> f64 {
        let (t0, seen) = (self.time.clock.now_ns(), self.events.lock().len());
        self.engine.seek(ms);
        let past = |e: &Event| matches!(e, Event::Position { index: 0, ms: at, .. } if (ms + 50..ms + 1_500).contains(at));
        assert!(self.time.until(Duration::from_secs(30), || self.events.lock().iter().skip(seen).any(past)), "heard from {ms} ms: {:?}", self.events.lock());
        (self.time.clock.now_ns() - t0) as f64 / 1e9
    }

    /// Plays until `b` is heard; returns the clock time taken and the seconds heard.
    fn on_to_b(&self) -> (f64, f64) {
        let (t0, before, seen) = (self.time.clock.now_ns(), self.card.secs(), self.events.lock().len());
        let limit = Duration::from_secs(A_SECS as u64 + 60);
        assert!(self.time.until(limit, || self.heard_song_since(seen, "b") || !self.errors().is_empty()), "b came: {:?}", self.events.lock());
        assert!(self.errors().is_empty(), "a played without a failure: {:?} asked {:?}", self.errors(), self.server.asked("a"));
        ((self.time.clock.now_ns() - t0) as f64 / 1e9, self.card.secs() - before)
    }

    /// Plays `a` from `from_ms` until `b`; returns the seconds of `a` heard.
    fn play_a_to_its_end(&self, from_ms: i64) -> f64 {
        self.play_to_its_end(0, from_ms)
    }

    /// Plays queue `index` from `from_ms` until the next song; returns the seconds heard.
    fn play_to_its_end(&self, index: usize, from_ms: i64) -> f64 {
        let (id, next) = (["a", "b"][index], ["b", "c"][index]);
        let (before, seen) = (self.card.secs(), self.events.lock().len());
        self.engine.play_at(index, from_ms);
        let limit = Duration::from_secs(A_SECS as u64 + 30);
        assert!(self.time.until(limit, || self.heard_song_since(seen, next) || !self.errors().is_empty()), "{next} came: {:?}", self.events.lock());
        assert!(self.errors().is_empty(), "{id} played without a failure: {:?} asked {:?}", self.errors(), self.server.asked(id));
        let s = self.engine.status();
        assert!(s.index == Some(index + 1) && s.state == State::Playing, "{next} plays after {id}: {s:?}");
        let heard = self.card.secs();
        if heard >= before { heard - before } else { heard }
    }
}

#[test]
fn estimated_ogg_plays_whole() {
    // The last-page probe past the real end gets a 416 with the real length, once; the song plays whole.
    let Some(rig) = self::rig(200_000, true, Ends::Broken, None) else { return };
    let real = rig.server.files[0].1.len() as u64;
    let heard = rig.play_a_to_its_end(0);
    assert!(heard >= A_SECS as f64 - 0.1, "all of a heard: {heard} s");
    let asked = rig.server.asked("a");
    let past = asked.iter().filter(|&&f| f >= real).count();
    assert!((1..=2).contains(&past), "past the end asked for once or twice (the end probe, the body's end): {asked:?}");
    rig.engine.stop();

    // The same without the length in the 416: the reader probes again until it finds the last page.
    let Some(rig) = self::rig(200_000, false, Ends::Clean, None) else { return };
    let heard = rig.play_a_to_its_end(0);
    assert!(heard >= A_SECS as f64 - 0.1, "all of a heard: {heard} s");
    rig.engine.stop();
}

/// A seek near the end uses the real length and the song ends there without failing.
#[test]
fn estimated_song_seek_near_end_plays() {
    for says in [true, false] {
        let Some(rig) = self::rig(200_000, says, Ends::Broken, None) else { return };
        let from_ms = A_SECS as i64 * 1000 - 1_500;
        let heard = rig.play_a_to_its_end(from_ms);
        assert!((1.3..=2.0).contains(&heard), "the last second and a half of a, then b (says {says}): {heard} s");
        rig.engine.stop();
    }
}

#[test]
fn early_ends_end_song() {
    // A body ending cleanly short of the promise is the song's end; the short song plays into the next.
    let Some(rig) = self::rig(200_000, false, Ends::Clean, None) else { return };
    let heard = rig.play_to_its_end(1, 0);
    assert!(heard >= B_SECS as f64 - 0.1, "all of b heard: {heard} s");
    let real = rig.server.files[1].1.len() as u64;
    assert!(rig.server.asked("b").iter().all(|&f| f < real), "nothing asked for past the end: {:?}", rig.server.asked("b"));
    rig.engine.stop();

    // The song ends where its bytes do, whatever length the server gives, and the next starts there.
    for off_ms in [2_500, -2_500] {
        let Some(rig) = rig_making(200_000, true, Ends::Broken, None, uncached(A_SECS as i64 * 1000 + off_ms)) else { return };
        let first = rig.first_sound();
        assert!(first < Duration::from_millis(500), "heard at once (off by {off_ms} ms): {first:?}");
        rig.time.run(Duration::from_secs(5));
        let (took, heard) = rig.on_to_b();
        let all = rig.card.secs() - (heard - took).max(0.0);
        assert!((A_SECS as f64 - 0.1..=A_SECS as f64 + 0.5).contains(&rig.card.secs()), "all of a and no more (off by {off_ms} ms): {} s", rig.card.secs());
        assert!((took - heard).abs() < 0.2, "b right after a, no silence between (off by {off_ms} ms): {took} s for {heard} s heard, {all}");
        assert_eq!(rig.engine.status().underruns, 0, "no gap (off by {off_ms} ms)");
        assert!(rig.server.charged().is_empty(), "nothing asked past the start (off by {off_ms} ms): {:?}", rig.server.charged());
        // A seek between the real and the stated end: the next song plays, no failure.
        if off_ms > 0 {
            let heard = rig.play_a_to_its_end(A_SECS as i64 * 1000 + off_ms / 2);
            assert!(heard < 0.5, "nothing of a past its end: {heard} s");
        }
        rig.engine.stop();
    }
}

/// A connection dropped mid-song is resumed, not taken for the end.
#[test]
fn dropped_network_resumes_song() {
    // b is short enough that the break itself is resumed.
    let Some(rig) = self::rig(200_000, true, Ends::Broken, Some(("b", 32_000))) else { return };
    let heard = rig.play_to_its_end(1, 0);
    assert!(heard >= B_SECS as f64 - 0.1, "all of b heard: {heard} s");
    let asked = rig.server.asked("b");
    assert_eq!(asked[..2], [0, 32_000], "asked again where it broke");
    rig.engine.stop();
}

#[test]
fn uncached_transcode() {
    // A fresh transcode is heard at once (no probe past its start) and plays whole to its real end.
    let Some(rig) = rig_making(200_000, true, Ends::Broken, None, uncached(A_SECS as i64 * 1000 + 400)) else { return };
    let first = rig.first_sound();
    eprintln!("the first sound of an uncached transcode after {first:?}");
    assert!(first < Duration::from_millis(500), "heard at once: {first:?}");
    let (_, heard) = rig.on_to_b();
    assert!(heard >= A_SECS as f64 - 0.1, "all of a heard: {heard} s");
    assert!(rig.server.charged().is_empty(), "nothing asked past the start of a transcode being made: {:?}", rig.server.charged());
    rig.engine.stop();

    // Seeks during a transcode read on through the arriving bytes instead of asking for a costly range.
    let Some(rig) = rig_making(200_000, true, Ends::Broken, None, uncached(A_SECS as i64 * 1000 + 400)) else { return };
    rig.engine.position_updates(Some(Duration::from_millis(500)));
    rig.first_sound();
    rig.time.run(Duration::from_secs(2));
    // Already here (~30 s have come).
    let waited = rig.seek_heard(10_000);
    assert!(waited <= 0.6, "heard at once, as soon as the next position is said: {waited} s");
    rig.time.run(Duration::from_secs(1));
    // Not sent yet: heard a few seconds later as the transcode arrives, not after a 20 s range. Position
    // events are off: each would wake an engine waiting for bytes and hold the clock.
    rig.engine.position_updates(None);
    let far = A_SECS as i64 * 1000 - 10_000;
    let (t0, before) = (rig.time.clock.now_ns(), rig.card.secs());
    rig.engine.seek(far);
    assert!(rig.time.until(Duration::from_secs(30), || rig.card.secs() >= before + 1.3), "the far place heard");
    let waited = (rig.time.clock.now_ns() - t0) as f64 / 1e9;
    eprintln!("a seek past what had come heard after {waited} s");
    assert!(waited < 8.0, "heard once the transcode came that far: {waited} s, asked {:?}", rig.server.requests.lock());
    let (_, heard) = rig.on_to_b();
    assert!(heard <= 10.0, "no more than the rest of its last ten seconds: {heard} s");
    assert!(rig.server.charged().is_empty(), "no range asked of a transcode being made: {:?}", rig.server.charged());
    rig.engine.stop();

    // A seek 15 s before the end of an arriving transcode (empty cache) plays on through the next songs.
    let making = Making { cache: true, ..uncached(A_SECS as i64 * 1000 + 400) };
    let Some(rig) = rig_making(200_000, true, Ends::Broken, None, making) else { return };
    rig.first_sound();
    rig.time.run(Duration::from_secs(1));
    rig.engine.seek(A_SECS as i64 * 1000 - 15_000);
    let (_, heard) = rig.on_to_b();
    assert!(heard <= 15.5, "the last fifteen seconds of a: {heard} s");
    let seen = rig.events.lock().len();
    assert!(rig.time.until(Duration::from_secs(20), || rig.heard_song_since(seen, "c") || !rig.errors().is_empty()), "c after b: {:?}", rig.events.lock());
    assert!(rig.errors().is_empty(), "{:?}", rig.errors());
    assert!(rig.server.charged().is_empty(), "nothing asked past the start: {:?}", rig.server.charged());
    rig.engine.stop();
}

/// Without a length: heard at once, played whole, and seeks read on.
#[test]
fn unsized_song_plays_and_seeks() {
    let Some(rig) = rig_making(200_000, true, Ends::Broken, None, uncached(0)) else { return };
    let first = rig.first_sound();
    assert!(first < Duration::from_millis(500), "heard at once: {first:?}");
    let (_, heard) = rig.on_to_b();
    assert!(heard >= A_SECS as f64 - 0.1, "all of a heard: {heard} s");
    let heard = rig.play_a_to_its_end(A_SECS as i64 * 1000 - 5_000);
    assert!((4.8..=5.6).contains(&heard), "the last five seconds: {heard} s");
    assert!(rig.server.charged().is_empty(), "nothing asked past the start: {:?}", rig.server.charged());
    rig.engine.stop();
}

