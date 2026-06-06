//! Database Layer
//!
//! Handles SQLite database initialization and connection management.
//! Ported from `src/db/index.ts` (+ the relevant surface of
//! `src/db/sqlite-adapter.ts`, whose dual better-sqlite3 / node:sqlite / wasm
//! backend collapses to rusqlite — the reported backend is always `"native"`).

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::migrations::{CURRENT_SCHEMA_VERSION, get_current_version, run_migrations};
use crate::error::{CodeGraphError, Result};
use crate::types::SchemaVersion;

/// The embedded schema (copied verbatim from `src/db/schema.sql`).
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// Epoch milliseconds (`Date.now()` parity).
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The active SQLite backend. Only one in the Rust port (rusqlite, bundled
/// SQLite compiled in). Kept as a named type so `codegraph status` can still
/// report which backend is live — the TS dual-backend story collapses here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqliteBackend {
    #[serde(rename = "native")]
    Native,
}

impl SqliteBackend {
    pub fn as_str(&self) -> &'static str {
        "native"
    }
}

impl std::fmt::Display for SqliteBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Shared database handle — the Rust analog of the TS `SqliteDatabase` that
/// `DatabaseConnection` and `QueryBuilder` both hold. Cloning is cheap (Rc).
///
/// Not `Send`: rusqlite's `Connection` must stay on one thread, mirroring the
/// single-threaded TS runtime. Confine all DB access to one thread (the MCP
/// daemon should funnel queries through a channel if it spawns workers).
#[derive(Debug, Clone)]
pub struct Db {
    conn: Rc<Connection>,
    /// Transaction nesting depth — outermost level uses BEGIN/COMMIT,
    /// nested levels use SAVEPOINTs (mirrors better-sqlite3 semantics,
    /// which the TS QueryBuilder relies on: e.g. `deleteFile`'s transaction
    /// calls `deleteNodesByFile`, and callers may wrap both in their own).
    txn_depth: Rc<Cell<u32>>,
}

impl Db {
    pub fn new(conn: Connection) -> Self {
        Db {
            conn: Rc::new(conn),
            txn_depth: Rc::new(Cell::new(0)),
        }
    }

    /// Borrow the underlying rusqlite connection.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Execute a batch of SQL statements (TS `db.exec`).
    pub fn exec(&self, sql: &str) -> Result<()> {
        self.conn.execute_batch(sql)?;
        Ok(())
    }

    /// Execute a closure within a transaction (TS `db.transaction(fn)()`).
    /// Nested calls become SAVEPOINTs. On `Err` the (sub)transaction is
    /// rolled back and the error propagated.
    pub fn transaction<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let depth = self.txn_depth.get();
        if depth == 0 {
            self.conn.execute_batch("BEGIN")?;
        } else {
            self.conn
                .execute_batch(&format!("SAVEPOINT cg_sp_{depth}"))?;
        }
        self.txn_depth.set(depth + 1);
        let result = f();
        self.txn_depth.set(depth);
        match result {
            Ok(v) => {
                if depth == 0 {
                    self.conn.execute_batch("COMMIT")?;
                } else {
                    self.conn.execute_batch(&format!("RELEASE cg_sp_{depth}"))?;
                }
                Ok(v)
            }
            Err(e) => {
                let _ = if depth == 0 {
                    self.conn.execute_batch("ROLLBACK")
                } else {
                    self.conn
                        .execute_batch(&format!("ROLLBACK TO cg_sp_{depth}; RELEASE cg_sp_{depth}"))
                };
                Err(e)
            }
        }
    }
}

impl std::ops::Deref for Db {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.conn
    }
}

/// Apply connection-level PRAGMAs. Shared by `initialize` and `open` so the two
/// paths can't drift.
///
/// `busy_timeout` is set FIRST, before any pragma that might touch the database
/// file (notably `journal_mode`). If another process holds a write lock at open
/// time, the later pragmas — and the connection's first query — then wait out
/// the lock instead of throwing "database is locked" immediately. See issue #238.
///
/// The 5s window (was 120s) rides out a normal incremental sync; the old
/// 2-minute wait presented as a frozen, hung agent. With WAL, reads never block
/// on a writer, so this timeout only governs cross-process write contention
/// (e.g. the git-hook `codegraph sync` running while the MCP server writes).
fn configure_connection(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA busy_timeout = 5000;      -- MUST be first — see above
         PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;     -- safe with WAL mode
         PRAGMA cache_size = -64000;      -- 64 MB page cache
         PRAGMA temp_store = MEMORY;      -- temp tables in memory
         PRAGMA mmap_size = 268435456;    -- 256 MB memory-mapped I/O",
    )?;
    // The QueryBuilder keeps ~30 distinct prepared statements hot via
    // prepare_cached (the TS lazily-initialized `stmts` map); raise the
    // cache above rusqlite's default of 16 so none thrash.
    conn.set_prepared_statement_cache_capacity(64);
    Ok(())
}

/// Database connection wrapper with lifecycle management.
#[derive(Debug)]
pub struct DatabaseConnection {
    db: Option<Db>,
    db_path: PathBuf,
    backend: SqliteBackend,
}

impl DatabaseConnection {
    /// Initialize a new database at the given path.
    pub fn initialize(db_path: impl AsRef<Path>) -> Result<DatabaseConnection> {
        let db_path = db_path.as_ref();

        // Ensure parent directory exists
        if let Some(dir) = db_path.parent() {
            if !dir.as_os_str().is_empty() && !dir.exists() {
                fs::create_dir_all(dir)?;
            }
        }

        // Create and configure database
        let conn = Connection::open(db_path)?;
        configure_connection(&conn)?;

        // Run schema initialization
        conn.execute_batch(SCHEMA_SQL)?;

        let db = Db::new(conn);

        // Record current schema version so migrations aren't re-applied on open
        let current_version = get_current_version(&db);
        if current_version < CURRENT_SCHEMA_VERSION {
            db.conn().execute(
                "INSERT OR IGNORE INTO schema_versions (version, applied_at, description) VALUES (?, ?, ?)",
                rusqlite::params![
                    CURRENT_SCHEMA_VERSION,
                    now_ms(),
                    "Initial schema includes all migrations"
                ],
            )?;
        }

        Ok(DatabaseConnection {
            db: Some(db),
            db_path: db_path.to_path_buf(),
            backend: SqliteBackend::Native,
        })
    }

    /// Open an existing database.
    pub fn open(db_path: impl AsRef<Path>) -> Result<DatabaseConnection> {
        let db_path = db_path.as_ref();
        if !db_path.exists() {
            return Err(CodeGraphError::other(format!(
                "Database not found: {}",
                db_path.display()
            )));
        }

        let conn = Connection::open(db_path)?;
        configure_connection(&conn)?;

        let db = Db::new(conn);

        // Check and run migrations if needed
        let current_version = get_current_version(&db);
        if current_version < CURRENT_SCHEMA_VERSION {
            run_migrations(&db, current_version)?;
        }

        Ok(DatabaseConnection {
            db: Some(db),
            db_path: db_path.to_path_buf(),
            backend: SqliteBackend::Native,
        })
    }

    fn db_ref(&self) -> Result<&Db> {
        self.db
            .as_ref()
            .ok_or_else(|| CodeGraphError::database("The database connection is not open", "open"))
    }

    /// Get the underlying database handle (cheap clone of the shared Rc;
    /// TS `getDb()`). Errors if the connection has been closed.
    pub fn get_db(&self) -> Result<Db> {
        Ok(self.db_ref()?.clone())
    }

    /// Get the SQLite backend serving this connection. Per-instance so
    /// MCP cross-project queries report the right backend even when
    /// multiple project DBs are open in the same process.
    pub fn get_backend(&self) -> SqliteBackend {
        self.backend
    }

    /// Get database file path.
    pub fn get_path(&self) -> &Path {
        &self.db_path
    }

    /// The journal mode actually in effect (e.g. 'wal', 'delete').
    ///
    /// SQLite silently keeps the prior mode if WAL can't be enabled — e.g. on
    /// filesystems without shared-memory support (some network/virtualized mounts,
    /// WSL2 /mnt). So the effective mode can differ from what
    /// `configure_connection` requested. Surfaced in `codegraph status` so
    /// a "database is locked" report is triageable: 'wal' ⇒ readers never block on a
    /// writer; anything else ⇒ they can. See issue #238.
    pub fn get_journal_mode(&self) -> Result<String> {
        let db = self.db_ref()?;
        let mode: String = db
            .conn()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        Ok(mode.to_lowercase())
    }

    /// Get current schema version.
    pub fn get_schema_version(&self) -> Result<Option<SchemaVersion>> {
        let db = self.db_ref()?;
        let mut stmt = db.conn().prepare_cached(
            "SELECT version, applied_at, description FROM schema_versions ORDER BY version DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some(SchemaVersion {
                version: row.get(0)?,
                applied_at: row.get(1)?,
                description: row.get::<_, Option<String>>(2)?,
            })),
            None => Ok(None),
        }
    }

    /// Execute a function within a transaction.
    pub fn transaction<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.db_ref()?.transaction(f)
    }

    /// Get database file size in bytes.
    pub fn get_size(&self) -> Result<u64> {
        Ok(fs::metadata(&self.db_path)?.len())
    }

    /// Optimize database (vacuum and analyze).
    pub fn optimize(&self) -> Result<()> {
        let db = self.db_ref()?;
        db.exec("VACUUM")?;
        db.exec("ANALYZE")?;
        Ok(())
    }

    /// Lightweight, non-blocking maintenance to run after bulk writes
    /// (indexAll, sync). Two operations:
    ///
    ///   - `PRAGMA optimize` — incremental ANALYZE; SQLite only re-analyzes
    ///     tables whose row counts changed materially since the last
    ///     ANALYZE. Without it, the query planner has no statistics on the
    ///     freshly-bulk-loaded tables and can pick suboptimal indexes.
    ///
    ///   - `PRAGMA wal_checkpoint(PASSIVE)` — fold pending WAL pages back
    ///     into the main database file so the WAL file doesn't grow
    ///     unboundedly between automatic checkpoints (auto-fires at 1000
    ///     pages by default; large indexAll runs blow past that).
    ///
    /// Both operations are silently swallowed on failure — they're a
    /// best-effort optimization, never load-bearing for correctness.
    pub fn run_maintenance(&self) {
        if let Some(db) = self.db.as_ref() {
            let _ = db.exec("PRAGMA optimize");
            let _ = db.exec("PRAGMA wal_checkpoint(PASSIVE)");
        }
    }

    /// Close the database connection.
    pub fn close(&mut self) {
        self.db = None;
    }

    /// Check if the database connection is open.
    pub fn is_open(&self) -> bool {
        self.db.is_some()
    }
}

/// Default database filename.
pub const DATABASE_FILENAME: &str = "codegraph.db";

/// Get the default database path for a project.
pub fn get_database_path(project_root: impl AsRef<Path>) -> PathBuf {
    project_root
        .as_ref()
        .join(".codegraph")
        .join(DATABASE_FILENAME)
}
