use std::collections::{BTreeMap, HashMap};

use crate::types::Node;

pub(super) struct NodeLru {
    map: HashMap<String, (u64, Node)>,
    order: BTreeMap<u64, String>,
    seq: u64,
    cap: usize,
}

impl NodeLru {
    pub(super) fn new(cap: usize) -> Self {
        NodeLru {
            map: HashMap::new(),
            order: BTreeMap::new(),
            seq: 0,
            cap,
        }
    }

    /// Get + LRU-touch (TS delete-and-re-add on the Map).
    pub(super) fn get_touch(&mut self, id: &str) -> Option<Node> {
        let (old_seq, node) = self.map.get(id).map(|(s, n)| (*s, n.clone()))?;
        self.order.remove(&old_seq);
        self.seq += 1;
        self.order.insert(self.seq, id.to_string());
        if let Some(entry) = self.map.get_mut(id) {
            entry.0 = self.seq;
        }
        Some(node)
    }

    /// Add a node to the cache, evicting oldest if needed (TS `cacheNode`).
    pub(super) fn insert(&mut self, node: Node) {
        if let Some((old_seq, _)) = self.map.remove(&node.id) {
            self.order.remove(&old_seq);
        } else if self.map.len() >= self.cap {
            // Evict oldest (first) entry
            if let Some((&oldest, _)) = self.order.iter().next() {
                if let Some(id) = self.order.remove(&oldest) {
                    self.map.remove(&id);
                }
            }
        }
        self.seq += 1;
        self.order.insert(self.seq, node.id.clone());
        self.map.insert(node.id.clone(), (self.seq, node));
    }

    pub(super) fn remove(&mut self, id: &str) {
        if let Some((seq, _)) = self.map.remove(id) {
            self.order.remove(&seq);
        }
    }

    /// Invalidate cache for nodes in a file (TS `deleteNodesByFile` loop).
    pub(super) fn remove_by_file(&mut self, file_path: &str) {
        let ids: Vec<String> = self
            .map
            .iter()
            .filter(|(_, (_, n))| n.file_path == file_path)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.remove(&id);
        }
    }

    pub(super) fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}
