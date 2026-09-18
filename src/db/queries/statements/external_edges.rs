//! External edges: references of this project resolved into another graph
//! (a dependency's shared shard or a linked project's index).
//!
//! The row carries the target's identity in that graph — graph kind + key,
//! node id, and the node's name, qualified name, kind, file and line — so a
//! reader can follow it without re-resolving, and the reference as written
//! (name, kind, position, metadata) so it can be turned back into an
//! unresolved reference when the target graph changes or disappears.

use std::collections::HashSet;

use rusqlite::{Row, params, params_from_iter};
use serde::{Deserialize, Serialize};

use super::rows::placeholders;
use super::{QueryBuilder, SQLITE_PARAM_CHUNK_SIZE};
use crate::error::Result;
use crate::types::{EdgeKind, Metadata, NodeKind};
use crate::utils::safe_json_parse;

/// Which kind of graph an external edge points into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalGraphKind {
    /// A dependency version's shard: key `<ecosystem>/<name>-<version>`
    /// (`crates/serde_json-1.0.150`), the directory under
    /// `codegraph_home()/deps/`.
    Dependency,
    /// A linked project's own index: key = its canonical checkout root (the
    /// atlas key).
    Project,
}

impl ExternalGraphKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dependency => "dependency",
            Self::Project => "project",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dependency" => Some(Self::Dependency),
            "project" => Some(Self::Project),
            _ => None,
        }
    }
}

/// One reference resolved into another graph.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalEdge {
    /// The referencing node, in this project.
    pub source: String,
    /// `calls`, `instantiates`, `references`, `implements`, …
    pub kind: EdgeKind,
    pub target_graph_kind: ExternalGraphKind,
    pub target_graph_key: String,
    /// The target's node id in that graph (when it was resolved; a rebuilt
    /// graph keeps ids stable while the item does not move — readers fall
    /// back to qualified name + kind + file when the id is gone).
    pub target_node_id: String,
    pub target_name: String,
    pub target_qualified_name: String,
    pub target_kind: NodeKind,
    /// Relative to that graph's root (a shard's source directory, a linked
    /// project's root).
    pub target_file_path: String,
    pub target_line: Option<u32>,
    /// The reference as written (`serde_json::from_str`, `from_str`).
    pub reference_name: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub confidence: f64,
    pub resolved_by: String,
    /// The reference's own metadata (receiver text and the like).
    pub metadata: Option<Metadata>,
}

/// One item of another graph, as an external edge records its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalTarget<'a> {
    pub id: &'a str,
    pub qualified_name: &'a str,
    pub kind: NodeKind,
    pub file_path: &'a str,
}

impl<'a> ExternalTarget<'a> {
    /// `node` of its own graph.
    pub fn of(node: &'a crate::types::Node) -> Self {
        Self {
            id: &node.id,
            qualified_name: &node.qualified_name,
            kind: node.kind,
            file_path: &node.file_path,
        }
    }
}

/// External edges per target graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalEdgeCount {
    pub graph_kind: ExternalGraphKind,
    pub graph_key: String,
    pub edges: u64,
}

const COLUMNS: &str = "source, kind, target_graph_kind, target_graph_key, target_node_id, \
     target_name, target_qualified_name, target_kind, target_file_path, target_line, \
     reference_name, line, col, confidence, resolved_by, metadata";

fn external_edge_from_row(row: &Row<'_>) -> rusqlite::Result<Option<ExternalEdge>> {
    let kind: String = row.get(1)?;
    let graph_kind: String = row.get(2)?;
    let target_kind: String = row.get(7)?;
    let (Ok(kind), Some(graph_kind), Ok(target_kind)) = (
        kind.parse::<EdgeKind>(),
        ExternalGraphKind::parse(&graph_kind),
        target_kind.parse::<NodeKind>(),
    ) else {
        // A newer writer's kind: skipped, not an error.
        return Ok(None);
    };
    let metadata: Option<String> = row.get(15)?;
    Ok(Some(ExternalEdge {
        source: row.get(0)?,
        kind,
        target_graph_kind: graph_kind,
        target_graph_key: row.get(3)?,
        target_node_id: row.get(4)?,
        target_name: row.get(5)?,
        target_qualified_name: row.get(6)?,
        target_kind,
        target_file_path: row.get(8)?,
        target_line: row.get(9)?,
        reference_name: row.get(10)?,
        line: row.get(11)?,
        column: row.get(12)?,
        confidence: row.get(13)?,
        resolved_by: row.get(14)?,
        metadata: metadata.and_then(|text| safe_json_parse::<Option<Metadata>>(&text, None)),
    }))
}

impl QueryBuilder {
    /// Store external edges (duplicates ignored); rows whose source node no
    /// longer exists are skipped. Returns how many were new.
    pub fn insert_external_edges(&self, edges: &[ExternalEdge]) -> Result<usize> {
        if edges.is_empty() {
            return Ok(0);
        }
        self.db.transaction(|| {
            let sources: Vec<&String> = edges.iter().map(|edge| &edge.source).collect();
            let existing = self.get_existing_node_ids(&sources)?;
            let now = crate::db::connection::now_ms();
            let mut stmt = self.db.conn().prepare_cached(
                "INSERT OR IGNORE INTO external_edges (source, kind, target_graph_kind,
                   target_graph_key, target_node_id, target_name, target_qualified_name,
                   target_kind, target_file_path, target_line, reference_name, line, col,
                   confidence, resolved_by, metadata, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            )?;
            let mut inserted = 0;
            for edge in edges.iter().filter(|edge| existing.contains(&edge.source)) {
                let metadata = edge
                    .metadata
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?;
                inserted += stmt.execute(params![
                    edge.source,
                    edge.kind.as_str(),
                    edge.target_graph_kind.as_str(),
                    edge.target_graph_key,
                    edge.target_node_id,
                    edge.target_name,
                    edge.target_qualified_name,
                    edge.target_kind.as_str(),
                    edge.target_file_path,
                    edge.target_line,
                    edge.reference_name,
                    edge.line,
                    edge.column,
                    edge.confidence,
                    edge.resolved_by,
                    metadata,
                    now,
                ])?;
            }
            Ok(inserted)
        })
    }

    /// External edges leaving `source_ids` (every kind), ordered by source
    /// position.
    pub fn get_outgoing_external_edges(&self, source_ids: &[String]) -> Result<Vec<ExternalEdge>> {
        let mut out = Vec::new();
        for chunk in source_ids.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT {COLUMNS} FROM external_edges WHERE source IN ({})
                 ORDER BY source, IFNULL(line, -1), IFNULL(col, -1), id",
                placeholders(chunk.len())
            );
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(
                params_from_iter(chunk.iter().map(String::as_str)),
                external_edge_from_row,
            )?;
            for row in rows {
                out.extend(row?);
            }
        }
        Ok(out)
    }

    /// External edges into `graph_key`, all of them or only those into
    /// `target_node_id` (who in this project uses that item).
    pub fn get_external_edges_into(
        &self,
        graph_key: &str,
        target_node_id: Option<&str>,
    ) -> Result<Vec<ExternalEdge>> {
        let mut stmt = self.db.conn().prepare_cached(&format!(
            "SELECT {COLUMNS} FROM external_edges
             WHERE target_graph_key = ?1 AND (?2 IS NULL OR target_node_id = ?2)
             ORDER BY target_node_id, source, id"
        ))?;
        let rows = stmt.query_map(params![graph_key, target_node_id], external_edge_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.extend(row?);
        }
        Ok(out)
    }

    /// External edges into `graph_key` that point at one of `targets` —
    /// by id, or, for edges recorded before that graph was rebuilt and ids
    /// moved, by qualified name + kind + file — at most `limit`, ordered
    /// by source and position.
    pub fn get_external_edges_into_targets(
        &self,
        graph_key: &str,
        targets: &[ExternalTarget<'_>],
        limit: usize,
    ) -> Result<Vec<ExternalEdge>> {
        /// Four parameters per target plus the key stay well under
        /// SQLite's variable limit.
        const TARGET_CHUNK: usize = 100;
        let mut out = Vec::new();
        for chunk in targets.chunks(TARGET_CHUNK) {
            if out.len() >= limit {
                break;
            }
            let ids = placeholders(chunk.len());
            let triples = vec!["(?, ?, ?)"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT {COLUMNS} FROM external_edges
                 WHERE target_graph_key = ?
                   AND (target_node_id IN ({ids})
                        OR (target_qualified_name, target_kind, target_file_path) IN (VALUES {triples}))
                 ORDER BY source, IFNULL(line, -1), IFNULL(col, -1), id
                 LIMIT ?"
            );
            let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(2 + chunk.len() * 4);
            values.push(graph_key.to_string().into());
            values.extend(chunk.iter().map(|target| target.id.to_string().into()));
            for target in chunk {
                values.push(target.qualified_name.to_string().into());
                values.push(target.kind.as_str().to_string().into());
                values.push(target.file_path.to_string().into());
            }
            values.push(((limit - out.len()) as i64).into());
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(values), external_edge_from_row)?;
            for row in rows {
                out.extend(row?);
            }
        }
        Ok(out)
    }

    /// Every external edge (tests, reports).
    pub fn get_all_external_edges(&self) -> Result<Vec<ExternalEdge>> {
        let mut stmt = self.db.conn().prepare(&format!(
            "SELECT {COLUMNS} FROM external_edges ORDER BY target_graph_key, source, id"
        ))?;
        let rows = stmt.query_map([], external_edge_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.extend(row?);
        }
        Ok(out)
    }

    /// How many external edges go into each graph.
    pub fn count_external_edges(&self) -> Result<Vec<ExternalEdgeCount>> {
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT target_graph_kind, target_graph_key, COUNT(*) FROM external_edges
             GROUP BY target_graph_kind, target_graph_key ORDER BY 3 DESC, 2",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (kind, key, edges) = row?;
            if let Some(graph_kind) = ExternalGraphKind::parse(&kind) {
                out.push(ExternalEdgeCount {
                    graph_kind,
                    graph_key: key,
                    edges: edges.max(0) as u64,
                });
            }
        }
        Ok(out)
    }

    /// The graph keys external edges currently point into.
    pub fn external_edge_graph_keys(&self) -> Result<HashSet<String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT DISTINCT target_graph_key FROM external_edges")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<HashSet<_>>>()?)
    }

    /// Turn every external edge into `graph_keys` back into an unresolved
    /// reference (as written, with its metadata) and drop the edge, in one
    /// transaction. Returns how many were restored.
    pub fn restore_external_edges(&self, graph_keys: &[String]) -> Result<usize> {
        if graph_keys.is_empty() {
            return Ok(0);
        }
        self.db.transaction(|| {
            let mut restored = 0;
            for chunk in graph_keys.chunks(SQLITE_PARAM_CHUNK_SIZE) {
                let marks = placeholders(chunk.len());
                // `instantiates` was a call the target type answered.
                restored += self.db.conn().execute(
                    &format!(
                        "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind,
                           line, col, metadata, file_path, language)
                         SELECT e.source, e.reference_name,
                                CASE e.kind WHEN 'instantiates' THEN 'calls' ELSE e.kind END,
                                IFNULL(e.line, n.start_line), IFNULL(e.col, n.start_column),
                                e.metadata, n.file_path, n.language
                         FROM external_edges e JOIN nodes n ON n.id = e.source
                         WHERE e.target_graph_key IN ({marks})"
                    ),
                    params_from_iter(chunk.iter().map(String::as_str)),
                )?;
                self.db.conn().execute(
                    &format!("DELETE FROM external_edges WHERE target_graph_key IN ({marks})"),
                    params_from_iter(chunk.iter().map(String::as_str)),
                )?;
            }
            Ok(restored)
        })
    }
}
