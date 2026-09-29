//! The engine never says it plays while nothing can be heard (the S22's classical playlist, 2026-09-26:
//! the player said it played, no output was open, no song was being fetched, and nothing was said; every
//! song after it was silent until the app was started again). Whatever leaves it so - a panic on its own
//! thread, a song's loader that died, an output that stopped taking music - the music is made again from
//! scratch, said as an error, and a song that does it twice is skipped as one that would not play.
//!
//! On the clock the test moves (`common::Virtual`); the songs are tones, each its own pitch, so what the
//! card hears says which song it is.

mod common;

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

/// A tone as a WAV file, 16-bit stereo.
fn wav(hz: f64) -> Arc<Vec<u8>> {
    static MADE: Mutex<Vec<(u64, Arc<Vec<u8>>)>> = Mutex::new(Vec::new());
    if let Some((_, w)) = MADE.lock().iter().find(|(h, _)| *h == hz.to_bits()) {
        return w.clone();
    }
    let frames = (SECS * RATE) as usize;
    let data = frames as u32 * 4;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 4).to_le_bytes());
    w.extend_from_slice(&4u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for i in 0..frames {
        let v = ((std::f64::consts::TAU * hz * i as f64 / RATE as f64).sin() * 12_000.0) as i16;
        w.extend_from_slice(&v.to_le_bytes());
        w.extend_from_slice(&v.to_le_bytes());
    }
    let w = Arc::new(w);
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
    let engine = Engine::start_on(Songs { net, trouble: trouble.clone() }, app, queue, Box::new(card.clone()), None, config, clock.clone(), move |e| seen.lock().push(e));
    engine.queue_changed();
    Rig { engine, time: Stepper::new(clock, card.pull.clone()), card, events, trouble, ids }
}

impl Rig {
    /// Song `k` is what the card hears now: a second of it, at its pitch.
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

/// The engine's own thread panics as a song is opened: it is said (an error naming the song and the
/// panic), the song is let go of from scratch - its bytes and what the disk keeps of it - and opened
/// again, and it plays. The engine goes on taking commands after.
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

/// A song the engine panics on every time it is opened is skipped as one that would not play, and the
/// song after it plays: never a player that says it plays and makes no sound.
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

/// The loader's thread panics with the song half fetched: the song fails, as one whose bytes stopped
/// coming (said, skipped), rather than waiting for bytes for ever.
#[test]
fn loader_panic_fails_song() {
    let r = rig(3);
    r.trouble.lock().panics_on_read.insert(r.ids[1].clone(), 10);
    r.engine.play_at(1, 0);
    assert!(r.wait_to_hear(2, Duration::from_secs(10)), "the song after the one whose loader died plays: {}", r.state());
    let errors = r.errors();
    assert!(errors.iter().any(|(id, _)| id == &r.ids[1]), "the song's failure is said: {errors:?}");
}

/// The output stops taking music while the engine plays (a device gone quiet): after a while standing
/// still with nothing on its way, the music is made again from scratch where the ear was, on a new output,
/// and the song plays on from there. Said as an error.
#[test]
fn silent_output_restarts_song() {
    let r = rig(2);
    r.engine.play_at(0, 0);
    assert!(r.wait_to_hear(0, Duration::from_secs(5)), "the song plays: {}", r.state());
    r.time.run(Duration::from_secs(3));
    let opened = r.card.opened.lock().len();
    // The device stops pulling, as a dead track would.
    r.card.pull.lock().pause_pulling();
    r.time.run(Duration::from_secs(8));
    assert_eq!(r.card.opened.lock().len(), opened, "not before it has stood still a while: {}", r.state());
    assert!(r.time.until(Duration::from_secs(40), || r.card.opened.lock().len() > opened), "the output is opened again: {}", r.state());
    assert!(r.wait_to_hear(0, Duration::from_secs(5)), "and the same song plays on: {}", r.state());
    let errors = r.errors();
    assert!(errors.iter().any(|(id, m)| id == &r.ids[0] && m.contains("no music")), "said: {errors:?}");
}

/// Paused, the music standing still is no fault: nothing is made again, nothing said.
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
