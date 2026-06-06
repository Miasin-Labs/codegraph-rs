# db module port notes

Ported from `src/db/{index,queries,migrations,sqlite-adapter}.ts` into
`rust/src/db/{mod,connection,queries,migrations}.rs`. `schema.sql` is embedded
via `include_str!` (`db::SCHEMA_SQL`). All 49 integration tests
(`tests/db_test.rs`) + 11 unit tests pass; `cargo check` is clean for these
files.

## Backend collapse (sqlite-adapter.ts)

- The TS dual backend (node:sqlite / wasm fallback) collapses to rusqlite
  (`bundled`). `SqliteBackend` is a one-variant enum whose `as_str()` /
  serde value is **`"native"`** (per PORTING.md; the TS string was
  `'node-sqlite'` — status/MCP output should report `"native"`).
- **FTS5 is available** in the bundled rusqlite build — verified by
  `fts5_module_is_available_in_bundled_sqlite` in `tests/db_test.rs`
  (`CREATE VIRTUAL TABLE … USING fts5` + MATCH both work). No Cargo.toml
  change needed.

## Key API shape (for the wiring wave)

```rust
// connection.rs
pub struct DatabaseConnection;            // TS DatabaseConnection
impl DatabaseConnection {
    pub fn initialize(db_path: impl AsRef<Path>) -> Result<Self>;
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self>;     // Err "Database not found: <path>"
    pub fn get_db(&self) -> Result<Db>;                         // cheap Rc clone
    pub fn get_backend(&self) -> SqliteBackend;                 // "native"
    pub fn get_path(&self) -> &Path;
    pub fn get_journal_mode(&self) -> Result<String>;           // lowercased, e.g. "wal"
    pub fn get_schema_version(&self) -> Result<Option<SchemaVersion>>;
    pub fn transaction<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T>;
    pub fn get_size(&self) -> Result<u64>;
    pub fn optimize(&self) -> Result<()>;                       // VACUUM; ANALYZE
    pub fn run_maintenance(&self);                              // best-effort, never errors
    pub fn close(&mut self);                                    // note: &mut
    pub fn is_open(&self) -> bool;
}
pub struct Db;                            // TS SqliteDatabase analog; Clone; Deref<Target=Connection>
impl Db { pub fn conn(&self) -> &Connection; pub fn exec(&self, sql) -> Result<()>;
          pub fn transaction<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T>; }
pub const DATABASE_FILENAME: &str = "codegraph.db";
pub fn get_database_path(project_root) -> PathBuf;              // <root>/.codegraph/codegraph.db

// queries.rs — QueryBuilder::new(db: Db). Full TS surface, snake_cased:
// insert_node/insert_nodes/update_node/delete_node/delete_nodes_by_file,
// get_node_by_id -> Result<Option<Node>>, get_nodes_by_ids(&[String]) -> Result<HashMap<String,Node>>,
// clear_cache, get_nodes_by_file, get_dominant_file -> Result<Option<DominantFile>>,
// get_top_route_file -> Result<Option<TopRouteFile>>,
// get_routing_manifest(limit: Option<usize> /*default 40*/) -> Result<Option<RoutingManifest>>,
// get_nodes_by_kind, iterate_nodes_by_kind(kind, FnMut(Node)->bool) (see below), get_all_nodes,
// get_nodes_by_name/get_nodes_by_qualified_name_exact/get_nodes_by_lower_name,
// search_nodes(query, &SearchOptions) -> Result<Vec<SearchResult>>,
// find_nodes_by_exact_name(&[String], &SearchOptions),
// find_nodes_by_name_substring(substr, &SearchOptions, exclude_prefix: bool),
// insert_edge/insert_edges (endpoint-validated vs DB, skips missing),
// delete_edges_by_source,
// get_outgoing_edges(id, kinds: Option<&[EdgeKind]>, provenance: Option<&str>),
// get_outgoing_edges_for_sources/get_incoming_edges/get_incoming_edges_for_targets/find_edges_between_nodes,
// upsert_file/delete_file/get_file_by_path/get_all_files/get_last_indexed_at/get_stale_files(&HashMap),
// insert_unresolved_ref/insert_unresolved_refs_batch/delete_unresolved_by_node/
// get_unresolved_by_name/get_unresolved_references/get_unresolved_references_count/
// get_unresolved_references_batch(offset, limit)/
// get_unresolved_references_batch_after_id(after_id: i64, limit) -> Result<UnresolvedBatch>,
// get_all_file_paths/get_all_node_names,
// get_unresolved_references_by_files/get_unresolved_references_by_names (chunked at 500),
// clear_unresolved_references/delete_resolved_references/delete_specific_resolved_references(&[ResolvedRefKey]),
// get_node_and_edge_count -> Result<NodeEdgeCount>, get_stats -> Result<GraphStats>,
// get_metadata/set_metadata/get_all_metadata, clear.
// New result structs: DominantFile, TopRouteFile, RoutingManifest(+Entry),
// NodeEdgeCount, UnresolvedBatch, ResolvedRefKey (all re-exported from db::).

// migrations.rs
pub const CURRENT_SCHEMA_VERSION: u32 = 4;
pub fn get_current_version(&Db) -> u32;        // 0 if schema_versions missing
pub fn run_migrations(&Db, from_version: u32) -> Result<()>;
pub fn needs_migration(&Db) -> bool;
pub fn get_pending_migrations(&Db) -> Vec<&'static Migration>;
pub fn get_migration_history(&Db) -> Result<Vec<MigrationRecord>>;
```

## Deviations from TS (deliberate)

1. **Threading:** `Db` is `Rc<Connection>`-based → **not `Send`**. This mirrors
   the single-threaded TS runtime. The MCP daemon / any threaded consumer must
   confine DB access to one thread (channel-funnel pattern). If the wiring
   wave needs `Send`, the options are an owning DB thread + crossbeam channel,
   or swapping `Rc` for `Arc<Mutex<…>>` (beware: `iterate_nodes_by_kind`'s
   open-cursor-plus-reentrant-query contract would deadlock a plain Mutex).
2. **`iterateNodesByKind` generator → visitor callback**:
   `iterate_nodes_by_kind(kind, FnMut(Node) -> bool)` (return `false` to stop).
   Same O(1)-memory streaming property (#610); a Rust lending iterator over a
   rusqlite cursor would be self-referential. Re-entrant queries from inside
   the callback are tested and safe.
3. **Transactions:** TS relied on better-sqlite3's nested-transaction
   savepoints. `Db::transaction` reimplements that: depth 0 = BEGIN/COMMIT,
   nested = SAVEPOINT/RELEASE, rollback on `Err`. NOT panic-safe (a panic
   inside the closure leaves the txn open) — acceptable for tests; fix with a
   drop-guard if panics must be survivable.
4. **Error handling:** TS throws → Rust `crate::error::Result`. Methods that
   TS made infallible-by-swallowing keep that behavior (`run_maintenance`
   swallows everything incl. closed-DB; `searchNodesFTS`'s try/catch → returns
   empty vec on FTS syntax error).
5. **insert validation:** TS falsy-checks id/kind/name/filePath/language;
   kind/language are enums in Rust (always valid), so only empty
   id/name/file_path skip with the same `console.error` →
   `log_error("Skipping node …")`. Empty `qualified_name` falls back to `name`
   (TS used `??` for undefined; Rust has no undefined — closest intent match).
6. **`updated_at == 0` → `Date.now()`** on insert/update (TS `?? Date.now()`
   catches undefined; Rust uses 0 as the "unset" sentinel — `Node::new`
   defaults to 0).
7. **Node bool flags:** rows always produce `Some(true/false)` for
   isExported/isAsync/isStatic/isAbstract (TS rowToNode always set booleans),
   so JSON wire output matches TS.
8. **Unknown enum strings from DB:** TS casts blindly. Rust: unknown
   `language` → `Language::Unknown`; unknown `visibility`/`provenance` →
   `None`; unknown `kind` → conversion error (we never write one).
9. **u64 vs SQLite:** rusqlite 0.40 dropped `FromSql/ToSql for u64`; counts
   are read as `i64` and clamped `.max(0) as u64`.
10. **`delete_resolved_references` is unchunked** (faithful: the TS version
    builds one IN-list; only the *by_files/by_names* getters chunk at 500).
11. **Prepared statements:** the TS lazily-initialized `stmts` map becomes
    rusqlite `prepare_cached` (cache capacity raised to 64 in
    `configure_connection`). Dynamic IN-list SQL uses plain `prepare` to avoid
    cache pollution.

## Local copies pending de-duplication (IMPORTANT for wiring wave)

`queries.rs` has a private `pub(crate) mod local` with faithful ports of
helpers whose owning modules were stubs at port time:

- `src/search/query-utils.ts` → `kind_bonus`, `name_match_bonus`,
  `score_path_relevance` (+ `extract_search_terms` stems-off path,
  `is_test_file`, STOP_WORDS)
- `src/search/query-parser.ts` → `parse_query`/`ParsedQuery`,
  `bounded_edit_distance`
- `src/extraction/generated-detection.ts` → `is_generated_file`

When the `search` module and `extraction::generated_detection` land, swap
these imports (top of `queries.rs`) for the canonical versions and delete
`mod local`. It is `pub(crate)` so the search-module owner may also choose to
re-export/move it. Behavior notes: `bounded_edit_distance` compares `char`s
(TS compares UTF-16 units — differs only for non-BMP input);
`extract_search_terms` implements only the `stems: false` path (the only mode
queries.ts uses); `getStemVariants`/`isDistinctiveIdentifier` were NOT ported
here (they belong to the search module).

## Dropped Node-isms

- `__dirname`-relative schema load → `include_str!("schema.sql")` (no
  copy-assets step needed; the file ships inside the binary).
- better-sqlite3 `db.open` flag → `DatabaseConnection::is_open()` over an
  `Option<Db>`; `close()` takes `&mut self`. Note an outstanding
  `Db`/`QueryBuilder` clone keeps the underlying SQLite handle alive after
  `close()` (Rc semantics) — fine on Unix; on Windows temp-dir deletion may
  fail while clones live.
- `getJournalMode`'s array/object pragma-shape juggling → single
  `query_row("PRAGMA journal_mode")`.

## Tests ported

- `__tests__/db-perf.test.ts` → all 11 cases in `tests/db_test.rs` (no
  absolute timings existed; nothing to relax).
- `__tests__/foundation.test.ts` → "Database Connection" (4) + "Query Builder"
  (5) + db-flavored "Database" cases (size/optimize/clear/schema) adapted to
  QueryBuilder (the TS versions go through the CodeGraph facade).
- `__tests__/iterate-nodes-by-kind.test.ts` → both cases + an early-stop case
  (fixture built by direct inserts — the TS version indexes a TS file through
  CodeGraph, which isn't ported yet).
- `__tests__/sqlite-backend.test.ts` / `node-sqlite-backend.test.ts` → only
  the semantics that survive the collapse: backend string, WAL journal mode,
  FTS5 write/read path, named-param insert path. Backend *selection/fallback*
  logic is N/A in Rust (single backend).
- `__tests__/symbol-lookup.test.ts` targets the **MCP tools layer**
  (`findSymbolMatches`/`matchesSymbol`), not QueryBuilder — left to the MCP
  port. The QueryBuilder primitives it leans on (exact/lower/qualified name
  lookup, `::`-separator FTS handling per #173) are covered in
  `name_lookups_exact_lower_and_qualified`.
- Extra coverage: legacy-v1 → v4 migration on open (columns added, narrow
  edge indexes dropped, history recorded), savepoint nesting, rollback on
  error, FTS5 availability probe, routing manifest / dominant-file heuristics,
  unresolved-ref pagination + precise deletion, stats/metadata round-trips.

## Integration needs

- `CodeGraph` facade (TS `src/index.ts`) should hold `DatabaseConnection` +
  `QueryBuilder` (constructed via `QueryBuilder::new(conn.get_db()?)`); both
  share the same `Rc<Connection>`.
- `GraphStats.db_size_bytes` is left 0 by `get_stats()` — caller fills it from
  `DatabaseConnection::get_size()` (same as TS).
- `codegraph status` should surface `get_backend().as_str()` ("native") and
  `get_journal_mode()`.
- `search_nodes` takes `&SearchOptions` (crate::types). `include_patterns`/
  `exclude_patterns`/`case_sensitive` are accepted-but-unused — same as TS.
