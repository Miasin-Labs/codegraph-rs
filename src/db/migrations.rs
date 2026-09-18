//! Database Migrations
//!
//! Schema versioning and migration support.
//! Ported from `src/db/migrations.ts`.

use std::path::Path;

use crate::db::connection::{Db, now_ms};
use crate::error::Result;

/// Current schema version.
///
/// Version 9 is a shared superset that both the Rust and TypeScript v9 readers
/// can open. Shape repair remains idempotent because a foreign v9 database is
/// already version-current and would otherwise skip Rust's missing columns.
/// Version 10 adds `external_edges` (cross-graph references); every table a
/// v9 reader uses is unchanged.
pub const CURRENT_SCHEMA_VERSION: u32 = 10;

/// The oldest schema whose `nodes`/`edges`/`files` tables this build reads
/// as its own. Read-only consumers of *another* graph (a dependency shard, a
/// linked project's index) accept anything from here to
/// [`CURRENT_SCHEMA_VERSION`]; only v10's `external_edges` is missing below.
pub const MIN_READABLE_SCHEMA_VERSION: u32 = 9;

/// Migration definition.
pub struct Migration {
    pub version: u32,
    pub description: &'static str,
    up: fn(&Db) -> Result<()>,
}

/// All migrations in order.
///
/// Note: Version 1 is the initial schema, handled by schema.sql.
/// Future migrations go here.
static MIGRATIONS: [Migration; 9] = [
    Migration {
        version: 2,
        description: "Add project metadata, provenance tracking, and unresolved ref context",
        up: |db| {
            db.exec(
                "CREATE TABLE IF NOT EXISTS project_metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                ALTER TABLE unresolved_refs ADD COLUMN file_path TEXT NOT NULL DEFAULT '';
                ALTER TABLE unresolved_refs ADD COLUMN language TEXT NOT NULL DEFAULT 'unknown';
                ALTER TABLE edges ADD COLUMN provenance TEXT DEFAULT NULL;
                CREATE INDEX IF NOT EXISTS idx_unresolved_file_path ON unresolved_refs(file_path);
                CREATE INDEX IF NOT EXISTS idx_edges_provenance ON edges(provenance);",
            )
        },
    },
    Migration {
        version: 3,
        description: "Add lower(name) expression index for memory-efficient case-insensitive lookups",
        up: |db| db.exec("CREATE INDEX IF NOT EXISTS idx_nodes_lower_name ON nodes(lower(name));"),
    },
    Migration {
        version: 4,
        description: "Drop redundant idx_edges_source / idx_edges_target (covered by source_kind / target_kind composites)",
        up: |db| {
            db.exec(
                "DROP INDEX IF EXISTS idx_edges_source;
                 DROP INDEX IF EXISTS idx_edges_target;",
            )
        },
    },
    Migration {
        version: 5,
        description: "Add nullable start_byte / end_byte byte offsets to nodes (tree-sitter byte ranges; backfill NULL, populated on re-index)",
        up: |db| {
            db.exec(
                "ALTER TABLE nodes ADD COLUMN start_byte INTEGER;
                 ALTER TABLE nodes ADD COLUMN end_byte INTEGER;",
            )
        },
    },
    Migration {
        version: 6,
        description: "Add nullable address / size to nodes (binary virtual address + size for decompiled IDA/Hex-Rays output; backfill NULL, populated on re-index)",
        up: |db| {
            db.exec(
                "ALTER TABLE nodes ADD COLUMN address INTEGER;
                 ALTER TABLE nodes ADD COLUMN size INTEGER;",
            )
        },
    },
    Migration {
        version: 7,
        description: "Add nullable metadata to unresolved_refs (copied onto resolved edges; backfill NULL, populated on re-index)",
        up: |db| db.exec("ALTER TABLE unresolved_refs ADD COLUMN metadata TEXT;"),
    },
    Migration {
        version: 8,
        description: "Unify Rust and TypeScript schema-7 lineages, deduplicate edges, and add prompt vocabulary",
        up: migrate_unified_schema_v8,
    },
    Migration {
        version: 9,
        description: "Reconcile Rust and TypeScript v9 store columns and indexes",
        up: repair_shared_schema_v9,
    },
    Migration {
        version: 10,
        description: "Add external_edges: references resolved into dependency shards and linked projects",
        up: create_external_edges,
    },
];

/// v10: the cross-graph edge table (idempotent; `schema.sql` holds the same
/// DDL for fresh databases).
pub(crate) fn create_external_edges(db: &Db) -> Result<()> {
    db.exec(EXTERNAL_EDGES_DDL)
}

/// The `external_edges` table and its indexes — kept in step with
/// `schema.sql` (a test compares them).
pub(crate) const EXTERNAL_EDGES_DDL: &str = "
CREATE TABLE IF NOT EXISTS external_edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL,
    kind TEXT NOT NULL,
    target_graph_kind TEXT NOT NULL,
    target_graph_key TEXT NOT NULL,
    target_node_id TEXT NOT NULL,
    target_name TEXT NOT NULL,
    target_qualified_name TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_file_path TEXT NOT NULL,
    target_line INTEGER,
    reference_name TEXT NOT NULL,
    line INTEGER,
    col INTEGER,
    confidence REAL NOT NULL,
    resolved_by TEXT NOT NULL,
    metadata TEXT,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (source) REFERENCES nodes(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_external_edges_identity
  ON external_edges(source, kind, target_graph_key, target_node_id, IFNULL(line, -1), IFNULL(col, -1));
CREATE INDEX IF NOT EXISTS idx_external_edges_source ON external_edges(source, kind);
CREATE INDEX IF NOT EXISTS idx_external_edges_target
  ON external_edges(target_graph_key, target_node_id);
";

fn table_has_column(db: &Db, table: &str, column: &str) -> Result<bool> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = db.conn().prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_exists(db: &Db, table: &str) -> Result<bool> {
    let count: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        [table],
        |row| row.get(0),
    )?;
    Ok(count == 1)
}

fn index_exists(db: &Db, index: &str) -> Result<bool> {
    let count: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
        [index],
        |row| row.get(0),
    )?;
    Ok(count == 1)
}

fn add_column_if_missing(db: &Db, table: &str, column: &str, ddl: &str) -> Result<()> {
    if !table_has_column(db, table, column)? {
        db.exec(ddl)?;
    }
    Ok(())
}

/// Reconcile databases written by either schema-7 lineage.
fn migrate_unified_schema_v8(db: &Db) -> Result<()> {
    add_column_if_missing(
        db,
        "nodes",
        "start_byte",
        "ALTER TABLE nodes ADD COLUMN start_byte INTEGER;",
    )?;
    add_column_if_missing(
        db,
        "nodes",
        "end_byte",
        "ALTER TABLE nodes ADD COLUMN end_byte INTEGER;",
    )?;
    add_column_if_missing(
        db,
        "nodes",
        "address",
        "ALTER TABLE nodes ADD COLUMN address INTEGER;",
    )?;
    add_column_if_missing(
        db,
        "nodes",
        "size",
        "ALTER TABLE nodes ADD COLUMN size INTEGER;",
    )?;
    add_column_if_missing(
        db,
        "nodes",
        "return_type",
        "ALTER TABLE nodes ADD COLUMN return_type TEXT;",
    )?;
    add_column_if_missing(
        db,
        "unresolved_refs",
        "metadata",
        "ALTER TABLE unresolved_refs ADD COLUMN metadata TEXT;",
    )?;

    // This body also runs on every open (via `repair_shared_schema_v9`). Once
    // the unique index exists duplicates are impossible, so skip the dedupe:
    // it is a full edges scan that costs seconds on a multi-million-edge graph.
    if !index_exists(db, "idx_edges_identity")? {
        db.exec(
            "DELETE FROM edges
             WHERE id NOT IN (
               SELECT MIN(id) FROM edges
               GROUP BY source, target, kind, IFNULL(line, -1), IFNULL(col, -1)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_identity
               ON edges(source, target, kind, IFNULL(line, -1), IFNULL(col, -1));",
        )?;
    }
    db.exec(
        "CREATE TABLE IF NOT EXISTS name_segment_vocab (
           segment TEXT NOT NULL,
           name TEXT NOT NULL,
           PRIMARY KEY (segment, name)
         ) WITHOUT ROWID;",
    )
}

/// Idempotently reconcile the current Rust and TypeScript v9 shapes.
pub(crate) fn repair_shared_schema_v9(db: &Db) -> Result<()> {
    migrate_unified_schema_v8(db)?;
    add_column_if_missing(
        db,
        "unresolved_refs",
        "status",
        "ALTER TABLE unresolved_refs ADD COLUMN status TEXT NOT NULL DEFAULT 'pending';",
    )?;
    add_column_if_missing(
        db,
        "unresolved_refs",
        "name_tail",
        "ALTER TABLE unresolved_refs ADD COLUMN name_tail TEXT NOT NULL DEFAULT '';",
    )?;
    if table_exists(db, "files")? {
        add_column_if_missing(
            db,
            "files",
            "generated",
            "ALTER TABLE files ADD COLUMN generated INTEGER NOT NULL DEFAULT 0;",
        )?;
        db.exec(
            "CREATE INDEX IF NOT EXISTS idx_files_generated
               ON files(path) WHERE generated = 1;",
        )?;
    }
    db.exec(
        "CREATE INDEX IF NOT EXISTS idx_unresolved_status ON unresolved_refs(status);
         CREATE INDEX IF NOT EXISTS idx_unresolved_failed_tail
           ON unresolved_refs(name_tail) WHERE status = 'failed';",
    )
}

/// Get the current schema version from the database.
pub fn get_current_version(db: &Db) -> u32 {
    // Table may not exist yet — treat any error as version 0.
    db.conn()
        .query_row(
            "SELECT MAX(version) as version FROM schema_versions",
            [],
            |row| row.get::<_, Option<u32>>(0),
        )
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// Record a migration as applied.
fn record_migration(db: &Db, version: u32, description: &str) -> Result<()> {
    db.conn().execute(
        "INSERT INTO schema_versions (version, applied_at, description) VALUES (?, ?, ?)",
        rusqlite::params![version, now_ms(), description],
    )?;
    Ok(())
}

/// Run all pending migrations.
pub fn run_migrations(db: &Db, from_version: u32) -> Result<()> {
    // MIGRATIONS is declared in version order; filter to the pending ones.
    let mut pending: Vec<&Migration> = MIGRATIONS
        .iter()
        .filter(|m| m.version > from_version)
        .collect();

    if pending.is_empty() {
        return Ok(());
    }

    // Sort by version
    pending.sort_by_key(|m| m.version);

    // Run each migration in a transaction
    for migration in pending {
        db.transaction(|| {
            (migration.up)(db)?;
            record_migration(db, migration.version, migration.description)
        })?;
    }
    Ok(())
}

/// Check if the database needs migration.
pub fn needs_migration(db: &Db) -> bool {
    get_current_version(db) < CURRENT_SCHEMA_VERSION
}

/// Whether the database file at `db_path` is already at the current schema,
/// checked through a read-only connection that can never start a migration.
/// Unreadable or missing databases report `false`.
pub fn database_schema_is_current(db_path: &Path) -> bool {
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    conn.query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
        row.get::<_, Option<u32>>(0)
    })
    .ok()
    .flatten()
    .is_some_and(|version| version >= CURRENT_SCHEMA_VERSION)
}

/// Get list of pending migrations.
pub fn get_pending_migrations(db: &Db) -> Vec<&'static Migration> {
    let current = get_current_version(db);
    let mut pending: Vec<&'static Migration> =
        MIGRATIONS.iter().filter(|m| m.version > current).collect();
    pending.sort_by_key(|m| m.version);
    pending
}

/// One applied-migration record from `schema_versions`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationRecord {
    pub version: u32,
    pub applied_at: i64,
    pub description: Option<String>,
}

/// Get migration history from database.
pub fn get_migration_history(db: &Db) -> Result<Vec<MigrationRecord>> {
    let mut stmt = db.conn().prepare_cached(
        "SELECT version, applied_at, description FROM schema_versions ORDER BY version",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(MigrationRecord {
            version: row.get(0)?,
            applied_at: row.get(1)?,
            description: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}
