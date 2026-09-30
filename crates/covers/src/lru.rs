//! Least recently used bookkeeping for the memory and disk caches: entries by key with their byte sizes,
//! in order of use.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

pub(crate) struct Lru<K, V> {
    /// Value, bytes and last-use clock by key.
    entries: HashMap<K, (V, u64, u64)>,
    /// Last-use clock -> key, oldest first.
    order: BTreeMap<u64, K>,
    pub(crate) bytes: u64,
    clock: u64,
}

impl<K, V> Default for Lru<K, V> {
    fn default() -> Self {
        Lru { entries: HashMap::new(), order: BTreeMap::new(), bytes: 0, clock: 0 }
    }
}

impl<K: Copy + Eq + Hash, V> Lru<K, V> {
    /// Marks `key` used; its value, or None if not kept.
    pub(crate) fn touch(&mut self, key: K) -> Option<&mut V> {
        let (value, _, used) = self.entries.get_mut(&key)?;
        self.order.remove(used);
        self.clock += 1;
        *used = self.clock;
        self.order.insert(self.clock, key);
        Some(value)
    }

    /// Keeps `value` of `bytes` as the most recently used, replacing any under `key`.
    pub(crate) fn insert(&mut self, key: K, value: V, bytes: u64) {
        self.remove(key);
        self.clock += 1;
        self.entries.insert(key, (value, bytes, self.clock));
        self.order.insert(self.clock, key);
        self.bytes += bytes;
    }

    pub(crate) fn remove(&mut self, key: K) -> Option<V> {
        let (value, bytes, used) = self.entries.remove(&key)?;
        self.order.remove(&used);
        self.bytes -= bytes;
        Some(value)
    }

    pub(crate) fn get(&self, key: K) -> Option<&V> {
        self.entries.get(&key).map(|e| &e.0)
    }

    /// Drops the least recently used entries until the rest fits `limit`, handing each key to `gone`.
    pub(crate) fn fit(&mut self, limit: u64, mut gone: impl FnMut(K)) {
        while self.bytes > limit {
            let Some((_, key)) = self.order.pop_first() else { break };
            let (_, bytes, _) = self.entries.remove(&key).expect("every key in the order is kept");
            self.bytes -= bytes;
            gone(key);
        }
    }

    /// Every key with its bytes.
    pub(crate) fn sizes(&self) -> impl Iterator<Item = (&K, u64)> {
        self.entries.iter().map(|(k, e)| (k, e.1))
    }
}
