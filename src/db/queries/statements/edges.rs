use std::collections::HashSet;

use rusqlite::params_from_iter;
use rusqlite::types::Value;

use super::QueryBuilder;
use super::rows::edge_from_row;
use super::search::push_edge_kind_filter;
use crate::error::Result;
use crate::types::{Edge, EdgeKind};

impl QueryBuilder {
    // =========================================================================
    // Edge Operations
    // =========================================================================

    /// Insert a new edge.
    pub fn insert_edge(&self, edge: &Edge) -> Result<()> {
        let mut stmt = self.db.conn().prepare_cached(
            "INSERT OR IGNORE INTO edges (source, target, kind, metadata, line, col, provenance)
             VALUES (@source, @target, @kind, @metadata, @line, @col, @provenance)",
        )?;
        let metadata: Option<String> = match &edge.metadata {
            Some(m) => Some(serde_json::to_string(m)?),
            None => None,
        };
        stmt.execute(rusqlite::named_params! {
            "@source": edge.source,
            "@target": edge.target,
            "@kind": edge.kind.as_str(),
            "@metadata": metadata,
            "@line": edge.line,
            "@col": edge.column,
            "@provenance": edge.provenance.map(|p| p.as_str()),
        })?;
        Ok(())
    }

    /// Insert multiple edges in a transaction.
    /// Edges whose endpoints don't exist in the DB are skipped — endpoint
    /// existence is validated from the database, not the (possibly stale)
    /// node cache.
    pub fn insert_edges(&self, edges: &[Edge]) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }

        self.db.transaction(|| {
            let mut endpoint_ids: Vec<&String> = Vec::new();
            let mut seen: HashSet<&str> = HashSet::new();
            for edge in edges {
                if seen.insert(edge.source.as_str()) {
                    endpoint_ids.push(&edge.source);
                }
                if seen.insert(edge.target.as_str()) {
                    endpoint_ids.push(&edge.target);
                }
            }
            let existing_node_ids = self.get_existing_node_ids(&endpoint_ids)?;

            for edge in edges {
                if !existing_node_ids.contains(&edge.source)
                    || !existing_node_ids.contains(&edge.target)
                {
                    continue;
                }
                self.insert_edge(edge)?;
            }
            Ok(())
        })
    }

    /// Delete all edges from a source node.
    pub fn delete_edges_by_source(&self, source_id: &str) -> Result<()> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("DELETE FROM edges WHERE source = ?")?;
        stmt.execute([source_id])?;
        Ok(())
    }

    /// Get outgoing edges from a node.
    pub fn get_outgoing_edges(
        &self,
        source_id: &str,
        kinds: Option<&[EdgeKind]>,
        provenance: Option<&str>,
    ) -> Result<Vec<Edge>> {
        let has_kinds = kinds.map(|k| !k.is_empty()).unwrap_or(false);
        if has_kinds || provenance.is_some() {
            let mut sql = String::from("SELECT * FROM edges WHERE source = ?");
            let mut params: Vec<Value> = vec![Value::Text(source_id.to_string())];

            push_edge_kind_filter(&mut sql, &mut params, kinds);

            if let Some(p) = provenance {
                sql.push_str(" AND provenance = ?");
                params.push(Value::Text(p.to_string()));
            }

            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), edge_from_row)?;
            return rows.map(|r| r.map_err(Into::into)).collect();
        }

        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM edges WHERE source = ?")?;
        let rows = stmt.query_map([source_id], edge_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get outgoing edges from multiple source nodes in one query.
    pub fn get_outgoing_edges_for_sources(
        &self,
        source_ids: &[String],
        kinds: Option<&[EdgeKind]>,
    ) -> Result<Vec<Edge>> {
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json = serde_json::to_string(source_ids)?;
        let mut sql =
            String::from("SELECT * FROM edges WHERE source IN (SELECT value FROM json_each(?))");
        let mut params: Vec<Value> = vec![Value::Text(ids_json)];
        push_edge_kind_filter(&mut sql, &mut params, kinds);

        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), edge_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get incoming edges to a node.
    pub fn get_incoming_edges(
        &self,
        target_id: &str,
        kinds: Option<&[EdgeKind]>,
    ) -> Result<Vec<Edge>> {
        let has_kinds = kinds.map(|k| !k.is_empty()).unwrap_or(false);
        if has_kinds {
            let mut sql = String::from("SELECT * FROM edges WHERE target = ?");
            let mut params: Vec<Value> = vec![Value::Text(target_id.to_string())];
            push_edge_kind_filter(&mut sql, &mut params, kinds);
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), edge_from_row)?;
            return rows.map(|r| r.map_err(Into::into)).collect();
        }

        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM edges WHERE target = ?")?;
        let rows = stmt.query_map([target_id], edge_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get incoming edges to multiple target nodes in one query.
    pub fn get_incoming_edges_for_targets(
        &self,
        target_ids: &[String],
        kinds: Option<&[EdgeKind]>,
    ) -> Result<Vec<Edge>> {
        if target_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json = serde_json::to_string(target_ids)?;
        let mut sql =
            String::from("SELECT * FROM edges WHERE target IN (SELECT value FROM json_each(?))");
        let mut params: Vec<Value> = vec![Value::Text(ids_json)];
        push_edge_kind_filter(&mut sql, &mut params, kinds);

        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), edge_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Find all edges where both source and target are in the given node set.
    /// Useful for recovering inter-node connectivity after BFS.
    pub fn find_edges_between_nodes(
        &self,
        node_ids: &[String],
        kinds: Option<&[EdgeKind]>,
    ) -> Result<Vec<Edge>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids_json = serde_json::to_string(node_ids)?;
        let mut sql = String::from(
            "SELECT * FROM edges WHERE source IN (SELECT value FROM json_each(?)) AND target IN (SELECT value FROM json_each(?))",
        );
        let mut params: Vec<Value> = vec![Value::Text(ids_json.clone()), Value::Text(ids_json)];
        push_edge_kind_filter(&mut sql, &mut params, kinds);

        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), edge_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }
}
