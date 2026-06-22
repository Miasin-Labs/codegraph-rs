use std::collections::{HashMap, HashSet};

use rusqlite::params_from_iter;

use super::super::rows::{node_from_row, placeholders};
use super::super::{QueryBuilder, SQLITE_PARAM_CHUNK_SIZE};
use crate::error::Result;
use crate::types::{Node, NodeKind};

impl QueryBuilder {
    /// Get a node by ID.
    pub fn get_node_by_id(&self, id: &str) -> Result<Option<Node>> {
        // Check cache first (get + LRU touch)
        if let Some(cached) = self.node_cache.borrow_mut().get_touch(id) {
            return Ok(Some(cached));
        }

        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE id = ?")?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => {
                let node = node_from_row(row)?;
                self.cache_node(node.clone());
                Ok(Some(node))
            }
            None => Ok(None),
        }
    }

    /// Batch lookup: fetch many nodes by ID in a single SQL round-trip.
    ///
    /// Replaces the N+1 pattern in graph traversal where every edge would
    /// trigger its own `get_node_by_id` call. For a function with 50 callers
    /// this collapses 50 point reads into one IN-list query (~10-50x
    /// faster end-to-end).
    ///
    /// Returns a map keyed by id so callers can preserve their own ordering
    /// (typically the order edges were returned from the graph). Missing IDs
    /// are simply absent from the map.
    ///
    /// Cache-aware: ids already in the LRU cache are served from memory and
    /// the SQL query only touches the misses.
    pub fn get_nodes_by_ids(&self, ids: &[String]) -> Result<HashMap<String, Node>> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }

        // Serve cache hits first; build the miss list for SQL.
        let mut misses: Vec<&String> = Vec::new();
        {
            let mut cache = self.node_cache.borrow_mut();
            for id in ids {
                match cache.get_touch(id) {
                    Some(node) => {
                        out.insert(id.clone(), node);
                    }
                    None => misses.push(id),
                }
            }
        }
        if misses.is_empty() {
            return Ok(out);
        }

        // Chunk under SQLite's parameter limit (default 999; chunk at 500
        // for safety and to keep the query plan simple).
        for chunk in misses.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT * FROM nodes WHERE id IN ({})",
                placeholders(chunk.len())
            );
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(
                params_from_iter(chunk.iter().map(|s| s.as_str())),
                node_from_row,
            )?;
            for row in rows {
                let node = row?;
                out.insert(node.id.clone(), node.clone());
                self.cache_node(node);
            }
        }
        Ok(out)
    }

    pub(in crate::db::queries::statements) fn get_existing_node_ids(
        &self,
        ids: &[&String],
    ) -> Result<HashSet<String>> {
        let mut out = HashSet::new();
        if ids.is_empty() {
            return Ok(out);
        }

        let mut unique_ids: Vec<&String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for id in ids {
            if seen.insert(id.as_str()) {
                unique_ids.push(id);
            }
        }

        for chunk in unique_ids.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT id FROM nodes WHERE id IN ({})",
                placeholders(chunk.len())
            );
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(chunk.iter().map(|s| s.as_str())), |row| {
                    row.get::<_, String>(0)
                })?;
            for row in rows {
                out.insert(row?);
            }
        }
        Ok(out)
    }

    /// Add a node to the cache, evicting oldest if needed.
    fn cache_node(&self, node: Node) {
        self.node_cache.borrow_mut().insert(node);
    }

    /// Clear the node cache.
    pub fn clear_cache(&self) {
        self.node_cache.borrow_mut().clear();
    }

    /// Get all nodes in a file.
    pub fn get_nodes_by_file(&self, file_path: &str) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE file_path = ? ORDER BY start_line")?;
        let rows = stmt.query_map([file_path], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get all nodes of a specific kind.
    pub fn get_nodes_by_kind(&self, kind: NodeKind) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE kind = ?")?;
        let rows = stmt.query_map([kind.as_str()], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Stream every node of a kind one at a time (lazy) instead of
    /// materializing them all like [`Self::get_nodes_by_kind`]. For unbounded
    /// kinds (`function`, `method`) on a symbol-dense project the full vector
    /// is gigabytes; the dynamic-edge synthesizers only scan-and-filter, so
    /// they iterate to keep memory O(1) in the node count rather than
    /// O(nodes) (#610).
    ///
    /// Rust deviation: a generator becomes a visitor callback. Return `true`
    /// from `f` to continue, `false` to stop early. Other queries on the same
    /// connection are safe to run from inside `f` (the cursor stays valid).
    pub fn iterate_nodes_by_kind(
        &self,
        kind: NodeKind,
        mut f: impl FnMut(Node) -> bool,
    ) -> Result<()> {
        // Fresh statement per call (not a cached one): an iterator holds an
        // open cursor, so a shared statement would conflict across
        // overlapping scans.
        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT * FROM nodes WHERE kind = ?")?;
        let mut rows = stmt.query([kind.as_str()])?;
        while let Some(row) = rows.next()? {
            let node = node_from_row(row)?;
            if !f(node) {
                break;
            }
        }
        Ok(())
    }

    /// Get all nodes in the database.
    pub fn get_all_nodes(&self) -> Result<Vec<Node>> {
        let mut stmt = self.db.conn().prepare_cached("SELECT * FROM nodes")?;
        let rows = stmt.query_map([], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get nodes by exact name match (uses idx_nodes_name index).
    pub fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE name = ?")?;
        let rows = stmt.query_map([name], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get nodes by exact qualified name match (uses idx_nodes_qualified_name index).
    pub fn get_nodes_by_qualified_name_exact(&self, qualified_name: &str) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE qualified_name = ?")?;
        let rows = stmt.query_map([qualified_name], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get nodes by lowercase name match (uses idx_nodes_lower_name expression index).
    pub fn get_nodes_by_lower_name(&self, lower_name: &str) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE lower(name) = ?")?;
        let rows = stmt.query_map([lower_name], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }
}
