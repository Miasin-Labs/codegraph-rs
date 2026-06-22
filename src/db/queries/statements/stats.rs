use std::collections::HashMap;

use super::QueryBuilder;
use super::models::NodeEdgeCount;
use crate::db::connection::now_ms;
use crate::error::Result;
use crate::types::GraphStats;

impl QueryBuilder {
    // =========================================================================
    // Statistics
    // =========================================================================

    /// Lightweight (nodes, edges) count snapshot. Used around an index/sync
    /// run to compute true additions across extraction + resolution +
    /// synthesis — the per-phase counter in the orchestrator only sees
    /// extraction's contribution, which is why the CLI summary under-reported
    /// the edge count (resolution + synthesizer edges were invisible).
    pub fn get_node_and_edge_count(&self) -> Result<NodeEdgeCount> {
        let (nodes, edges): (i64, i64) = self.db.conn().query_row(
            "SELECT (SELECT COUNT(*) FROM nodes) AS nodes, (SELECT COUNT(*) FROM edges) AS edges",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(NodeEdgeCount {
            nodes: nodes.max(0) as u64,
            edges: edges.max(0) as u64,
        })
    }

    /// Get graph statistics.
    pub fn get_stats(&self) -> Result<GraphStats> {
        // Single query for all three aggregate counts
        let (node_count, edge_count, file_count): (i64, i64, i64) = self.db.conn().query_row(
            "SELECT
                   (SELECT COUNT(*) FROM nodes) AS node_count,
                   (SELECT COUNT(*) FROM edges) AS edge_count,
                   (SELECT COUNT(*) FROM files) AS file_count",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        let group_count = |sql: &str| -> Result<HashMap<String, u64>> {
            let mut stmt = self.db.conn().prepare_cached(sql)?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })?;
            let mut out = HashMap::new();
            for row in rows {
                let (key, count) = row?;
                out.insert(key, count);
            }
            Ok(out)
        };

        let nodes_by_kind = group_count("SELECT kind, COUNT(*) as count FROM nodes GROUP BY kind")?;
        let edges_by_kind = group_count("SELECT kind, COUNT(*) as count FROM edges GROUP BY kind")?;
        let files_by_language =
            group_count("SELECT language, COUNT(*) as count FROM files GROUP BY language")?;

        Ok(GraphStats {
            node_count: node_count.max(0) as u64,
            edge_count: edge_count.max(0) as u64,
            file_count: file_count.max(0) as u64,
            nodes_by_kind,
            edges_by_kind,
            files_by_language,
            db_size_bytes: 0, // Set by caller using DatabaseConnection::get_size()
            last_updated: now_ms(),
        })
    }

    // =========================================================================
    // Project Metadata
    // =========================================================================

    /// Get a metadata value by key.
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT value FROM project_metadata WHERE key = ?")?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Set a metadata key-value pair (upsert).
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        let mut stmt = self.db.conn().prepare_cached(
            "INSERT INTO project_metadata (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )?;
        stmt.execute(rusqlite::params![key, value, now_ms()])?;
        Ok(())
    }

    /// Get all metadata as a key-value map.
    pub fn get_all_metadata(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT key, value FROM project_metadata")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (key, value) = row?;
            out.insert(key, value);
        }
        Ok(out)
    }

    /// Clear all data from the database.
    pub fn clear(&self) -> Result<()> {
        self.node_cache.borrow_mut().clear();
        self.db.transaction(|| {
            self.db.exec("DELETE FROM unresolved_refs")?;
            self.db.exec("DELETE FROM edges")?;
            self.db.exec("DELETE FROM nodes")?;
            self.db.exec("DELETE FROM files")?;
            Ok(())
        })
    }
}

// =============================================================================
// SQL filter helpers
// =============================================================================
