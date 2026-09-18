//! Read-only lookups into a repository's codegraph index
//! (`<repo>/.codegraph/codegraph.db`): a file's content hash (to tell
//! whether it changed since a session read it) and whether an identifier
//! names an indexed symbol (only those are stored in the clear).
//!
//! The index belongs to its project: it is opened the atlas's way
//! ([`crate::atlas::ro`]), which never creates its `-wal`/`-shm` files.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

/// A file modified this long after a touch still counts as what the touch
/// saw (an edit's own write lands a moment after the call starts).
const SETTLE_MS: i64 = 5_000;

/// An open, read-only view of one repository's index.
pub(crate) struct IndexProbe {
    conn: Connection,
}

impl IndexProbe {
    /// Open the index of `repo_root`, if it has one that answers queries.
    pub(crate) fn open(repo_root: &Path) -> Option<Self> {
        let path = crate::db::get_database_path(repo_root);
        if !path.is_file() {
            return None;
        }
        let conn =
            crate::atlas::ro::open_read_only(&path, std::time::Duration::from_millis(200)).ok()?;
        conn.query_row("SELECT 1 FROM files LIMIT 1", [], |_| Ok(()))
            .optional()
            .ok()?;
        Some(Self { conn })
    }

    /// `(content hash, modified_at)` of an indexed file.
    pub(crate) fn file_state(&self, rel: &str) -> Option<(String, i64)> {
        self.conn
            .query_row(
                "SELECT content_hash, modified_at FROM files WHERE path = ?1",
                params![rel],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// The content a touch at `at_ms` saw, when the index still holds it:
    /// the file's hash if it was last modified no later than the touch.
    pub(crate) fn fingerprint(&self, rel: &str, at_ms: Option<i64>) -> Option<String> {
        let at = at_ms?;
        let (hash, modified) = self.file_state(rel)?;
        (modified <= at + SETTLE_MS).then_some(hash)
    }

    /// Whether `name` is an indexed symbol name.
    pub(crate) fn has_symbol(&self, name: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM nodes WHERE name = ?1 LIMIT 1",
                params![name],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    /// Line of the definition of `name` in `rel`, if it has one.
    pub(crate) fn symbol_line(&self, name: &str, rel: &str) -> Option<u32> {
        self.conn
            .query_row(
                "SELECT start_line FROM nodes WHERE name = ?1 AND file_path = ?2 \
                 ORDER BY start_line LIMIT 1",
                params![name, rel],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|l| u32::try_from(l).ok())
    }
}
