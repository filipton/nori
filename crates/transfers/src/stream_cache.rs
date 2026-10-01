//! The stream cache's eviction order: keys an earlier run left and this one has not used go first, then
//! least recently used. The platform's cache holds the bytes; this names the next key to drop.

use std::collections::HashMap;

use parking_lot::Mutex;

/// Key use times: positive touch counts for this process, negative (counting down) for keys an earlier
/// run left, so those are always older.
#[derive(Default)]
struct CacheOrder {
    used: HashMap<String, i64>,
    clock: i64,
    left: i64,
}

impl CacheOrder {
    pub fn touch(&mut self, key: &str) {
        self.clock += 1;
        match self.used.get_mut(key) {
            Some(u) => *u = self.clock,
            None => {
                self.used.insert(key.to_string(), self.clock);
            }
        }
    }

    /// The keys the cache held at startup; unknown ones join as older than anything used.
    pub fn seed<'a>(&mut self, held: impl IntoIterator<Item = &'a str>) {
        for k in held {
            if !self.used.contains_key(k) {
                self.left -= 1;
                self.used.insert(k.to_string(), self.left);
            }
        }
    }

    /// The next key to drop, forgotten as it is returned (a later use makes it known again).
    pub(crate) fn pop_oldest(&mut self) -> Option<String> {
        let key = self.used.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone())?;
        self.used.remove(&key);
        Some(key)
    }

    /// Every cached copy of `id` at any quality, forgotten as they are returned (the caller drops them).
    pub fn copies(&mut self, id: &str) -> Vec<String> {
        let keys: Vec<String> = self.used.keys().filter(|k| nori_net::stream::is_copy(id, k)).cloned().collect();
        for k in &keys {
            self.used.remove(k);
        }
        keys
    }

    pub fn clear(&mut self) {
        self.used.clear();
    }
}

/// Global: the Android cache evictor reaches it through JNI entry points with no handle.
static ORDER: Mutex<Option<CacheOrder>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut CacheOrder) -> R) -> R {
    f(ORDER.lock().get_or_insert_with(CacheOrder::default))
}

pub fn touch(key: &str) {
    with(|o| o.touch(key))
}

pub fn seed<'a>(held: impl IntoIterator<Item = &'a str>) {
    with(|o| o.seed(held))
}

pub fn next() -> Option<String> {
    with(CacheOrder::pop_oldest)
}

pub fn copies(id: &str) -> Vec<String> {
    with(|o| o.copies(id))
}

pub fn clear() {
    with(CacheOrder::clear)
}

/// Evicts [`next`] keys through `remove` while `space()` exceeds `max_bytes`. Twin of
/// `ResizableEvictor.trimLocked` (core/.../playback/MediaSources.kt).
pub fn trim(max_bytes: i64, mut space: impl FnMut() -> i64, mut remove: impl FnMut(&str)) {
    while space() > max_bytes {
        let Some(key) = next() else { return };
        remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_leftovers_go_first_then_least_recent() {
        let mut o = CacheOrder::default();
        o.touch("a");
        o.touch("b");
        o.seed(["a", "old", "b"]);
        o.touch("a");
        assert_eq!([o.pop_oldest(), o.pop_oldest(), o.pop_oldest(), o.pop_oldest()], [Some("old".into()), Some("b".into()), Some("a".into()), None]);
        o.touch("b");
        assert_eq!(o.pop_oldest().as_deref(), Some("b"), "known again after use");
    }

    #[test]
    fn copies_take_every_quality_of_one_song() {
        let mut o = CacheOrder::default();
        o.touch("x:0");
        o.touch("x:192opus");
        o.touch("xy:0");
        o.seed(["x:320mp3"]);
        let mut c = o.copies("x");
        c.sort();
        assert_eq!(c, ["x:0", "x:192opus", "x:320mp3"]);
        assert_eq!(o.pop_oldest().as_deref(), Some("xy:0"), "only the copies were dropped");
    }
}
