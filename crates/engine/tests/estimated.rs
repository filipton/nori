//! A transcoding server that promises an estimated length (Navidrome's `estimateContentLength`) longer
//! than what it sends, and answers a range from past the real end with a 416: the song plays whole, its
//! length and seeks are the real ones, and neither the answer past the end nor a clean end short of the
//! promise counts as a failure. A network that drops in the middle still does, and is asked again.
//!
//! The Ogg Opus songs are made by ffmpeg on the machine running the tests; without it the tests say so
//! and pass. The engine runs on a clock the test moves (`common::Virtual`).

use crate::common;

use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_engine::{Body, ByteSource, Config, Engine, Event, Library, Located, OpenError, Recent, SharedQueue, Source, State, Store};
use nori_player::sim;
use nori_player::transitions::WindowSong;
use parking_lot::Mutex;

use common::ffmpeg;

/// `a`, `b` and `c`, made once for every test in the binary: None without ffmpeg.
fn songs() -> Option<&'static [Arc<Vec<u8>>; 3]> {
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

/// A tone of `secs` as Ogg Opus at 192 kbps, as the server transcodes for a phone on mobile data.
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
    /// Cleanly, where the real bytes do.
    Clean,
    /// With an error there: a body shorter than its Content-Length, as OkHttp reads it.
    Broken,
}

/// The transcoding server. Every answer promises `extra` bytes more than there are; a range from the
/// real end on is a 416, saying the real length when `says`. A song's first body sends its first
/// `hold` bytes at once and the rest only a moment later, so the container reader looks for the end
/// while the song is still on its way; `cut`: the named song's first body breaks off with an error at
/// that byte, once.
///
/// A song's transcode is made as it is first sent: a range from anywhere but its start, asked before
/// the first body has sent it all, makes the server transcode the whole song first (Navidrome skips
/// through it from the start), which takes `charge` of the test's time. With `arrives`, a body from the
/// start sends its first bytes at once and the rest at so many bytes a second of the test's clock: the
/// transcode coming out, faster than the song plays.
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
    /// Each file's transcode is whole on the server: its ranges are answered at once.
    made: Vec<Arc<AtomicBool>>,
    /// The ranges that had to wait for a whole transcode to be made.
    charged: Mutex<Vec<(String, u64)>>,
    /// Each file has been asked for from past its start (the reader looking for its last page).
    probed: Vec<Arc<AtomicBool>>,
}

struct Sent {
    file: Arc<Vec<u8>>,
    at: usize,
    hold: Option<usize>,
    /// Ends the hold early: the file was asked for from further on meanwhile.
    probed: Arc<AtomicBool>,
    cut: Option<usize>,
    ends: Ends,
    /// The first bytes at once, the rest at so many a second from the time given, on the test's clock.
    arrives: Option<(usize, u64, i64, Virtual)>,
    /// Set once the whole transcode has been sent.
    made: Arc<AtomicBool>,
}

/// Longest the rest of a held body waits, in real time, for the reader to look past it.
const HOLD: Duration = Duration::from_millis(300);

impl Read for Sent {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.hold.is_some_and(|h| self.at >= h) {
            // The rest comes a moment later, in real time: the engine's clock stands still meanwhile. The
            // moment is over once the reader has looked further on (what the hold is there to make it do
            // while the song is still on its way), or after HOLD for a reader that never does.
            let held = Instant::now();
            while !self.probed.load(Ordering::Acquire) && held.elapsed() < HOLD {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.hold = None;
        }
        // As far as the transcode has come, waiting for the next byte of it.
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
            // Nothing past the start is sent before the whole song has been transcoded.
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
        let sent = Sent { file, at: from as usize, hold, probed, cut, ends: self.ends, arrives, made };
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

/// The songs, each with its length as the server's tags say it: 0 when they say none.
/// Kept in `2`'s stream cache as they come, when there is one.
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

/// Seconds of song `a`, and of `b` and `c` after it. `a` is long enough that an Ogg reader's look for its
/// last page lands further past what has come than the loader reads on to: the fetch starts again
/// there, as it did on a phone for a 6 MB transcode. `b` is short: the look waits for the bytes.
const A_SECS: u32 = 60;
const B_SECS: u32 = 5;

/// `a`, `b` and `c` from a [`Transcoder`], each promised `extra` bytes too many (well past one Ogg page, so an
/// Ogg reader looking for the last page looks past the real end). None without ffmpeg.
fn rig(extra: u64, says: bool, ends: Ends, cut: Option<(&str, usize)>) -> Option<Rig> {
    rig_making(extra, says, ends, cut, Making::default())
}

/// How a [`Transcoder`] makes its songs, and the length the server's tags give `a`.
#[derive(Clone, Copy)]
struct Making {
    charge: Duration,
    arrives: Option<(usize, u64)>,
    hold: usize,
    a_ms: i64,
    /// The songs go through a stream cache, empty at first.
    cache: bool,
}

impl Default for Making {
    /// Ranges answered at once, a song's first 48 kB sent at once and the rest a moment later, and `a`
    /// a fraction of a second longer than it is: the server's tags round the length, as a library's do.
    fn default() -> Making {
        Making { charge: Duration::ZERO, arrives: None, hold: 48 * 1024, a_ms: A_SECS as i64 * 1000 + 400, cache: false }
    }
}

/// A transcode the server has not made yet: some twelve seconds of `a` sent at once, the rest eight times
/// as fast as it plays (all of it in some seven seconds), and a range past the start of it twenty
/// seconds' work. The server gives `a` `a_ms`.
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

    /// Whether `id` was heard since the `from`th event.
    fn heard_song_since(&self, from: usize, id: &str) -> bool {
        self.events.lock().iter().skip(from).any(|e| matches!(e, Event::Song { id: i, .. } if i == id))
    }

    /// Starts `a` from its start: the time, on the test's clock, until the first of it is heard.
    fn first_sound(&self) -> Duration {
        let t0 = self.time.clock.now_ns();
        self.engine.play_at(0, 0);
        assert!(self.time.until(Duration::from_secs(60), || !self.card.heard.lock().is_empty() || !self.errors().is_empty()), "a heard");
        assert!(self.errors().is_empty(), "a opened: {:?}", self.errors());
        Duration::from_nanos((self.time.clock.now_ns() - t0) as u64)
    }

    /// Seeks `a` to `ms`: the time, on the test's clock, until the ear is past that place.
    fn seek_heard(&self, ms: i64) -> f64 {
        let (t0, seen) = (self.time.clock.now_ns(), self.events.lock().len());
        self.engine.seek(ms);
        let past = |e: &Event| matches!(e, Event::Position { index: 0, ms: at } if (ms + 50..ms + 1_500).contains(at));
        assert!(self.time.until(Duration::from_secs(30), || self.events.lock().iter().skip(seen).any(past)), "heard from {ms} ms: {:?}", self.events.lock());
        (self.time.clock.now_ns() - t0) as f64 / 1e9
    }

    /// Plays on until `b` is heard: the time that took on the test's clock, and the seconds heard.
    fn on_to_b(&self) -> (f64, f64) {
        let (t0, before, seen) = (self.time.clock.now_ns(), self.card.secs(), self.events.lock().len());
        let limit = Duration::from_secs(A_SECS as u64 + 60);
        assert!(self.time.until(limit, || self.heard_song_since(seen, "b") || !self.errors().is_empty()), "b came: {:?}", self.events.lock());
        assert!(self.errors().is_empty(), "a played without a failure: {:?} asked {:?}", self.errors(), self.server.asked("a"));
        ((self.time.clock.now_ns() - t0) as f64 / 1e9, self.card.secs() - before)
    }

    /// Plays `a` from `from_ms` until `b` is heard: the seconds of `a` the card heard.
    fn play_a_to_its_end(&self, from_ms: i64) -> f64 {
        self.play_to_its_end(0, from_ms)
    }

    /// Plays the song at queue `index` from `from_ms` until the next is heard: the seconds of it heard.
    fn play_to_its_end(&self, index: usize, from_ms: i64) -> f64 {
        let (id, next) = (["a", "b"][index], ["b", "c"][index]);
        let (before, seen) = (self.card.secs(), self.events.lock().len());
        self.engine.play_at(index, from_ms);
        let limit = Duration::from_secs(A_SECS as u64 + 30);
        assert!(self.time.until(limit, || self.heard_song_since(seen, next) || !self.errors().is_empty()), "{next} came: {:?}", self.events.lock());
        assert!(self.errors().is_empty(), "{id} played without a failure: {:?} asked {:?}", self.errors(), self.server.asked(id));
        let s = self.engine.status();
        assert!(s.index == Some(index + 1) && s.state == State::Playing, "{next} plays after {id}: {s:?}");
        // What was heard up to the next song's first sound: this one's seconds, and a little of the next.
        let heard = self.card.secs();
        // The card is opened again for a song of another shape, and hears from nothing then.
        if heard >= before { heard - before } else { heard }
    }
}

/// The Ogg reader looks for the song's last page (for its length) where the promised length puts it,
/// past the real end; the server says the real length in its 416. The song plays whole, and the 416 is
/// asked for once, not tried again and again until the song counts as failed.
#[test]
fn estimated_ogg_plays_whole_with_real_length() {
    let Some(rig) = rig(200_000, true, Ends::Broken, None) else { return };
    let real = rig.server.files[0].1.len() as u64;
    let heard = rig.play_a_to_its_end(0);
    assert!(heard >= A_SECS as f64 - 0.1, "all of a heard: {heard} s");
    let asked = rig.server.asked("a");
    let past = asked.iter().filter(|&&f| f >= real).count();
    assert!((1..=2).contains(&past), "past the end asked for once or twice (the end probe, the body's end): {asked:?}");
    rig.engine.stop();
}

/// The same without the server saying the real length: each look past the end learns only that it is
/// sooner, and the reader looks again until it finds the last page.
#[test]
fn estimated_ogg_plays_whole_without_real_length() {
    let Some(rig) = rig(200_000, false, Ends::Clean, None) else { return };
    let heard = rig.play_a_to_its_end(0);
    assert!(heard >= A_SECS as f64 - 0.1, "all of a heard: {heard} s");
    rig.engine.stop();
}

/// Played from near its end: the seek finds its place with the real length, and the song ends there
/// rather than failing (a seek bar that took the estimate would put the place past the real end).
#[test]
fn estimated_song_seek_near_end_plays() {
    for says in [true, false] {
        let Some(rig) = rig(200_000, says, Ends::Broken, None) else { return };
        let from_ms = A_SECS as i64 * 1000 - 1_500;
        let heard = rig.play_a_to_its_end(from_ms);
        assert!((1.3..=2.0).contains(&heard), "the last second and a half of a, then b (says {says}): {heard} s");
        rig.engine.stop();
    }
}

/// A body that ends cleanly short of the promised length: that is where the song ends. A short song's
/// look for its last page waits for the bytes to come rather than asking past the end, so it is the clean
/// end that tells the real length; the song plays whole into the next.
#[test]
fn clean_early_end_ends_song() {
    let Some(rig) = rig(200_000, false, Ends::Clean, None) else { return };
    let heard = rig.play_to_its_end(1, 0);
    assert!(heard >= B_SECS as f64 - 0.1, "all of b heard: {heard} s");
    let real = rig.server.files[1].1.len() as u64;
    assert!(rig.server.asked("b").iter().all(|&f| f < real), "nothing asked for past the end: {:?}", rig.server.asked("b"));
    rig.engine.stop();
}

/// A network that drops in the middle of the song is not taken for its end: the bytes are asked for
/// again from where they stopped, and the song plays whole.
#[test]
fn dropped_network_resumes_song() {
    // b: short enough that nothing jumps ahead of the break, so it is the break that is asked again.
    let Some(rig) = rig(200_000, true, Ends::Broken, Some(("b", 32_000))) else { return };
    let heard = rig.play_to_its_end(1, 0);
    assert!(heard >= B_SECS as f64 - 0.1, "all of b heard: {heard} s");
    let asked = rig.server.asked("b");
    assert_eq!(asked[..2], [0, 32_000], "asked again where it broke");
    rig.engine.stop();
}

/// The first play of a song the server has not transcoded yet: nothing past its start is asked for (an
/// Ogg reader looking for the last page, for the song's length, made the server transcode the whole song
/// before it answered), so it is heard at once. It plays whole, ended by the real end of its bytes.
#[test]
fn uncached_transcode_starts_at_once() {
    let Some(rig) = rig_making(200_000, true, Ends::Broken, None, uncached(A_SECS as i64 * 1000 + 400)) else { return };
    let first = rig.first_sound();
    eprintln!("the first sound of an uncached transcode after {first:?}");
    assert!(first < Duration::from_millis(500), "heard at once: {first:?}");
    let (_, heard) = rig.on_to_b();
    assert!(heard >= A_SECS as f64 - 0.1, "all of a heard: {heard} s");
    assert!(rig.server.charged().is_empty(), "nothing asked past the start of a transcode being made: {:?}", rig.server.charged());
    rig.engine.stop();
}

/// Seeks while the transcode is still coming: to a place already here, and to one not sent yet. Each is
/// read on to in the bytes coming rather than asked of the server as a range it would transcode the
/// whole song for, and plays from its place to the song's end.
#[test]
fn uncached_transcode_seeks_read_on() {
    let Some(rig) = rig_making(200_000, true, Ends::Broken, None, uncached(A_SECS as i64 * 1000 + 400)) else { return };
    rig.engine.position_updates(Some(Duration::from_millis(500)));
    rig.first_sound();
    rig.time.run(Duration::from_secs(2));
    // Here already: the transcode has come some thirty seconds by now.
    let waited = rig.seek_heard(10_000);
    assert!(waited <= 0.6, "heard at once, as soon as the next position is said: {waited} s");
    rig.time.run(Duration::from_secs(1));
    // Not sent yet (some forty seconds have come): heard once the transcode has come that far and a
    // little past it (`source::READY`), a few seconds on - and not the twenty a range would cost.
    // No positions said meanwhile: each wakes an engine waiting for bytes, which holds the test's clock.
    rig.engine.position_updates(None);
    let far = A_SECS as i64 * 1000 - 10_000;
    let (t0, before) = (rig.time.clock.now_ns(), rig.card.secs());
    rig.engine.seek(far);
    // Heard: a second of it past what the seek's dip let play out.
    assert!(rig.time.until(Duration::from_secs(30), || rig.card.secs() >= before + 1.3), "the far place heard");
    let waited = (rig.time.clock.now_ns() - t0) as f64 / 1e9;
    eprintln!("a seek past what had come heard after {waited} s");
    assert!(waited < 8.0, "heard once the transcode came that far: {waited} s, asked {:?}", rig.server.requests.lock());
    let (_, heard) = rig.on_to_b();
    assert!(heard <= 10.0, "no more than the rest of its last ten seconds: {heard} s");
    assert!(rig.server.charged().is_empty(), "no range asked of a transcode being made: {:?}", rig.server.charged());
    rig.engine.stop();
}

/// The server's length is a little longer, or shorter, than the audio: the song ends where its bytes do,
/// neither cut short nor followed by silence, and the next one starts right there.
#[test]
fn bytes_end_ends_song() {
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
        // A seek between the real end and the one the server gives: the song is over there, and the next
        // one plays, rather than a failure.
        if off_ms > 0 {
            let heard = rig.play_a_to_its_end(A_SECS as i64 * 1000 + off_ms / 2);
            assert!(heard < 0.5, "nothing of a past its end: {heard} s");
        }
        rig.engine.stop();
    }
}

/// A server that gives the song no length: it is not looked for at the song's end either, and the song
/// is of no known length until its bytes end. It is heard at once and plays whole, and a seek still
/// reads on to its place.
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

/// A seek to fifteen seconds before the end of a transcode still coming, into a stream cache that is
/// empty, and on through the songs after it: each plays in turn, the next one's bytes coming as they
/// should, rather than the player standing at the end of the song.
#[test]
fn uncached_transcode_seek_near_end_plays_on() {
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
