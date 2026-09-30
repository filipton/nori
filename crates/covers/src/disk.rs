//! LRU cache of raw cover files in a directory, bounded in bytes, one file per MD5 of the cover key.
//!
//! The index lives in memory and is rebuilt from the directory on open; a file's mtime is its first use
//! in a process (set at its first read, or its write), so the order survives restarts without a metadata
//! write on every read. Renames and deletes happen under the index lock
//! together with the index update so the two never disagree; writing and reading file contents happen
//! outside it.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use parking_lot::Mutex;

use crate::lru::Lru;

/// MD5 of a cover's key; the hex form is the file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(pub [u8; 16]);

impl Key {
    /// Key of the cover at `url`, ignoring the auth parameters (nori-core's `cover_key_parts`), so the
    /// same cover under another token or salt maps to the same file.
    pub fn of(url: &str) -> Key {
        let mut h = Md5::new();
        nori_core::covers::cover_key_parts(url, |p| h.update(p));
        Key(h.finalize().into())
    }

    /// Legacy key: MD5 of the full URL. Checked on a miss and migrated to [`Key::of`].
    pub fn of_address(url: &str) -> Key {
        Key(Md5::digest(url.as_bytes()).into())
    }

    fn name(&self) -> String {
        format!("{:032x}", u128::from_be_bytes(self.0))
    }

    fn parse(name: &str) -> Option<Key> {
        let hex = name.len() == 32 && name.bytes().all(|c| c.is_ascii_hexdigit());
        hex.then(|| Key(u128::from_str_radix(name, 16).expect("32 hex digits").to_be_bytes()))
    }
}

struct Entry {
    /// The put that wrote the file (its temp file's number; `u64::MAX` for a file found at open). A failed
    /// read only forgets the entry if it matches.
    written: u64,
    /// The file's mtime was set in this process.
    stamped: bool,
}

pub struct DiskCache {
    dir: PathBuf,
    limit: u64,
    index: Mutex<Lru<Key, Entry>>,
    /// Numbers puts, so concurrent writers never share a temp file.
    writes: AtomicU64,
}
impl DiskCache {
    /// Opens (creating) the cache in `dir` with a `limit` in bytes, deleting leftover temp files and
    /// anything over the limit.
    pub fn open(dir: impl Into<PathBuf>, limit: u64) -> io::Result<DiskCache> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let mut found: Vec<(SystemTime, Key, u64)> = Vec::new();
        for e in fs::read_dir(&dir)? {
            let e = e?;
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(meta) = e.metadata() else { continue };
            match Key::parse(name) {
                Some(key) if meta.is_file() => found.push((meta.modified().unwrap_or(UNIX_EPOCH), key, meta.len())),
                _ if name.ends_with(".tmp") => {
                    let _ = fs::remove_file(e.path());
                }
                _ => {}
            }
        }
        found.sort_unstable();
        let mut index = Lru::default();
        for (_, key, bytes) in found {
            index.insert(key, Entry { written: u64::MAX, stamped: false }, bytes);
        }
        let cache = DiskCache { dir, limit, index: Mutex::new(Lru::default()), writes: AtomicU64::new(0) };
        cache.trim(&mut index);
        *cache.index.lock() = index;
        Ok(cache)
    }

    /// File path for `key`, whether or not it exists.
    pub fn path(&self, key: Key) -> PathBuf {
        self.dir.join(key.name())
    }

    pub fn contains(&self, key: Key) -> bool {
        self.index.lock().get(key).is_some()
    }

    /// Reads `key` into `buf` (cleared first) and marks it used; false on a miss.
    pub fn read(&self, key: Key, buf: &mut Vec<u8>) -> bool {
        let Some((written, stamp)) = self.index.lock().touch(key).map(|e| (e.written, !std::mem::replace(&mut e.stamped, true))) else { return false };
        buf.clear();
        let path = self.path(key);
        let read = File::open(&path).and_then(|mut f| {
            f.read_to_end(buf)?;
            // Persists the LRU order across restarts.
            if stamp {
                let _ = f.set_modified(SystemTime::now());
            }
            Ok(())
        });
        if read.is_err() {
            // Deleted externally: forget it, unless a newer put replaced it meanwhile.
            let mut index = self.index.lock();
            if index.get(key).is_some_and(|e| e.written == written) {
                index.remove(key);
            }
            return false;
        }
        true
    }

    /// Stores `bytes` under `key`, then evicts over the limit. A file larger than the limit is dropped.
    pub fn put(&self, key: Key, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() as u64 > self.limit {
            return Ok(());
        }
        let path = self.path(key);
        let n = self.writes.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("{}-{n}.tmp", std::process::id()));
        let written = File::create(&tmp).and_then(|mut f| f.write_all(bytes)).and_then(|_| {
            let mut index = self.index.lock();
            fs::rename(&tmp, &path)?;
            index.insert(key, Entry { written: n, stamped: true }, bytes.len() as u64);
            self.trim(&mut index);
            Ok(())
        });
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
    }

    pub fn remove(&self, key: Key) {
        let mut index = self.index.lock();
        if index.remove(key).is_some() {
            let _ = fs::remove_file(self.path(key));
        }
    }

    /// Deletes every cover.
    pub fn clear(&self) {
        let mut index = self.index.lock();
        for (key, _) in std::mem::take(&mut *index).sizes() {
            let _ = fs::remove_file(self.path(*key));
        }
    }

    pub fn bytes(&self) -> u64 {
        self.index.lock().bytes
    }

    /// Evicts over the limit; the caller holds the index lock.
    fn trim(&self, index: &mut Lru<Key, Entry>) {
        index.fit(self.limit, |key| {
            let _ = fs::remove_file(self.path(key));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> nori_testdir::TempDir {
        nori_testdir::TempDir::new(&format!("covers-{name}"))
    }

    #[test]
    fn key_file_name_round_trips() {
        let k = Key::of("http://x/rest/getCoverArt?id=1&size=320");
        assert_eq!(Key::parse(&k.name()), Some(k));
        assert_eq!(Key::parse("nope"), None);
        assert_eq!(Key::parse("zz000000000000000000000000000000"), None);
    }

    #[test]
    fn evicts_least_recently_used_and_read_counts_as_use() {
        let d = dir("lru");
        let c = DiskCache::open(d.path(), 30).unwrap();
        let (a, b, x) = (Key::of("a"), Key::of("b"), Key::of("c"));
        c.put(a, &[1; 10]).unwrap();
        c.put(b, &[2; 10]).unwrap();
        c.put(x, &[3; 10]).unwrap();
        let mut buf = Vec::new();
        assert!(c.read(a, &mut buf));
        assert_eq!(buf, [1; 10]);
        // b is now the oldest, a having been read.
        c.put(Key::of("d"), &[4; 10]).unwrap();
        assert!(!c.contains(b) && !c.path(b).exists());
        assert!(c.contains(a) && c.contains(x));
        assert_eq!(c.bytes(), 30);
        // Larger than the limit: dropped, nothing evicted.
        c.put(Key::of("e"), &[5; 31]).unwrap();
        assert!(!c.contains(Key::of("e")) && c.contains(a));
    }

    #[test]
    fn reopen_restores_lru_order_and_trims_to_new_limit() {
        let d = dir("reopen");
        {
            let c = DiskCache::open(d.path(), 100).unwrap();
            for (i, name) in ["a", "b", "c"].iter().enumerate() {
                c.put(Key::of(name), &[i as u8; 10]).unwrap();
                // Distinct mtimes regardless of file system resolution.
                File::options().write(true).open(c.path(Key::of(name))).unwrap().set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1000 + i as u64)).unwrap();
            }
            File::create(d.join("0123.5-1.tmp")).unwrap();
        }
        let c = DiskCache::open(d.path(), 20).unwrap();
        assert!(!c.contains(Key::of("a")) && c.contains(Key::of("b")) && c.contains(Key::of("c")));
        assert!(!d.join("0123.5-1.tmp").exists());
        assert_eq!(c.bytes(), 20);
    }

    #[test]
    fn a_file_is_stamped_once_per_process() {
        let d = dir("stamp");
        let old = UNIX_EPOCH + std::time::Duration::from_secs(1000);
        let age = |c: &DiskCache| File::options().write(true).open(c.path(Key::of("a"))).unwrap().set_modified(old).unwrap();
        {
            let c = DiskCache::open(d.path(), 100).unwrap();
            c.put(Key::of("a"), &[1; 10]).unwrap();
            age(&c);
        }
        let c = DiskCache::open(d.path(), 100).unwrap();
        let mtime = || fs::metadata(c.path(Key::of("a"))).unwrap().modified().unwrap();
        let mut buf = Vec::new();
        assert!(c.read(Key::of("a"), &mut buf));
        assert!(mtime() > old, "the first read marks the use");
        age(&c);
        assert!(c.read(Key::of("a"), &mut buf));
        assert_eq!(mtime(), old, "later reads write nothing");
    }

    #[test]
    fn externally_deleted_file_is_a_miss() {
        let d = dir("gone");
        let c = DiskCache::open(d.path(), 100).unwrap();
        c.put(Key::of("a"), &[1; 4]).unwrap();
        fs::remove_file(c.path(Key::of("a"))).unwrap();
        assert!(!c.read(Key::of("a"), &mut Vec::new()));
        assert_eq!(c.bytes(), 0);
    }

    /// Checks the index matches the directory: same files, same sizes, same total.
    fn agree(c: &DiskCache) -> Result<(), String> {
        let index = c.index.lock();
        let mut on_disk = std::collections::HashMap::new();
        for e in fs::read_dir(&c.dir).unwrap() {
            let e = e.unwrap();
            if let Some(key) = e.file_name().to_str().and_then(Key::parse) {
                on_disk.insert(key, e.metadata().unwrap().len());
            }
        }
        for (key, bytes) in index.sizes() {
            if on_disk.get(key) != Some(&bytes) {
                return Err(format!("{key:?} indexed at {bytes} bytes, on disk {:?}", on_disk.get(key)));
            }
        }
        if on_disk.len() != index.sizes().count() {
            return Err(format!("{} files on disk, {} indexed", on_disk.len(), index.sizes().count()));
        }
        if index.bytes != on_disk.values().sum::<u64>() {
            return Err(format!("{} bytes indexed, {} on disk", index.bytes, on_disk.values().sum::<u64>()));
        }
        Ok(())
    }

    #[test]
    fn concurrent_ops_keep_index_and_directory_in_sync() {
        let d = dir("race");
        let c = std::sync::Arc::new(DiskCache::open(d.path(), 60).unwrap());
        let keys: Vec<Key> = (0..3).map(|i| Key::of(&i.to_string())).collect();
        // Many short rounds: a later put of the same key would mask a race in one long run.
        for round in 0..300 {
            let go = std::sync::Arc::new(std::sync::Barrier::new(4));
            let threads: Vec<_> = (0..4)
                .map(|t| {
                    let (c, keys, go) = (c.clone(), keys.clone(), go.clone());
                    std::thread::spawn(move || {
                        let mut buf = Vec::new();
                        go.wait();
                        for i in 0..40usize {
                            let key = keys[(i * 7 + t + round) % keys.len()];
                            match (i + t) % 5 {
                                0..=2 => c.put(key, &vec![t as u8; 10 + t * 10]).unwrap(),
                                3 => {
                                    c.read(key, &mut buf);
                                }
                                _ => c.remove(key),
                            }
                        }
                    })
                })
                .collect();
            for t in threads {
                t.join().unwrap();
            }
            if let Err(e) = agree(&c) {
                panic!("round {round}: {e}");
            }
        }
    }

    #[test]
    fn clear_deletes_everything_and_cache_stays_usable() {
        let d = dir("clear");
        let c = DiskCache::open(d.path(), 100).unwrap();
        c.put(Key::of("a"), &[1; 4]).unwrap();
        c.put(Key::of("b"), &[2; 4]).unwrap();
        c.clear();
        assert_eq!((c.bytes(), fs::read_dir(&d).unwrap().count()), (0, 0));
        c.put(Key::of("c"), &[3; 4]).unwrap();
        assert!(c.contains(Key::of("c")) && c.bytes() == 4);
    }
}
