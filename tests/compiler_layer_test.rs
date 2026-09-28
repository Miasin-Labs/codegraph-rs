//! The compiler layer end to end over `tests/compiler_fixture/` — a small
//! crate with trait dispatch, generics, a `macro_rules!`-generated function
//! and a cross-file generic bound — and its checked-in SCIP index
//! (`index.scip`, written by rust-analyzer, project root rewritten).
//!
//! The checked-in index needs no rust-analyzer; the tests that run it skip
//! when it is not on `PATH`, like the other tool-dependent tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use codegraph::compiler::{
    CompilerOptions,
    CompilerOutcome,
    CompilerReport,
    RunStatus,
    rust_analyzer_program,
    rust_analyzer_version,
};
use codegraph::resolution::external::graphs::FederationHome;
use codegraph::{CodeGraph, IndexOptions};
use rusqlite::Connection;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compiler_fixture")
}

/// A copy of the fixture crate (without its index) in a temp dir.
fn fixture_copy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    for file in ["Cargo.toml", "src/lib.rs", "src/other.rs"] {
        std::fs::copy(fixture_dir().join(file), dir.path().join(file)).unwrap();
    }
    dir
}

fn options(home: &Path) -> CompilerOptions {
    let mut options = CompilerOptions::from_env();
    options.home = FederationHome::at(home);
    options.run_budget = Duration::from_secs(300);
    options
}

async fn indexed(root: &Path) -> CodeGraph {
    let cg = CodeGraph::init_sync(root).unwrap();
    let result = cg.index_all(&IndexOptions::default()).await.unwrap();
    assert!(result.success);
    cg
}

fn db(root: &Path) -> Connection {
    Connection::open(root.join(".codegraph/codegraph.db")).unwrap()
}

/// `source-qname -kind-> target-qname [provenance] {metadata.compiler}`
fn edges(root: &Path) -> Vec<String> {
    let conn = db(root);
    let mut stmt = conn
        .prepare(
            "SELECT s.qualified_name, e.kind, t.qualified_name, IFNULL(e.provenance, '-'),
                    IFNULL(json_extract(e.metadata, '$.compiler'), '-'),
                    IFNULL(json_extract(e.metadata, '$.compilerVerified'), 0)
             FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
             WHERE e.kind != 'contains' ORDER BY 1, 2, 3",
        )
        .unwrap();
    stmt.query_map([], |row| {
        Ok(format!(
            "{} -{}-> {} [{}] {{{}}}{}",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            if row.get::<_, i64>(5)? == 1 {
                " verified"
            } else {
                ""
            }
        ))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

fn unresolved(root: &Path) -> Vec<String> {
    let conn = db(root);
    let mut stmt = conn
        .prepare("SELECT reference_name FROM unresolved_refs ORDER BY 1")
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The checked-in index as if a run had written it, with the record of
/// the files it describes (their hashes as copied).
fn install_checked_in_index(root: &Path) {
    let compiler_dir = root.join(".codegraph/compiler");
    std::fs::create_dir_all(&compiler_dir).unwrap();
    std::fs::copy(
        fixture_dir().join("index.scip"),
        compiler_dir.join("index.scip"),
    )
    .unwrap();
    let files = ["src/lib.rs", "src/other.rs"]
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(root.join(path)).unwrap();
            (
                path.to_string(),
                codegraph::utils::sha256_hex(text.as_bytes()),
            )
        })
        .collect();
    let state = codegraph::compiler::CompilerState {
        version: codegraph::compiler::run::COMPILER_STATE_VERSION,
        fingerprint: "checked-in".to_string(),
        files,
        ..Default::default()
    };
    std::fs::write(
        compiler_dir.join("state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
}

fn has(edges: &[String], needle: &str) -> bool {
    edges.iter().any(|edge| edge.contains(needle))
}

fn report(outcome: &CompilerOutcome) -> &CompilerReport {
    outcome.report.as_ref().expect("an index was applied")
}

/// What the fixture's index says, however it was produced.
fn assert_fixture_verdicts(root: &Path, report: &CompilerReport) {
    assert_eq!(report.documents.applied, 2, "{report:#?}");
    assert!(report.complete);
    assert!(report.edges.confirmed >= 10, "{report:#?}");
    assert_eq!(report.edges.corrected, 0, "{report:#?}");
    assert_eq!(
        report.edges.refuted_std
            + report.edges.refuted_generated
            + report.edges.refuted_dependency
            + report.edges.refuted_local,
        0,
        "{report:#?}"
    );
    assert_eq!(report.definitions.generated, 1, "{report:#?}");
    assert_eq!(report.dispatch.confirmed, 2, "{report:#?}");
    assert_eq!(report.dispatch.removed, 0, "{report:#?}");

    let edges = edges(root);
    for expected in [
        // A call tree-sitter resolved, now compiler-verified.
        "uses_macro -calls-> helper [scip] {confirmed}",
        "call_sq -calls-> Sq::area [scip] {confirmed}",
        "Sq -implements-> Shape [scip] {confirmed}",
        // A reference tree-sitter left unresolved: a macro-generated fn.
        "uses_macro -calls-> generated_one [scip] {resolved}",
        // A trait method on a generic receiver: the trait's declaration,
        // which tree-sitter's trait dispatch now names itself.
        "generic -calls-> Shape::area [scip] {confirmed}",
        // A trait method on a bounded generic, which tree-sitter resolved.
        "bounded -calls-> Shape::area [scip]",
        // What tree-sitter never recorded.
        "call_sq -instantiates-> Circle [scip] {added}",
        "bounded -references-> Shape [scip] {added}",
        // Trait → impl dispatch, checked against the impls.
        "Shape::area -calls-> Sq::area [heuristic] {-} verified",
        "Shape::area -calls-> Circle::area [heuristic] {-} verified",
    ] {
        assert!(has(&edges, expected), "missing {expected}:\n{edges:#?}");
    }
    // The generated item is a node of its own, marked generated.
    let conn = db(root);
    let generated: (String, String, i64) = conn
        .query_row(
            "SELECT n.qualified_name, n.kind, n.start_line FROM nodes n
             JOIN compiler_symbols c ON c.node_id = n.id WHERE c.generated = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        generated,
        ("generated_one".to_string(), "function".to_string(), 28)
    );
    let symbols: i64 = conn
        .query_row("SELECT COUNT(*) FROM compiler_symbols", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(symbols >= 14, "{symbols}");
    // What is outside the project stays unresolved.
    let left = unresolved(root);
    for name in ["Box", "v.len"] {
        assert!(left.iter().any(|n| n == name), "{name}: {left:?}");
    }
    assert!(!left.iter().any(|n| n == "generated_one" || n == "t.area"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_checked_in_index_verifies_resolves_and_extends_the_graph() {
    let project = fixture_copy();
    let home = tempfile::tempdir().unwrap();
    let root = project.path();
    let cg = indexed(root).await;
    let before = edges(root);
    assert!(!has(&before, "generated_one"), "{before:#?}");

    install_checked_in_index(root);
    let outcome = cg
        .compiler_sync(&options(home.path()), false)
        .await
        .unwrap();
    assert!(!outcome.locked);
    assert_fixture_verdicts(root, report(&outcome));
    // Applying only the cached index never turns the layer on.
    assert!(!cg.compiler_layer_enabled());

    // Idempotent: a second pass keeps what the first wrote.
    let first = edges(root);
    let again = cg
        .compiler_sync(&options(home.path()), false)
        .await
        .unwrap();
    let again = report(&again);
    assert_eq!(again.edges.confirmed, 0, "{again:#?}");
    assert!(again.edges.prior_kept >= 10, "{again:#?}");
    assert_eq!(again.edges.prior_changed, 0, "{again:#?}");
    assert_eq!(edges(root), first);

    // Re-extracting lib.rs restores the references tree-sitter made into
    // it (`shape.area` in other.rs, re-resolved by tree-sitter) but not
    // what only the compiler knew (`bounded`'s reference to `Shape`).
    let lib = root.join("src/lib.rs");
    let text = std::fs::read_to_string(&lib).unwrap();
    std::fs::write(&lib, format!("{text}\n// edited\n")).unwrap();
    cg.sync(&IndexOptions::default()).await.unwrap();
    let left = unresolved(root);
    assert!(!left.iter().any(|n| n == "Shape"), "{left:?}");
    let after = edges(root);
    assert!(
        has(&after, "bounded -calls-> Shape::area [-]"),
        "{after:#?}"
    );
    assert!(!has(&after, "bounded -references-> Shape"), "{after:#?}");

    // The index no longer describes lib.rs: only other.rs is applied.
    let stale = cg
        .compiler_sync(&options(home.path()), false)
        .await
        .unwrap();
    let stale = report(&stale);
    assert_eq!(stale.documents.stale, 1, "{stale:#?}");
    assert_eq!(stale.documents.applied, 1, "{stale:#?}");
    assert_eq!(stale.definitions.generated, 0, "{stale:#?}");
    let generated: i64 = db(root)
        .query_row(
            "SELECT COUNT(*) FROM compiler_symbols WHERE generated = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(generated, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_rust_analyzer_leaves_the_graph_as_it_was() {
    let project = fixture_copy();
    let home = tempfile::tempdir().unwrap();
    let root = project.path();
    let cg = indexed(root).await;
    let before = edges(root);
    let mut options = options(home.path());
    options.rust_analyzer = root.join("no-such-rust-analyzer");
    let outcome = cg.compiler_sync(&options, true).await.unwrap();
    assert_eq!(outcome.run, RunStatus::Missing);
    assert!(outcome.report.is_none());
    assert_eq!(edges(root), before);
    assert!(!cg.compiler_layer_enabled());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_run_past_its_budget_is_killed_and_nothing_is_applied() {
    use std::os::unix::fs::PermissionsExt;

    let project = fixture_copy();
    let home = tempfile::tempdir().unwrap();
    let root = project.path();
    let cg = indexed(root).await;
    let before = edges(root);
    // A stand-in that answers `--version` and then never finishes.
    let tool = home.path().join("slow-rust-analyzer");
    std::fs::write(
        &tool,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo slow 0.0; exit 0; fi\nsleep 60\n",
    )
    .unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut options = options(home.path());
    options.rust_analyzer = tool;
    options.run_budget = Duration::from_millis(500);
    let started = std::time::Instant::now();
    let outcome = cg.compiler_sync(&options, true).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(outcome.run, RunStatus::TimedOut);
    assert!(outcome.report.is_none());
    assert_eq!(edges(root), before);
    assert!(!root.join(".codegraph/compiler/index.scip").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_analyzer_verifies_the_fixture_crate() {
    let program = rust_analyzer_program();
    if rust_analyzer_version(&program).is_none() {
        eprintln!("skipping: rust-analyzer is not on PATH");
        return;
    }
    let project = fixture_copy();
    let home = tempfile::tempdir().unwrap();
    let root = project.path();
    let cg = indexed(root).await;
    let options = options(home.path());
    let outcome = cg.compiler_sync(&options, true).await.unwrap();
    assert_eq!(outcome.run, RunStatus::Ran, "{outcome:#?}");
    assert!(!outcome.stale);
    assert_fixture_verdicts(root, report(&outcome));
    assert!(cg.compiler_layer_enabled());
    assert!(cg.compiler_layer_summary().is_some());
    // The same inputs reuse the cached index.
    let again = cg.compiler_sync(&options, true).await.unwrap();
    assert_eq!(again.run, RunStatus::Cached);
    // An edit makes it stale.
    let other = root.join("src/other.rs");
    let text = std::fs::read_to_string(&other).unwrap();
    std::fs::write(&other, format!("{text}\n// edited\n")).unwrap();
    let files = codegraph::compiler::rust_files(&open_queries(root)).unwrap();
    assert!(codegraph::compiler::index_is_stale(root, &files));
}

fn open_queries(root: &Path) -> codegraph::db::QueryBuilder {
    let connection =
        codegraph::db::DatabaseConnection::open(root.join(".codegraph/codegraph.db")).unwrap();
    codegraph::db::QueryBuilder::new(connection.get_db().unwrap())
}
