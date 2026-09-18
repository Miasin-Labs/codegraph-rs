//! [`Atlas`]: the SQLite store behind the atlas.
//!
//! Writers ([`Atlas::open`]) create the file owner-only (0600, in the 0700
//! machine-wide directory), run it in WAL mode with a bounded busy timeout,
//! and migrate it; readers ([`Atlas::open_read_only`]) never create or change
//! anything and report "no atlas yet" as `None`.

use std::path::Path;
use std::time::Duration;
use std::{fs, io};

use rusqlite::Connection;
use serde::Serialize;

use super::schema;

/// How long a writer waits for another writer before giving up (the
/// caller skips its registration and says so; it never blocks a command).
pub(crate) const WRITE_BUSY_TIMEOUT: Duration = Duration::from_millis(1_500);
/// How long a reader waits on a checkpoint.
const READ_BUSY_TIMEOUT: Duration = Duration::from_millis(500);

/// Errors from the atlas store.
#[derive(Debug, thiserror::Error)]
pub enum AtlasError {
    #[error("atlas database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("atlas I/O: {0}")]
    Io(#[from] io::Error),
    /// Another writer held the atlas past the busy timeout.
    #[error("the atlas is busy (another codegraph process is writing it)")]
    Busy,
    /// The atlas was written by a newer build; this one leaves it alone.
    #[error("the atlas is at schema v{found}, newer than this build's v{supported}")]
    TooNew { found: i64, supported: i64 },
}

impl AtlasError {
    /// SQLite lock contention, however it surfaced.
    pub fn is_busy(&self) -> bool {
        match self {
            Self::Busy => true,
            Self::Sqlite(rusqlite::Error::SqliteFailure(e, _)) => matches!(
                e.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ),
            _ => false,
        }
    }

    pub(crate) fn from_sqlite(err: rusqlite::Error) -> Self {
        let busy = Self::Sqlite(err);
        if busy.is_busy() { Self::Busy } else { busy }
    }
}

/// What a prune did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneReport {
    /// Projects checked.
    pub checked: usize,
    /// Newly marked `missing`.
    pub marked_missing: Vec<String>,
    /// Rows deleted (`--remove`).
    pub removed: Vec<String>,
}

/// The atlas store.
pub struct Atlas {
    pub(super) conn: Connection,
}

impl Atlas {
    /// Open for writing, creating the file (0600) and its directory (0700)
    /// when absent, and migrating it to the current schema.
    pub fn open(path: &Path) -> Result<Self, AtlasError> {
        create_private(path)?;
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(WRITE_BUSY_TIMEOUT)?;
        // WAL: `projects list` and MCP readers never wait on a registration.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .map_err(AtlasError::from_sqlite)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let found = schema::user_version(&conn).map_err(AtlasError::from_sqlite)?;
        if found > schema::CURRENT_VERSION {
            return Err(AtlasError::TooNew {
                found,
                supported: schema::CURRENT_VERSION,
            });
        }
        schema::migrate(&mut conn).map_err(AtlasError::from_sqlite)?;
        Ok(Self { conn })
    }

    /// In-memory atlas (tests).
    pub fn open_in_memory() -> Result<Self, AtlasError> {
        let mut conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", true)?;
        schema::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Open an existing atlas read-only. `Ok(None)` when there is none yet
    /// (no file, or no tables); never creates the file, its directory, or
    /// SQLite's `-wal`/`-shm` side files.
    pub fn open_read_only(path: &Path) -> Result<Option<Self>, AtlasError> {
        if !path.is_file() {
            return Ok(None);
        }
        let conn = super::ro::open_read_only(path, READ_BUSY_TIMEOUT)?;
        let has_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'projects')",
            [],
            |r| r.get(0),
        )?;
        Ok(has_table.then_some(Self { conn }))
    }

    /// The store's schema version (`PRAGMA user_version`).
    pub fn schema_version(&self) -> Result<i64, AtlasError> {
        Ok(schema::user_version(&self.conn)?)
    }
}

/// Create `path` owner-only (0600) — and its missing parent directories
/// 0700 — if it doesn't exist yet. Existing files are left as they are.
fn create_private(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if !dir.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(dir)?;
        }
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}
