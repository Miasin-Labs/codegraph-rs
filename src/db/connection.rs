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

use crate::db::migrations::{
    CURRENT_SCHEMA_VERSION,
    get_current_version,
    repair_shared_schema_v9,
    run_migrations,
};
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
    // Without a journal_size_limit the -wal file never shrinks below its
    // high-water mark while a connection lives: checkpoints fold frames back
    // but leave the file at full size, so one giant deferred-sync WAL stays
    // giant forever. With the limit set, any checkpoint that resets the WAL
    // truncates the file back down. Killed-process leftovers are handled
    // separately by `heal_oversized_wal` at open. (#1431, #1539)
    conn.execute_batch(&format!(
        "PRAGMA journal_size_limit = {}",
        wal_heal_threshold_bytes()
    ))?;
    // The QueryBuilder keeps ~30 distinct prepared statements hot via
    // prepare_cached (the TS lazily-initialized `stmts` map); raise the
    // cache above rusqlite's default of 16 so none thrash.
    conn.set_prepared_statement_cache_capacity(64);
    Ok(())
}

/// Default WAL heal / `journal_size_limit` threshold in bytes (64 MiB).
const DEFAULT_WAL_HEAL_BYTES: u64 = 64 * 1024 * 1024;

/// WAL size past which [`DatabaseConnection::heal_oversized_wal`] (run at every
/// `open`) checkpoints and truncates the file, and to which
/// `journal_size_limit` clips the WAL after any resetting checkpoint.
///
/// A SIGKILL'd process (the #850 liveness watchdog, OOM, a crash) can leave an
/// arbitrarily large WAL behind — a whole deferred-sync run's worth (#1248,
/// #1539) — and before this fix no later session ever shrank it: the file just
/// grew, killed session after killed session, until the disk filled (64 GiB
/// observed in #1539, 25.6 GB in #1431). 64 MiB is far above anything a healthy
/// open ever sees (a clean close deletes the WAL) yet small enough to cap the
/// leak. Override with `CODEGRAPH_WAL_HEAL_MB` (also feeds `journal_size_limit`).
pub fn wal_heal_threshold_bytes() -> u64 {
    resolve_wal_heal_bytes(std::env::var("CODEGRAPH_WAL_HEAL_MB").ok().as_deref())
}

/// Resolve the heal threshold from the env override (MB); invalid ⇒ 64 MiB.
/// Mirrors TS `resolveWalHealBytes`.
pub fn resolve_wal_heal_bytes(env_val: Option<&str>) -> u64 {
    if let Some(v) = env_val {
        if !v.is_empty() {
            if let Ok(n) = v.trim().parse::<f64>() {
                if n.is_finite() && n > 0.0 {
                    return (n * 1024.0 * 1024.0).floor() as u64;
                }
            }
        }
    }
    DEFAULT_WAL_HEAL_BYTES
}

/// Result of a [`DatabaseConnection::heal_oversized_wal`] pass. Mirrors the TS
/// `healOversizedWal` return shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalHealResult {
    /// Whether the WAL file shrank as a result of the heal.
    pub healed: bool,
    /// WAL size in bytes before the heal ran.
    pub before_bytes: u64,
    /// WAL size in bytes after the heal ran.
    pub after_bytes: u64,
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
        repair_shared_schema_v9(&db)?;

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
        repair_shared_schema_v9(&db)?;

        let conn = DatabaseConnection {
            db: Some(db),
            db_path: db_path.to_path_buf(),
            backend: SqliteBackend::Native,
        };

        // Self-heal a killed session's leftover oversized WAL (#1431, #1539) —
        // one stat when healthy, checkpoint+truncate when not. Unlike the TS
        // (which fires this off-thread against a worker connection), the port
        // owns the only connection and runs it inline at open, before any
        // writer is live, so a multi-GB WAL cannot silently ratchet across
        // killed daemons until the disk fills.
        conn.heal_oversized_wal();

        Ok(conn)
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

    /// Size of the `-wal` sidecar file in bytes. 0 when it doesn't exist
    /// (non-WAL journal mode, in-memory DB, or no write since the last
    /// checkpoint+reset). Mirrors TS `getWalSizeBytes`.
    pub fn get_wal_size_bytes(&self) -> u64 {
        wal_size_bytes(&self.db_path)
    }

    /// Size of the main DB file in bytes (0 for unknown). Mirrors TS
    /// `getDbFileSizeBytes`.
    pub fn get_db_file_size_bytes(&self) -> u64 {
        fs::metadata(&self.db_path).map(|m| m.len()).unwrap_or(0)
    }

    /// `PRAGMA wal_checkpoint(<mode>)`. Returns SQLite's checkpoint result row
    /// `(busy, log, checkpointed)` — `log == checkpointed` with `busy == 0`
    /// means the ENTIRE WAL was backfilled, so the writer's next commit
    /// restarts the WAL from the top and (with `journal_size_limit` set) the
    /// file is clipped. `TRUNCATE` additionally chops the file to zero when no
    /// reader holds a WAL mark. Best-effort: `None` on any failure. Mirrors TS
    /// `checkpointWalPassive` / `checkpointWalTruncate` (which the port runs on
    /// the single owning connection rather than a worker thread — at `open`
    /// there is no live writer to block).
    fn checkpoint_wal(&self, mode: &str) -> Option<(i64, i64, i64)> {
        let db = self.db.as_ref()?;
        db.conn()
            .query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .ok()
    }

    /// Shrink a leftover oversized WAL (#1431, #1539). A SIGKILL'd session — the
    /// #850 liveness watchdog, OOM, a crash — leaves its WAL on disk, the next
    /// session appends to the same file, and (pre-fix) nothing ever truncated
    /// it: PASSIVE checkpoints fold frames but keep the file at its high-water
    /// mark, and the one shrinking path (a clean last-connection close) is
    /// exactly what the killed world never takes. Unbounded growth until the
    /// disk fills (64 GiB in #1539).
    ///
    /// Called from every `open`: cost is one `stat` when the WAL is small (the
    /// overwhelmingly common case). Past the threshold it runs a PASSIVE fold
    /// then TRUNCATE, retrying a few times so a racing reader/writer degrades a
    /// checkpoint pass to a busy no-op the next attempt (or open) retries
    /// rather than a stall.
    pub fn heal_oversized_wal(&self) -> WalHealResult {
        let before_bytes = self.get_wal_size_bytes();
        let threshold = wal_heal_threshold_bytes();
        if before_bytes <= threshold {
            return WalHealResult {
                healed: false,
                before_bytes,
                after_bytes: before_bytes,
            };
        }
        // A racing reader/writer (another session healing the same file, a
        // query pool warming up) degrades a checkpoint pass to a busy no-op —
        // retry a few times before leaving the rest to the next open.
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            let _ = self.checkpoint_wal("PASSIVE");
            let _ = self.checkpoint_wal("TRUNCATE");
            if self.get_wal_size_bytes() <= threshold {
                break;
            }
        }
        let after_bytes = self.get_wal_size_bytes();
        if std::env::var_os("CODEGRAPH_WAL_VALVE_DEBUG").is_some() {
            crate::error::log_debug(
                &format!(
                    "[wal-heal] oversized WAL at open: {}MB -> {}MB",
                    before_bytes / (1024 * 1024),
                    after_bytes / (1024 * 1024)
                ),
                None,
            );
        }
        WalHealResult {
            healed: after_bytes < before_bytes,
            before_bytes,
            after_bytes,
        }
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

/// Size of the `-wal` sidecar for `db_path` in bytes; 0 when it doesn't exist.
fn wal_size_bytes(db_path: &Path) -> u64 {
    let mut wal = db_path.as_os_str().to_os_string();
    wal.push("-wal");
    fs::metadata(PathBuf::from(wal))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Default database filename.
pub const DATABASE_FILENAME: &str = "codegraph.db";

/// Get the default database path for a project.
pub fn get_database_path(project_root: impl AsRef<Path>) -> PathBuf {
    crate::directory::get_codegraph_dir(project_root.as_ref()).join(DATABASE_FILENAME)
}

#[cfg(test)]
mod wal_heal_tests {
    //! Regression tests for #1431 / #1539: a SIGKILL'd session leaves the
    //! SQLite WAL on disk; the next session appends to the same file; and
    //! before the fix NOTHING ever truncated it — PASSIVE checkpoints fold
    //! frames but keep the file at its high-water mark, and the only shrinking
    //! path (a clean last-connection close) is exactly what a killed-daemon
    //! world never takes. Observed in the wild at 64 GiB (#1539).
    //!
    //! The fix: `journal_size_limit` on every connection (resetting checkpoints
    //! now clip the file), plus `heal_oversized_wal()` fired from every
    //! `DatabaseConnection::open`.
    use tempfile::tempdir;

    use super::*;

    const MB: u64 = 1024 * 1024;

    #[test]
    fn resolves_heal_threshold_from_env_override_defaulting_to_64mb() {
        assert_eq!(resolve_wal_heal_bytes(None), 64 * MB);
        assert_eq!(resolve_wal_heal_bytes(Some("")), 64 * MB);
        assert_eq!(resolve_wal_heal_bytes(Some("nope")), 64 * MB);
        assert_eq!(resolve_wal_heal_bytes(Some("-3")), 64 * MB);
        assert_eq!(resolve_wal_heal_bytes(Some("0")), 64 * MB);
        assert_eq!(resolve_wal_heal_bytes(Some("128")), 128 * MB);
    }

    #[test]
    fn sets_journal_size_limit_on_every_connection() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("codegraph.db");
        DatabaseConnection::initialize(&db_path).unwrap().close();

        let conn = DatabaseConnection::open(&db_path).unwrap();
        let limit: i64 = conn
            .get_db()
            .unwrap()
            .conn()
            .query_row("PRAGMA journal_size_limit", [], |row| row.get(0))
            .unwrap();
        assert_eq!(limit as u64, wal_heal_threshold_bytes());
    }

    #[test]
    fn leaves_healthy_small_wals_alone() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("codegraph.db");
        DatabaseConnection::initialize(&db_path).unwrap().close();

        let conn = DatabaseConnection::open(&db_path).unwrap();
        let res = conn.heal_oversized_wal();
        assert!(!res.healed);
        assert!(res.before_bytes <= wal_heal_threshold_bytes());
    }

    /// Reproduce the ratchet with a tiny threshold: an orphaned oversized WAL
    /// (as a SIGKILL leaves behind) is folded + truncated on the next
    /// `heal_oversized_wal`, while all committed data survives.
    #[test]
    fn heals_an_orphaned_oversized_wal_and_keeps_the_data() {
        // Shrink the threshold so the test doesn't need to write 64 MiB.
        // Safe within a single test process (serialized by cargo per binary
        // only across threads — guard with a fixed value the assertions use).
        let prev = std::env::var("CODEGRAPH_WAL_HEAL_MB").ok();
        std::env::set_var("CODEGRAPH_WAL_HEAL_MB", "1");
        let threshold = wal_heal_threshold_bytes();
        assert_eq!(threshold, MB);

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("codegraph.db");
        DatabaseConnection::initialize(&db_path).unwrap().close();

        // Grow the WAL well past the 1 MiB threshold with autocheckpoint off
        // (deferred-checkpoint sync mode, #1248), then DROP the connection
        // WITHOUT a clean checkpoint to mimic a killed session's leftover.
        {
            let conn = DatabaseConnection::open(&db_path).unwrap();
            let db = conn.get_db().unwrap();
            db.exec("PRAGMA wal_autocheckpoint = 0").unwrap();
            db.exec("CREATE TABLE junk (id INTEGER PRIMARY KEY, blob BLOB)")
                .unwrap();
            let chunk = vec![0xabu8; 256 * 1024];
            while wal_size_bytes(&db_path) < 4 * MB {
                db.exec("BEGIN").unwrap();
                {
                    let mut stmt = db
                        .conn()
                        .prepare("INSERT INTO junk (blob) VALUES (?)")
                        .unwrap();
                    for _ in 0..20 {
                        stmt.execute(rusqlite::params![chunk]).unwrap();
                    }
                }
                db.exec("COMMIT").unwrap();
            }
            // Leak the connection so Drop cannot run a clean checkpoint —
            // the WAL file survives just as after a SIGKILL.
            std::mem::forget(conn);
        }

        let before = wal_size_bytes(&db_path);
        assert!(
            before > threshold,
            "WAL should be oversized: {before} bytes"
        );

        // A fresh open both auto-heals and lets us call the heal explicitly.
        let conn = DatabaseConnection::open(&db_path).unwrap();
        conn.heal_oversized_wal();
        let after = wal_size_bytes(&db_path);
        assert!(
            after <= threshold,
            "WAL should be clipped below threshold: {after} bytes"
        );
        // The folded data is all there.
        let n: i64 = conn
            .get_db()
            .unwrap()
            .conn()
            .query_row("SELECT COUNT(*) FROM junk", [], |row| row.get(0))
            .unwrap();
        assert!(n > 0);

        // Restore env for other tests in this binary.
        match prev {
            Some(v) => std::env::set_var("CODEGRAPH_WAL_HEAL_MB", v),
            None => std::env::remove_var("CODEGRAPH_WAL_HEAL_MB"),
        }
    }
}
