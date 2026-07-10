//! Integration tests for the db module.
//!
//! Ports the db-layer cases from:
//! - `__tests__/db-perf.test.ts` (batch lookup, cache invalidation,
//!   edge-endpoint validation, runMaintenance)
//! - `__tests__/foundation.test.ts` ("Database Connection" + "Query Builder"
//!   suites)
//! - `__tests__/iterate-nodes-by-kind.test.ts` (#610 streaming)
//! - the rusqlite-applicable semantics of `__tests__/sqlite-backend.test.ts`
//!   and `__tests__/node-sqlite-backend.test.ts` (backend report, WAL mode,
//!   FTS5 read/write paths; the dual-backend selection logic is N/A)
//!
//! Real files, real SQLite, no mocks — same as the TS suite.

use std::collections::HashMap;

use codegraph::db::{CURRENT_SCHEMA_VERSION, DatabaseConnection, QueryBuilder, ResolvedRefKey};
use codegraph::types::{
    Edge,
    EdgeKind,
    FileRecord,
    Language,
    Node,
    NodeKind,
    SearchOptions,
    UnresolvedReference,
};
use tempfile::tempdir;

fn make_node(id: &str, name: &str) -> Node {
    Node::new(
        id,
        NodeKind::Function,
        name,
        name,
        "a.ts",
        Language::Typescript,
        1,
        1,
    )
}

fn setup() -> (tempfile::TempDir, DatabaseConnection, QueryBuilder) {
    let dir = tempdir().expect("tempdir");
    let db = DatabaseConnection::initialize(dir.path().join("test.db")).expect("initialize");
    let q = QueryBuilder::new(db.get_db().expect("get_db"));
    (dir, db, q)
}

// =============================================================================
// db-perf.test.ts — getNodesByIds (batch lookup)
// =============================================================================

#[test]
fn get_nodes_by_ids_returns_map_keyed_by_id() {
    let (_dir, _db, q) = setup();
    q.insert_nodes(&[
        make_node("n1", "n1"),
        make_node("n2", "n2"),
        make_node("n3", "n3"),
    ])
    .unwrap();
    let out = q
        .get_nodes_by_ids(&["n1".into(), "n2".into(), "n3".into()])
        .unwrap();
    assert_eq!(out.len(), 3);
    assert_eq!(out.get("n1").unwrap().name, "n1");
    assert_eq!(out.get("n3").unwrap().name, "n3");
}

#[test]
fn get_nodes_by_ids_omits_missing_ids() {
    let (_dir, _db, q) = setup();
    q.insert_nodes(&[make_node("n1", "n1"), make_node("n2", "n2")])
        .unwrap();
    let out = q
        .get_nodes_by_ids(&["n1".into(), "missing".into(), "n2".into()])
        .unwrap();
    assert_eq!(out.len(), 2);
    assert!(!out.contains_key("missing"));
    assert!(out.contains_key("n1"));
    assert!(out.contains_key("n2"));
}

#[test]
fn get_nodes_by_ids_handles_empty_input() {
    let (_dir, _db, q) = setup();
    assert_eq!(q.get_nodes_by_ids(&[]).unwrap().len(), 0);
}

#[test]
fn get_nodes_by_ids_chunks_over_param_limit() {
    // Insert 1500 nodes; the helper chunks at 500 internally.
    let (_dir, _db, q) = setup();
    let nodes: Vec<Node> = (0..1500)
        .map(|i| make_node(&format!("n{i}"), &format!("n{i}")))
        .collect();
    q.insert_nodes(&nodes).unwrap();
    let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let out = q.get_nodes_by_ids(&ids).unwrap();
    assert_eq!(out.len(), 1500);
    // Spot-check a few from the first / middle / last chunk.
    assert!(out.contains_key("n0"));
    assert!(out.contains_key("n750"));
    assert!(out.contains_key("n1499"));
}

#[test]
fn node_writes_populate_name_segment_vocabulary() {
    let (_dir, _db, q) = setup();
    let mut state_machine = make_node("state", "OrderStateMachine");
    let mut checkout = make_node("checkout", "CheckoutService");
    checkout.kind = NodeKind::Class;
    let mut file = make_node("file", "checkout_service.ts");
    file.kind = NodeKind::File;
    q.insert_nodes(&[state_machine.clone(), checkout, file])
        .unwrap();

    assert_eq!(
        q.get_names_for_segment("state", 10).unwrap(),
        ["OrderStateMachine"]
    );
    assert!(
        q.get_names_for_segment("checkout", 10)
            .unwrap()
            .contains(&"CheckoutService".to_string())
    );
    assert!(
        q.get_names_for_segment("ts", 10).unwrap().is_empty(),
        "file basenames must not contribute vocabulary rows"
    );

    let matches = q
        .get_segment_co_occurrence(
            &[
                ("state".into(), "state".into()),
                ("machine".into(), "machine".into()),
            ],
            2,
            10,
        )
        .unwrap();
    assert_eq!(matches, [("OrderStateMachine".to_string(), 2)]);

    state_machine.name = "OrderWorkflow".into();
    state_machine.qualified_name = "OrderWorkflow".into();
    q.update_node(&state_machine).unwrap();
    assert_eq!(
        q.get_names_for_segment("workflow", 10).unwrap(),
        ["OrderWorkflow"]
    );
}

#[test]
fn segment_vocabulary_can_be_rebuilt_without_orphans() {
    let (_dir, _db, q) = setup();
    let mut node = make_node("n", "BeforeRename");
    q.insert_node(&node).unwrap();
    node.name = "AfterRename".into();
    node.qualified_name = "AfterRename".into();
    q.update_node(&node).unwrap();
    assert!(!q.get_names_for_segment("before", 10).unwrap().is_empty());

    q.rebuild_name_segment_vocab(1).unwrap();
    assert!(q.get_names_for_segment("before", 10).unwrap().is_empty());
    assert_eq!(
        q.get_names_for_segment("after", 10).unwrap(),
        ["AfterRename"]
    );
    assert!(!q.is_name_segment_vocab_empty().unwrap());
}

#[test]
fn get_nodes_by_ids_serves_cache_hits_from_memory() {
    let (_dir, db, q) = setup();
    q.insert_nodes(&[
        make_node("n1", "n1"),
        make_node("n2", "n2"),
        make_node("n3", "n3"),
    ])
    .unwrap();
    // Warm the cache for n1 only.
    q.get_node_by_id("n1").unwrap();
    // Replace the underlying row to make a miss-vs-cache-hit detectable.
    db.get_db()
        .unwrap()
        .execute(
            "UPDATE nodes SET name = ? WHERE id = ?",
            rusqlite::params!["changed", "n1"],
        )
        .unwrap();
    let out = q.get_nodes_by_ids(&["n1".into(), "n2".into()]).unwrap();
    // The cached n1 (still 'n1', not 'changed') must be returned.
    assert_eq!(out.get("n1").unwrap().name, "n1");
    assert_eq!(out.get("n2").unwrap().name, "n2");
}

// =============================================================================
// db-perf.test.ts — insertNode cache invalidation
// =============================================================================

#[test]
fn insert_node_invalidates_cache_after_insert_or_replace() {
    // Regression: insertNode (which uses INSERT OR REPLACE) used to skip
    // cache invalidation, so the next getNodeById returned the pre-replace
    // version until LRU eviction.
    let (_dir, _db, q) = setup();
    let original = make_node("n1", "oldName");
    q.insert_node(&original).unwrap();
    let before_replace = q.get_node_by_id("n1").unwrap().unwrap();
    assert_eq!(before_replace.name, "oldName");

    // Replace via insert_node (the bug path).
    let mut replaced = original.clone();
    replaced.name = "newName".to_string();
    q.insert_node(&replaced).unwrap();
    let after_replace = q.get_node_by_id("n1").unwrap().unwrap();
    assert_eq!(after_replace.name, "newName");
}

// =============================================================================
// db-perf.test.ts — insertEdges endpoint validation
// =============================================================================

#[test]
fn insert_edges_skips_edges_with_missing_endpoints() {
    let (_dir, _db, q) = setup();
    q.insert_nodes(&[
        make_node("source", "source"),
        make_node("target", "target"),
        make_node("other", "other"),
    ])
    .unwrap();

    q.insert_edges(&[
        Edge::new("source", "target", EdgeKind::Calls),
        Edge::new("source", "missing-target", EdgeKind::Calls),
        Edge::new("missing-source", "other", EdgeKind::References),
    ])
    .expect("must not fail the whole batch");

    let edges = q.get_outgoing_edges("source", None, None).unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].source, "source");
    assert_eq!(edges[0].target, "target");
    assert_eq!(edges[0].kind, EdgeKind::Calls);
}

#[test]
fn insert_edges_does_not_trust_stale_cached_nodes() {
    let (_dir, db, q) = setup();
    q.insert_nodes(&[make_node("source", "source"), make_node("target", "target")])
        .unwrap();
    assert_eq!(q.get_node_by_id("target").unwrap().unwrap().id, "target");

    db.get_db()
        .unwrap()
        .execute(
            "DELETE FROM nodes WHERE id = ?",
            rusqlite::params!["target"],
        )
        .unwrap();

    q.insert_edges(&[Edge::new("source", "target", EdgeKind::Calls)])
        .expect("must not throw");
    assert!(
        q.get_outgoing_edges("source", None, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn edge_identity_index_deduplicates_coordinate_less_edges() {
    let (_dir, _db, q) = setup();
    q.insert_nodes(&[make_node("source", "source"), make_node("target", "target")])
        .unwrap();

    let mut extracted = Edge::new("source", "target", EdgeKind::Calls);
    extracted.metadata = Some(
        serde_json::json!({"pass": "extraction"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let mut synthesized = Edge::new("source", "target", EdgeKind::Calls);
    synthesized.metadata = Some(
        serde_json::json!({"pass": "synthesis"})
            .as_object()
            .unwrap()
            .clone(),
    );
    synthesized.provenance = Some(codegraph::types::Provenance::Heuristic);

    q.insert_edges(&[extracted, synthesized]).unwrap();
    let edges = q.get_outgoing_edges("source", None, None).unwrap();
    assert_eq!(edges.len(), 1);
}

// =============================================================================
// db-perf.test.ts — runMaintenance
// =============================================================================

#[test]
fn run_maintenance_on_fresh_database() {
    let (_dir, db, _q) = setup();
    db.run_maintenance(); // must not panic / propagate
}

#[test]
fn run_maintenance_after_writes() {
    let (_dir, db, q) = setup();
    q.insert_nodes(&[make_node("n1", "n1"), make_node("n2", "n2")])
        .unwrap();
    db.run_maintenance();
}

#[test]
fn run_maintenance_swallows_failures_after_close() {
    let (_dir, mut db, _q) = setup();
    db.close();
    db.run_maintenance(); // best-effort: must not propagate
}

// =============================================================================
// foundation.test.ts — Database Connection
// =============================================================================

#[test]
fn initializes_new_database() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let mut db = DatabaseConnection::initialize(&db_path).unwrap();

    assert!(db.is_open());
    assert!(db_path.exists());

    db.close();
    assert!(!db.is_open());
}

#[test]
fn gets_schema_version() {
    let (_dir, db, _q) = setup();
    let version = db.get_schema_version().unwrap();
    assert!(version.is_some());
    assert_eq!(version.unwrap().version, 8);
    assert_eq!(CURRENT_SCHEMA_VERSION, 8);
}

#[test]
fn supports_transactions() {
    let (_dir, db, _q) = setup();
    let result = db.transaction(|| Ok(42)).unwrap();
    assert_eq!(result, 42);
}

#[test]
fn transaction_rolls_back_on_error() {
    let (_dir, db, q) = setup();
    let result: codegraph::Result<()> = db.transaction(|| {
        q.insert_node(&make_node("doomed", "doomed"))?;
        Err(codegraph::CodeGraphError::other("boom"))
    });
    assert!(result.is_err());
    assert!(q.get_node_by_id("doomed").unwrap().is_none());
}

#[test]
fn nested_transactions_use_savepoints() {
    let (_dir, db, q) = setup();
    db.transaction(|| {
        q.insert_node(&make_node("outer", "outer"))?;
        // Inner failure rolls back only the inner savepoint.
        let inner: codegraph::Result<()> = db.transaction(|| {
            q.insert_node(&make_node("inner", "inner"))?;
            Err(codegraph::CodeGraphError::other("inner boom"))
        });
        assert!(inner.is_err());
        Ok(())
    })
    .unwrap();
    assert!(q.get_node_by_id("outer").unwrap().is_some());
    assert!(q.get_node_by_id("inner").unwrap().is_none());
}

#[test]
fn open_nonexistent_database_errors() {
    let dir = tempdir().unwrap();
    let err = DatabaseConnection::open(dir.path().join("nonexistent.db")).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("not found"),
        "expected /not found/i in: {err}"
    );
}

#[test]
fn creates_database_with_correct_schema() {
    let (_dir, db, _q) = setup();
    let handle = db.get_db().unwrap();
    let mut stmt = handle
        .conn()
        .prepare("SELECT name FROM sqlite_master WHERE type IN ('table', 'view')")
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    for expected in [
        "nodes",
        "edges",
        "files",
        "unresolved_refs",
        "nodes_fts",
        "project_metadata",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing table {expected}"
        );
    }
}

#[test]
fn returns_database_size() {
    let (_dir, db, _q) = setup();
    assert!(db.get_size().unwrap() > 0);
}

#[test]
fn supports_optimize_operation() {
    let (_dir, db, q) = setup();
    q.insert_nodes(&[make_node("n1", "n1")]).unwrap();
    db.optimize().unwrap();
}

// =============================================================================
// foundation.test.ts — Query Builder (empty-result behavior)
// =============================================================================

#[test]
fn returns_none_for_nonexistent_node() {
    let (_dir, _db, q) = setup();
    assert!(q.get_node_by_id("nonexistent").unwrap().is_none());
}

#[test]
fn returns_empty_for_nodes_in_nonexistent_file() {
    let (_dir, _db, q) = setup();
    assert!(q.get_nodes_by_file("nonexistent.ts").unwrap().is_empty());
}

#[test]
fn returns_empty_for_edges_from_nonexistent_node() {
    let (_dir, _db, q) = setup();
    assert!(
        q.get_outgoing_edges("nonexistent", None, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn returns_none_for_nonexistent_file() {
    let (_dir, _db, q) = setup();
    assert!(q.get_file_by_path("nonexistent.ts").unwrap().is_none());
}

#[test]
fn returns_empty_for_files_when_none_tracked() {
    let (_dir, _db, q) = setup();
    assert!(q.get_all_files().unwrap().is_empty());
}

// =============================================================================
// sqlite-backend.test.ts / node-sqlite-backend.test.ts — applicable semantics
// (backend selection/fallback collapses to rusqlite; only the reporting and
// the WAL/FTS5 behavior carries over)
// =============================================================================

#[test]
fn backend_reports_native_in_wal_mode() {
    let (_dir, db, _q) = setup();
    assert_eq!(db.get_backend().as_str(), "native");
    assert_eq!(db.get_journal_mode().unwrap(), "wal");
}

#[test]
fn fts5_module_is_available_in_bundled_sqlite() {
    // PORTING.md key detail: rusqlite is "bundled" — verify FTS5 exists.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(x)")
        .expect("bundled SQLite must ship FTS5");
    conn.execute("INSERT INTO t (x) VALUES ('hello world')", [])
        .unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM t WHERE t MATCH 'hello'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn fts5_search_returns_indexed_symbol() {
    // Read path through nodes_fts (kept in sync by triggers on the write path).
    let (_dir, _db, q) = setup();
    let mut node = make_node("auth1", "AuthService");
    node.file_path = "src/auth/service.ts".to_string();
    q.insert_node(&node).unwrap();

    let results = q.search_nodes("auth", &SearchOptions::default()).unwrap();
    assert!(
        results.iter().any(|r| r.node.name == "AuthService"),
        "FTS prefix search must find AuthService"
    );
}

#[test]
fn like_fallback_finds_mid_name_substring() {
    // FTS tokenizes "signInWithGoogle" as one token, so "WithGoogle" can only
    // be found by the LIKE substring fallback.
    let (_dir, _db, q) = setup();
    q.insert_node(&make_node("s1", "signInWithGoogle")).unwrap();

    let results = q
        .search_nodes("WithGoogle", &SearchOptions::default())
        .unwrap();
    assert!(results.iter().any(|r| r.node.name == "signInWithGoogle"));
}

#[test]
fn fuzzy_fallback_finds_close_typo() {
    let (_dir, _db, q) = setup();
    q.insert_node(&make_node("u1", "getUser")).unwrap();

    let results = q
        .search_nodes("getUssr", &SearchOptions::default())
        .unwrap();
    assert!(
        results.iter().any(|r| r.node.name == "getUser"),
        "edit-distance fallback must find getUser for getUssr"
    );
}

#[test]
fn search_supports_kind_and_path_field_filters() {
    let (_dir, _db, q) = setup();
    let mut func = make_node("f1", "process");
    func.file_path = "src/api/process.ts".to_string();
    q.insert_node(&func).unwrap();
    let mut class = Node::new(
        "c1",
        NodeKind::Class,
        "process",
        "process",
        "src/other/process.ts",
        Language::Typescript,
        1,
        1,
    );
    class.file_path = "src/other/process.ts".to_string();
    q.insert_node(&class).unwrap();

    // kind: filter narrows to functions only
    let funcs = q
        .search_nodes("kind:function process", &SearchOptions::default())
        .unwrap();
    assert!(!funcs.is_empty());
    assert!(funcs.iter().all(|r| r.node.kind == NodeKind::Function));

    // path: filter is a hard gate
    let in_api = q
        .search_nodes("path:src/api process", &SearchOptions::default())
        .unwrap();
    assert!(!in_api.is_empty());
    assert!(
        in_api
            .iter()
            .all(|r| r.node.file_path.to_lowercase().contains("src/api"))
    );
}

#[test]
fn search_intersects_options_with_query_field_filters() {
    let (_dir, _db, q) = setup();
    let func = make_node("f1", "process");
    let class = Node::new(
        "c1",
        NodeKind::Class,
        "process",
        "process",
        "src/other/process.ts",
        Language::Typescript,
        1,
        1,
    );
    q.insert_nodes(&[func, class]).unwrap();

    let kind_results = q
        .search_nodes(
            "kind:class process",
            &SearchOptions {
                kinds: Some(vec![NodeKind::Function]),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        kind_results.is_empty(),
        "kind filters from options and query must be intersected"
    );

    let mut rust_func = make_node("f2", "shared");
    rust_func.language = Language::Rust;
    rust_func.file_path = "src/shared.rs".to_string();
    let mut ts_func = make_node("f3", "shared");
    ts_func.language = Language::Typescript;
    ts_func.file_path = "src/shared.ts".to_string();
    q.insert_nodes(&[rust_func, ts_func]).unwrap();

    let language_results = q
        .search_nodes(
            "lang:typescript shared",
            &SearchOptions {
                languages: Some(vec![Language::Rust]),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        language_results.is_empty(),
        "language filters from options and query must be intersected"
    );
}

// =============================================================================
// iterate-nodes-by-kind.test.ts (#610 streaming)
// =============================================================================

fn iterate_fixture(q: &QueryBuilder) {
    // Mirrors the TS fixture: two functions, one class with two methods.
    q.insert_nodes(&[
        make_node("foo", "foo"),
        make_node("bar", "bar"),
        Node::new(
            "C",
            NodeKind::Class,
            "C",
            "C",
            "a.ts",
            Language::Typescript,
            3,
            3,
        ),
        Node::new(
            "C.m",
            NodeKind::Method,
            "m",
            "C.m",
            "a.ts",
            Language::Typescript,
            3,
            3,
        ),
        Node::new(
            "C.n",
            NodeKind::Method,
            "n",
            "C.n",
            "a.ts",
            Language::Typescript,
            3,
            3,
        ),
    ])
    .unwrap();
}

#[test]
fn iterate_yields_same_nodes_as_eager_get_nodes_by_kind() {
    let (_dir, _db, q) = setup();
    iterate_fixture(&q);

    for kind in [NodeKind::Function, NodeKind::Method, NodeKind::Class] {
        let mut eager: Vec<String> = q
            .get_nodes_by_kind(kind)
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect();
        eager.sort();
        let mut streamed: Vec<String> = Vec::new();
        q.iterate_nodes_by_kind(kind, |n| {
            streamed.push(n.id);
            true
        })
        .unwrap();
        streamed.sort();
        assert_eq!(streamed, eager);
    }
    // sanity: the fixture actually produced functions + methods to stream
    let mut fn_count = 0;
    q.iterate_nodes_by_kind(NodeKind::Function, |_| {
        fn_count += 1;
        true
    })
    .unwrap();
    assert!(fn_count > 0);
    let mut m_count = 0;
    q.iterate_nodes_by_kind(NodeKind::Method, |_| {
        m_count += 1;
        true
    })
    .unwrap();
    assert!(m_count > 0);
}

#[test]
fn iterate_cursor_stays_valid_while_other_queries_run() {
    let (_dir, _db, q) = setup();
    iterate_fixture(&q);

    let mut seen = 0;
    q.iterate_nodes_by_kind(NodeKind::Function, |n| {
        // A different prepared statement stepped on the same connection while
        // the iterator's cursor is open must not corrupt it.
        let again = q.get_node_by_id(&n.id).unwrap();
        assert_eq!(again.map(|x| x.id), Some(n.id));
        seen += 1;
        true
    })
    .unwrap();
    assert_eq!(seen, q.get_nodes_by_kind(NodeKind::Function).unwrap().len());
}

#[test]
fn iterate_supports_early_stop() {
    let (_dir, _db, q) = setup();
    iterate_fixture(&q);
    let mut seen = 0;
    q.iterate_nodes_by_kind(NodeKind::Function, |_| {
        seen += 1;
        false // stop after first
    })
    .unwrap();
    assert_eq!(seen, 1);
}

// =============================================================================
// Migrations — a legacy v1 database is upgraded on open
// =============================================================================

#[test]
fn open_migrates_legacy_v1_database_to_current() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("legacy.db");

    // Hand-build a v1-shaped database (pre-migration-2 schema: no
    // project_metadata, no provenance on edges, no file_path/language on
    // unresolved_refs, the later-dropped narrow edge indexes present).
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL, description TEXT);
             INSERT INTO schema_versions VALUES (1, 0, 'Initial schema');
             CREATE TABLE nodes (
               id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
               qualified_name TEXT NOT NULL, file_path TEXT NOT NULL,
               language TEXT NOT NULL, start_line INTEGER NOT NULL,
               end_line INTEGER NOT NULL, start_column INTEGER NOT NULL,
               end_column INTEGER NOT NULL, docstring TEXT, signature TEXT,
               visibility TEXT, is_exported INTEGER DEFAULT 0,
               is_async INTEGER DEFAULT 0, is_static INTEGER DEFAULT 0,
               is_abstract INTEGER DEFAULT 0, decorators TEXT,
               type_parameters TEXT, updated_at INTEGER NOT NULL
             );
             CREATE TABLE edges (
               id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL,
               target TEXT NOT NULL, kind TEXT NOT NULL, metadata TEXT,
               line INTEGER, col INTEGER
             );
             CREATE TABLE files (
               path TEXT PRIMARY KEY, content_hash TEXT NOT NULL,
               language TEXT NOT NULL, size INTEGER NOT NULL,
               modified_at INTEGER NOT NULL, indexed_at INTEGER NOT NULL,
               node_count INTEGER DEFAULT 0, errors TEXT
             );
             CREATE TABLE unresolved_refs (
               id INTEGER PRIMARY KEY AUTOINCREMENT, from_node_id TEXT NOT NULL,
               reference_name TEXT NOT NULL, reference_kind TEXT NOT NULL,
               line INTEGER NOT NULL, col INTEGER NOT NULL, candidates TEXT
             );
             CREATE INDEX idx_edges_source ON edges(source);
             CREATE INDEX idx_edges_target ON edges(target);",
        )
        .unwrap();
    }

    let db = DatabaseConnection::open(&db_path).unwrap();
    assert_eq!(db.get_schema_version().unwrap().unwrap().version, 8);

    let handle = db.get_db().unwrap();
    // Migration 2 added columns + project_metadata
    let unresolved_cols: Vec<String> = {
        let mut stmt = handle
            .conn()
            .prepare("SELECT name FROM pragma_table_info('unresolved_refs')")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert!(unresolved_cols.iter().any(|c| c == "file_path"));
    assert!(unresolved_cols.iter().any(|c| c == "language"));
    assert!(unresolved_cols.iter().any(|c| c == "metadata"));
    let edge_cols: Vec<String> = {
        let mut stmt = handle
            .conn()
            .prepare("SELECT name FROM pragma_table_info('edges')")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert!(edge_cols.iter().any(|c| c == "provenance"));
    // Migration 4 dropped the redundant narrow indexes
    let dropped: i64 = handle
        .conn()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name IN ('idx_edges_source', 'idx_edges_target')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dropped, 0);
    // History records each applied migration
    let history = codegraph::db::get_migration_history(&handle).unwrap();
    let versions: Vec<u32> = history.iter().map(|h| h.version).collect();
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn open_does_not_rerun_migrations_on_current_database() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let _db = DatabaseConnection::initialize(&db_path).unwrap();
    }
    let db = DatabaseConnection::open(&db_path).unwrap();
    assert_eq!(db.get_schema_version().unwrap().unwrap().version, 8);
    let handle = db.get_db().unwrap();
    assert!(!codegraph::db::needs_migration(&handle));
    let history = codegraph::db::get_migration_history(&handle).unwrap();
    let versions: Vec<u32> = history.iter().map(|h| h.version).collect();
    assert_eq!(versions, vec![1, 8]);
}

#[test]
fn open_migrates_v4_database_adding_byte_offset_columns() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("v4.db");

    // Hand-build a v4-shaped database: the full pre-v5 schema (no
    // start_byte/end_byte on nodes) with versions 1–4 recorded, plus one
    // pre-existing node row.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL, description TEXT);
             INSERT INTO schema_versions VALUES (1, 0, 'Initial schema');
             INSERT INTO schema_versions VALUES (2, 0, 'metadata/provenance/refs');
             INSERT INTO schema_versions VALUES (3, 0, 'lower(name) index');
             INSERT INTO schema_versions VALUES (4, 0, 'drop narrow edge indexes');
             CREATE TABLE nodes (
               id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
               qualified_name TEXT NOT NULL, file_path TEXT NOT NULL,
               language TEXT NOT NULL, start_line INTEGER NOT NULL,
               end_line INTEGER NOT NULL, start_column INTEGER NOT NULL,
               end_column INTEGER NOT NULL, docstring TEXT, signature TEXT,
               visibility TEXT, is_exported INTEGER DEFAULT 0,
               is_async INTEGER DEFAULT 0, is_static INTEGER DEFAULT 0,
               is_abstract INTEGER DEFAULT 0, decorators TEXT,
               type_parameters TEXT, updated_at INTEGER NOT NULL
             );
             CREATE TABLE edges (
               id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL,
               target TEXT NOT NULL, kind TEXT NOT NULL, metadata TEXT,
               line INTEGER, col INTEGER, provenance TEXT DEFAULT NULL
             );
             CREATE TABLE files (
               path TEXT PRIMARY KEY, content_hash TEXT NOT NULL,
               language TEXT NOT NULL, size INTEGER NOT NULL,
               modified_at INTEGER NOT NULL, indexed_at INTEGER NOT NULL,
               node_count INTEGER DEFAULT 0, errors TEXT
             );
             CREATE TABLE unresolved_refs (
               id INTEGER PRIMARY KEY AUTOINCREMENT, from_node_id TEXT NOT NULL,
               reference_name TEXT NOT NULL, reference_kind TEXT NOT NULL,
               line INTEGER NOT NULL, col INTEGER NOT NULL, candidates TEXT,
               file_path TEXT NOT NULL DEFAULT '', language TEXT NOT NULL DEFAULT 'unknown'
             );
             INSERT INTO nodes (id, kind, name, qualified_name, file_path, language,
                                start_line, end_line, start_column, end_column, updated_at)
             VALUES ('old1', 'function', 'legacy', 'a.ts::legacy', 'a.ts', 'typescript',
                     1, 3, 0, 1, 0);",
        )
        .unwrap();
    }

    let db = DatabaseConnection::open(&db_path).unwrap();
    assert_eq!(db.get_schema_version().unwrap().unwrap().version, 8);
    let handle = db.get_db().unwrap();

    // v5 added the nullable byte-offset columns.
    let node_cols: Vec<String> = {
        let mut stmt = handle
            .conn()
            .prepare("SELECT name FROM pragma_table_info('nodes')")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert!(node_cols.iter().any(|c| c == "start_byte"));
    assert!(node_cols.iter().any(|c| c == "end_byte"));
    // v6 added the nullable binary address / size columns.
    assert!(node_cols.iter().any(|c| c == "address"));
    assert!(node_cols.iter().any(|c| c == "size"));

    // The pre-existing row was backfilled with NULL and reads back gracefully.
    let q = QueryBuilder::new(handle.clone());
    let legacy = q.get_node_by_id("old1").unwrap().expect("legacy row");
    assert_eq!(legacy.start_byte, None);
    assert_eq!(legacy.end_byte, None);
    assert_eq!(legacy.byte_range(), None);
    assert_eq!(legacy.address, None);
    assert_eq!(legacy.size, None);
    assert_eq!(legacy.return_type, None);

    // New writes round-trip byte offsets + address/size through the migration.
    let mut node = make_node("new1", "fresh");
    node.start_byte = Some(10);
    node.end_byte = Some(42);
    node.address = Some(0x1719D0);
    node.size = Some(308);
    node.return_type = Some("Widget".to_string());
    q.insert_node(&node).unwrap();
    q.clear_cache();
    let fresh = q.get_node_by_id("new1").unwrap().expect("fresh row");
    assert_eq!(fresh.start_byte, Some(10));
    assert_eq!(fresh.end_byte, Some(42));
    assert_eq!(fresh.byte_range(), Some(10..42));
    assert_eq!(fresh.address, Some(0x1719D0));
    assert_eq!(fresh.size, Some(308));
    assert_eq!(fresh.return_type.as_deref(), Some("Widget"));

    let history = codegraph::db::get_migration_history(&handle).unwrap();
    let versions: Vec<u32> = history.iter().map(|h| h.version).collect();
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn open_migrates_rust_v7_shape_and_enforces_edge_identity() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("rust-v7.db");

    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_versions (
               version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL, description TEXT
             );
             INSERT INTO schema_versions VALUES (7, 0, 'Rust schema 7');
             CREATE TABLE nodes (
               id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
               qualified_name TEXT NOT NULL, file_path TEXT NOT NULL,
               language TEXT NOT NULL, start_line INTEGER NOT NULL,
               end_line INTEGER NOT NULL, start_column INTEGER NOT NULL,
               end_column INTEGER NOT NULL, start_byte INTEGER, end_byte INTEGER,
               address INTEGER, size INTEGER, docstring TEXT, signature TEXT,
               visibility TEXT, is_exported INTEGER DEFAULT 0,
               is_async INTEGER DEFAULT 0, is_static INTEGER DEFAULT 0,
               is_abstract INTEGER DEFAULT 0, decorators TEXT,
               type_parameters TEXT, updated_at INTEGER NOT NULL
             );
             CREATE TABLE edges (
               id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL,
               target TEXT NOT NULL, kind TEXT NOT NULL, metadata TEXT,
               line INTEGER, col INTEGER, provenance TEXT DEFAULT NULL
             );
             CREATE TABLE unresolved_refs (
               id INTEGER PRIMARY KEY AUTOINCREMENT, from_node_id TEXT NOT NULL,
               reference_name TEXT NOT NULL, reference_kind TEXT NOT NULL,
               line INTEGER NOT NULL, col INTEGER NOT NULL, candidates TEXT,
               metadata TEXT, file_path TEXT NOT NULL DEFAULT '',
               language TEXT NOT NULL DEFAULT 'unknown'
             );
             INSERT INTO edges (source, target, kind, metadata, line, col, provenance)
               VALUES ('a', 'b', 'calls', '{\"first\":true}', NULL, NULL, 'tree-sitter');
             INSERT INTO edges (source, target, kind, metadata, line, col, provenance)
               VALUES ('a', 'b', 'calls', '{\"second\":true}', NULL, NULL, 'heuristic');
             INSERT INTO edges (source, target, kind, line, col)
               VALUES ('a', 'b', 'calls', 10, 4);
             INSERT INTO edges (source, target, kind, line, col)
               VALUES ('a', 'b', 'calls', 10, 4);",
        )
        .unwrap();
    }

    let db = DatabaseConnection::open(&db_path).unwrap();
    assert_eq!(db.get_schema_version().unwrap().unwrap().version, 8);
    let handle = db.get_db().unwrap();

    let return_type_exists: i64 = handle
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('nodes') WHERE name = 'return_type'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(return_type_exists, 1);

    let edge_count: i64 = handle
        .conn()
        .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
        .unwrap();
    assert_eq!(edge_count, 2);

    let identity_index: i64 = handle
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_edges_identity'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(identity_index, 1);

    let vocab_table: i64 = handle
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'name_segment_vocab'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(vocab_table, 1);

    let inserted = handle
        .conn()
        .execute(
            "INSERT OR IGNORE INTO edges (source, target, kind, line, col) VALUES ('a', 'b', 'calls', NULL, NULL)",
            [],
        )
        .unwrap();
    assert_eq!(inserted, 0);
}

#[test]
fn open_migrates_typescript_v7_shape_without_duplicate_column_failures() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("typescript-v7.db");

    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_versions (
               version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL, description TEXT
             );
             INSERT INTO schema_versions VALUES (7, 0, 'TypeScript schema 7');
             CREATE TABLE nodes (
               id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
               qualified_name TEXT NOT NULL, file_path TEXT NOT NULL,
               language TEXT NOT NULL, start_line INTEGER NOT NULL,
               end_line INTEGER NOT NULL, start_column INTEGER NOT NULL,
               end_column INTEGER NOT NULL, docstring TEXT, signature TEXT,
               return_type TEXT, visibility TEXT, is_exported INTEGER DEFAULT 0,
               is_async INTEGER DEFAULT 0, is_static INTEGER DEFAULT 0,
               is_abstract INTEGER DEFAULT 0, decorators TEXT,
               type_parameters TEXT, updated_at INTEGER NOT NULL
             );
             INSERT INTO nodes (
               id, kind, name, qualified_name, file_path, language,
               start_line, end_line, start_column, end_column, return_type, updated_at
             ) VALUES (
               'factory', 'function', 'makeWidget', 'makeWidget', 'a.cpp', 'cpp',
               1, 1, 0, 10, 'Widget', 0
             );
             CREATE TABLE edges (
               id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL,
               target TEXT NOT NULL, kind TEXT NOT NULL, metadata TEXT,
               line INTEGER, col INTEGER, provenance TEXT DEFAULT NULL
             );
             CREATE UNIQUE INDEX idx_edges_identity
               ON edges(source, target, kind, IFNULL(line, -1), IFNULL(col, -1));
             CREATE TABLE unresolved_refs (
               id INTEGER PRIMARY KEY AUTOINCREMENT, from_node_id TEXT NOT NULL,
               reference_name TEXT NOT NULL, reference_kind TEXT NOT NULL,
               line INTEGER NOT NULL, col INTEGER NOT NULL, candidates TEXT,
               file_path TEXT NOT NULL DEFAULT '', language TEXT NOT NULL DEFAULT 'unknown'
             );
             CREATE TABLE name_segment_vocab (
               segment TEXT NOT NULL, name TEXT NOT NULL,
               PRIMARY KEY (segment, name)
             ) WITHOUT ROWID;",
        )
        .unwrap();
    }

    let db = DatabaseConnection::open(&db_path).unwrap();
    assert_eq!(db.get_schema_version().unwrap().unwrap().version, 8);
    let handle = db.get_db().unwrap();

    let node_columns: Vec<String> = {
        let mut stmt = handle
            .conn()
            .prepare("SELECT name FROM pragma_table_info('nodes')")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    for expected in ["start_byte", "end_byte", "address", "size", "return_type"] {
        assert!(node_columns.iter().any(|column| column == expected));
    }

    let unresolved_metadata: i64 = handle
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('unresolved_refs') WHERE name = 'metadata'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unresolved_metadata, 1);

    let q = QueryBuilder::new(handle);
    let factory = q.get_node_by_id("factory").unwrap().expect("factory row");
    assert_eq!(factory.return_type.as_deref(), Some("Widget"));
    assert_eq!(factory.start_byte, None);
    assert_eq!(factory.address, None);
}

// =============================================================================
// Symbol-lookup helpers (QueryBuilder slice of __tests__/symbol-lookup.test.ts;
// the matchesSymbol ranking itself lives in the MCP tools layer)
// =============================================================================

#[test]
fn name_lookups_exact_lower_and_qualified() {
    let (_dir, _db, q) = setup();
    let mut run_a = Node::new(
        "r1",
        NodeKind::Function,
        "run",
        "src/configurator/stage_apply.rs::run",
        "src/configurator/stage_apply.rs",
        Language::Rust,
        1,
        3,
    );
    run_a.language = Language::Rust;
    let run_b = Node::new(
        "r2",
        NodeKind::Function,
        "run",
        "src/configurator/stage_detect.rs::run",
        "src/configurator/stage_detect.rs",
        Language::Rust,
        1,
        1,
    );
    q.insert_nodes(&[run_a, run_b]).unwrap();

    assert_eq!(q.get_nodes_by_name("run").unwrap().len(), 2);
    assert_eq!(q.get_nodes_by_lower_name("run").unwrap().len(), 2);
    let exact = q
        .get_nodes_by_qualified_name_exact("src/configurator/stage_apply.rs::run")
        .unwrap();
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].id, "r1");
    // FTS treats :: as a separator (#173) — a qualified query still finds run
    let results = q
        .search_nodes("stage_apply::run", &SearchOptions::default())
        .unwrap();
    assert!(results.iter().any(|r| r.node.name == "run"));
}

#[test]
fn find_nodes_by_exact_name_boosts_colocated_results() {
    let (_dir, _db, q) = setup();
    // "scrapeLoop" is distinctive (1 file); "run" is common.
    let mut scrape = make_node("s1", "scrapeLoop");
    scrape.file_path = "scrape/manager.go".to_string();
    let mut run_close = make_node("run1", "run");
    run_close.file_path = "scrape/manager.go".to_string();
    let mut run_far = make_node("run2", "run");
    run_far.file_path = "web/api.go".to_string();
    q.insert_nodes(&[scrape, run_close, run_far]).unwrap();

    let results = q
        .find_nodes_by_exact_name(
            &["scrapeLoop".to_string(), "run".to_string()],
            &SearchOptions::default(),
        )
        .unwrap();
    assert!(!results.is_empty());
    // The co-located "run" (same file as scrapeLoop) must outrank the other.
    let run_results: Vec<_> = results.iter().filter(|r| r.node.name == "run").collect();
    assert!(run_results.len() >= 2);
    assert_eq!(run_results[0].node.file_path, "scrape/manager.go");
}

#[test]
fn find_nodes_by_name_substring_orders_by_length() {
    let (_dir, _db, q) = setup();
    q.insert_nodes(&[
        make_node("a", "TransportSearchAction"),
        make_node("b", "SearchAction"),
    ])
    .unwrap();
    let results = q
        .find_nodes_by_name_substring("Search", &SearchOptions::default(), false)
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].node.name, "SearchAction"); // shorter first

    // exclude_prefix drops names that START with the substring
    let no_prefix = q
        .find_nodes_by_name_substring("Search", &SearchOptions::default(), true)
        .unwrap();
    assert_eq!(no_prefix.len(), 1);
    assert_eq!(no_prefix[0].node.name, "TransportSearchAction");
}

// =============================================================================
// Files / unresolved refs / stats / metadata round-trips
// =============================================================================

fn make_file(path: &str) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        content_hash: format!("hash-{path}"),
        language: Language::Typescript,
        size: 100,
        modified_at: 1000,
        indexed_at: 2000,
        node_count: 1,
        errors: None,
    }
}

#[test]
fn file_upsert_get_delete_roundtrip() {
    let (_dir, _db, q) = setup();
    q.upsert_file(&make_file("a.ts")).unwrap();
    q.upsert_file(&make_file("b.ts")).unwrap();

    let got = q.get_file_by_path("a.ts").unwrap().unwrap();
    assert_eq!(got.content_hash, "hash-a.ts");
    assert_eq!(q.get_all_files().unwrap().len(), 2);
    assert_eq!(q.get_all_file_paths().unwrap(), vec!["a.ts", "b.ts"]);
    assert_eq!(q.get_last_indexed_at().unwrap(), Some(2000));

    // Upsert updates in place
    let mut updated = make_file("a.ts");
    updated.content_hash = "new-hash".to_string();
    updated.indexed_at = 3000;
    q.upsert_file(&updated).unwrap();
    assert_eq!(
        q.get_file_by_path("a.ts").unwrap().unwrap().content_hash,
        "new-hash"
    );
    assert_eq!(q.get_last_indexed_at().unwrap(), Some(3000));

    // delete_file removes the record AND the file's nodes
    q.insert_node(&make_node("n1", "inA")).unwrap(); // file_path = a.ts
    q.delete_file("a.ts").unwrap();
    assert!(q.get_file_by_path("a.ts").unwrap().is_none());
    assert!(q.get_nodes_by_file("a.ts").unwrap().is_empty());
}

#[test]
fn get_stale_files_detects_hash_changes() {
    let (_dir, _db, q) = setup();
    q.upsert_file(&make_file("a.ts")).unwrap();
    q.upsert_file(&make_file("b.ts")).unwrap();

    let mut hashes = HashMap::new();
    hashes.insert("a.ts".to_string(), "different".to_string());
    hashes.insert("b.ts".to_string(), "hash-b.ts".to_string()); // unchanged

    let stale = q.get_stale_files(&hashes).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].path, "a.ts");
}

fn make_ref(from: &str, name: &str, file: &str) -> UnresolvedReference {
    UnresolvedReference {
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: EdgeKind::Calls,
        line: 1,
        column: 0,
        file_path: Some(file.to_string()),
        language: Some(Language::Typescript),
        candidates: None,
        metadata: None,
    }
}

#[test]
fn unresolved_refs_roundtrip() {
    let (_dir, _db, q) = setup();
    q.insert_nodes(&[make_node("n1", "n1"), make_node("n2", "n2")])
        .unwrap();
    let mut helper_ref = make_ref("n1", "helper", "a.ts");
    helper_ref.metadata = Some(serde_json::Map::from_iter([
        ("kind".to_string(), serde_json::json!("call_argument_roles")),
        ("callee".to_string(), serde_json::json!("memcpy")),
    ]));
    q.insert_unresolved_refs_batch(&[
        helper_ref,
        make_ref("n1", "other", "a.ts"),
        make_ref("n2", "helper", "b.ts"),
    ])
    .unwrap();

    assert_eq!(q.get_unresolved_references_count().unwrap(), 3);
    assert_eq!(q.get_unresolved_by_name("helper").unwrap().len(), 2);
    let all_refs = q.get_unresolved_references().unwrap();
    assert_eq!(all_refs.len(), 3);
    assert!(all_refs.iter().any(|r| {
        r.reference_name == "helper"
            && r.metadata
                .as_ref()
                .and_then(|m| m.get("kind"))
                .and_then(|v| v.as_str())
                == Some("call_argument_roles")
    }));
    assert_eq!(
        q.get_unresolved_references_by_files(&["a.ts".to_string()])
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        q.get_unresolved_references_by_names(&["helper".to_string(), String::new()])
            .unwrap()
            .len(),
        2
    );

    // Stable-id pagination
    let batch = q.get_unresolved_references_batch_after_id(0, 2).unwrap();
    assert_eq!(batch.refs.len(), 2);
    let rest = q
        .get_unresolved_references_batch_after_id(batch.last_id, 10)
        .unwrap();
    assert_eq!(rest.refs.len(), 1);

    // LIMIT/OFFSET pagination
    assert_eq!(q.get_unresolved_references_batch(1, 10).unwrap().len(), 2);

    // Precise deletion by (from, name, kind)
    q.delete_specific_resolved_references(&[ResolvedRefKey {
        from_node_id: "n1".to_string(),
        reference_name: "helper".to_string(),
        reference_kind: "calls".to_string(),
    }])
    .unwrap();
    assert_eq!(q.get_unresolved_references_count().unwrap(), 2);

    // Bulk deletion by from-node ids
    q.delete_resolved_references(&["n1".to_string()]).unwrap();
    assert_eq!(q.get_unresolved_references_count().unwrap(), 1);

    q.clear_unresolved_references().unwrap();
    assert_eq!(q.get_unresolved_references_count().unwrap(), 0);
}

#[test]
fn stats_and_metadata() {
    let (_dir, _db, q) = setup();
    q.insert_nodes(&[make_node("n1", "n1"), make_node("n2", "n2")])
        .unwrap();
    q.insert_edges(&[Edge::new("n1", "n2", EdgeKind::Calls)])
        .unwrap();
    q.upsert_file(&make_file("a.ts")).unwrap();

    let counts = q.get_node_and_edge_count().unwrap();
    assert_eq!(counts.nodes, 2);
    assert_eq!(counts.edges, 1);

    let stats = q.get_stats().unwrap();
    assert_eq!(stats.node_count, 2);
    assert_eq!(stats.edge_count, 1);
    assert_eq!(stats.file_count, 1);
    assert_eq!(stats.nodes_by_kind.get("function"), Some(&2));
    assert_eq!(stats.edges_by_kind.get("calls"), Some(&1));
    assert_eq!(stats.files_by_language.get("typescript"), Some(&1));

    assert_eq!(q.get_metadata("k").unwrap(), None);
    q.set_metadata("k", "v1").unwrap();
    q.set_metadata("k", "v2").unwrap(); // upsert
    assert_eq!(q.get_metadata("k").unwrap(), Some("v2".to_string()));
    assert_eq!(
        q.get_all_metadata().unwrap().get("k"),
        Some(&"v2".to_string())
    );

    q.clear().unwrap();
    let counts = q.get_node_and_edge_count().unwrap();
    assert_eq!(counts.nodes, 0);
    assert_eq!(counts.edges, 0);
    assert!(q.get_all_files().unwrap().is_empty());
}

#[test]
fn edge_queries_filter_by_kind_and_provenance() {
    let (_dir, _db, q) = setup();
    q.insert_nodes(&[
        make_node("a", "a"),
        make_node("b", "b"),
        make_node("c", "c"),
    ])
    .unwrap();
    let mut heuristic = Edge::new("a", "c", EdgeKind::References);
    heuristic.provenance = Some(codegraph::types::Provenance::Heuristic);
    q.insert_edges(&[Edge::new("a", "b", EdgeKind::Calls), heuristic])
        .unwrap();

    assert_eq!(q.get_outgoing_edges("a", None, None).unwrap().len(), 2);
    assert_eq!(
        q.get_outgoing_edges("a", Some(&[EdgeKind::Calls]), None)
            .unwrap()
            .len(),
        1
    );
    let h = q.get_outgoing_edges("a", None, Some("heuristic")).unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].target, "c");

    assert_eq!(q.get_incoming_edges("b", None).unwrap().len(), 1);
    assert_eq!(
        q.get_incoming_edges("b", Some(&[EdgeKind::References]))
            .unwrap()
            .len(),
        0
    );

    // json_each batched variants
    assert_eq!(
        q.get_outgoing_edges_for_sources(&["a".to_string()], None)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        q.get_incoming_edges_for_targets(&["b".to_string(), "c".to_string()], None)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        q.find_edges_between_nodes(&["a".to_string(), "b".to_string()], None)
            .unwrap()
            .len(),
        1
    );

    q.delete_edges_by_source("a").unwrap();
    assert!(q.get_outgoing_edges("a", None, None).unwrap().is_empty());
}

#[test]
fn dominant_and_route_queries_return_none_on_empty_db() {
    let (_dir, _db, q) = setup();
    assert!(q.get_dominant_file().unwrap().is_none());
    assert!(q.get_top_route_file().unwrap().is_none());
    assert!(q.get_routing_manifest(None).unwrap().is_none());
}

#[test]
fn dominant_file_requires_meaningful_concentration_and_skips_tests() {
    let (_dir, _db, q) = setup();
    // 25 in-file edges in src/core.ts (above the >= 20 threshold)…
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for i in 0..26 {
        let mut n = make_node(&format!("core{i}"), &format!("core{i}"));
        n.file_path = "src/core.ts".to_string();
        nodes.push(n);
    }
    for i in 0..25 {
        edges.push(Edge::new(
            format!("core{i}"),
            format!("core{}", i + 1),
            EdgeKind::Calls,
        ));
    }
    // …and even MORE edges inside a test file, which must be filtered out.
    for i in 0..40 {
        let mut n = make_node(&format!("t{i}"), &format!("t{i}"));
        n.file_path = "src/__tests__/big.test.ts".to_string();
        nodes.push(n);
    }
    for i in 0..39 {
        edges.push(Edge::new(
            format!("t{i}"),
            format!("t{}", i + 1),
            EdgeKind::Calls,
        ));
    }
    q.insert_nodes(&nodes).unwrap();
    q.insert_edges(&edges).unwrap();

    let dominant = q.get_dominant_file().unwrap().unwrap();
    assert_eq!(dominant.file_path, "src/core.ts");
    assert_eq!(dominant.edge_count, 25);
}

#[test]
fn routing_manifest_joins_routes_to_handlers() {
    let (_dir, _db, q) = setup();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for i in 0..4 {
        let mut route = Node::new(
            format!("route{i}"),
            NodeKind::Route,
            format!("GET /things/{i}"),
            format!("GET /things/{i}"),
            "src/routes.ts",
            Language::Typescript,
            (i + 1) as u32,
            (i + 1) as u32,
        );
        route.file_path = "src/routes.ts".to_string();
        nodes.push(route);
        let mut handler = make_node(&format!("handler{i}"), &format!("handler{i}"));
        handler.file_path = "src/handlers.ts".to_string();
        nodes.push(handler);
        edges.push(Edge::new(
            format!("route{i}"),
            format!("handler{i}"),
            EdgeKind::References,
        ));
    }
    q.insert_nodes(&nodes).unwrap();
    q.insert_edges(&edges).unwrap();

    let manifest = q.get_routing_manifest(None).unwrap().unwrap();
    assert_eq!(manifest.total_routes, 4);
    assert_eq!(manifest.entries.len(), 4);
    assert_eq!(
        manifest.top_handler_file.as_deref(),
        Some("src/handlers.ts")
    );
    assert_eq!(manifest.top_handler_file_count, 4);
    assert!(manifest.entries.iter().any(|e| e.url == "GET /things/0"));

    let top_route_file = q.get_top_route_file().unwrap().unwrap();
    assert_eq!(top_route_file.file_path, "src/routes.ts");
    assert_eq!(top_route_file.route_count, 4);
}

#[test]
fn node_json_roundtrips_optional_fields() {
    // decorators / type_parameters / docstring survive the JSON columns.
    let (_dir, _db, q) = setup();
    let mut node = make_node("d1", "decorated");
    node.docstring = Some("Does things.".to_string());
    node.signature = Some("fn decorated()".to_string());
    node.return_type = Some("Widget".to_string());
    node.visibility = Some(codegraph::types::Visibility::Private);
    node.is_async = Some(true);
    node.decorators = Some(vec!["@deprecated".to_string()]);
    node.type_parameters = Some(vec!["T".to_string()]);
    q.insert_node(&node).unwrap();
    q.clear_cache(); // force a DB read

    let got = q.get_node_by_id("d1").unwrap().unwrap();
    assert_eq!(got.docstring.as_deref(), Some("Does things."));
    assert_eq!(got.signature.as_deref(), Some("fn decorated()"));
    assert_eq!(got.return_type.as_deref(), Some("Widget"));
    assert_eq!(got.visibility, Some(codegraph::types::Visibility::Private));
    assert_eq!(got.is_async, Some(true));
    assert_eq!(got.is_exported, Some(false));
    assert_eq!(got.decorators, Some(vec!["@deprecated".to_string()]));
    assert_eq!(got.type_parameters, Some(vec!["T".to_string()]));
}

#[test]
fn update_and_delete_node() {
    let (_dir, _db, q) = setup();
    q.insert_node(&make_node("n1", "before")).unwrap();
    let mut changed = make_node("n1", "after");
    changed.start_line = 7;
    changed.return_type = Some("Updated".to_string());
    q.update_node(&changed).unwrap();
    let got = q.get_node_by_id("n1").unwrap().unwrap();
    assert_eq!(got.name, "after");
    assert_eq!(got.start_line, 7);
    assert_eq!(got.return_type.as_deref(), Some("Updated"));

    q.delete_node("n1").unwrap();
    assert!(q.get_node_by_id("n1").unwrap().is_none());

    // Validation: empty id is skipped silently (no error, no row)
    let bogus = make_node("", "nameless");
    q.insert_node(&bogus).unwrap();
    assert!(q.get_nodes_by_name("nameless").unwrap().is_empty());
}

// =============================================================================
// TS-written database compatibility — REAL (float) numeric columns
// =============================================================================

/// The TS implementation stores plain JS numbers; `files.modified_at` comes
/// from `fs.statSync().mtimeMs`, which is FRACTIONAL, so a TS-written
/// database holds REAL where the Rust port writes INTEGER. The readers must
/// accept both or a `codegraph status` against a Node-built index fails with
/// "Invalid column type Real" (found switching a real project from the npm
/// build to this crate).
#[test]
fn reads_ts_written_real_timestamps() {
    let (_dir, db, q) = setup();

    // Simulate Node writing fractional timestamps and a REAL updated_at.
    db.get_db()
        .unwrap()
        .execute_batch(
            "INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count)
             VALUES ('src/a.ts', 'abc', 'typescript', 120.0, 1749167991123.4561, 1749167999456.789, 3);",
        )
        .unwrap();
    db.get_db()
        .unwrap()
        .execute_batch(
            "INSERT INTO nodes (id, kind, name, qualified_name, file_path, language,
                                start_line, end_line, start_column, end_column,
                                is_exported, is_async, is_static, is_abstract, updated_at)
             VALUES ('n-real', 'function', 'f', 'src/a.ts::f', 'src/a.ts', 'typescript',
                     1, 2, 0, 0, 0, 0, 0, 0, 1749167991123.875);",
        )
        .unwrap();

    let f = q.get_file_by_path("src/a.ts").unwrap().expect("file row");
    assert_eq!(f.modified_at, 1749167991123); // truncated like a JS reader
    assert_eq!(f.indexed_at, 1749167999456);
    assert_eq!(f.size, 120);

    let n = q.get_node_by_id("n-real").unwrap().expect("node row");
    assert_eq!(n.updated_at, 1749167991123);

    // MAX(indexed_at) goes through its own lenient path.
    assert_eq!(q.get_last_indexed_at().unwrap(), Some(1749167999456));
}
