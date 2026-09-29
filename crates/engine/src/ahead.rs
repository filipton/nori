//! Fetches upcoming songs whole to disk (the songs after the next, which the engine's loader fetches).
//! The core picks them (`Client::precache_targets`), asked when a song starts or the queue changes
//! ([`crate::Library::ahead`]), while the network is awake anyway. Kept in a [`Keeping`]: nori-engine's
//! [`crate::Store`], or a client's cache fed through its [`ByteSource`] (media3 on Android).
//!
//! Each song is fetched in one burst on a thread that lives only while there is work. A new list
//! replaces the old: an unwanted song is left half way (kept to resume), a wanted one carries on. A
//! song the player is writing is left alone; one it takes mid-fetch is handed over where it got to
//! ([`Ahead::take_over`]), so no byte is fetched twice. With AutoMix on, songs are measured as they
//! arrive ([`crate::arriving`]).

use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use crate::arriving::Listening;
use crate::source::{open_watched, ByteSource, Cancel, OpenError};

/// Read size, as the loader's.
const CHUNK: usize = 256 * 1024;
/// Taken keys remembered before they are cleared in bulk.
const TAKEN_KEPT: usize = 256;

/// A song to fetch ahead: id, URL and cache key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AheadSong {
    pub id: String,
    pub url: String,
    pub key: String,
}

/// Where songs fetched ahead are kept.
pub trait Keeping: Send + Sync {
    /// Whether all of `key` is kept already.
    fn kept(&self, key: &str) -> bool;
    /// Whether someone else is writing `key` now (the player loading it).
    fn busy(&self, key: &str) -> bool;
    /// An entry to write `key` into, continuing an earlier fetch ([`Entry::written`]). A cache fed by
    /// the [`ByteSource`] itself returns one that only counts.
    fn entry(&self, key: &str) -> Option<Box<dyn Entry>>;
}

/// A song being written as it arrives.
pub trait Entry: Send {
    /// Writes bytes `from..`; false once the entry was given up.
    fn write(&mut self, from: u64, bytes: &[u8]) -> bool;
    /// Bytes held.
    fn written(&self) -> u64;
    /// The song ended at `len` bytes; returns whether it is kept whole.
    fn finish(self: Box<Self>, len: u64) -> bool;
    /// Left half way: keep what it holds, if the keeping can.
    fn leave(self: Box<Self>) {}
}

/// Makes what hears each song's bytes as they arrive (AutoMix's measuring).
pub type Takers = Arc<dyn Fn(&AheadSong) -> Option<Listening> + Send + Sync>;

/// The fetching ahead.
#[derive(Default)]
pub struct Ahead {
    plan: Mutex<Plan>,
    /// Signalled when a fetch stops, for [`Ahead::take_over`].
    cv: Condvar,
    /// Bumped when the list changes or a song is taken over; read per chunk without the lock.
    asked: AtomicU64,
}

#[derive(Default)]
struct Plan {
    songs: Vec<AheadSong>,
    /// Set while there is work.
    keeping: Option<Arc<dyn Keeping>>,
    bytes: Option<Arc<dyn ByteSource>>,
    takers: Option<Takers>,
    /// Keys given up on for this list.
    failed: HashSet<String>,
    /// Keys the player took over.
    taken: HashSet<String>,
    /// The key being fetched and its request, cancelled as soon as it is unwanted (a hung server never
    /// sends the next chunk).
    current: Option<String>,
    request: Cancel,
    running: bool,
}

/// How a fetch ended.
#[derive(Debug, PartialEq, Eq)]
enum Fetched {
    Kept,
    Failed,
    /// No longer wanted, or taken over by the player.
    Left,
}

type Job = (AheadSong, Arc<dyn Keeping>, Arc<dyn ByteSource>, Option<Takers>, Cancel);

impl Ahead {
    pub fn new() -> Arc<Ahead> {
        Arc::new(Ahead::default())
    }

    /// Fetches `songs` whole into `keeping` through `bytes`. A new list replaces the old; the same list
    /// changes nothing; an empty one stops.
    pub fn ask(self: &Arc<Self>, keeping: Arc<dyn Keeping>, bytes: Arc<dyn ByteSource>, songs: Vec<AheadSong>, takers: Option<Takers>) {
        let mut plan = self.plan.lock();
        if plan.songs == songs {
            return;
        }
        plan.songs = songs;
        plan.failed.clear();
        // Taken keys survive list changes (the player may take one as the list moves on).
        if plan.taken.len() > TAKEN_KEPT {
            plan.taken.clear();
        }
        self.asked.fetch_add(1, Ordering::AcqRel);
        if plan.current.as_ref().is_some_and(|k| !plan.songs.iter().any(|s| &s.key == k)) {
            plan.request.call_off(false);
        }
        if plan.songs.is_empty() {
            return;
        }
        plan.keeping = Some(keeping);
        plan.bytes = Some(bytes);
        plan.takers = takers;
        if plan.running {
            return;
        }
        plan.running = true;
        drop(plan);
        let me = self.clone();
        if std::thread::Builder::new().name("nori-precache".into()).spawn(move || me.run()).is_err() {
            self.plan.lock().running = false;
        }
    }

    /// Whether a fetch thread runs.
    pub fn busy(&self) -> bool {
        self.plan.lock().running
    }

    /// Whether the player took `key` over.
    pub fn taken(&self, key: &str) -> bool {
        self.plan.lock().taken.contains(key)
    }

    /// The player takes `key`: no longer fetched here; a fetch under way stops where it got to. Returns
    /// once it stopped (after a chunk at most).
    pub fn take_over(&self, key: &str) {
        let mut plan = self.plan.lock();
        if !plan.taken.contains(key) {
            plan.taken.insert(key.to_string());
        }
        if plan.current.as_deref() == Some(key) {
            self.asked.fetch_add(1, Ordering::AcqRel);
            // A hung request would never reach its next chunk.
            plan.request.call_off(false);
        }
        while plan.current.as_deref() == Some(key) {
            self.cv.wait(&mut plan);
        }
    }

    /// The first song asked for that is not kept, busy, taken or failed. None ends the thread.
    fn next(&self) -> Option<Job> {
        let (candidates, keeping, bytes, takers) = {
            let plan = self.plan.lock();
            let c: Vec<AheadSong> = plan.songs.iter().filter(|s| !plan.failed.contains(&s.key) && !plan.taken.contains(&s.key)).cloned().collect();
            (c, plan.keeping.clone(), plan.bytes.clone(), plan.takers.clone())
        };
        // Without the lock: on Android the keeping calls into the platform.
        let found = keeping.as_ref().and_then(|k| candidates.into_iter().find(|s| !k.kept(&s.key) && !k.busy(&s.key)));
        let mut plan = self.plan.lock();
        match (found, keeping, bytes) {
            (Some(song), Some(k), Some(b)) if plan.songs.contains(&song) && !plan.taken.contains(&song.key) => {
                plan.current = Some(song.key.clone());
                plan.request = Cancel::new();
                Some((song, k, b, takers, plan.request.clone()))
            }
            // The list changed meanwhile.
            (Some(_), Some(_), Some(_)) => {
                drop(plan);
                self.next()
            }
            _ => {
                plan.running = false;
                plan.keeping = None;
                plan.bytes = None;
                plan.takers = None;
                None
            }
        }
    }

    /// Whether `key` is still asked for and not the player's.
    fn wanted(&self, key: &str) -> bool {
        let plan = self.plan.lock();
        !plan.taken.contains(key) && plan.songs.iter().any(|s| s.key == key)
    }

    fn run(&self) {
        while let Some((song, keeping, bytes, takers, request)) = self.next() {
            let fetched = self.fetch(&*keeping, &*bytes, &song, takers.as_ref(), &request);
            let mut plan = self.plan.lock();
            plan.current = None;
            if fetched != Fetched::Kept {
                plan.failed.insert(song.key);
            }
            self.cv.notify_all();
        }
    }

    /// Fetches `song` whole into `keeping`. A broken body is asked again once from where it broke: a
    /// transcode breaks where its real end is, which the next answer confirms ([`OpenError::PastEnd`]).
    fn fetch(&self, keeping: &dyn Keeping, bytes: &dyn ByteSource, song: &AheadSong, takers: Option<&Takers>, request: &Cancel) -> Fetched {
        let Some(mut entry) = keeping.entry(&song.key) else { return Fetched::Failed };
        let start = entry.written();
        // A taker needs every byte from the first.
        let mut taker = if start == 0 { takers.and_then(|t| t(song)) } else { None };
        let mut chunk = vec![0u8; CHUNK];
        let mut asked = self.asked.load(Ordering::Acquire);
        let mut promised = None;
        for again in [false, true] {
            let from = entry.written();
            let body = match open_watched(bytes, &song.url, Some(&song.key), from, request) {
                Ok(b) if b.start == from => b,
                // Nothing past what the entry holds: complete.
                Err(OpenError::PastEnd { len }) if from > 0 && len.is_none_or(|l| l == from) => {
                    promised = None;
                    break;
                }
                // Cancelled: unwanted, or taken by the player.
                Err(_) if request.cancelled() && !request.timed_out() => {
                    entry.leave();
                    return Fetched::Left;
                }
                _ => return Fetched::Failed,
            };
            promised = body.len;
            let mut reader = body.reader;
            let broke = loop {
                let now = self.asked.load(Ordering::Acquire);
                if now != asked {
                    if !self.wanted(&song.key) {
                        // Kept for the player or a later list to resume.
                        entry.leave();
                        return Fetched::Left;
                    }
                    asked = now;
                }
                match reader.read(&mut chunk) {
                    Ok(0) => break false,
                    Ok(n) => {
                        if !entry.write(entry.written(), &chunk[..n]) {
                            return Fetched::Failed;
                        }
                        if let Some(t) = taker.as_mut() {
                            t.take(&chunk[..n]);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) if request.cancelled() && !request.timed_out() => {
                        entry.leave();
                        return Fetched::Left;
                    }
                    Err(_) => break true,
                }
            };
            if !broke {
                break;
            }
            if again {
                return Fetched::Failed;
            }
        }
        // A clean end is the real end, even short of the promised length (a transcode's estimate).
        let len = entry.written();
        if promised.is_some_and(|l| l < len) {
            return Fetched::Failed;
        }
        let kept = entry.finish(len);
        if let Some(t) = taker {
            t.end(kept);
        }
        if kept {
            Fetched::Kept
        } else {
            Fetched::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::source::Body;
    use crate::store::{Recent, Store};

    const LEN: usize = 600_000;

    /// Songs of `LEN` bytes; records each request's URL and offset. The `held` song pauses after its
    /// first chunk until released; `stopped` counts such pauses.
    #[derive(Default)]
    struct Net {
        asked: Mutex<Vec<String>>,
        from: Mutex<Vec<u64>>,
        held: Mutex<Option<(String, Receiver<()>)>>,
        stopped: Arc<AtomicU64>,
    }

    struct Held(Cursor<Vec<u8>>, Option<Receiver<()>>, Arc<AtomicU64>);

    impl Read for Held {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0.position() > 0 {
                if let Some(go) = self.1.take() {
                    self.2.fetch_add(1, Ordering::Release);
                    let _ = go.recv();
                }
            }
            let n = buf.len().min(CHUNK);
            self.0.read(&mut buf[..n])
        }
    }

    impl ByteSource for Net {
        fn open(&self, url: &str, from: u64) -> Result<Body, crate::source::OpenError> {
            self.asked.lock().push(url.to_string());
            self.from.lock().push(from);
            let gate = self.held.lock().take_if(|(u, _)| u == url).map(|(_, r)| r);
            let mut c = Cursor::new(vec![3u8; LEN]);
            c.set_position(from);
            Ok(Body { start: from, len: Some(LEN as u64), reader: Box::new(Held(c, gate, self.stopped.clone())) })
        }
    }

    /// A store in a temporary directory.
    fn store(name: &str) -> (nori_testdir::TempDir, Arc<Store>) {
        let d = nori_testdir::TempDir::new(&format!("ahead-{name}"));
        let s = Store::open(d.path(), 64 << 20, Box::new(Recent::default())).unwrap();
        (d, s)
    }

    fn settle(s: &Store) {
        let until = Instant::now() + Duration::from_secs(30);
        while s.fetching_ahead() {
            assert!(Instant::now() < until, "the fetching ends");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn songs(ids: &[&str]) -> Vec<AheadSong> {
        ids.iter().map(|id| AheadSong { id: id.to_string(), url: format!("http://m/{id}"), key: format!("{id}:0") }).collect()
    }

    /// Waits until the held song paused after its first chunk.
    fn wait_held(net: &Net) {
        let until = Instant::now() + Duration::from_secs(30);
        while net.stopped.swap(0, Ordering::AcqRel) == 0 {
            assert!(Instant::now() < until, "the held song stops after its first chunk");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn fetches_each_song_once_and_skips_busy_ones() {
        let (_dir, s) = store("whole");
        let net = Arc::new(Net::default());
        let mut w = s.writer("c:0").unwrap();
        assert!(w.write(0, &[1; 10]));
        s.fetch_ahead(net.clone(), songs(&["a", "b", "c"]), None);
        settle(&s);
        assert_eq!(std::fs::metadata(s.peek("a:0").unwrap()).unwrap().len(), LEN as u64, "whole");
        assert!(s.peek("b:0").is_some());
        assert!(s.peek("c:0").is_none(), "the player loading c keeps it");
        assert_eq!(*net.asked.lock(), ["http://m/a", "http://m/b"], "one request a song: one burst each");
        drop(w);
        // The same list again: nothing more.
        s.fetch_ahead(net.clone(), songs(&["a", "b", "c"]), None);
        settle(&s);
        assert_eq!(net.asked.lock().len(), 2);
        // What is on disk is not fetched again.
        s.fetch_ahead(net.clone(), songs(&["b", "c", "d"]), None);
        settle(&s);
        assert_eq!(net.asked.lock()[2..], ["http://m/c", "http://m/d"]);
    }

    #[test]
    fn unwanted_song_left_half_way_wanted_one_continues() {
        let (_dir, s) = store("moved");
        let net = Arc::new(Net::default());
        let (go, wait): (Sender<()>, Receiver<()>) = channel();
        *net.held.lock() = Some(("http://m/a".into(), wait));
        s.fetch_ahead(net.clone(), songs(&["a", "b"]), None);
        wait_held(&net);
        // a is no longer wanted.
        s.fetch_ahead(net.clone(), songs(&["b", "x"]), None);
        go.send(()).unwrap();
        settle(&s);
        assert!(s.peek("a:0").is_none() && !s.writing("a:0"), "left half way");
        assert!(s.peek("b:0").is_some() && s.peek("x:0").is_some());
        // Wanted again: resumes from what came.
        s.fetch_ahead(net.clone(), songs(&["a"]), None);
        settle(&s);
        assert_eq!(std::fs::metadata(s.peek("a:0").unwrap()).unwrap().len(), LEN as u64);
        let from: Vec<u64> = net.asked.lock().iter().zip(net.from.lock().iter()).filter(|(u, _)| u.ends_with("/a")).map(|(_, f)| *f).collect();
        assert!(from.len() == 2 && from[0] == 0 && from[1] > 0, "{from:?}");

        // Still wanted after a change: fetched once.
        let (go, wait) = channel();
        *net.held.lock() = Some(("http://m/y".into(), wait));
        s.fetch_ahead(net.clone(), songs(&["y", "z"]), None);
        wait_held(&net);
        s.fetch_ahead(net.clone(), songs(&["y"]), None);
        go.send(()).unwrap();
        settle(&s);
        assert!(s.peek("y:0").is_some() && s.peek("z:0").is_none());
        assert_eq!(net.asked.lock().iter().filter(|u| u.ends_with("/y")).count(), 1);

        s.fetch_ahead(net.clone(), Vec::new(), None);
        assert!(!s.fetching_ahead());
    }

    #[test]
    fn player_takes_over_mid_fetch() {
        let (_dir, s) = store("taken");
        let net = Arc::new(Net::default());
        let (go, wait) = channel();
        *net.held.lock() = Some(("http://m/a".into(), wait));
        s.fetch_ahead(net.clone(), songs(&["a", "b"]), None);
        wait_held(&net);
        // The player takes a mid-fetch; the fetch stops after the chunk.
        let (s2, taking) = (s.clone(), std::thread::spawn({
            let s = s.clone();
            move || s.writer_for_player("a:0")
        }));
        let until = Instant::now() + Duration::from_secs(10);
        while !s.taken_over("a:0") {
            assert!(Instant::now() < until, "the player asked to take a over");
            std::thread::sleep(Duration::from_millis(1));
        }
        go.send(()).unwrap();
        let mut w = taking.join().unwrap().expect("the player's entry, where the fetch left it");
        let got = w.written() as usize;
        assert!((CHUNK..LEN).contains(&got), "what came is kept, not fetched again: {got}");
        assert_eq!(w.read_back().unwrap().len(), got);
        assert!(w.write(got as u64, &vec![3u8; LEN - got]));
        assert!(w.finish(LEN as u64));
        settle(&s2);
        assert!(s.peek("a:0").is_some() && s.peek("b:0").is_some());
        assert_eq!(net.asked.lock().iter().filter(|u| u.ends_with("/a")).count(), 1, "a was asked of the network once here; the player asks for the rest only");
        // Taken over: left alone from now on.
        s.fetch_ahead(net.clone(), songs(&["b"]), None);
        settle(&s);
        assert_eq!(net.asked.lock().len(), 2);
    }

    /// Promises `LEN` bytes, sends `REAL` then errors (as OkHttp on a short body), and answers 416 past it.
    struct Transcoder(Mutex<Vec<u64>>);

    const REAL: usize = 450_000;

    struct Short(Cursor<Vec<u8>>);

    impl Read for Short {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.read(buf)? {
                0 => Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected end of stream")),
                n => Ok(n),
            }
        }
    }

    impl ByteSource for Transcoder {
        fn open(&self, _url: &str, from: u64) -> Result<Body, OpenError> {
            self.0.lock().push(from);
            if from >= REAL as u64 {
                return Err(OpenError::PastEnd { len: None });
            }
            let mut c = Cursor::new(vec![5u8; REAL]);
            c.set_position(from);
            Ok(Body { start: from, len: Some(LEN as u64), reader: Box::new(Short(c)) })
        }
    }

    #[test]
    fn short_transcode_kept_at_real_length() {
        let (_dir, s) = store("estimated");
        let net = Arc::new(Transcoder(Mutex::new(Vec::new())));
        s.fetch_ahead(net.clone(), songs(&["a"]), None);
        settle(&s);
        let kept = s.peek("a:0").expect("kept");
        assert_eq!(std::fs::metadata(kept).unwrap().len(), REAL as u64, "the real length");
        assert_eq!(*net.0.lock(), [0, REAL as u64], "asked again where it broke, and told that is the end");
    }
}
