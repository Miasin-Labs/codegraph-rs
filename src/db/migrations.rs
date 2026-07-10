//! Database Migrations
//!
//! Schema versioning and migration support.
//! Ported from `src/db/migrations.ts`.

use crate::db::connection::{Db, now_ms};
use crate::error::Result;

/// Current schema version.
///
/// Version 8 is the first shared superset of the historical Rust and
/// TypeScript schema-7 lineages. Both implementations independently used
/// versions 5-7 for different changes, so the v8 migration inspects the
/// database shape instead of assuming which lineage produced it.
pub const CURRENT_SCHEMA_VERSION: u32 = 8;

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
static MIGRATIONS: [Migration; 7] = [
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
];

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

    db.exec(
        "DELETE FROM edges
         WHERE id NOT IN (
           SELECT MIN(id) FROM edges
           GROUP BY source, target, kind, IFNULL(line, -1), IFNULL(col, -1)
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_identity
           ON edges(source, target, kind, IFNULL(line, -1), IFNULL(col, -1));
         CREATE TABLE IF NOT EXISTS name_segment_vocab (
           segment TEXT NOT NULL,
           name TEXT NOT NULL,
           PRIMARY KEY (segment, name)
         ) WITHOUT ROWID;",
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
