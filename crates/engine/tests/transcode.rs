//! Opus transcodes from a server that behaves as Navidrome does, over the test's own core: songs play
//! to their real end.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_core::settings::SavedQuality;
use nori_core::Song;
use nori_engine::core::{settings, Analyses, CoreApp, CoreLibrary, CoreQueue};
use nori_engine::{Body, ByteSource, Config, Engine, Event, OpenError, Recent, Store};
use parking_lot::Mutex;

use crate::common;
use crate::estimated::{songs, A_SECS};

/// Go's net/http refuses a write crossing the promised length whole; io.Copy writes this much at a time.
const COPY: usize = 32 * 1024;

/// Navidrome. The first request for a transcode transcodes as it sends: no ranges, and with
/// `estimateContentLength` a promised length of `estimate` times the real one, cut at the write that
/// crosses it, or ending short of it with a dropped connection. Later requests come from its transcode
/// cache: the exact length, ranges served.
struct Navidrome {
    songs: HashMap<String, Arc<Vec<u8>>>,
    estimate: f64,
    made: Mutex<HashSet<String>>,
}

/// Bytes `at..stop` of a song, then a clean end or a dropped connection.
struct Sent {
    song: Arc<Vec<u8>>,
    at: usize,
    stop: usize,
    dropped: bool,
}

impl Read for Sent {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at == self.stop {
            return if self.dropped { Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected end of stream")) } else { Ok(0) };
        }
        let n = buf.len().min(self.stop - self.at).min(COPY);
        buf[..n].copy_from_slice(&self.song[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

fn id_of(url: &str) -> &str {
    url.split("&id=").nth(1).unwrap_or("").split('&').next().unwrap_or("")
}

impl ByteSource for Navidrome {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        let id = id_of(url);
        let song = self.songs.get(id).ok_or("404")?.clone();
        let real = song.len();
        if !self.made.lock().insert(id.to_string()) {
            if from >= real as u64 {
                return Err(OpenError::PastEnd { len: Some(real as u64) });
            }
            return Ok(Body { start: from, len: Some(real as u64), reader: Box::new(Sent { song, at: from as usize, stop: real, dropped: false }) });
        }
        let (len, stop, dropped) = if url.contains("estimateContentLength=true") {
            let promised = (real as f64 * self.estimate) as usize;
            (Some(promised as u64), if real > promised { promised / COPY * COPY } else { real }, true)
        } else {
            (None, real, false)
        };
        Ok(Body { start: 0, len, reader: Box::new(Sent { song, at: 0, stop, dropped }) })
    }
}

struct Rig {
    engine: Engine,
    time: Stepper<Pull>,
    card: Card,
    events: Arc<Mutex<Vec<Event>>>,
    store: Option<Arc<Store>>,
    _dir: nori_testdir::TempDir,
}

impl Rig {
    /// `a` then `b` streamed as 192 kbps Opus through `bytes`, kept in a stream cache with `store`.
    fn new(bytes: Arc<dyn ByteSource>, store: bool) -> Rig {
        let dir = nori_testdir::TempDir::new("transcode");
        let (core, client) = common::own_core(&dir, |p| p.wifi = SavedQuality { bit_rate: 192, format: "opus".into() });
        let prefs = core.session.settings.current().unwrap();
        let store = store.then(|| Store::open(dir.join("music"), 256 << 20, Box::new(Recent::default())).unwrap());
        core.session.register(["a", "b"].map(|id| Song { id: id.into(), title: id.into(), duration: if id == "a" { A_SECS } else { 5 }, suffix: "mp3".into(), ..Default::default() }).to_vec());
        core.session.set(vec!["a".into(), "b".into()], Some(0), false, None);
        let library = CoreLibrary { analyses: Analyses::of(client.clone()), client, bytes, metered: false, store: store.clone() };
        let (card, clock) = (Card::new(), Virtual::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let config = Config { memory_mb: 128, settings: settings(&prefs, 0.0), ..Config::default() };
        let engine = Engine::start_on(library, CoreApp::new(core.session.clone()), CoreQueue(core.session.clone()), Box::new(card.clone()), None, config, clock.clone(), move |e| seen.lock().push(e));
        engine.queue_changed();
        Rig { engine, time: Stepper::new(clock, card.pull.clone()), card, events, store, _dir: dir }
    }

    /// Plays `a` from `from_ms` until `b` is heard; returns the seconds of `a`'s 440 Hz heard.
    fn play_a(&self, from_ms: i64) -> f64 {
        let (before, seen) = (self.card.heard.lock().len(), self.events.lock().len());
        self.engine.play_at(0, from_ms);
        let b = |e: &Event| matches!(e, Event::Song { id, .. } if id == "b");
        assert!(self.time.until(Duration::from_secs(A_SECS as u64 + 30), || self.events.lock().iter().skip(seen).any(b)), "b came: {:?}", self.events.lock());
        let failed: Vec<Event> = self.events.lock().iter().filter(|e| matches!(e, Event::Error { .. } | Event::Stopped { .. })).cloned().collect();
        assert!(failed.is_empty(), "{failed:?}");
        let rate = self.card.format().expect("opened").rate as usize;
        let left: Vec<f32> = self.card.heard.lock()[before..].chunks_exact(2).map(|f| f[0]).collect();
        // 440 Hz crosses zero 88 times a tenth of a second.
        let tenths = left.chunks_exact(rate / 10).filter(|w| w.windows(2).filter(|p| (p[0] < 0.0) != (p[1] < 0.0)).count().abs_diff(88) <= 4).count();
        tenths as f64 / 10.0
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.stop();
    }
}

fn navidrome(estimate: f64) -> Option<Arc<Navidrome>> {
    let [a, b, _] = songs()?;
    Some(Arc::new(Navidrome { songs: HashMap::from([("a".into(), a.clone()), ("b".into(), b.clone())]), estimate, made: Mutex::default() }))
}

#[test]
fn transcode_plays_to_its_end() {
    for estimate in [0.9, 1.1] {
        let Some(net) = navidrome(estimate) else { return };
        let rig = Rig::new(net.clone(), true);
        let heard = rig.play_a(0);
        assert!(heard >= A_SECS as f64 - 0.1, "all of a (estimate {estimate}): {heard} s");
        let kept = std::fs::metadata(rig.store.as_ref().unwrap().peek("a:192opus").expect("a cached")).unwrap().len();
        assert_eq!(kept, net.songs["a"].len() as u64, "the whole of a cached (estimate {estimate})");
        assert!(rig.play_a(0) >= A_SECS as f64 - 0.1, "all of a again, from the cache (estimate {estimate})");
    }
}

#[test]
fn seek_ahead_of_a_transcode_lands() {
    let Some(net) = navidrome(1.0) else { return };
    let rig = Rig::new(net, false);
    let heard = rig.play_a(A_SECS as i64 * 1000 - 10_000);
    assert!((9.9..=10.5).contains(&heard), "the last ten seconds of a: {heard} s");
}
