//! Simple LRU cache (port of `src/resolution/lru-cache.ts`).
//!
//! The TS version is backed by JavaScript's insertion-ordered `Map`.
//! Rust's `HashMap` has no iteration order, so this port keeps recency
//! explicitly: each entry carries a monotonically increasing stamp and a
//! `BTreeMap<stamp, key>` provides "first in iteration order" (= least
//! recently used) in O(log n).
//!
//! Used by ReferenceResolver to bound the per-resolver caches that
//! previously grew without limit and OOM'd on large codebases (20k+
//! files). Each cache is sized independently — see the resolver for
//! the chosen limits per cache type.
//!
//! Eviction is plain LRU: on `set`, if the cache is full, the
//! least-recently-used entry is evicted. Touching via `get` moves the
//! entry to the most-recently-used position so hot keys survive
//! eviction passes.
//!
//! API mapping from TS:
//! - `get(key)` takes `&mut self` (refreshing recency is a mutation in
//!   Rust; the TS `Map` hid it). Contexts exposing caches behind `&self`
//!   should wrap the cache in a `RefCell`.
//! - the `size` getter → [`LRUCache::len`].
//! - the constructor throws on a non-positive `max` → `new` panics with
//!   the same message (`max` is `usize`, so only `0` is invalid).

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

pub struct LRUCache<K, V> {
    max: usize,
    /// Monotonic recency counter; larger = more recently used.
    counter: u64,
    /// key → (recency stamp, value)
    store: HashMap<K, (u64, V)>,
    /// recency stamp → key; first entry is the LRU candidate.
    order: BTreeMap<u64, K>,
}

impl<K: Eq + Hash + Clone, V> LRUCache<K, V> {
    /// # Panics
    /// Panics if `max == 0` (TS: throws
    /// `LRUCache max must be a positive finite number, got {max}`).
    pub fn new(max: usize) -> Self {
        if max == 0 {
            panic!("LRUCache max must be a positive finite number, got {max}");
        }
        LRUCache {
            max,
            counter: 0,
            store: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    /// Number of entries currently cached (TS `size` getter).
    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Look up a key, refreshing its recency on a hit so hot keys survive
    /// eviction passes.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        // Refresh recency by re-stamping (TS: delete + re-insert).
        let entry = self.store.get_mut(key)?;
        self.counter += 1;
        self.order.remove(&entry.0);
        entry.0 = self.counter;
        self.order.insert(self.counter, key.clone());
        Some(&entry.1)
    }

    /// Membership test WITHOUT refreshing recency (mirrors TS `has`,
    /// which used `Map.has` and did not reorder).
    pub fn has(&self, key: &K) -> bool {
        self.store.contains_key(key)
    }

    pub fn set(&mut self, key: K, value: V) {
        if let Some((old_stamp, _)) = self.store.get(&key) {
            // Existing key: drop its old recency slot; re-inserted below.
            self.order.remove(old_stamp);
        } else if self.store.len() >= self.max {
            // Evict the oldest entry — first key in recency order.
            if let Some((_, oldest)) = self.order.pop_first() {
                self.store.remove(&oldest);
            }
        }
        self.counter += 1;
        self.order.insert(self.counter, key.clone());
        self.store.insert(key, (self.counter, value));
    }

    pub fn clear(&mut self) {
        self.store.clear();
        self.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "LRUCache max must be a positive finite number, got 0")]
    fn new_panics_on_zero_max() {
        let _ = LRUCache::<String, i32>::new(0);
    }

    #[test]
    fn stores_and_retrieves_values() {
        let mut c = LRUCache::new(3);
        c.set("a".to_string(), 1);
        c.set("b".to_string(), 2);
        assert_eq!(c.get(&"a".to_string()), Some(&1));
        assert_eq!(c.get(&"b".to_string()), Some(&2));
        assert_eq!(c.get(&"missing".to_string()), None);
        assert_eq!(c.len(), 2);
        assert!(!c.is_empty());
    }

    #[test]
    fn evicts_least_recently_used_on_overflow() {
        let mut c = LRUCache::new(2);
        c.set("a".to_string(), 1);
        c.set("b".to_string(), 2);
        c.set("c".to_string(), 3); // evicts "a" (oldest)
        assert_eq!(c.get(&"a".to_string()), None);
        assert_eq!(c.get(&"b".to_string()), Some(&2));
        assert_eq!(c.get(&"c".to_string()), Some(&3));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn get_refreshes_recency_so_hot_keys_survive() {
        let mut c = LRUCache::new(2);
        c.set("a".to_string(), 1);
        c.set("b".to_string(), 2);
        // Touch "a" — now "b" is the LRU.
        assert_eq!(c.get(&"a".to_string()), Some(&1));
        c.set("c".to_string(), 3); // evicts "b"
        assert_eq!(c.get(&"b".to_string()), None);
        assert_eq!(c.get(&"a".to_string()), Some(&1));
        assert_eq!(c.get(&"c".to_string()), Some(&3));
    }

    #[test]
    fn set_existing_key_updates_value_and_refreshes_recency() {
        let mut c = LRUCache::new(2);
        c.set("a".to_string(), 1);
        c.set("b".to_string(), 2);
        c.set("a".to_string(), 10); // refresh "a"; "b" becomes LRU
        assert_eq!(c.len(), 2);
        c.set("c".to_string(), 3); // evicts "b"
        assert_eq!(c.get(&"a".to_string()), Some(&10));
        assert_eq!(c.get(&"b".to_string()), None);
        assert_eq!(c.get(&"c".to_string()), Some(&3));
    }

    #[test]
    fn has_does_not_refresh_recency() {
        let mut c = LRUCache::new(2);
        c.set("a".to_string(), 1);
        c.set("b".to_string(), 2);
        // has() must NOT promote "a" (TS Map.has doesn't reorder).
        assert!(c.has(&"a".to_string()));
        c.set("c".to_string(), 3); // still evicts "a"
        assert!(!c.has(&"a".to_string()));
        assert!(c.has(&"b".to_string()));
        assert!(c.has(&"c".to_string()));
    }

    #[test]
    fn clear_empties_the_cache() {
        let mut c = LRUCache::new(2);
        c.set(1, "x");
        c.set(2, "y");
        c.clear();
        assert_eq!(c.len(), 0);
        assert!(c.is_empty());
        assert_eq!(c.get(&1), None);
        // Reusable after clear.
        c.set(3, "z");
        assert_eq!(c.get(&3), Some(&"z"));
    }

    #[test]
    fn capacity_one_replaces_on_every_distinct_set() {
        let mut c = LRUCache::new(1);
        c.set("a".to_string(), 1);
        c.set("b".to_string(), 2);
        assert_eq!(c.len(), 1);
        assert_eq!(c.get(&"a".to_string()), None);
        assert_eq!(c.get(&"b".to_string()), Some(&2));
    }
}
