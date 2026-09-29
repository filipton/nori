//! A server where some songs never come: octo-fiesta asked for a provider's song it cannot fetch (its
//! token gone) answers nothing, or its headers and then nothing. The user skips through such an album
//! and goes on to songs that play: they must play. The platform's HTTP client has only so many requests
//! to one server at once (OkHttp's dispatcher, a connection's HTTP/2 streams), modelled here as slots: a
//! request that hangs holds its slot until it is called off, so a player that leaves its hung requests
//! running runs out of them and nothing plays after that - what a phone did.
//!
//! The engine runs on a clock the test moves (`common::Virtual`); a request that hangs lets that time
//! move (`Virtual::hang_while`), as a server waiting on the clock does.

mod common;

use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_engine::{Body, ByteSource, Cancel, Config, Engine, Event, Library, Located, OpenError, SharedQueue, Source};
use nori_player::sim;
use nori_player::transitions::WindowSong;
use parking_lot::Mutex;

const RATE: u32 = 44_100;
/// Seconds of each song.
const SECS: u32 = 6;
/// Requests the platform lets run to the server at once.
const SLOTS: usize = 6;
/// Longest a hung request waits, in real time, for anyone to call it off: far past any test.
const FOREVER: Duration = Duration::from_secs(300);

/// A tone as a WAV file, 16-bit stereo.
fn wav(hz: f64) -> Arc<Vec<u8>> {
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
    Arc::new(w)
}

/// How a song's request hangs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hang {
    /// No answer at all: the headers never come.
    Headers,
    /// The headers, a length, and then not a byte.
    Body,
}

/// The server: the songs that play, the ones that hang and how, and the slots its requests take.
struct Server {
    clock: Virtual,
    files: Vec<(String, Arc<Vec<u8>>)>,
    hang: Hang,
    /// Requests running now: each holds a slot until its answer is let go or it is called off.
    running: Mutex<usize>,
    /// Requests that hung, and the ones called off.
    hung: AtomicU64,
    called_off: AtomicU64,
}

impl Server {
    fn hangs(url: &str) -> bool {
        url.starts_with("ext-")
    }

    fn running(&self) -> usize {
        *self.running.lock()
    }

    /// Waits for a slot, as the platform queues a request behind the ones running: false when none came
    /// before the request was called off.
    fn slot(self: &Arc<Self>, call: &Call) -> bool {
        let me = self.clone();
        let c = call.clone();
        self.clock.hang_while(move || *me.running.lock() >= SLOTS && !c.off(), FOREVER);
        let mut running = self.running.lock();
        if call.off() || *running >= SLOTS {
            return false;
        }
        *running += 1;
        true
    }

    /// Hangs until the request is called off: as OkHttp's call, cancelled, fails its wait at once.
    fn hang(&self, call: &Call) {
        self.hung.fetch_add(1, Ordering::Relaxed);
        let c = call.clone();
        self.clock.hang_while(move || !c.off(), FOREVER);
        if call.off() {
            self.called_off.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A request as the platform's HTTP client runs it: cancelled when the player calls it off.
#[derive(Clone, Default)]
struct Call(Arc<std::sync::atomic::AtomicBool>);

impl Call {
    fn off(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A slot, given back when the answer is let go.
struct Slot(Arc<Server>);

impl Drop for Slot {
    fn drop(&mut self) {
        *self.0.running.lock() -= 1;
    }
}

/// A song's bytes, or none ever.
struct Answer {
    bytes: Option<Cursor<Arc<Vec<u8>>>>,
    call: Call,
    _slot: Slot,
}

impl Read for Answer {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.bytes.as_mut() {
            Some(c) => {
                let a: &[u8] = c.get_ref();
                let at = c.position() as usize;
                let n = buf.len().min(a.len() - at);
                buf[..n].copy_from_slice(&a[at..at + n]);
                c.set_position((at + n) as u64);
                Ok(n)
            }
            None => {
                self._slot.0.hang(&self.call);
                Err(std::io::Error::other("no bytes ever came"))
            }
        }
    }
}

/// The platform's HTTP client: a request runs until it ends, or until the player calls it off.
struct Net(Arc<Server>);

impl ByteSource for Net {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        self.open_cancellable(url, None, from, &Cancel::new())
    }

    fn open_cancellable(&self, url: &str, _key: Option<&str>, from: u64, cancel: &Cancel) -> Result<Body, OpenError> {
        let s = &self.0;
        let call = Call::default();
        let c = call.clone();
        cancel.on_cancel(move || c.0.store(true, Ordering::Release));
        if !s.slot(&call) {
            return Err("called off before it was made".into());
        }
        let slot = Slot(s.clone());
        if Server::hangs(url) && s.hang == Hang::Headers {
            s.hang(&call);
            return Err("no answer ever came".into());
        }
        let file = s.files.iter().find(|(u, _)| u == url).map(|(_, f)| f.clone()).ok_or("404")?;
        let len = file.len() as u64;
        let bytes = (!Server::hangs(url)).then(|| {
            let mut c = Cursor::new(file);
            c.set_position(from);
            c
        });
        Ok(Body { start: from, len: Some(len), reader: Box::new(Answer { bytes, call, _slot: slot }) })
    }
}

struct Songs(Arc<Server>);

impl Library for Songs {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let bytes: Arc<dyn ByteSource> = Arc::new(Net(self.0.clone()));
        Ok(Located { source: Source::Url { url: id.to_string(), bytes }, hint: Some("wav".into()), duration_ms: Some(SECS as i64 * 1000), estimated: false })
    }

    fn about(&self, id: &str) -> WindowSong {
        WindowSong { id: id.into(), title: id.into(), duration_ms: SECS as i64 * 1000, ..Default::default() }
    }

    /// A provider's song is never fetched before it is asked for, as the core's rule says.
    fn fetch_ahead(&self, id: &str) -> bool {
        !Server::hangs(id)
    }
}

struct Rig {
    engine: Engine,
    time: Stepper<Pull>,
    card: Card,
    events: Arc<Mutex<Vec<Event>>>,
    server: Arc<Server>,
    queue: SharedQueue,
    album: Vec<String>,
    playlist: Vec<String>,
}

/// A provider's album of `hung` songs that never come, queued, and a playlist of `fine` songs that play.
fn rig(hang: Hang, hung: usize, fine: usize) -> Rig {
    let clock = Virtual::default();
    let album: Vec<String> = (0..hung).map(|k| format!("ext-{k}")).collect();
    let playlist: Vec<String> = (0..fine).map(|k| format!("n{k}")).collect();
    let files = album.iter().chain(&playlist).enumerate().map(|(k, id)| (id.clone(), wav(220.0 + 40.0 * k as f64))).collect();
    let server = Arc::new(Server { clock: clock.clone(), files, hang, running: Mutex::new(0), hung: AtomicU64::new(0), called_off: AtomicU64::new(0) });
    let queue = SharedQueue::default();
    queue.0.lock().set(album.clone(), Some(0), false, 0);
    let card = Card::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut app = sim::App::new();
    app.prefs = sim::prefs_off();
    let config = Config { memory_mb: 256, ..Config::default() };
    let engine = Engine::start_on(Songs(server.clone()), app, queue.clone(), Box::new(card.clone()), None, config, clock.clone(), move |e| seen.lock().push(e));
    engine.queue_changed();
    Rig { engine, time: Stepper::new(clock, card.pull.clone()), card, events, server, queue, album, playlist }
}

impl Rig {
    fn heard_since(&self, from: usize, id: &str) -> bool {
        self.events.lock().iter().skip(from).any(|e| matches!(e, Event::Song { id: i, .. } if i == id))
    }

    /// Plays the album from its start and presses next through it, a moment on each song.
    fn skip_through_the_album(&self) {
        self.engine.play_at(0, 0);
        self.time.run(Duration::from_millis(300));
        for _ in 1..self.album.len() {
            self.engine.next();
            self.time.run(Duration::from_millis(300));
        }
        assert_eq!(self.queue.0.lock().current(), Some(self.album.len() - 1), "on the album's last song");
    }

    fn state(&self) -> String {
        format!(
            "{:.2} s heard, {} requests running, {} hung, {} called off; last events {:?}",
            self.card.secs(),
            self.server.running(),
            self.server.hung.load(Ordering::Relaxed),
            self.server.called_off.load(Ordering::Relaxed),
            self.events.lock().iter().rev().take(8).collect::<Vec<_>>()
        )
    }

    /// Goes to the playlist, as a user taps its song `index`: it is heard, and a second of it.
    fn playlist_plays(&self, index: usize) {
        let id = self.playlist[index].clone();
        let seen = self.events.lock().len();
        self.queue.0.lock().set(self.playlist.clone(), Some(index), false, 0);
        self.engine.queue_changed();
        self.engine.play_at(index, 0);
        self.card.heard.lock().clear();
        let heard = self.time.until(Duration::from_secs(10), || self.heard_since(seen, &id) && self.card.secs() >= 1.0);
        assert!(heard, "{id} plays after the hung songs: {}", self.state());
    }

    /// Nothing the album asked for still holds a request.
    fn album_let_go(&self) {
        assert!(self.time.until(Duration::from_secs(1), || self.server.running() <= 2), "the hung requests were called off: {}", self.state());
        assert_eq!(self.server.hung.load(Ordering::Relaxed), self.server.called_off.load(Ordering::Relaxed), "every hung request called off: {}", self.state());
    }
}

/// Twelve songs whose server never answers, skipped through, then a playlist that plays: every hung
/// request is called off as its song is left, so none holds a slot the playlist's songs need.
#[test]
fn hung_requests_skipped_leave_playlist_playing() {
    let r = rig(Hang::Headers, 12, 3);
    r.skip_through_the_album();
    r.playlist_plays(1);
    r.engine.next();
    let seen = r.events.lock().len();
    assert!(r.time.until(Duration::from_secs(10), || r.heard_since(seen, "n2")), "next plays on: {}", r.state());
    r.album_let_go();
}

/// The same with songs whose server answers and then never sends a byte.
#[test]
fn stalled_bodies_skipped_leave_playlist_playing() {
    let r = rig(Hang::Body, 12, 3);
    r.skip_through_the_album();
    r.playlist_plays(0);
    r.album_let_go();
}

/// The queue is where the user put it, whether or not a song there was ever heard: a skip onto songs that
/// never come, and a jump made while paused (held until play), move it at once, so a queue saved then (a
/// program closed and started again) comes back on that song and not where the music last sounded.
#[test]
fn jump_moves_queue_even_unheard() {
    let r = rig(Hang::Headers, 6, 0);
    let at = || r.queue.0.lock().current();
    r.engine.play_at(0, 0);
    r.time.run(Duration::from_millis(300));
    r.engine.next();
    r.engine.next();
    r.time.run(Duration::from_millis(300));
    assert_eq!(at(), Some(2), "{}", r.state());
    r.engine.pause();
    r.time.run(Duration::from_millis(300));
    let asked = r.server.hung.load(Ordering::Relaxed);
    r.engine.go_to(4, 0);
    r.time.run(Duration::from_millis(300));
    assert_eq!(at(), Some(4), "a jump held while paused: {}", r.state());
    assert_eq!(r.server.hung.load(Ordering::Relaxed), asked, "and nothing asked for it until play");
}
