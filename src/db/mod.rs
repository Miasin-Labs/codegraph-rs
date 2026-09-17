//! Database layer — SQLite-backed storage for the knowledge graph.
//!
//! Mirrors `src/db/` in the TS implementation:
//! - `connection` ← `index.ts` (DatabaseConnection + pragmas + bootstrap)
//!   and the surviving surface of `sqlite-adapter.ts` (backend reporting —
//!   the dual better-sqlite3/wasm backend collapses to rusqlite/"native").
//! - `queries` ← `queries.ts` (QueryBuilder, the full prepared-statement
//!   surface).
//! - `migrations` ← `migrations.ts`.

pub mod connection;
pub mod migrations;
pub mod queries;

pub use connection::{
    DATABASE_FILENAME,
    DatabaseConnection,
    Db,
    SCHEMA_SQL,
    SqliteBackend,
    WalHealResult,
    get_database_path,
    resolve_wal_heal_bytes,
    wal_heal_threshold_bytes,
};
pub use migrations::{
    CURRENT_SCHEMA_VERSION,
    Migration,
    MigrationRecord,
    database_schema_is_current,
    get_current_version,
    get_migration_history,
    get_pending_migrations,
    needs_migration,
    run_migrations,
};
pub use queries::{
    DominantFile,
    NodeEdgeCount,
    QueryBuilder,
    ResolvedRefKey,
    RoutingManifest,
    RoutingManifestEntry,
    TopRouteFile,
    UnresolvedBatch,
};
