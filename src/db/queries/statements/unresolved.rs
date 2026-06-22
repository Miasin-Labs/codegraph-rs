use std::collections::HashSet;

use rusqlite::params_from_iter;

use super::models::{ResolvedRefKey, UnresolvedBatch};
use super::rows::{placeholders, unresolved_from_row};
use super::{QueryBuilder, SQLITE_PARAM_CHUNK_SIZE};
use crate::error::Result;
use crate::types::{Language, UnresolvedReference};

impl QueryBuilder {
    // =========================================================================
    // Unresolved References
    // =========================================================================

    /// Insert an unresolved reference.
    pub fn insert_unresolved_ref(&self, reference: &UnresolvedReference) -> Result<()> {
        let mut stmt = self.db.conn().prepare_cached(
            "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col, candidates, file_path, language)
             VALUES (@fromNodeId, @referenceName, @referenceKind, @line, @col, @candidates, @filePath, @language)",
        )?;
        let candidates: Option<String> = match &reference.candidates {
            Some(c) => Some(serde_json::to_string(c)?),
            None => None,
        };
        stmt.execute(rusqlite::named_params! {
            "@fromNodeId": reference.from_node_id,
            "@referenceName": reference.reference_name,
            "@referenceKind": reference.reference_kind.as_str(),
            "@line": reference.line,
            "@col": reference.column,
            "@candidates": candidates,
            "@filePath": reference.file_path.as_deref().unwrap_or(""),
            "@language": reference.language.unwrap_or(Language::Unknown).as_str(),
        })?;
        Ok(())
    }

    /// Insert multiple unresolved references in a transaction.
    pub fn insert_unresolved_refs_batch(&self, refs: &[UnresolvedReference]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        self.db.transaction(|| {
            for reference in refs {
                self.insert_unresolved_ref(reference)?;
            }
            Ok(())
        })
    }

    /// Delete unresolved references from a node.
    pub fn delete_unresolved_by_node(&self, node_id: &str) -> Result<()> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("DELETE FROM unresolved_refs WHERE from_node_id = ?")?;
        stmt.execute([node_id])?;
        Ok(())
    }

    /// Get unresolved references by name (for resolution).
    pub fn get_unresolved_by_name(&self, name: &str) -> Result<Vec<UnresolvedReference>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM unresolved_refs WHERE reference_name = ?")?;
        let rows = stmt.query_map([name], unresolved_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get all unresolved references.
    pub fn get_unresolved_references(&self) -> Result<Vec<UnresolvedReference>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM unresolved_refs")?;
        let rows = stmt.query_map([], unresolved_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get the count of unresolved references without loading them into memory.
    pub fn get_unresolved_references_count(&self) -> Result<u64> {
        let count: i64 = self.db.conn().query_row(
            "SELECT COUNT(*) as count FROM unresolved_refs",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// Get a batch of unresolved references using LIMIT/OFFSET pagination.
    /// Used to process references in bounded memory chunks.
    pub fn get_unresolved_references_batch(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<UnresolvedReference>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM unresolved_refs LIMIT ? OFFSET ?")?;
        let rows = stmt.query_map([limit as i64, offset as i64], unresolved_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get unresolved references after a stable row id. This lets full-index
    /// resolution scan each currently-unresolved row once while keeping unresolved
    /// rows for future target-side sync repair.
    pub fn get_unresolved_references_batch_after_id(
        &self,
        after_id: i64,
        limit: usize,
    ) -> Result<UnresolvedBatch> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM unresolved_refs WHERE id > ? ORDER BY id LIMIT ?")?;
        let rows = stmt.query_map([after_id, limit as i64], |row| {
            let id: i64 = row.get("id")?;
            let reference = unresolved_from_row(row)?;
            Ok((id, reference))
        })?;
        let mut refs = Vec::new();
        let mut last_id = after_id;
        for row in rows {
            let (id, reference) = row?;
            last_id = id;
            refs.push(reference);
        }
        Ok(UnresolvedBatch { refs, last_id })
    }

    /// Get all tracked file paths (lightweight — no full FileRecord objects).
    pub fn get_all_file_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT path FROM files ORDER BY path")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get all distinct node names (lightweight — just name strings for pre-filtering).
    pub fn get_all_node_names(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT DISTINCT name FROM nodes")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get unresolved references scoped to specific file paths.
    /// Uses the idx_unresolved_file_path index for efficient lookup.
    pub fn get_unresolved_references_by_files(
        &self,
        file_paths: &[String],
    ) -> Result<Vec<UnresolvedReference>> {
        if file_paths.is_empty() {
            return Ok(Vec::new());
        }

        // Chunk under SQLite's parameter limit: the first sync of a very large
        // repo passes every changed file here, which an unbounded `IN (...)`
        // would bind as one parameter each — exceeding MAX_VARIABLE_NUMBER and
        // aborting with "too many SQL variables". (#540)
        let mut out = Vec::new();
        for chunk in file_paths.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT * FROM unresolved_refs WHERE file_path IN ({})",
                placeholders(chunk.len())
            );
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(
                params_from_iter(chunk.iter().map(|s| s.as_str())),
                unresolved_from_row,
            )?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// Get unresolved references whose target name may have changed.
    /// Used by incremental sync to repair refs in unchanged files after a new or
    /// modified file introduces a previously-missing symbol.
    pub fn get_unresolved_references_by_names(
        &self,
        names: &[String],
    ) -> Result<Vec<UnresolvedReference>> {
        let mut unique_names: Vec<&String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for name in names {
            if !name.is_empty() && seen.insert(name.as_str()) {
                unique_names.push(name);
            }
        }
        if unique_names.is_empty() {
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        for chunk in unique_names.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT * FROM unresolved_refs WHERE reference_name IN ({})",
                placeholders(chunk.len())
            );
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(
                params_from_iter(chunk.iter().map(|s| s.as_str())),
                unresolved_from_row,
            )?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// Delete all unresolved references (after resolution).
    pub fn clear_unresolved_references(&self) -> Result<()> {
        self.db.exec("DELETE FROM unresolved_refs")
    }

    /// Delete resolved references by their IDs.
    pub fn delete_resolved_references(&self, from_node_ids: &[String]) -> Result<()> {
        if from_node_ids.is_empty() {
            return Ok(());
        }
        let sql = format!(
            "DELETE FROM unresolved_refs WHERE from_node_id IN ({})",
            placeholders(from_node_ids.len())
        );
        let mut stmt = self.db.conn().prepare(&sql)?;
        stmt.execute(params_from_iter(from_node_ids.iter().map(|s| s.as_str())))?;
        Ok(())
    }

    /// Delete specific resolved references by (from_node_id, reference_name,
    /// reference_kind) tuples. More precise than
    /// [`Self::delete_resolved_references`] — only removes refs that were
    /// actually resolved.
    pub fn delete_specific_resolved_references(&self, refs: &[ResolvedRefKey]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        self.db.transaction(|| {
            let mut stmt = self.db.conn().prepare_cached(
                "DELETE FROM unresolved_refs WHERE from_node_id = ? AND reference_name = ? AND reference_kind = ?",
            )?;
            for r in refs {
                stmt.execute([
                    r.from_node_id.as_str(),
                    r.reference_name.as_str(),
                    r.reference_kind.as_str(),
                ])?;
            }
            Ok(())
        })
    }
}
