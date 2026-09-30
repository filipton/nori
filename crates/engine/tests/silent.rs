//! The engine never claims to play while silent: after a panic on its thread, a dead loader, or an
//! output that stops taking music, the song restarts from scratch with an error, and a song that fails
//! twice is skipped. (Regression: a phone once stayed silent while "playing" until restarted.)
//!
//! On the clock the test moves (`common::Virtual`); the songs are tones, each its own pitch, so what the
//! card hears says which song it is.

use crate::common;

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_engine::{Body, ByteSource, Config, Engine, Event, Library, Located, OpenError, SharedQueue, Source};
use nori_player::sim;
use nori_player::transitions::WindowSong;
use parking_lot::Mutex;

const RATE: u32 = 44_100;
/// Seconds of each song: long enough to stand still in for longer than the engine lets it.
const SECS: u32 = 40;

/// A tone as a WAV file, made once per pitch.
fn wav(hz: f64) -> Arc<Vec<u8>> {
    static MADE: Mutex<Vec<(u64, Arc<Vec<u8>>)>> = Mutex::new(Vec::new());
    if let Some((_, w)) = MADE.lock().iter().find(|(h, _)| *h == hz.to_bits()) {
        return w.clone();
    }
    let w = Arc::new(common::wav(RATE, &common::sine(RATE, hz, SECS as f64, 12_000.0)));
    MADE.lock().push((hz.to_bits(), w.clone()));
    w
}

/// The songs, by id, and each one's pitch.
fn hz(k: usize) -> f64 {
    300.0 + 100.0 * k as f64
}

/// What goes wrong, and how often: song ids that make the engine's thread panic as they are located, and
/// ids whose bytes make the loader's thread panic as they are read.
#[derive(Default)]
struct Trouble {
    panics_on_locate: HashMap<String, u32>,
    panics_on_read: HashMap<String, u32>,
    /// The songs the engine let go of from scratch ([`Library::forget`]).
    forgotten: Vec<String>,
}

struct Net {
    files: Arc<Vec<(String, Arc<Vec<u8>>)>>,
    trouble: Arc<Mutex<Trouble>>,
}

/// A song's bytes from where they were asked for.
struct Bytes(Arc<Vec<u8>>, usize);

impl Read for Bytes {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(self.0.len().saturating_sub(self.1));
        buf[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
        self.1 += n;
        Ok(n)
    }
}

/// A body whose read panics: a loader's thread that dies with the song half fetched.
struct Boom;

impl Read for Boom {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("the platform's body panicked as it was read");
    }
}

impl ByteSource for Net {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        let file = self.files.iter().find(|(u, _)| u == url).map(|(_, f)| f.clone()).ok_or("404")?;
        let len = file.len() as u64;
        let boom = {
            let mut t = self.trouble.lock();
            match t.panics_on_read.get_mut(url) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    true
                }
                _ => false,
            }
        };
        if boom {
            return Ok(Body { start: from, len: Some(len), reader: Box::new(Boom) });
        }
        Ok(Body { start: from, len: Some(len), reader: Box::new(Bytes(file, from as usize)) })
    }
}

struct Songs {
    net: Arc<Net>,
    trouble: Arc<Mutex<Trouble>>,
}

impl Library for Songs {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let panics = {
            let mut t = self.trouble.lock();
            match t.panics_on_locate.get_mut(id) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    true
                }
                _ => false,
            }
        };
        if panics {
            panic!("the library panicked locating {id}");
        }
        let bytes: Arc<dyn ByteSource> = self.net.clone();
        Ok(Located { source: Source::Url { url: id.to_string(), bytes }, hint: Some("wav".into()), duration_ms: Some(SECS as i64 * 1000), estimated: false })
    }

    fn about(&self, id: &str) -> WindowSong {
        WindowSong { id: id.into(), title: id.into(), duration_ms: SECS as i64 * 1000, ..Default::default() }
    }

    fn forget(&mut self, id: &str) {
        self.trouble.lock().forgotten.push(id.to_string());
    }
}

struct Rig {
    engine: Engine,
    time: Stepper<Pull>,
    card: Card,
    events: Arc<Mutex<Vec<Event>>>,
    trouble: Arc<Mutex<Trouble>>,
    ids: Vec<String>,
}

fn rig(songs: usize) -> Rig {
    let clock = Virtual::default();
    let ids: Vec<String> = (0..songs).map(|k| format!("s{k}")).collect();
    let files = Arc::new(ids.iter().enumerate().map(|(k, id)| (id.clone(), wav(hz(k)))).collect::<Vec<_>>());
    let trouble = Arc::new(Mutex::new(Trouble::default()));
    let net = Arc::new(Net { files, trouble: trouble.clone() });
    let queue = SharedQueue::default();
    queue.0.lock().set(ids.clone(), Some(0), false, 0);
    let card = Card::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut app = sim::App::new();
    app.prefs = sim::prefs_off();
    let config = Config { memory_mb: 256, ..Config::default() };
    let engine = Engine::start_on(Songs { net, trouble: trouble.clone() }, app, queue, Box::new(card.clone()), None, config, clock.clone(), { let mut t = common::golden::Trace::new(&clock); let h = card.heard.clone(); t.watch(move || common::golden::floats("heard", &h.lock())); let o = card.opened.clone(); t.watch(move || format!("opened {:?}", o.lock())); t.around(move |e| seen.lock().push(e)) });
    engine.queue_changed();
    Rig { engine, time: Stepper::new(clock, card.pull.clone()), card, events, trouble, ids }
}

impl Rig {
    /// The card heard a second of song `k` (by its pitch).
    fn hears(&self, k: usize) -> bool {
        self.card.secs() >= 1.0 && (self.card.last_second_hz() - hz(k)).abs() < 3.0
    }

    fn wait_to_hear(&self, k: usize, limit: Duration) -> bool {
        self.card.heard.lock().clear();
        self.time.until(limit, || self.hears(k))
    }

    fn errors(&self) -> Vec<(String, String)> {
        self.events.lock().iter().filter_map(|e| if let Event::Error { id, message } = e { Some((id.clone(), message.clone())) } else { None }).collect()
    }

    fn state(&self) -> String {
        format!("{:.2} s heard; opened {} times; forgotten {:?}; last events {:?}", self.card.secs(), self.card.opened.lock().len(), self.trouble.lock().forgotten, self.events.lock().iter().rev().filter(|e| !matches!(e, Event::Position { .. })).take(10).collect::<Vec<_>>())
    }
}

#[test]
fn panic_restarts_song() {
    let r = rig(3);
    r.engine.play_at(0, 0);
    assert!(r.wait_to_hear(0, Duration::from_secs(5)), "the first song plays: {}", r.state());
    r.trouble.lock().panics_on_locate.insert(r.ids[1].clone(), 1);
    r.engine.play_at(1, 0);
    assert!(r.wait_to_hear(1, Duration::from_secs(5)), "the song the engine panicked on plays, opened again: {}", r.state());
    let errors = r.errors();
    assert!(errors.iter().any(|(id, m)| id == &r.ids[1] && m.contains("panicked") && m.contains("locating s1")), "the panic is said, with the song: {errors:?}");
    assert!(r.trouble.lock().forgotten.contains(&r.ids[1]), "and what the disk kept of it went: {}", r.state());
    r.engine.next();
    assert!(r.wait_to_hear(2, Duration::from_secs(5)), "the engine still takes commands: {}", r.state());
}

#[test]
fn repeated_panic_skips_song() {
    let r = rig(3);
    r.engine.play_at(0, 0);
    assert!(r.wait_to_hear(0, Duration::from_secs(5)), "the first song plays: {}", r.state());
    r.trouble.lock().panics_on_locate.insert(r.ids[1].clone(), 2);
    r.engine.play_at(1, 0);
    assert!(r.wait_to_hear(2, Duration::from_secs(5)), "the song after the one that panics plays: {}", r.state());
    assert!(r.events.lock().iter().any(|e| matches!(e, Event::Song { id, .. } if id == &r.ids[2])), "and is said: {}", r.state());
}

#[test]
fn loader_panic_fails_song() {
    let r = rig(3);
    r.trouble.lock().panics_on_read.insert(r.ids[1].clone(), 10);
    r.engine.play_at(1, 0);
    assert!(r.wait_to_hear(2, Duration::from_secs(10)), "the song after the one whose loader died plays: {}", r.state());
    let errors = r.errors();
    assert!(errors.iter().any(|(id, _)| id == &r.ids[1]), "the song's failure is said: {errors:?}");
}

#[test]
fn silent_output_restarts_song() {
    let r = rig(2);
    r.engine.play_at(0, 0);
    assert!(r.wait_to_hear(0, Duration::from_secs(5)), "the song plays: {}", r.state());
    r.time.run(Duration::from_secs(3));
    let opened = r.card.opened.lock().len();
    r.card.pull.lock().pause_pulling();
    r.time.run(Duration::from_secs(8));
    assert_eq!(r.card.opened.lock().len(), opened, "not before it has stood still a while: {}", r.state());
    assert!(r.time.until(Duration::from_secs(40), || r.card.opened.lock().len() > opened), "the output is opened again: {}", r.state());
    assert!(r.wait_to_hear(0, Duration::from_secs(5)), "and the same song plays on: {}", r.state());
    let errors = r.errors();
    assert!(errors.iter().any(|(id, m)| id == &r.ids[0] && m.contains("no music")), "said: {errors:?}");
}

#[test]
fn paused_is_not_a_stall() {
    let r = rig(2);
    r.engine.play_at(0, 0);
    assert!(r.wait_to_hear(0, Duration::from_secs(5)), "the song plays: {}", r.state());
    r.engine.pause();
    let opened = r.card.opened.lock().len();
    r.time.run(Duration::from_secs(60));
    assert_eq!(r.card.opened.lock().len(), opened, "paused, nothing is made again: {}", r.state());
    assert!(r.errors().is_empty(), "and nothing said: {:?}", r.errors());
}
