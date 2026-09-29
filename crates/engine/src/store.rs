//! Songs on disk in a client-named directory: the stream cache (by the core's `<id>:<quality>` keys)
//! and downloads (by id), as plain files.
//!
//! A streamed song is written as it loads and becomes an entry only when whole; until then it is a
//! `.part` file. The cache is held to a size limit, evicting whole songs in the [`Order`]'s order (the
//! core's `stream_cache`: unused this run first, then least recently used).

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

/// Eviction order for the stream cache.
pub trait Order: Send + Sync {
    /// `key` was just used.
    fn touch(&self, key: &str);
    /// What an earlier run left in the cache.
    fn seed(&self, held: &[String]);
    /// The next key to evict (forgotten once returned).
    fn next(&self) -> Option<String>;
    /// The cache was emptied.
    fn clear(&self);
}

/// The core's rule without the core: an earlier run's leftovers first, then least recently used.
#[derive(Default)]
pub struct Recent(Mutex<Stamps>);

/// Each key's last use; leftovers from an earlier run get stamps below zero.
#[derive(Default)]
struct Stamps {
    used: HashMap<String, i64>,
    newest: i64,
    oldest: i64,
}

impl Order for Recent {
    fn touch(&self, key: &str) {
        let mut o = self.0.lock();
        o.newest += 1;
        let t = o.newest;
        o.used.insert(key.to_string(), t);
    }

    fn seed(&self, held: &[String]) {
        let mut o = self.0.lock();
        for k in held {
            if !o.used.contains_key(k) {
                o.oldest -= 1;
                let t = o.oldest;
                o.used.insert(k.clone(), t);
            }
        }
    }

    fn next(&self) -> Option<String> {
        let mut o = self.0.lock();
        let key = o.used.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone())?;
        o.used.remove(&key);
        Some(key)
    }

    fn clear(&self) {
        self.0.lock().used.clear();
    }
}

const STREAM: &str = "stream";
const DOWNLOADS: &str = "downloads";
const PART: &str = ".part";

/// A key as a portable file name: alphanumerics, `-`, `_` and non-leading `.` kept, the rest `%XX`.
fn file_name(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 8);
    for b in key.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') || (b == b'.' && !out.is_empty()) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The key of an entry's file name; None for anything else.
fn key_of(name: &str) -> Option<String> {
    if name.ends_with(PART) {
        return None;
    }
    let b = name.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(name.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

struct Held {
    /// Bytes cached, once counted.
    bytes: Option<u64>,
    limit: u64,
    /// Keys being written: one writer each (a second would truncate the first's file).
    writing: HashSet<String>,
    /// Keys whose partial entry the fetching ahead left for the player to resume.
    left: HashSet<String>,
}

/// The songs on disk, shared by loaders, the downloader and the measurer.
pub struct Store {
    dir: PathBuf,
    order: Box<dyn Order>,
    held: Mutex<Held>,
    /// [`Store::fetch_ahead`].
    pub(crate) ahead: Arc<crate::ahead::Ahead>,
    /// [`Store::on_whole`] listeners.
    whole: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
    me: std::sync::Weak<Store>,
}

impl Store {
    /// Opens the store in `dir` (created if needed), the stream cache limited to `limit` bytes.
    pub fn open(dir: impl Into<PathBuf>, limit: u64, order: Box<dyn Order>) -> io::Result<Arc<Store>> {
        let dir = dir.into();
        fs::create_dir_all(dir.join(STREAM))?;
        // Partial entries from an earlier run are stale.
        if let Ok(entries) = fs::read_dir(dir.join(STREAM)) {
            for e in entries.flatten() {
                if e.file_name().to_str().is_some_and(|n| n.ends_with(PART)) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        fs::create_dir_all(dir.join(DOWNLOADS))?;
        Ok(Arc::new_cyclic(|me| Store {
            dir,
            order,
            held: Mutex::new(Held { bytes: None, limit, writing: HashSet::new(), left: HashSet::new() }),
            ahead: crate::ahead::Ahead::new(),
            whole: Mutex::new(Vec::new()),
            me: me.clone(),
        }))
    }

    /// Registers `told`, called (on the writing thread) whenever a streamed song becomes whole.
    pub fn on_whole(&self, told: Box<dyn Fn() + Send + Sync>) {
        self.whole.lock().push(told);
    }

    fn me(&self) -> Option<Arc<Store>> {
        self.me.upgrade()
    }

    fn stream_path(&self, key: &str) -> PathBuf {
        self.dir.join(STREAM).join(file_name(key))
    }

    /// A finished download's path.
    pub fn download_path(&self, id: &str) -> PathBuf {
        self.dir.join(DOWNLOADS).join(file_name(id))
    }

    /// A download's path while in progress.
    pub fn download_part(&self, id: &str) -> PathBuf {
        let mut p = self.download_path(id).into_os_string();
        p.push(PART);
        p.into()
    }

    /// The finished download of `id`, if present.
    pub fn downloaded(&self, id: &str) -> Option<PathBuf> {
        let p = self.download_path(id);
        p.is_file().then_some(p)
    }

    /// The cached song under `key`, if present; counts as a use.
    pub fn cached(&self, key: &str) -> Option<PathBuf> {
        let p = self.stream_path(key);
        if !p.is_file() {
            return None;
        }
        self.order.touch(key);
        Some(p)
    }

    /// [`Store::cached`] without counting as a use (background reads such as measuring).
    pub fn peek(&self, key: &str) -> Option<PathBuf> {
        let p = self.stream_path(key);
        p.is_file().then_some(p)
    }

    /// A writer for `key`; None if the file cannot be made or another writer has it (first come keeps it).
    pub fn writer(self: &Arc<Self>, key: &str) -> Option<Writer> {
        let resume = {
            let mut h = self.held.lock();
            if !h.writing.insert(key.to_string()) {
                return None;
            }
            h.left.remove(key)
        };
        let mut part = self.stream_path(key).into_os_string();
        part.push(PART);
        let part = PathBuf::from(part);
        // Resume a part the fetching ahead left; otherwise start afresh.
        let opened = if resume { fs::OpenOptions::new().append(true).open(&part).and_then(|f| Ok((f.metadata()?.len(), f))) } else { File::create(&part).map(|f| (0, f)) };
        let Ok((at, file)) = opened.or_else(|_| File::create(&part).map(|f| (0, f))) else {
            self.held.lock().writing.remove(key);
            return None;
        };
        Some(Writer { store: self.clone(), key: key.to_string(), part: Some(part), file: Some(file), at })
    }

    /// The player's writer for `key`: takes it over from the fetching ahead where it got to
    /// ([`crate::ahead::Ahead::take_over`]), which may block for a chunk (call on the loader's thread).
    pub fn writer_for_player(self: &Arc<Self>, key: &str) -> Option<Writer> {
        self.ahead.take_over(key);
        self.writer(key)
    }

    /// Whether `key` is being written.
    pub fn writing(&self, key: &str) -> bool {
        self.held.lock().writing.contains(key)
    }

    /// Fetches `songs` whole into the stream cache ahead of their turn ([`crate::ahead`]). A new list
    /// replaces the old; an empty one stops.
    pub fn fetch_ahead(self: &Arc<Self>, bytes: Arc<dyn crate::source::ByteSource>, songs: Vec<crate::ahead::AheadSong>, takers: Option<crate::ahead::Takers>) {
        self.ahead.ask(self.clone(), bytes, songs, takers);
    }

    /// Whether songs are being fetched ahead.
    pub fn fetching_ahead(&self) -> bool {
        self.ahead.busy()
    }

    /// Whether the player took `key` over from the fetching ahead.
    pub fn taken_over(&self, key: &str) -> bool {
        self.ahead.taken(key)
    }

    /// Sets the cache limit, evicting what is over it.
    pub fn set_limit(&self, limit: u64) {
        self.held.lock().limit = limit;
        self.trim(0);
    }

    /// Drops the cached `keys` (e.g. a song now downloaded).
    pub fn drop_cached(&self, keys: &[String]) {
        for k in keys {
            self.remove(k);
        }
    }

    /// Empties the stream cache; downloads stay.
    pub fn clear_cache(&self) {
        for (k, _) in self.entries() {
            self.remove(&k);
        }
        self.order.clear();
        self.held.lock().bytes = Some(0);
    }

    /// Bytes the stream cache holds.
    pub fn cache_bytes(&self) -> u64 {
        self.counted()
    }

    fn entries(&self) -> Vec<(String, u64)> {
        let Ok(dir) = fs::read_dir(self.dir.join(STREAM)) else { return Vec::new() };
        dir.filter_map(|e| {
            let e = e.ok()?;
            let key = key_of(e.file_name().to_str()?)?;
            Some((key, e.metadata().ok()?.len()))
        })
        .collect()
    }

    /// The cache's size, counted from disk on first use (seeding the order with what is there).
    fn counted(&self) -> u64 {
        if let Some(b) = self.held.lock().bytes {
            return b;
        }
        let entries = self.entries();
        let keys: Vec<String> = entries.iter().map(|e| e.0.clone()).collect();
        self.order.seed(&keys);
        let total = entries.iter().map(|e| e.1).sum();
        let mut h = self.held.lock();
        *h.bytes.get_or_insert(total)
    }

    fn remove(&self, key: &str) {
        let p = self.stream_path(key);
        let Ok(len) = fs::metadata(&p).map(|m| m.len()) else { return };
        if fs::remove_file(&p).is_ok() {
            if let Some(b) = self.held.lock().bytes.as_mut() {
                *b = b.saturating_sub(len);
            }
        }
    }

    /// Accounts `added` bytes and evicts whole songs until the cache fits.
    fn trim(&self, added: u64) {
        let total = self.counted();
        let limit = {
            let mut h = self.held.lock();
            let b = h.bytes.get_or_insert(total);
            *b += added;
            if *b <= h.limit {
                return;
            }
            h.limit
        };
        while self.held.lock().bytes.is_some_and(|b| b > limit) {
            let Some(key) = self.order.next() else { return };
            self.remove(&key);
        }
    }

    /// `key` is fully written: turns the part into an entry.
    fn finished(&self, key: &str, part: &Path, len: u64) -> bool {
        // Count first, so the new entry is counted once.
        self.counted();
        if fs::rename(part, self.stream_path(key)).is_err() {
            let _ = fs::remove_file(part);
            return false;
        }
        self.order.touch(key);
        self.trim(len);
        for told in self.whole.lock().iter() {
            told();
        }
        true
    }
}

/// A stream cache entry being written. Only contiguous bytes are taken; a gap (a seek past what was
/// loaded) abandons it.
pub struct Writer {
    store: Arc<Store>,
    key: String,
    /// The part file, removed on drop unless handed on (finished, or left for the next writer).
    part: Option<PathBuf>,
    file: Option<File>,
    at: u64,
}

impl Writer {
    /// Writes bytes `from..`; false once the entry was abandoned.
    pub fn write(&mut self, from: u64, bytes: &[u8]) -> bool {
        let Some(f) = self.file.as_mut() else { return false };
        if from != self.at || f.write_all(bytes).is_err() {
            self.file = None;
            return false;
        }
        self.at += bytes.len() as u64;
        true
    }

    /// The song ended at `len` bytes; kept if all of it was written.
    pub fn finish(mut self, len: u64) -> bool {
        let Some(mut f) = self.file.take() else { return false };
        if self.at != len || f.flush().is_err() || f.seek(SeekFrom::End(0)).map_or(true, |e| e != len) {
            return false;
        }
        drop(f);
        let Some(part) = self.part.take() else { return false };
        self.store.finished(&self.key, &part, len)
    }

    /// [`Writer::finish`] and open the entry for reading; None if not kept or already evicted.
    pub fn finish_open(self, len: u64) -> Option<File> {
        let (store, key) = (self.store.clone(), self.key.clone());
        if !self.finish(len) {
            return None;
        }
        File::open(store.stream_path(&key)).ok().filter(|f| f.metadata().is_ok_and(|m| m.len() == len))
    }

    /// Bytes written.
    pub fn written(&self) -> u64 {
        self.at
    }

    /// Reads back what it holds (to resume an entry the fetching ahead began).
    pub fn read_back(&mut self) -> io::Result<Vec<u8>> {
        if let Some(f) = self.file.as_mut() {
            f.flush()?;
        }
        let bytes = fs::read(self.part.as_ref().ok_or_else(|| io::Error::other("the entry was handed on"))?)?;
        if bytes.len() as u64 != self.at {
            return Err(io::Error::other("the entry is not what it was"));
        }
        Ok(bytes)
    }

    /// Keeps the part for the next writer to resume.
    pub fn leave(mut self) {
        let Some(mut f) = self.file.take() else { return };
        if f.flush().is_err() {
            return;
        }
        self.store.held.lock().left.insert(self.key.clone());
        self.part = None;
    }
}

impl crate::ahead::Keeping for Store {
    fn kept(&self, key: &str) -> bool {
        self.peek(key).is_some()
    }

    fn busy(&self, key: &str) -> bool {
        self.writing(key)
    }

    fn entry(&self, key: &str) -> Option<Box<dyn crate::ahead::Entry>> {
        // The trait takes `&self`; the store always lives in an Arc.
        let me = self.me()?;
        me.writer(key).map(|w| Box::new(w) as Box<dyn crate::ahead::Entry>)
    }
}

impl crate::ahead::Entry for Writer {
    fn write(&mut self, from: u64, bytes: &[u8]) -> bool {
        Writer::write(self, from, bytes)
    }

    fn written(&self) -> u64 {
        self.at
    }

    fn finish(self: Box<Self>, len: u64) -> bool {
        Writer::finish(*self, len)
    }

    fn leave(self: Box<Self>) {
        Writer::leave(*self)
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(part) = &self.part {
            let _ = fs::remove_file(part);
        }
        self.store.held.lock().writing.remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary directory.
    fn dir(name: &str) -> nori_testdir::TempDir {
        nori_testdir::TempDir::new(name)
    }

    fn put(s: &Arc<Store>, key: &str, len: usize) {
        let mut w = s.writer(key).unwrap();
        assert!(w.write(0, &vec![7u8; len]));
        assert!(w.finish(len as u64));
    }

    #[test]
    fn keys_become_file_names_and_back() {
        for k in ["a1:0", "x:192opus", "dl:s/1", ".hidden", "é"] {
            assert_eq!(key_of(&file_name(k)).as_deref(), Some(k));
        }
        assert!(!file_name("../up").contains('/'));
        assert_eq!(key_of("a%3A0.part"), None, "half written is not an entry");
    }

    #[test]
    fn entry_only_when_whole() {
        let d = dir("store-whole");
        let s = Store::open(d.path(), 1 << 20, Box::new(Recent::default())).unwrap();
        let mut w = s.writer("a:0").unwrap();
        assert!(w.write(0, &[1; 100]));
        assert!(s.cached("a:0").is_none(), "not while it loads");
        assert!(!w.write(150, &[1; 10]), "a gap gives it up");
        assert!(!w.finish(110));
        assert!(s.cached("a:0").is_none());
        put(&s, "a:0", 100);
        assert_eq!(fs::read(s.cached("a:0").unwrap()).unwrap().len(), 100);
        let w = s.writer("b:0").unwrap();
        assert!(s.writer("b:0").is_none() && s.writing("b:0"), "one writer at a time: a second would truncate the first's file");
        drop(w);
        assert!(!s.writing("b:0"));
        assert!(fs::read_dir(s.dir.join(STREAM)).unwrap().count() == 1, "an entry let go leaves nothing behind");
    }

    #[test]
    fn evicts_leftovers_then_least_recently_used() {
        let d = dir("store-limit");
        let s = Store::open(d.path(), 1000, Box::new(Recent::default())).unwrap();
        put(&s, "old:0", 300);
        drop(s);
        let s = Store::open(d.path(), 1000, Box::new(Recent::default())).unwrap();
        put(&s, "a:0", 300);
        put(&s, "b:0", 300);
        put(&s, "c:0", 300);
        assert!(s.cached("old:0").is_none(), "what this run never used went first");
        assert!(s.cached("b:0").is_some() && s.cached("c:0").is_some());
        put(&s, "d:0", 300);
        assert!(s.cached("a:0").is_none(), "then a, the least recently used");
        assert!(s.cached("b:0").is_some() && s.cached("c:0").is_some() && s.cached("d:0").is_some());
        s.set_limit(400);
        assert_eq!(s.cache_bytes(), 300);
        assert!(s.cached("d:0").is_some(), "the latest used stays");
        s.clear_cache();
        assert_eq!(s.cache_bytes(), 0);
    }
}
