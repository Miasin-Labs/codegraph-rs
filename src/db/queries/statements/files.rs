use std::collections::{HashMap, HashSet};

use super::rows::{file_from_row, placeholders};
use super::{QueryBuilder, SQLITE_PARAM_CHUNK_SIZE};
use crate::error::Result;
use crate::extraction::{GeneratedFilePredicate, GenerationStatus};
use crate::types::{FileRecord, Language};

impl QueryBuilder {
    // =========================================================================
    // File Operations
    // =========================================================================

    /// Insert or update a file record.
    pub fn upsert_file(&self, file: &FileRecord) -> Result<()> {
        let mut stmt = self.db.conn().prepare_cached(
            "INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count, errors)
             VALUES (@path, @contentHash, @language, @size, @modifiedAt, @indexedAt, @nodeCount, @errors)
             ON CONFLICT(path) DO UPDATE SET
               content_hash = @contentHash,
               language = @language,
               size = @size,
               modified_at = @modifiedAt,
               indexed_at = @indexedAt,
               node_count = @nodeCount,
               errors = @errors",
        )?;
        let errors: Option<String> = match &file.errors {
            Some(e) => Some(serde_json::to_string(e)?),
            None => None,
        };
        stmt.execute(rusqlite::named_params! {
            "@path": file.path,
            "@contentHash": file.content_hash,
            "@language": file.language.as_str(),
            "@size": file.size as i64,
            "@modifiedAt": file.modified_at,
            "@indexedAt": file.indexed_at,
            "@nodeCount": file.node_count,
            "@errors": errors,
        })?;
        Ok(())
    }

    /// Insert or update a file record and rewrite its generated-source verdict.
    pub fn upsert_file_with_generation(
        &self,
        file: &FileRecord,
        generation: GenerationStatus,
    ) -> Result<()> {
        let mut stmt = self.db.conn().prepare_cached(
            "INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count, errors, generated)
             VALUES (@path, @contentHash, @language, @size, @modifiedAt, @indexedAt, @nodeCount, @errors, @generated)
             ON CONFLICT(path) DO UPDATE SET
               content_hash = @contentHash,
               language = @language,
               size = @size,
               modified_at = @modifiedAt,
               indexed_at = @indexedAt,
               node_count = @nodeCount,
               errors = @errors,
               generated = @generated",
        )?;
        let errors = file
            .errors
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        stmt.execute(rusqlite::named_params! {
            "@path": file.path,
            "@contentHash": file.content_hash,
            "@language": file.language.as_str(),
            "@size": file.size as i64,
            "@modifiedAt": file.modified_at,
            "@indexedAt": file.indexed_at,
            "@nodeCount": file.node_count,
            "@errors": errors,
            "@generated": generation.as_i64(),
        })?;
        Ok(())
    }

    /// Build a generated-file predicate over only the supplied candidate paths.
    pub fn generated_file_predicate(
        &self,
        file_paths: &[String],
    ) -> Result<GeneratedFilePredicate> {
        let unique: HashSet<&str> = file_paths.iter().map(String::as_str).collect();
        let mut found = HashSet::new();
        for chunk in unique
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .chunks(SQLITE_PARAM_CHUNK_SIZE)
        {
            let sql = format!(
                "SELECT path FROM files WHERE generated = 1 AND path IN ({})",
                placeholders(chunk.len())
            );
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                found.insert(row?);
            }
        }
        Ok(GeneratedFilePredicate::new(found))
    }

    /// Count files whose persisted source metadata marks them generated.
    pub fn count_generated_files(&self) -> Result<i64> {
        let count = self.db.conn().query_row(
            "SELECT COUNT(*) FROM files WHERE generated = 1",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count)
    }

    /// Delete a file record and its nodes.
    pub fn delete_file(&self, file_path: &str) -> Result<()> {
        self.db.transaction(|| {
            self.delete_nodes_by_file(file_path)?;
            let mut stmt = self
                .db
                .conn()
                .prepare_cached("DELETE FROM files WHERE path = ?")?;
            stmt.execute([file_path])?;
            Ok(())
        })
    }

    /// Get a file record by path.
    pub fn get_file_by_path(&self, file_path: &str) -> Result<Option<FileRecord>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM files WHERE path = ?")?;
        let mut rows = stmt.query([file_path])?;
        match rows.next()? {
            Some(row) => Ok(Some(file_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Path and language of every tracked file at or under `prefix` (every
    /// file when it is empty), in path order: a range scan on the primary
    /// key that reads two columns, for callers that only need to know which
    /// files exist (the text search walks 70K-file indexes with it).
    pub fn get_file_languages_under(&self, prefix: &str) -> Result<Vec<(String, Language)>> {
        let row = |row: &rusqlite::Row<'_>| {
            let language: String = row.get(1)?;
            Ok((
                row.get::<_, String>(0)?,
                language.parse().unwrap_or(Language::Unknown),
            ))
        };
        let conn = self.db.conn();
        if prefix.is_empty() {
            let mut stmt = conn.prepare_cached("SELECT path, language FROM files ORDER BY path")?;
            let rows = stmt.query_map([], row)?;
            return rows.map(|r| r.map_err(Into::into)).collect();
        }
        // `prefix/` <= path < `prefix0` is exactly the paths under the
        // directory ('0' is the byte after '/').
        let mut stmt = conn.prepare_cached(
            "SELECT path, language FROM files
             WHERE path = ?1 OR (path >= ?2 AND path < ?3)
             ORDER BY path",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![prefix, format!("{prefix}/"), format!("{prefix}0")],
            row,
        )?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get all tracked files.
    pub fn get_all_files(&self) -> Result<Vec<FileRecord>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM files ORDER BY path")?;
        let rows = stmt.query_map([], file_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Return files with non-containment edges into any node in `file_path`.
    pub fn get_dependent_file_paths(&self, file_path: &str) -> Result<Vec<String>> {
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT DISTINCT source.file_path
             FROM edges
             JOIN nodes AS target ON target.id = edges.target
             JOIN nodes AS source ON source.id = edges.source
             WHERE target.file_path = ?
               AND edges.kind != 'contains'
               AND source.file_path != ?
             ORDER BY source.file_path",
        )?;
        let rows = stmt.query_map([file_path, file_path], |row| row.get::<_, String>(0))?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// Languages present in the tracked file table. Synthesis uses this one
    /// indexed query to skip whole-graph passes whose result must be empty.
    pub fn get_distinct_file_languages(&self) -> Result<HashSet<String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT DISTINCT language FROM files")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// Most recent index timestamp (ms since epoch) across all tracked files, or
    /// None when nothing is indexed yet. One indexed aggregate, no per-row scan. (#329)
    pub fn get_last_indexed_at(&self) -> Result<Option<i64>> {
        let last: Option<i64> =
            self.db
                .conn()
                .query_row("SELECT MAX(indexed_at) AS last FROM files", [], |row| {
                    // Lenient like `lenient_i64`: a TS-written database may
                    // hold REAL timestamps (fractional mtimeMs-style numbers).
                    use rusqlite::types::ValueRef;
                    match row.get_ref(0)? {
                        ValueRef::Null => Ok(None),
                        ValueRef::Integer(i) => Ok(Some(i)),
                        ValueRef::Real(f) => Ok(Some(f as i64)),
                        other => Err(rusqlite::Error::FromSqlConversionFailure(
                            0,
                            other.data_type(),
                            "expected INTEGER or REAL for MAX(indexed_at)".into(),
                        )),
                    }
                })?;
        Ok(last)
    }

    /// Get files that need re-indexing (hash changed).
    pub fn get_stale_files(
        &self,
        current_hashes: &HashMap<String, String>,
    ) -> Result<Vec<FileRecord>> {
        let files = self.get_all_files()?;
        Ok(files
            .into_iter()
            .filter(|f| {
                current_hashes
                    .get(&f.path)
                    .map(|h| !h.is_empty() && h != &f.content_hash)
                    .unwrap_or(false)
            })
            .collect())
    }
}
