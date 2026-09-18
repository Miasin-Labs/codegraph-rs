//! The atlas's versioned schema (`PRAGMA user_version`) and its migrations.
//! Each migration is idempotent and runs in its own transaction, so an atlas
//! at any older version upgrades in place.

use rusqlite::{Connection, Transaction};

/// Schema version this build writes.
pub(crate) const CURRENT_VERSION: i64 = 1;

struct Migration {
    version: i64,
    apply: fn(&Transaction<'_>) -> rusqlite::Result<()>,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    apply: v1_projects_and_links,
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

/// v1: projects (one row per indexed checkout) and the links between them.
///
/// Every stat column is nullable: a project whose index can't be read is
/// still registered (status `unreadable`/`missing`) and its stats fill in
/// on the next registration. `languages` is a JSON array of
/// `[language, files]` pairs, most files first.
fn v1_projects_and_links(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (
             id                 INTEGER PRIMARY KEY,
             root               TEXT NOT NULL UNIQUE,
             name               TEXT NOT NULL,
             checkout_root      TEXT,
             repo_root          TEXT,
             is_worktree        INTEGER NOT NULL DEFAULT 0,
             remote             TEXT,
             branch             TEXT,
             head_commit        TEXT,
             languages          TEXT,
             file_count         INTEGER,
             node_count         INTEGER,
             edge_count         INTEGER,
             counts_exact       INTEGER NOT NULL DEFAULT 1,
             index_bytes        INTEGER,
             index_schema       INTEGER,
             extraction_version INTEGER,
             engine_version     TEXT,
             last_indexed       INTEGER,
             last_seen          INTEGER NOT NULL,
             registered_at      INTEGER NOT NULL,
             status             TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_projects_name ON projects(name);
         CREATE INDEX IF NOT EXISTS idx_projects_remote ON projects(remote)
             WHERE remote IS NOT NULL;
         CREATE INDEX IF NOT EXISTS idx_projects_repo ON projects(repo_root)
             WHERE repo_root IS NOT NULL;
         CREATE TABLE IF NOT EXISTS project_links (
             id            INTEGER PRIMARY KEY,
             from_project  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
             to_project    INTEGER REFERENCES projects(id) ON DELETE SET NULL,
             to_path       TEXT NOT NULL,
             kind          TEXT NOT NULL,
             evidence_path TEXT,
             evidence_line INTEGER,
             detail        TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_links_from ON project_links(from_project, kind);
         CREATE INDEX IF NOT EXISTS idx_links_to ON project_links(to_project, kind);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_links_identity ON project_links(
             from_project, kind, to_path, IFNULL(evidence_path, ''), IFNULL(evidence_line, -1)
         );",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn fresh_database_migrates_to_the_current_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(user_version(&conn).unwrap(), 0);
        migrate(&mut conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), CURRENT_VERSION);
        assert_eq!(tables(&conn), ["project_links", "projects"]);
    }

    #[test]
    fn migrations_are_idempotent_and_keep_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO projects (root, name, last_seen, registered_at, status)
             VALUES ('/r', 'r', 1, 1, 'ok')",
            [],
        )
        .unwrap();
        // Re-running every migration from scratch must neither fail nor lose rows.
        conn.pragma_update(None, "user_version", 0).unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        let dup = conn.execute(
            "INSERT INTO projects (root, name, last_seen, registered_at, status)
             VALUES ('/r', 'r', 1, 1, 'ok')",
            [],
        );
        assert!(dup.is_err(), "root must be unique");
    }
}
