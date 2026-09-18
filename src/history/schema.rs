//! The history DB's versioned schema (`PRAGMA user_version`) and its
//! migrations. Each migration is idempotent and runs in its own
//! transaction, so a DB at any version — including the unversioned ones the
//! first release created — upgrades in place.

use rusqlite::{Connection, Transaction};

/// Schema version this build writes.
pub(crate) const CURRENT_VERSION: i64 = 2;

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
