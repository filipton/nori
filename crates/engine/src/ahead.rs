//! Songs fetched whole onto the disk ahead of their turn: the one fetcher of the songs coming up for
//! every client. Which ones and how many is the core's (`Client::precache_targets`: the user's count for
//! the network the device is on, none on a metered one by default, never a provider's song or one the
//! downloads have), asked as a song starts or the queue is edited ([`crate::Library::ahead`]), a moment
//! the network is awake for the next song anyway; the engine's loader fetches that next song itself, and
//! these are the ones after it. When their turn comes they play from the disk and the network stays
//! asleep. Where they are kept is a [`Keeping`]: nori-engine's own [`crate::Store`], or a client's cache
//! that keeps what is read through its [`ByteSource`] (media3's stream cache on Android).
//!
//! Burst-y, like everything that touches the network here: each song is fetched in one go, as fast as
//! it comes, in large reads, one after another on a thread that lives only while there is something to
//! fetch, and then nothing until the next song starts. Never a trickle, never a poll. The songs asked for
//! replace what was asked before: a song no longer wanted is left half way (what came of it is kept to go
//! on from, should it be wanted again or the player take it), one still wanted carries on where it is. A song someone else is writing (the player loading it) is left to
//! them, and one the player comes to take while it is being fetched here is handed over where it got to
//! ([`Ahead::take_over`]): no byte of it crosses the network twice.
//!
//! With AutoMix on, a song is measured as it comes ([`crate::arriving`]), on the same bytes in the same
//! burst: it is never read back from the disk to be decoded again.

use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use crate::arriving::Listening;
use crate::source::{open_watched, ByteSource, Cancel, OpenError};

/// How much is read at a time: the loader's own chunk.
const CHUNK: usize = 256 * 1024;
/// Keys the player took that are remembered before they are all let go.
const TAKEN_KEPT: usize = 256;

/// A song to fetch ahead: its id, where it comes from and the cache key it is kept under.
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
    /// An entry to write `key` into as it comes, from where an earlier fetch left it
    /// ([`Entry::written`]); none when it cannot be made. A cache that keeps what is read through the
    /// [`ByteSource`] by itself hands out one that only counts.
    fn entry(&self, key: &str) -> Option<Box<dyn Entry>>;
}

/// A song being kept as it comes.
pub trait Entry: Send {
    /// Bytes `from..` of the song; false once the entry was given up.
    fn write(&mut self, from: u64, bytes: &[u8]) -> bool;
    /// How much of the song it holds.
    fn written(&self) -> u64;
    /// The song ended at `len` bytes: whether all of it is kept.
    fn finish(self: Box<Self>, len: u64) -> bool;
    /// Left half way for the player to go on with: what it holds stays, where the keeping can.
    fn leave(self: Box<Self>) {}
}

/// What hears a song's bytes as they come (AutoMix's measuring), made per song; none when nothing does.
pub type Takers = Arc<dyn Fn(&AheadSong) -> Option<Listening> + Send + Sync>;

/// The fetching ahead.
#[derive(Default)]
pub struct Ahead {
    plan: Mutex<Plan>,
    /// Woken when a song stops being fetched here, for [`Ahead::take_over`].
    cv: Condvar,
    /// Moves whenever the songs asked for change or one is taken over: read per chunk without the lock.
    asked: AtomicU64,
}

#[derive(Default)]
struct Plan {
    songs: Vec<AheadSong>,
    /// Where they go, how they come and who hears them, while there is something to fetch.
    keeping: Option<Arc<dyn Keeping>>,
    bytes: Option<Arc<dyn ByteSource>>,
    takers: Option<Takers>,
    /// Keys given up on for this list (would not open, broke off): not tried again until it changes.
    failed: HashSet<String>,
    /// Keys the player has taken over: its own from now on.
    taken: HashSet<String>,
    /// The key being fetched now, and its request: called off the moment it is no longer wanted here,
    /// rather than at its next chunk, which a server that stopped answering never sends.
    current: Option<String>,
    request: Cancel,
    running: bool,
}

/// How a song's fetch came out.
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

    /// Fetches `songs` whole into `keeping` through `bytes`, `takers` hearing each as it comes. Called
    /// again, the new list replaces the old one; the same list changes nothing; an empty one stops the
    /// fetching.
    pub fn ask(self: &Arc<Self>, keeping: Arc<dyn Keeping>, bytes: Arc<dyn ByteSource>, songs: Vec<AheadSong>, takers: Option<Takers>) {
        let mut plan = self.plan.lock();
        if plan.songs == songs {
            return;
        }
        plan.songs = songs;
        plan.failed.clear();
        // What the player took stays its own across lists (it may take a song just as the list moves on),
        // and is forgotten only in bulk.
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

    /// Whether songs are being fetched now: for a test to wait until they are.
    pub fn busy(&self) -> bool {
        self.plan.lock().running
    }

    /// Whether the player has asked to take `key` over: for a test to hold a fetch until it has.
    pub fn taken(&self, key: &str) -> bool {
        self.plan.lock().taken.contains(key)
    }

    /// The player takes `key` (to play it, or as the next song): it is not fetched here from now on, and
    /// one being fetched here is left where it got to, for the player to go on with; returns once it has
    /// been let go, after a chunk at most.
    pub fn take_over(&self, key: &str) {
        let mut plan = self.plan.lock();
        if !plan.taken.contains(key) {
            plan.taken.insert(key.to_string());
        }
        if plan.current.as_deref() == Some(key) {
            self.asked.fetch_add(1, Ordering::AcqRel);
            // Waiting on a server that does not answer, it would never let go.
            plan.request.call_off(false);
        }
        while plan.current.as_deref() == Some(key) {
            self.cv.wait(&mut plan);
        }
    }

    /// The next song to fetch: the first asked for that is not kept already, being written by someone
    /// else, taken over or given up on. None ends the thread, and lets go of where they went.
    fn next(&self) -> Option<Job> {
        let (candidates, keeping, bytes, takers) = {
            let plan = self.plan.lock();
            let c: Vec<AheadSong> = plan.songs.iter().filter(|s| !plan.failed.contains(&s.key) && !plan.taken.contains(&s.key)).cloned().collect();
            (c, plan.keeping.clone(), plan.bytes.clone(), plan.takers.clone())
        };
        // Asked of the keeping without the lock: on Android it is a call into the platform.
        let found = keeping.as_ref().and_then(|k| candidates.into_iter().find(|s| !k.kept(&s.key) && !k.busy(&s.key)));
        let mut plan = self.plan.lock();
        match (found, keeping, bytes) {
            (Some(song), Some(k), Some(b)) if plan.songs.contains(&song) && !plan.taken.contains(&song.key) => {
                plan.current = Some(song.key.clone());
                plan.request = Cancel::new();
                Some((song, k, b, takers, plan.request.clone()))
            }
            // The list changed meanwhile: looked at again.
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

    /// Whether `key` is still asked for, and not the player's.
    fn wanted(&self, key: &str) -> Result<(), ()> {
        let plan = self.plan.lock();
        if plan.taken.contains(key) || !plan.songs.iter().any(|s| s.key == key) {
            return Err(());
        }
        Ok(())
    }

    fn run(&self) {
        while let Some((song, keeping, bytes, takers, request)) = self.next() {
            let fetched = self.fetch(&*keeping, &*bytes, &song, takers.as_ref(), &request);
            let mut plan = self.plan.lock();
            plan.current = None;
            // Left half way because it is no longer wanted, it is not asked for any more either.
            if fetched != Fetched::Kept {
                plan.failed.insert(song.key);
            }
            self.cv.notify_all();
        }
    }

    /// `song` fetched whole into `keeping`, `takers` hearing it as it comes. A body that breaks is asked
    /// for again from where it broke, once: a transcode's first answer promises an estimated length, and
    /// the body breaks where the real one ends, which the answer past it says ([`OpenError::PastEnd`]).
    fn fetch(&self, keeping: &dyn Keeping, bytes: &dyn ByteSource, song: &AheadSong, takers: Option<&Takers>, request: &Cancel) -> Fetched {
        let Some(mut entry) = keeping.entry(&song.key) else { return Fetched::Failed };
        let start = entry.written();
        // Heard from its first byte only: a song taken up half way is measured once it is whole.
        let mut taker = if start == 0 { takers.and_then(|t| t(song)) } else { None };
        let mut chunk = vec![0u8; CHUNK];
        let mut asked = self.asked.load(Ordering::Acquire);
        let mut promised = None;
        for again in [false, true] {
            let from = entry.written();
            let body = match open_watched(bytes, &song.url, Some(&song.key), from, request) {
                Ok(b) if b.start == from => b,
                // Nothing past what the entry holds: all of the song is there.
                Err(OpenError::PastEnd { len }) if from > 0 && len.is_none_or(|l| l == from) => {
                    promised = None;
                    break;
                }
                // Called off while it waited for the answer: no longer wanted here, or the player's now.
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
                    match self.wanted(&song.key) {
                        Ok(()) => asked = now,
                        // Left where it got to: the player may be taking it (asked for as the list moved on), and a
                        // song wanted again later goes on from there too.
                        Err(_) => {
                            entry.leave();
                            return Fetched::Left;
                        }
                    }
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
                    // Called off while it waited for bytes: left where it got to, as above.
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
        // Read to a clean end: that is where the song ends, even short of a length the server promised
        // (an estimate, for a transcode), as the player's loader takes it.
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

    /// Every song is `LEN` bytes; each request is counted with where it started; a song named in `held`
    /// stops after its first chunk until the test lets it go on; `stopped` counts the songs stopped there.
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

    /// A store in a directory of the test's own, gone with the guard.
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

    /// Until the held song has its first chunk written and stops for the next: its entry being open is
    /// not enough, as the fetch checks it is still wanted before its first byte.
    fn wait_held(net: &Net) {
        let until = Instant::now() + Duration::from_secs(30);
        while net.stopped.swap(0, Ordering::AcqRel) == 0 {
            assert!(Instant::now() < until, "the held song stops after its first chunk");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_songs_asked_for_are_fetched_whole_once_each_and_one_being_written_is_left_to_its_writer() {
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
        // The same songs asked again (every song start asks): nothing more.
        s.fetch_ahead(net.clone(), songs(&["a", "b", "c"]), None);
        settle(&s);
        assert_eq!(net.asked.lock().len(), 2);
        // The next song start: what is on the disk is not fetched again.
        s.fetch_ahead(net.clone(), songs(&["b", "c", "d"]), None);
        settle(&s);
        assert_eq!(net.asked.lock()[2..], ["http://m/c", "http://m/d"]);
    }

    #[test]
    fn a_song_no_longer_wanted_is_left_half_way_and_one_still_wanted_goes_on() {
        let (_dir, s) = store("moved");
        let net = Arc::new(Net::default());
        let (go, wait): (Sender<()>, Receiver<()>) = channel();
        *net.held.lock() = Some(("http://m/a".into(), wait));
        s.fetch_ahead(net.clone(), songs(&["a", "b"]), None);
        wait_held(&net);
        // The queue moved on: a is not wanted now.
        s.fetch_ahead(net.clone(), songs(&["b", "x"]), None);
        go.send(()).unwrap();
        settle(&s);
        assert!(s.peek("a:0").is_none() && !s.writing("a:0"), "left half way");
        assert!(s.peek("b:0").is_some() && s.peek("x:0").is_some());
        // Wanted again (the queue edited back): it goes on from what came, and nothing crosses twice.
        s.fetch_ahead(net.clone(), songs(&["a"]), None);
        settle(&s);
        assert_eq!(std::fs::metadata(s.peek("a:0").unwrap()).unwrap().len(), LEN as u64);
        let from: Vec<u64> = net.asked.lock().iter().zip(net.from.lock().iter()).filter(|(u, _)| u.ends_with("/a")).map(|(_, f)| *f).collect();
        assert!(from.len() == 2 && from[0] == 0 && from[1] > 0, "{from:?}");

        // Still wanted after the change: it carries on, fetched once.
        let (go, wait) = channel();
        *net.held.lock() = Some(("http://m/y".into(), wait));
        s.fetch_ahead(net.clone(), songs(&["y", "z"]), None);
        wait_held(&net);
        s.fetch_ahead(net.clone(), songs(&["y"]), None);
        go.send(()).unwrap();
        settle(&s);
        assert!(s.peek("y:0").is_some() && s.peek("z:0").is_none());
        assert_eq!(net.asked.lock().iter().filter(|u| u.ends_with("/y")).count(), 1);
        // Nothing asked for: nothing runs.
        s.fetch_ahead(net.clone(), Vec::new(), None);
        assert!(!s.fetching_ahead());
    }

    #[test]
    fn a_song_the_player_takes_while_it_is_fetched_ahead_goes_on_from_where_it_got_to() {
        let (_dir, s) = store("taken");
        let net = Arc::new(Net::default());
        let (go, wait) = channel();
        *net.held.lock() = Some(("http://m/a".into(), wait));
        s.fetch_ahead(net.clone(), songs(&["a", "b"]), None);
        wait_held(&net);
        // The player comes for a (a skip onto it) while its first chunk is in: it waits for the fetch
        // to let go, which it does after that chunk.
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
        // Taken over, it is the player's: asked again, the fetching ahead leaves it alone.
        s.fetch_ahead(net.clone(), songs(&["b"]), None);
        settle(&s);
        assert_eq!(net.asked.lock().len(), 2);
    }

    /// A transcoding server: it promises `LEN` bytes, sends `REAL` and breaks off there (as OkHttp reads a
    /// body shorter than its Content-Length), and answers a range from `REAL` on with a 416.
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
    fn a_transcode_that_breaks_off_short_of_its_estimated_length_is_kept_whole_at_its_real_length() {
        let (_dir, s) = store("estimated");
        let net = Arc::new(Transcoder(Mutex::new(Vec::new())));
        s.fetch_ahead(net.clone(), songs(&["a"]), None);
        settle(&s);
        let kept = s.peek("a:0").expect("kept");
        assert_eq!(std::fs::metadata(kept).unwrap().len(), REAL as u64, "the real length");
        assert_eq!(*net.0.lock(), [0, REAL as u64], "asked again where it broke, and told that is the end");
    }
}
