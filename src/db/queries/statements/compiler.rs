//! Reads and writes of the compiler layer (`src/compiler/`): the rows it
//! compares with SCIP — by row id, so a correction touches exactly the row
//! it judged — and one transaction that applies its verdicts.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, params};

use super::QueryBuilder;
use super::external_edges::{COLUMNS, ExternalEdge, external_edge_from_row};
use super::rows::{edge_from_row, unresolved_from_row};
use crate::error::Result;
use crate::types::{Edge, EdgeKind, Language, Metadata, Node, Provenance, UnresolvedReference};

/// An edge with its row id.
#[derive(Debug, Clone)]
pub struct EdgeRow {
    pub id: i64,
    pub edge: Edge,
}

/// An unresolved reference with its row id.
#[derive(Debug, Clone)]
pub struct UnresolvedRow {
    pub id: i64,
    pub reference: UnresolvedReference,
}

/// An external edge with its row id.
#[derive(Debug, Clone)]
pub struct ExternalEdgeRow {
    pub id: i64,
    pub edge: ExternalEdge,
}

/// A new target, kind and provenance for an edge row.
#[derive(Debug, Clone)]
pub struct EdgeUpdate {
    pub id: i64,
    pub target: String,
    pub kind: EdgeKind,
    pub metadata: Metadata,
    pub provenance: Option<Provenance>,
}

/// Everything one compiler pass changes, applied in one transaction by
/// [`QueryBuilder::apply_compiler_changes`].
#[derive(Debug, Clone, Default)]
pub struct CompilerChanges {
    /// Generated nodes no longer produced (their edges and symbol rows go
    /// with them).
    pub remove_generated: Vec<String>,
    /// Generated nodes to insert (or refresh).
    pub insert_generated: Vec<Node>,
    /// The complete `(node id, symbol, generated)` table.
    pub symbols: Vec<(String, String, bool)>,
    pub edge_updates: Vec<EdgeUpdate>,
    pub edge_deletes: Vec<i64>,
    pub edge_inserts: Vec<Edge>,
    pub unresolved_deletes: Vec<i64>,
    pub unresolved_inserts: Vec<UnresolvedReference>,
    pub external_deletes: Vec<i64>,
    pub external_inserts: Vec<ExternalEdge>,
}

impl QueryBuilder {
    /// Edges (not `contains`) whose source node is in `file_path`.
    pub fn get_edge_rows_from_file(&self, file_path: &str) -> Result<Vec<EdgeRow>> {
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT e.* FROM edges e JOIN nodes n ON n.id = e.source
             WHERE n.file_path = ?1 AND e.kind != 'contains'",
        )?;
        let rows = stmt.query_map([file_path], |row| {
            Ok(EdgeRow {
                id: row.get("id")?,
                edge: edge_from_row(row)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// Unresolved references made in `file_path` (by the denormalized path,
    /// or — for rows written without it — by their source node's file).
    pub fn get_unresolved_rows_for_file(&self, file_path: &str) -> Result<Vec<UnresolvedRow>> {
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT * FROM unresolved_refs WHERE file_path = ?1
             UNION ALL
             SELECT u.* FROM unresolved_refs u JOIN nodes n ON n.id = u.from_node_id
             WHERE u.file_path = '' AND n.file_path = ?1",
        )?;
        let rows = stmt.query_map([file_path], |row| {
            Ok(UnresolvedRow {
                id: row.get("id")?,
                reference: unresolved_from_row(row)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// External edges whose source node is in `file_path`.
    pub fn get_external_edge_rows_from_file(
        &self,
        file_path: &str,
    ) -> Result<Vec<ExternalEdgeRow>> {
        let mut stmt = self.db.conn().prepare_cached(&format!(
            "SELECT {COLUMNS}, id FROM external_edges
             WHERE source IN (SELECT id FROM nodes WHERE file_path = ?1)"
        ))?;
        let rows = stmt.query_map([file_path], |row| {
            let id: i64 = row.get(16)?;
            Ok(external_edge_from_row(row)?.map(|edge| ExternalEdgeRow { id, edge }))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.extend(row?);
        }
        Ok(out)
    }

    /// Heuristic `interface-impl` dispatch edges (trait/interface method →
    /// implementing method) out of `language` nodes.
    pub fn get_interface_dispatch_rows(&self, language: Language) -> Result<Vec<EdgeRow>> {
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT e.* FROM edges e JOIN nodes n ON n.id = e.source
             WHERE e.kind = 'calls' AND e.provenance = 'heuristic' AND n.language = ?1
               AND json_extract(e.metadata, '$.synthesizedBy') = 'interface-impl'",
        )?;
        let rows = stmt.query_map([language.as_str()], |row| {
            Ok(EdgeRow {
                id: row.get("id")?,
                edge: edge_from_row(row)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// Ids of the nodes a previous compiler pass generated.
    pub fn get_compiler_generated_ids(&self) -> Result<HashSet<String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT node_id FROM compiler_symbols WHERE generated = 1")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `(nodes with a compiler symbol, of which generated)`.
    pub fn get_compiler_symbol_counts(&self) -> Result<(u64, u64)> {
        let counts = self
            .db
            .conn()
            .query_row(
                "SELECT COUNT(*), IFNULL(SUM(generated), 0) FROM compiler_symbols",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0));
        Ok((counts.0.max(0) as u64, counts.1.max(0) as u64))
    }

    /// Edges the compiler layer verified, corrected or added.
    pub fn count_compiler_edges(&self) -> Result<u64> {
        let count: i64 = self.db.conn().query_row(
            "SELECT COUNT(*) FROM edges WHERE provenance = 'scip'",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// Apply one compiler pass's verdicts in one transaction.
    pub fn apply_compiler_changes(&self, changes: &CompilerChanges) -> Result<()> {
        {
            let mut cache = self.node_cache.borrow_mut();
            for id in &changes.remove_generated {
                cache.remove(id);
            }
        }
        self.db.transaction(|| {
            let conn = self.db.conn();
            {
                let mut delete = conn.prepare_cached("DELETE FROM nodes WHERE id = ?1")?;
                for id in &changes.remove_generated {
                    delete.execute([id])?;
                }
            }
            for node in &changes.insert_generated {
                let exists: bool = conn
                    .prepare_cached("SELECT 1 FROM nodes WHERE id = ?1")?
                    .exists([&node.id])?;
                if exists {
                    self.update_node(node)?;
                } else {
                    self.insert_node(node)?;
                }
            }
            conn.execute("DELETE FROM compiler_symbols", [])?;
            {
                let mut insert = conn.prepare_cached(
                    "INSERT OR REPLACE INTO compiler_symbols (node_id, symbol, generated)
                     SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM nodes WHERE id = ?1)",
                )?;
                for (node, symbol, generated) in &changes.symbols {
                    insert.execute(params![node, symbol, *generated as i64])?;
                }
            }
            {
                let mut delete = conn.prepare_cached("DELETE FROM edges WHERE id = ?1")?;
                for id in &changes.edge_deletes {
                    delete.execute([id])?;
                }
                let mut update = conn.prepare_cached(
                    "UPDATE OR IGNORE edges SET target = ?2, kind = ?3, metadata = ?4, provenance = ?5
                     WHERE id = ?1 AND EXISTS (SELECT 1 FROM nodes WHERE id = ?2)",
                )?;
                for change in &changes.edge_updates {
                    let metadata = serde_json::to_string(&change.metadata)?;
                    let updated = update.execute(params![
                        change.id,
                        change.target,
                        change.kind.as_str(),
                        metadata,
                        change.provenance.map(|p| p.as_str()),
                    ])?;
                    // The same edge already exists under its corrected
                    // target: this row is a duplicate now.
                    if updated == 0 {
                        delete.execute([change.id])?;
                    }
                }
            }
            self.insert_edges(&changes.edge_inserts)?;
            {
                let mut delete = conn.prepare_cached("DELETE FROM unresolved_refs WHERE id = ?1")?;
                for id in &changes.unresolved_deletes {
                    delete.execute([id])?;
                }
            }
            self.insert_unresolved_refs_batch(&changes.unresolved_inserts)?;
            {
                let mut delete = conn.prepare_cached("DELETE FROM external_edges WHERE id = ?1")?;
                for id in &changes.external_deletes {
                    delete.execute([id])?;
                }
            }
            self.insert_external_edges(&changes.external_inserts)?;
            Ok(())
        })
    }
}
