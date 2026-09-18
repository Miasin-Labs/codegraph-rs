//! Cross-graph resolution end to end (federation phase 2): a project's
//! references resolve into dependency shards and a linked project's index
//! as `external_edges`, only where the target is certain; graphs that
//! appear, change or vanish are followed; other graphs are only read.

#[path = "federation/fixture.rs"]
mod fixture;

use std::cell::RefCell;
use std::fs;
use std::path::Path;

use codegraph::ExternalScope;
use codegraph::deps::trigger::queue_reresolution_with;
use codegraph::deps::{DepKey, Ecosystem, Registry};
use codegraph::resolution::external::discover;
use codegraph::sync::background::BackgroundSync;
use fixture::{Federation, snapshot};

fn has(edges: &[String], needle: &str) -> bool {
    edges.iter().any(|edge| edge.contains(needle))
}

#[tokio::test(flavor = "multi_thread")]
async fn resolves_paths_receivers_and_chains_into_shards_and_linked_projects() {
    let machine = Federation::new();
    machine.prepare(true).await;
    let report = machine.resolve(ExternalScope::AllUnresolved).await;
    assert!(report.complete && report.full, "{report:#?}");
    assert_eq!(report.project_graphs, 1, "{report:#?}");
    let edges = machine.external_edges();

    for expected in [
        // Paths naming a dependency crate, directly or through a `use`.
        "run jsonish::from_str -> dependency:jsonish-1.0.0::from_str (qualified-name, calls)",
        "run treelike::parse -> dependency:treelike-0.2.0::parse (qualified-name, calls)",
        "run Connection::open -> dependency:sqlish-0.3.0::Connection::open (qualified-name, calls)",
        // A re-exported crate (`pub use facade_builder::*`).
        "run facade::Command::new -> dependency:facade_builder-4.0.0::Command::new (re-export, calls)",
        "Parser -> dependency:facade_builder-4.0.0::Parser (re-export, implements)",
        // A receiver typed by its annotation.
        "run root.walk -> dependency:treelike-0.2.0::Node::walk (instance-method, calls)",
        // Chains typed through dependency return types.
        "run tree.root_node -> dependency:treelike-0.2.0::Tree::root_node (dependency-chain, calls)",
        "run conn.prepare -> dependency:sqlish-0.3.0::Connection::prepare (dependency-chain, calls)",
        "run stmt.query_map -> dependency:sqlish-0.3.0::Statement::query_map (dependency-chain, calls)",
        "run query_map -> dependency:sqlish-0.3.0::Statement::query_map (dependency-chain, calls)",
        "run value.as_text -> dependency:jsonish-1.0.0::Value::as_text (dependency-chain, calls)",
        // Through a `Deref` impl's `Target` and a type alias.
        "run query_map -> dependency:sqlish-0.3.0::Statement::query_map (dependency-chain, calls)",
        "run conn.statement -> dependency:sqlish-0.3.0::Connection::statement (dependency-chain, calls)",
        // The linked project's own index, by its crate paths.
        "run linkme::trace_detail -> project:linkme::trace_detail (qualified-name, calls)",
        "run linkme::Field::text -> project:linkme::Field::text (qualified-name, calls)",
        "run linkme::deep::helper -> project:linkme::helper (qualified-name, calls)",
    ] {
        assert!(has(&edges, expected), "missing {expected}:\n{edges:#?}");
    }
    // Never a guess: overloads, a private fn, a project type's receiver.
    for absent in ["Value::from", "Node::new", "unknown.walk"] {
        assert!(
            !edges
                .iter()
                .any(|edge| edge.contains(&format!(" {absent} ->"))
                    || edge.contains(&format!("::{absent} "))),
            "{absent} resolved:\n{edges:#?}"
        );
    }
    assert_eq!(machine.unresolved_named("jsonish::Value::from"), 1);
    assert_eq!(machine.unresolved_named("Node::new"), 1);
    assert_eq!(machine.unresolved_named("unknown.walk"), 1);
    assert_eq!(machine.unresolved_named("jsonish::from_str"), 0);
    assert!(
        report.opens.opened > 0 && report.opens.failed == 0,
        "{report:#?}"
    );

    // `query_map` three ways: on `Statement`, through `CachedStatement`'s
    // `Deref`, through the `Stmt` alias.
    let query_maps = edges
        .iter()
        .filter(|edge| {
            edge.starts_with("run query_map -> dependency:sqlish-0.3.0::Statement::query_map")
        })
        .count();
    assert_eq!(query_maps, 3, "{edges:#?}");
}

/// Two types of one name in one crate (`regex::Regex`, `regex::bytes::Regex`)
/// are told apart by the path the code names them by.
#[tokio::test(flavor = "multi_thread")]
async fn same_named_types_resolve_by_their_module_path() {
    let machine = Federation::new();
    machine.prepare(true).await;
    machine.resolve(ExternalScope::AllUnresolved).await;
    let targets = machine.external_edge_targets();
    for (reference, file) in [
        ("Regex::new", "src/string.rs"),
        ("text_re.is_match", "src/string.rs"),
        ("rexish::bytes::Regex::new", "src/bytes.rs"),
        ("bytes_re.is_match", "src/bytes.rs"),
    ] {
        assert!(
            targets.contains(&(reference.to_string(), file.to_string())),
            "{reference} should land in {file}: {targets:#?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_shards_resolve_nothing_until_they_are_built() {
    let machine = Federation::new();
    machine.prepare(false).await;

    // No shard yet: only the linked project resolves; the dependency
    // references stay unresolved (not an error).
    let first = machine.resolve(ExternalScope::AllUnresolved).await;
    assert!(first.complete, "{first:#?}");
    assert_eq!(first.dependency_graphs, 0);
    assert!(first.skipped.no_shard >= 5, "{first:#?}");
    let edges = machine.external_edges();
    assert!(has(&edges, "project:linkme::trace_detail"), "{edges:#?}");
    assert!(!has(&edges, "dependency:"), "{edges:#?}");
    assert_eq!(machine.unresolved_named("jsonish::from_str"), 1);

    // Nothing changed: a sync examines nothing.
    let idle = machine.resolve(ExternalScope::GraphChangesOnly).await;
    assert!(!idle.graphs_changed && idle.examined == 0, "{idle:#?}");

    // The shards appear: the next sync notices and resolves into them.
    machine.build_shards().await;
    let after = machine.resolve(ExternalScope::GraphChangesOnly).await;
    assert!(after.graphs_changed && after.full, "{after:#?}");
    assert!(after.into_dependencies >= 10, "{after:#?}");
    let edges = machine.external_edges();
    assert!(
        has(
            &edges,
            "run jsonish::from_str -> dependency:jsonish-1.0.0::from_str"
        ),
        "{edges:#?}"
    );
    assert!(
        has(
            &edges,
            "run query_map -> dependency:sqlish-0.3.0::Statement::query_map"
        ),
        "{edges:#?}"
    );
    assert_eq!(machine.unresolved_named("jsonish::from_str"), 0);

    // Idempotent: a second pass over the same graphs adds nothing.
    let count = edges.len();
    let again = machine.resolve(ExternalScope::AllUnresolved).await;
    assert_eq!(again.resolved(), 0, "{again:#?}");
    assert_eq!(machine.external_edges().len(), count);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_vanished_graph_turns_its_edges_back_into_references() {
    let machine = Federation::new();
    machine.prepare(true).await;
    machine.resolve(ExternalScope::AllUnresolved).await;
    assert!(has(&machine.external_edges(), "dependency:jsonish-1.0.0"));

    fs::remove_dir_all(machine.deps_home().shard_dir(&DepKey::new(
        Ecosystem::Crates,
        "jsonish",
        "1.0.0",
    )))
    .unwrap();
    let report = machine.resolve(ExternalScope::GraphChangesOnly).await;
    assert!(report.graphs_changed && report.restored >= 2, "{report:#?}");
    let edges = machine.external_edges();
    assert!(!has(&edges, "jsonish"), "{edges:#?}");
    assert!(
        has(&edges, "dependency:sqlish-0.3.0"),
        "other graphs keep theirs"
    );
    assert_eq!(machine.unresolved_named("jsonish::from_str"), 1);
    assert_eq!(machine.unresolved_named("value.as_text"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_other_graphs_creates_nothing() {
    let machine = Federation::new();
    machine.prepare(true).await;
    let store = machine.deps_home().root().join("crates");
    let linked_index = machine.linked.join(".codegraph");
    let (store_before, linked_before, sources_before) = (
        snapshot(&store),
        snapshot(&linked_index),
        snapshot(&machine.cargo_home),
    );

    let reach = discover(&machine.federation_home(), &machine.app);
    assert_eq!(reach.graphs().len(), 7, "6 shards + the linked project");
    machine.resolve(ExternalScope::AllUnresolved).await;

    assert_eq!(snapshot(&store), store_before, "shards are only read");
    assert_eq!(
        snapshot(&linked_index),
        linked_before,
        "no -wal/-shm beside it"
    );
    assert_eq!(snapshot(&machine.cargo_home), sources_before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_build_queues_the_projects_that_use_it() {
    let machine = Federation::new();
    machine.prepare(true).await;
    let registry = Registry::open(&machine.deps_home().registry_path()).unwrap();
    let spawned: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let spawn = |root: &Path| {
        spawned
            .borrow_mut()
            .push(root.to_string_lossy().into_owned());
        BackgroundSync::Started
    };
    let built = [DepKey::new(Ecosystem::Crates, "jsonish", "1.0.0")];
    let queued = queue_reresolution_with(&registry, &built, None, &spawn);
    let app = codegraph::deps::project::canonical_root(&machine.app);
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert_eq!(*spawned.borrow(), vec![app]);

    // A version nobody uses queues nothing.
    let unused = [DepKey::new(Ecosystem::Crates, "nobody", "9.9.9")];
    assert!(queue_reresolution_with(&registry, &unused, None, &spawn).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pass_cut_short_by_its_budget_is_continued_not_restarted() {
    let machine = Federation::new();
    machine.prepare(true).await;
    let mut options = machine.options();
    options.budget = std::time::Duration::ZERO;
    let cut = machine
        .resolve_with(ExternalScope::AllUnresolved, options)
        .await;
    assert!(!cut.complete && cut.full, "{cut:#?}");
    assert!(machine.external_edges().is_empty());

    // The next sync over the same graphs picks up where it stopped (its
    // graphs were never recorded as done, so it is a full pass).
    let next = machine.resolve(ExternalScope::GraphChangesOnly).await;
    assert!(
        next.complete && next.full && next.restored == 0,
        "{next:#?}"
    );
    assert!(has(&machine.external_edges(), "dependency:jsonish-1.0.0"));
    let idle = machine.resolve(ExternalScope::GraphChangesOnly).await;
    assert!(!idle.graphs_changed && idle.examined == 0, "{idle:#?}");
}
