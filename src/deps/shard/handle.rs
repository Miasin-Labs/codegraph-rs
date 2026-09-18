//! Read-only access to a published shard.
//!
//! A shard is immutable once published (a rebuild replaces the directory
//! by rename), so its database is opened `mode=ro&immutable=1`: SQLite
//! takes no locks and creates no `-wal`/`-shm` files, and opening a shard
//! never writes anything anywhere.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use super::meta::ShardMeta;
use crate::db::{DATABASE_FILENAME, Db, QueryBuilder};
use crate::deps::model::DepKey;
use crate::deps::store::DepsHome;
use crate::error::Result;
use crate::types::Node;

/// Most rows a qualified-name suffix lookup returns.
const SUFFIX_LOOKUP_LIMIT: usize = 200;

/// An open, read-only shard.
pub struct ShardHandle {
    dir: PathBuf,
    meta: ShardMeta,
    queries: QueryBuilder,
}

impl std::fmt::Debug for ShardHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardHandle")
            .field("dir", &self.dir)
            .field("key", &self.meta.key())
            .finish_non_exhaustive()
    }
}

impl ShardHandle {
    /// Open the shard for `key` in `home`, or `None` when there is none, it
    /// was built for a different key, or its schema is not this build's.
    pub fn open(home: &DepsHome, key: &DepKey) -> Option<ShardHandle> {
        Self::open_dir(&home.shard_dir(key)).filter(|h| h.meta.key() == *key)
    }

    /// Open the shard in `dir` read-only.
    pub fn open_dir(dir: &Path) -> Option<ShardHandle> {
        let meta = ShardMeta::read(dir)?;
        if !meta.is_readable() || !meta.state.has_shard() {
            return None;
        }
        let db = open_immutable(&dir.join(DATABASE_FILENAME)).ok()?;
        Some(ShardHandle {
            dir: dir.to_path_buf(),
            meta,
            queries: QueryBuilder::new(db),
        })
    }

    pub fn key(&self) -> DepKey {
        self.meta.key()
    }

    pub fn meta(&self) -> &ShardMeta {
        &self.meta
    }

    /// The shard directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The dependency's source tree; node `file_path`s are relative to it.
    pub fn source_dir(&self) -> &Path {
        Path::new(&self.meta.source_dir)
    }

    /// Built within a budget that cut it short (see `meta().partial_reasons`).
    pub fn is_partial(&self) -> bool {
        !self.meta.partial_reasons.is_empty()
    }

    /// Built by an older extractor; usable, rebuilt lazily.
    pub fn is_stale(&self) -> bool {
        !self.meta.is_current()
    }

    /// The full read-side query surface over the shard's graph.
    pub fn queries(&self) -> &QueryBuilder {
        &self.queries
    }

    /// Symbols named exactly `name`.
    pub fn symbols_named(&self, name: &str) -> Result<Vec<Node>> {
        self.queries.get_nodes_by_name(name)
    }

    /// Symbols whose qualified name is `qualified`, or — when none is — ends
    /// with it at a `::` or `.` boundary (`Deserializer::deserialize_any`
    /// finds `src/de.rs::Deserializer::deserialize_any`).
    pub fn symbols_qualified(&self, qualified: &str) -> Result<Vec<Node>> {
        let exact = self.queries.get_nodes_by_qualified_name_exact(qualified)?;
        if !exact.is_empty() {
            return Ok(exact);
        }
        let escaped = qualified
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let conn = self.queries.db().conn();
        let mut stmt = conn.prepare(
            "SELECT id FROM nodes WHERE qualified_name LIKE '%::' || ?1 ESCAPE '\\' \
             OR qualified_name LIKE '%.' || ?1 ESCAPE '\\' ORDER BY qualified_name LIMIT ?2",
        )?;
        let ids = stmt
            .query_map(
                rusqlite::params![escaped, SUFFIX_LOOKUP_LIMIT as i64],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let mut by_id = self.queries.get_nodes_by_ids(&ids)?;
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// [`Self::symbols_qualified`] when `query` looks qualified (`::`, `.`,
    /// `/`), else [`Self::symbols_named`].
    pub fn lookup(&self, query: &str) -> Result<Vec<Node>> {
        if query.contains("::") || query.contains('.') || query.contains('/') {
            self.symbols_qualified(query)
        } else {
            self.symbols_named(query)
        }
    }
}

/// Open `path` read-only and immutable (see the module docs).
fn open_immutable(path: &Path) -> rusqlite::Result<Db> {
    let mut uri = String::from("file:");
    for c in path.to_string_lossy().chars() {
        match c {
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            '%' => uri.push_str("%25"),
            c => uri.push(c),
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.set_prepared_statement_cache_capacity(64);
    Ok(Db::new(conn))
}
