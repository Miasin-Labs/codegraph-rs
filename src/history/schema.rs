//! The history DB's versioned schema (`PRAGMA user_version`) and its
//! migrations. Each migration is idempotent and runs in its own
//! transaction, so a DB at any version — including the unversioned ones the
//! first release created — upgrades in place.

use rusqlite::{Connection, Transaction};

/// Schema version this build writes.
pub(crate) const CURRENT_VERSION: i64 = 3;

struct Migration {
    version: i64,
    apply: fn(&Transaction<'_>) -> rusqlite::Result<()>,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        apply: v1_base_table,
    },
    Migration {
        version: 2,
        apply: v2_call_keys,
    },
    Migration {
        version: 3,
        apply: v3_memory,
    },
];

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

/// v1: the original, unversioned layout (what pre-versioning DBs already have).
fn v1_base_table(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS tool_events (
             id           INTEGER PRIMARY KEY AUTOINCREMENT,
             ts           TEXT,
             session      TEXT,
             project      TEXT,
             tool_kind    TEXT NOT NULL,
             primary_cmd  TEXT,
             chain        TEXT,
             path         TEXT,
             redacted     INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_tool_events_project   ON tool_events(project);
         CREATE INDEX IF NOT EXISTS idx_tool_events_kind      ON tool_events(tool_kind);
         CREATE INDEX IF NOT EXISTS idx_tool_events_session   ON tool_events(session);
         CREATE INDEX IF NOT EXISTS idx_tool_events_path      ON tool_events(path);
         CREATE INDEX IF NOT EXISTS idx_tool_events_sess_path ON tool_events(session, path);",
    )
}

/// v2: one row per real call. Adds `source` and the unique `call_key`
/// (hash of the native call id) that makes ingest idempotent, and drops
/// the v1 rows: they were keyed on log lines, not calls (≈4.6× duplicated),
/// and were built by the weaker v1 redactor. Re-ingesting rebuilds them from
/// the agent's logs. `secure_delete` (set by the opener) zeroes their pages.
fn v2_call_keys(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    for (column, ty) in [("source", "TEXT"), ("call_key", "TEXT")] {
        if !has_column(tx, "tool_events", column)? {
            tx.execute_batch(&format!("ALTER TABLE tool_events ADD COLUMN {column} {ty}"))?;
        }
    }
    tx.execute("DELETE FROM tool_events WHERE call_key IS NULL", [])?;
    tx.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_tool_events_call_key ON tool_events(call_key);
         CREATE INDEX IF NOT EXISTS idx_tool_events_source ON tool_events(source);",
    )
}

/// v3: the cross-session memory (see `history::memory`). Sessions split
/// into episodes at human prompts; per episode, the files touched, the
/// identifiers looked up, and build/test/commit outcomes; plus co-edited
/// file pairs and a precomputed per-repository digest for the prompt hook.
/// Only repo-relative paths, index-resolved identifiers (else a hash),
/// masked command templates, codes, hashes and numbers are stored.
/// `tool_events.remembered` marks calls already folded into the memory, so
/// a v2 store's rows are folded in when their calls are seen again.
fn v3_memory(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if !has_column(tx, "tool_events", "remembered")? {
        tx.execute_batch(
            "ALTER TABLE tool_events ADD COLUMN remembered INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS repos (
             id   INTEGER PRIMARY KEY,
             root TEXT NOT NULL UNIQUE
         );
         CREATE TABLE IF NOT EXISTS sessions (
             id         INTEGER PRIMARY KEY,
             source     TEXT NOT NULL,
             native_key TEXT NOT NULL UNIQUE,
             parent_id  INTEGER,
             root_id    INTEGER,
             repo_id    INTEGER,
             started_at INTEGER,
             ended_at   INTEGER,
             calls      INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_sessions_repo ON sessions(repo_id, started_at);
         CREATE TABLE IF NOT EXISTS episodes (
             id         INTEGER PRIMARY KEY,
             session_id INTEGER NOT NULL,
             prompt_key TEXT NOT NULL UNIQUE,
             repo_id    INTEGER,
             started_at INTEGER NOT NULL,
             ended_at   INTEGER,
             calls      INTEGER NOT NULL DEFAULT 0,
             outcome    TEXT,
             outcome_ts INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_episodes_session ON episodes(session_id, started_at);
         CREATE INDEX IF NOT EXISTS idx_episodes_repo ON episodes(repo_id, started_at);
         CREATE TABLE IF NOT EXISTS files (
             id      INTEGER PRIMARY KEY,
             repo_id INTEGER NOT NULL,
             path    TEXT NOT NULL,
             UNIQUE (repo_id, path)
         );
         CREATE TABLE IF NOT EXISTS touches (
             episode_id  INTEGER NOT NULL,
             file_id     INTEGER NOT NULL,
             op          TEXT NOT NULL CHECK (op IN ('e', 'r', 's')),
             n           INTEGER NOT NULL DEFAULT 0,
             bytes       INTEGER NOT NULL DEFAULT 0,
             first_ts    INTEGER,
             last_ts     INTEGER,
             fingerprint TEXT,
             PRIMARY KEY (episode_id, file_id, op)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS idx_touches_file ON touches(file_id, last_ts);
         CREATE TABLE IF NOT EXISTS idents (
             id      INTEGER PRIMARY KEY,
             repo_id INTEGER NOT NULL,
             name    TEXT NOT NULL,
             UNIQUE (repo_id, name)
         );
         CREATE TABLE IF NOT EXISTS lookups (
             episode_id    INTEGER NOT NULL,
             ident_id      INTEGER NOT NULL,
             n             INTEGER NOT NULL DEFAULT 0,
             found_file_id INTEGER,
             found_line    INTEGER,
             last_ts       INTEGER,
             PRIMARY KEY (episode_id, ident_id)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS idx_lookups_ident ON lookups(ident_id);
         CREATE TABLE IF NOT EXISTS outcomes (
             id         INTEGER PRIMARY KEY,
             call_key   TEXT NOT NULL UNIQUE,
             episode_id INTEGER NOT NULL,
             repo_id    INTEGER,
             kind       TEXT NOT NULL,
             template   TEXT NOT NULL,
             ok         INTEGER,
             err_codes  TEXT,
             err_sig    TEXT,
             ts         INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_outcomes_repo ON outcomes(repo_id, ts);
         CREATE INDEX IF NOT EXISTS idx_outcomes_template ON outcomes(repo_id, template, ts);
         CREATE INDEX IF NOT EXISTS idx_outcomes_sig ON outcomes(repo_id, err_sig);
         CREATE TABLE IF NOT EXISTS coedits (
             repo_id  INTEGER NOT NULL,
             file_a   INTEGER NOT NULL,
             file_b   INTEGER NOT NULL,
             episodes INTEGER NOT NULL DEFAULT 0,
             commits  INTEGER NOT NULL DEFAULT 0,
             last_ts  INTEGER,
             PRIMARY KEY (repo_id, file_a, file_b)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS idx_coedits_b ON coedits(repo_id, file_b);
         CREATE TABLE IF NOT EXISTS repo_digests (
             repo_id  INTEGER PRIMARY KEY,
             built_at INTEGER NOT NULL,
             body     TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS ingest_state (
             source TEXT NOT NULL,
             key    TEXT NOT NULL,
             value  TEXT NOT NULL,
             PRIMARY KEY (source, key)
         ) WITHOUT ROWID;",
    )
}

fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout (and rows) the unversioned first release wrote.
    fn legacy_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tool_events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, session TEXT, project TEXT,
                 tool_kind TEXT NOT NULL, primary_cmd TEXT, chain TEXT, path TEXT,
                 redacted INTEGER NOT NULL DEFAULT 0);
             INSERT INTO tool_events (tool_kind, primary_cmd) VALUES ('Bash', 'cd');
             INSERT INTO tool_events (tool_kind, path) VALUES ('Read}:execute_tool_with_id{x', 'a.rs');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn legacy_db_upgrades_and_drops_line_keyed_rows() {
        let mut conn = legacy_db();
        assert_eq!(user_version(&conn).unwrap(), 0);
        migrate(&mut conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), CURRENT_VERSION);
        assert!(has_column(&conn, "tool_events", "call_key").unwrap());
        assert!(has_column(&conn, "tool_events", "source").unwrap());
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn migrations_are_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO tool_events (call_key, source, tool_kind) VALUES ('k', 'jfc', 'Read')",
            [],
        )
        .unwrap();
        // Re-running every migration from scratch must neither fail nor lose keyed rows.
        conn.pragma_update(None, "user_version", 0).unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        let dup = conn.execute(
            "INSERT INTO tool_events (call_key, source, tool_kind) VALUES ('k', 'jfc', 'Read')",
            [],
        );
        assert!(dup.is_err(), "call_key must be unique");
    }
}
