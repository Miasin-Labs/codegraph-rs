//! The registry's versioned schema (`PRAGMA user_version`). Each migration
//! is idempotent and runs in its own transaction.

use rusqlite::{Connection, Transaction};

/// Schema version this build writes.
pub(crate) const CURRENT_VERSION: i64 = 1;

struct Migration {
    version: i64,
    apply: fn(&Transaction<'_>) -> rusqlite::Result<()>,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    apply: v1_base,
}];

/// Bring `conn` up to [`CURRENT_VERSION`].
pub(crate) fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    debug_assert_eq!(MIGRATIONS.last().map(|m| m.version), Some(CURRENT_VERSION));
    let current = user_version(conn)?;
    for m in MIGRATIONS.iter().filter(|m| m.version > current) {
        let tx = conn.transaction()?;
        (m.apply)(&tx)?;
        tx.pragma_update(None, "user_version", m.version)?;
        tx.commit()?;
    }
    Ok(())
}

pub(crate) fn user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

/// v1: dependency versions with their shard state, the projects that use
/// them (keyed by canonical checkout root — the atlas's project key), and
/// one usage row per (project, version, lockfile).
fn v1_base(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS packages (
             id                INTEGER PRIMARY KEY,
             ecosystem         TEXT NOT NULL,
             name              TEXT NOT NULL,
             version           TEXT NOT NULL,
             source_kind       TEXT NOT NULL CHECK (source_kind IN ('registry', 'git', 'path')),
             source_url        TEXT,
             source_rev        TEXT,
             state             TEXT NOT NULL DEFAULT 'missing' CHECK (state IN
                                 ('missing', 'building', 'ready', 'partial', 'unavailable', 'failed')),
             shard_bytes       INTEGER NOT NULL DEFAULT 0,
             files             INTEGER,
             nodes             INTEGER,
             edges             INTEGER,
             build_ms          INTEGER,
             built_at          INTEGER,
             extractor_version INTEGER,
             schema_version    INTEGER,
             partial_reasons   TEXT,
             error             TEXT,
             built_from        TEXT,
             first_seen        INTEGER NOT NULL,
             last_used         INTEGER NOT NULL,
             UNIQUE (ecosystem, name, version)
         );
         CREATE INDEX IF NOT EXISTS idx_packages_state ON packages(state);
         CREATE INDEX IF NOT EXISTS idx_packages_last_used ON packages(last_used);
         CREATE TABLE IF NOT EXISTS projects (
             id               INTEGER PRIMARY KEY,
             root             TEXT NOT NULL UNIQUE,
             lock_fingerprint TEXT,
             lockfiles        INTEGER NOT NULL DEFAULT 0,
             recorded_at      INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS usages (
             project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
             package_id INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
             lockfile   TEXT NOT NULL,
             direct     INTEGER,
             source_dir TEXT,
             path       TEXT,
             PRIMARY KEY (project_id, package_id, lockfile)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS idx_usages_package ON usages(package_id);",
    )
}
