//! Small insertion-ordered collections for JS Map/Set parity.

use std::collections::{HashMap, HashSet};

/// Insertion-ordered string-keyed map (JS `Map` parity: `set` on an existing
/// key updates the value but keeps the original position; iteration follows
/// first-insertion order).
pub(super) struct OrderedMap<V> {
    keys: Vec<String>,
    map: HashMap<String, V>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> OrderedMap<V> {
    pub(super) fn new() -> Self {
        OrderedMap {
            keys: Vec::new(),
            map: HashMap::new(),
        }
    }
    pub(super) fn len(&self) -> usize {
        self.keys.len()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub(super) fn get(&self, k: &str) -> Option<&V> {
        self.map.get(k)
    }
    pub(super) fn contains_key(&self, k: &str) -> bool {
        self.map.contains_key(k)
    }
    pub(super) fn set(&mut self, k: &str, v: V) {
        if !self.map.contains_key(k) {
            self.keys.push(k.to_string());
        }
        self.map.insert(k.to_string(), v);
    }
    pub(super) fn entry_or_default(&mut self, k: &str) -> &mut V
    where
        V: Default,
    {
        if !self.map.contains_key(k) {
            self.keys.push(k.to_string());
            self.map.insert(k.to_string(), V::default());
        }
        self.map.get_mut(k).expect("just inserted")
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.keys
            .iter()
            .map(move |k| (k.as_str(), self.map.get(k).expect("key tracked")))
    }
}

/// Insertion-ordered string set (JS `Set` parity).
#[derive(Default)]
pub(super) struct OrderedSet {
    items: Vec<String>,
    seen: HashSet<String>,
}

impl OrderedSet {
    pub(super) fn add(&mut self, v: &str) {
        if self.seen.insert(v.to_string()) {
            self.items.push(v.to_string());
        }
    }
    pub(super) fn len(&self) -> usize {
        self.items.len()
    }
    pub(super) fn iter(&self) -> std::slice::Iter<'_, String> {
        self.items.iter()
    }
}
