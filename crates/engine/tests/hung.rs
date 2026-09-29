//! Songs whose server never answers (octo-fiesta without a provider token): skipping through them must
//! leave later songs playable. The HTTP client's per-host request limit is modelled as slots; a hung
//! request holds its slot until cancelled, so an engine that leaks hung requests starves later songs
//! (what a phone did). Hung requests let the virtual clock move (`Virtual::hang_while`).

use crate::common;

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
/// Concurrent requests the client allows per host.
const SLOTS: usize = 6;
/// Real time a hung request waits for cancellation: far past any test.
const FOREVER: Duration = Duration::from_secs(300);

fn wav(hz: f64) -> Arc<Vec<u8>> {
    Arc::new(common::wav(RATE, &common::sine(RATE, hz, SECS as f64, 12_000.0)))
}

/// How a song's request hangs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hang {
    /// No headers ever.
    Headers,
    /// Headers, then no body bytes.
    Body,
}

/// The server: songs, how the `ext-` ones hang, and the request slots.
struct Server {
    clock: Virtual,
    files: Vec<(String, Arc<Vec<u8>>)>,
    hang: Hang,
    /// Requests holding a slot.
    running: Mutex<usize>,
    /// Requests that hung, and those cancelled.
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

    /// Waits for a free slot; false if cancelled first.
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

    /// Hangs until the request is cancelled.
    fn hang(&self, call: &Call) {
        self.hung.fetch_add(1, Ordering::Relaxed);
        let c = call.clone();
        self.clock.hang_while(move || !c.off(), FOREVER);
        if call.off() {
            self.called_off.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A client request's cancelled flag.
#[derive(Clone, Default)]
struct Call(Arc<std::sync::atomic::AtomicBool>);

impl Call {
    fn off(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A held slot, freed on drop.
struct Slot(Arc<Server>);

impl Drop for Slot {
    fn drop(&mut self) {
        *self.0.running.lock() -= 1;
    }
}

/// A body: the song's bytes, or none ever.
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

/// An album of `hung` songs that never come, queued, and a playlist of `fine` songs.
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

    /// Plays the album and presses next through it.
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

    /// Switches to the playlist at `index` and asserts a second of it is heard.
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

    /// Asserts every hung request was cancelled.
    fn album_let_go(&self) {
        assert!(self.time.until(Duration::from_secs(1), || self.server.running() <= 2), "the hung requests were called off: {}", self.state());
        assert_eq!(self.server.hung.load(Ordering::Relaxed), self.server.called_off.load(Ordering::Relaxed), "every hung request called off: {}", self.state());
    }
}

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

#[test]
fn stalled_bodies_skipped_leave_playlist_playing() {
    let r = rig(Hang::Body, 12, 3);
    r.skip_through_the_album();
    r.playlist_plays(0);
    r.album_let_go();
}

/// The queue follows skips and held jumps at once, so a saved queue restores that song.
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
