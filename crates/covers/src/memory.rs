//! LRU cache of decoded covers keyed by URL and size, bounded in bytes. Generic over the picture type
//! (RGBA [`Image`] or a platform handle whose byte size is given on insert).

use std::sync::Arc;

use parking_lot::Mutex;

use crate::disk::Key;
use crate::lru::Lru;

/// A decoded cover: tight rows of `width` x `height` RGBA pixels.
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Box<[u8]>,
}

/// A cover at one size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sized {
    pub key: Key,
    pub width: u32,
    pub height: u32,
}

pub struct MemoryCache<P = Arc<Image>> {
    limit: usize,
    kept: Mutex<Lru<Sized, P>>,
}

impl<P: Clone> MemoryCache<P> {
    pub fn new(limit: usize) -> MemoryCache<P> {
        MemoryCache { limit, kept: Mutex::new(Lru::default()) }
    }

    /// Looks up a cover; a hit marks it most recently used.
    pub fn get(&self, key: &Sized) -> Option<P> {
        if self.limit == 0 {
            return None;
        }
        self.kept.lock().touch(*key).cloned()
    }

    /// Inserts `picture` of `size` bytes, evicting least recently used entries until it fits. A picture
    /// larger than the limit is dropped.
    pub fn put(&self, key: Sized, picture: P, size: usize) {
        if size > self.limit {
            return;
        }
        let mut k = self.kept.lock();
        k.insert(key, picture, size as u64);
        k.fit(self.limit as u64, |_| {});
    }

    /// Drops everything (low-memory signal).
    pub fn clear(&self) {
        *self.kept.lock() = Lru::default();
    }

    pub fn bytes(&self) -> usize {
        self.kept.lock().bytes as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(side: u32) -> Arc<Image> {
        Arc::new(Image { width: side, height: side, pixels: vec![0; (side * side * 4) as usize].into() })
    }

    fn at(url: &str, side: u32) -> Sized {
        Sized { key: Key::of(url), width: side, height: side }
    }

    fn put(m: &MemoryCache, url: &str, side: u32) {
        let i = image(side);
        let size = i.pixels.len();
        m.put(at(url, side), i, size);
    }

    #[test]
    fn evicts_least_recently_used() {
        // Room for three 4x4 covers (64 bytes each).
        let m = MemoryCache::new(200);
        put(&m, "a", 4);
        put(&m, "b", 4);
        put(&m, "c", 4);
        assert!(m.get(&at("a", 4)).is_some());
        put(&m, "d", 4);
        assert!(m.get(&at("b", 4)).is_none());
        assert!(m.get(&at("a", 4)).is_some() && m.get(&at("c", 4)).is_some() && m.get(&at("d", 4)).is_some());
        assert_eq!(m.bytes(), 192);
        // Sizes are separate entries.
        assert!(m.get(&at("a", 2)).is_none());
        // A bigger cover evicts as many as it needs.
        put(&m, "e", 6);
        assert_eq!(m.bytes(), 144);
        assert!(m.get(&at("e", 6)).is_some());
        // Larger than the limit: dropped, nothing evicted.
        put(&m, "f", 8);
        assert!(m.get(&at("f", 8)).is_none() && m.get(&at("e", 6)).is_some());
        m.clear();
        assert_eq!(m.bytes(), 0);
    }

    #[test]
    fn zero_limit_keeps_nothing() {
        let m = MemoryCache::new(0);
        put(&m, "a", 1);
        assert!(m.get(&at("a", 1)).is_none());
        assert_eq!(m.bytes(), 0);
    }
}
