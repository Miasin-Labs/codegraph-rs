//! Cross-graph queries end to end (federation phase 3): the MCP graph
//! tools follow a project's external edges into dependency shards and
//! linked projects, read symbols that live there, and answer "who calls
//! this" across every project that uses it — bounded, degrading to "not
//! available" when a graph is missing, and creating nothing anywhere.

#[path = "federation/fixture.rs"]
mod fixture;

use std::fs;
use std::path::Path;
use std::rc::Rc;

use codegraph::deps::{DepKey, Ecosystem};
use codegraph::federation::FederationOptions;
use codegraph::mcp::tools::{ToolHandler, tools};
use codegraph::{CodeGraph, ExternalScope, OpenOptions};
use fixture::{Federation, snapshot, write};
use serde_json::{Value, json};

/// A machine with shards built and both projects resolved.
async fn machine() -> Federation {
    let machine = Federation::new();
    machine.prepare(true).await;
    machine.resolve(ExternalScope::AllUnresolved).await;
    machine
}

/// The MCP tools over the project at `dir`, answering cross-graph
/// questions from the machine's scratch home.
fn tools_at(machine: &Federation, dir: &Path) -> (Rc<CodeGraph>, ToolHandler) {
    let cg = Rc::new(CodeGraph::open(dir, &OpenOptions::default()).unwrap());
    let handler = ToolHandler::new(Some(Rc::clone(&cg)));
    handler.set_federation_options(FederationOptions::at(machine.federation_home()));
    (cg, handler)
}

fn call(handler: &ToolHandler, tool: &str, args: Value) -> codegraph::mcp::tools::ToolResult {
    let result = handler.execute(tool, &args);
    assert_ne!(result.is_error, Some(true), "{tool}: {}", result.text());
    result
}

/// Every object in `value` carries only the properties `schema` declares
/// (what clients enforce with `additionalProperties: false`).
fn conforms(value: &Value, schema: &Value, at: &str) {
    match value {
        Value::Object(map) => {
            let props = schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{at}: no properties in {schema}"));
            for key in map.keys() {
                let sub = props
                    .get(key)
                    .unwrap_or_else(|| panic!("{at}.{key} is not declared"));
                conforms(&map[key], sub, &format!("{at}.{key}"));
            }
            for required in schema["required"].as_array().into_iter().flatten() {
                assert!(
                    map.contains_key(required.as_str().unwrap()),
                    "{at} lacks required {required}"
                );
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                conforms(item, &schema["items"], &format!("{at}[{i}]"));
            }
        }
        _ => {}
    }
}

/// The success branch of `tool`'s output schema whose `kind` is `kind`.
fn success_schema(tool: &str, kind: &str) -> Value {
    let definition = tools().into_iter().find(|t| t.name == tool).unwrap();
    let schema = definition.output_schema.unwrap();
    schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|branch| branch["properties"]["kind"]["const"] == kind)
        .cloned()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn callees_follow_external_edges_into_their_graphs() {
    let machine = machine().await;
    let (cg, handler) = tools_at(&machine, &machine.app);
    let text = call(
        &handler,
        "codegraph_callees",
        json!({ "symbol": "run", "limit": 50 }),
    )
    .text()
    .to_string();
    for expected in [
        "### In other graphs",
        "- from_str (function) - jsonish@1.0.0 src/lib.rs:",
        "- Connection::open (method) - sqlish@0.3.0 src/lib.rs:",
        "- Statement::query_map (method) - sqlish@0.3.0 src/statement.rs:",
        "- trace_detail (function) - linkme src/lib.rs:2",
    ] {
        assert!(text.contains(expected), "missing {expected}:\n{text}");
    }
    assert!(!text.contains("not available"), "{text}");
    cg.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn node_reads_a_dependency_symbol_from_its_own_graph() {
    let machine = machine().await;
    let (cg, handler) = tools_at(&machine, &machine.app);
    let result = call(
        &handler,
        "codegraph_node",
        json!({ "symbol": "jsonish::from_str" }),
    );
    let payload = result.structured_content.clone().unwrap();
    let found = &payload["matches"][0];
    assert_eq!(found["graph"], "jsonish@1.0.0", "{payload:#}");
    assert_eq!(found["name"], "from_str");
    let source = machine.crate_dir("jsonish", "1.0.0").join("src/lib.rs");
    assert_eq!(found["file"], source.to_string_lossy().as_ref());
    assert!(
        found["code"]
            .as_str()
            .unwrap()
            .starts_with("pub fn from_str(text: &str) -> Value"),
        "{payload:#}"
    );
    assert_eq!(found["callers"][0]["name"], "run", "{payload:#}");
    conforms(&payload, &success_schema("codegraph_node", "node"), "node");

    // The same through a bare name and the graph it lives in.
    let hinted = call(
        &handler,
        "codegraph_node",
        json!({ "symbol": "from_str", "graph": "jsonish" }),
    );
    assert_eq!(
        hinted.structured_content.unwrap()["matches"][0]["graph"],
        "jsonish@1.0.0"
    );

    // A project symbol lists what it references in other graphs.
    let run = call(&handler, "codegraph_node", json!({ "symbol": "run" }));
    let payload = run.structured_content.clone().unwrap();
    let external = payload["matches"][0]["external"].as_array().unwrap();
    assert!(
        external
            .iter()
            .any(|row| row["name"] == "trace_detail" && row["graph"] == "linkme"),
        "{payload:#}"
    );
    conforms(&payload, &success_schema("codegraph_node", "node"), "node");
    assert!(run.text().contains("Into other graphs:"), "{}", run.text());
    cg.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn callers_of_linked_and_dependency_code_come_from_every_user() {
    let machine = machine().await;
    // Asked in the linked project: its dependents' call sites.
    let (linked, handler) = tools_at(&machine, &machine.linked);
    let text = call(
        &handler,
        "codegraph_callers",
        json!({ "symbol": "trace_detail" }),
    )
    .text()
    .to_string();
    assert!(text.contains("### Callers in other projects"), "{text}");
    assert!(text.contains("#### app (1)"), "{text}");
    assert!(text.contains("- run (function) - src/lib.rs:"), "{text}");
    linked.close();

    // Asked in the app about a dependency's symbol: every user of that
    // dependency version (the app itself here).
    let (app, handler) = tools_at(&machine, &machine.app);
    let text = call(
        &handler,
        "codegraph_callers",
        json!({ "symbol": "jsonish::from_str" }),
    )
    .text()
    .to_string();
    assert!(
        text.contains("## Callers of from_str (function) in jsonish@1.0.0"),
        "{text}"
    );
    assert!(text.contains("#### app (1)"), "{text}");
    app.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn impact_crosses_into_dependent_projects() {
    let machine = machine().await;
    let (linked, handler) = tools_at(&machine, &machine.linked);
    let text = call(
        &handler,
        "codegraph_impact",
        json!({ "symbol": "trace_detail" }),
    )
    .text()
    .to_string();
    assert!(text.contains("### Across projects"), "{text}");
    assert!(text.contains("#### app (1 calling in"), "{text}");
    assert!(text.contains("run:"), "{text}");
    linked.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_graph_degrades_to_not_available() {
    let machine = machine().await;
    fs::remove_dir_all(machine.deps_home().shard_dir(&DepKey::new(
        Ecosystem::Crates,
        "jsonish",
        "1.0.0",
    )))
    .unwrap();
    let (cg, handler) = tools_at(&machine, &machine.app);
    let text = call(
        &handler,
        "codegraph_callees",
        json!({ "symbol": "run", "limit": 50 }),
    )
    .text()
    .to_string();
    assert!(
        text.contains("- from_str (function) - jsonish@1.0.0 src/lib.rs:14 (target not available: shard not built)"),
        "{text}"
    );
    assert!(
        text.contains("- trace_detail (function) - linkme src/lib.rs:2"),
        "{text}"
    );
    cg.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuilt_graph_is_followed_by_name_when_ids_moved() {
    let machine = machine().await;
    // The linked project moves `trace_detail` down two lines and is
    // re-indexed; the app keeps the edge it resolved against the old id.
    write(
        &machine.linked.join("src/lib.rs"),
        "pub mod deep;\n\n\npub fn trace_detail() {}\n\
         pub struct Field;\n\
         impl Field {\n    pub fn text(label: &str) -> Field { Field }\n}\n",
    );
    machine.reindex(&machine.linked).await;
    let (cg, handler) = tools_at(&machine, &machine.app);
    let text = call(
        &handler,
        "codegraph_callees",
        json!({ "symbol": "run", "limit": 50 }),
    )
    .text()
    .to_string();
    assert!(
        text.contains("- trace_detail (function) - linkme src/lib.rs:4"),
        "moved target found again:\n{text}"
    );
    cg.close();

    // And the reverse: the linked project's new node finds the old edge.
    let (linked, handler) = tools_at(&machine, &machine.linked);
    let text = call(
        &handler,
        "codegraph_callers",
        json!({ "symbol": "trace_detail" }),
    )
    .text()
    .to_string();
    assert!(text.contains("#### app (1)"), "{text}");
    linked.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn cross_project_callers_are_bounded_and_say_what_they_skipped() {
    let machine = machine().await;
    let calls_it = "pub fn second() { linkme::trace_detail(); }\n";
    let resolved = machine.write_dependent("second", calls_it);
    let unresolved = machine.write_dependent("third", calls_it);
    for dir in [&resolved, &unresolved] {
        machine.index(dir).await;
        machine.register(dir);
    }
    machine
        .resolve_project(&resolved, ExternalScope::AllUnresolved, machine.options())
        .await;

    let (linked, handler) = tools_at(&machine, &machine.linked);
    let text = call(
        &handler,
        "codegraph_callers",
        json!({ "symbol": "trace_detail" }),
    )
    .text()
    .to_string();
    assert!(text.contains("#### app (1)"), "{text}");
    assert!(text.contains("#### second (1)"), "{text}");
    assert!(
        text.contains("third (external resolution has not run"),
        "{text}"
    );

    let mut options = FederationOptions::at(machine.federation_home());
    options.max_projects = 1;
    handler.set_federation_options(options);
    let text = call(
        &handler,
        "codegraph_callers",
        json!({ "symbol": "trace_detail" }),
    )
    .text()
    .to_string();
    assert!(text.contains("#### app (1)"), "{text}");
    assert!(!text.contains("#### second"), "{text}");
    assert!(text.contains("second (over the project cap)"), "{text}");
    linked.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn following_edges_creates_nothing_in_other_graphs() {
    let machine = machine().await;
    let store = machine.deps_home().root().join("crates");
    let linked_index = machine.linked.join(".codegraph");
    let before = (
        snapshot(&store),
        snapshot(&linked_index),
        snapshot(&machine.cargo_home),
    );
    let (cg, handler) = tools_at(&machine, &machine.app);
    call(&handler, "codegraph_callees", json!({ "symbol": "run" }));
    call(&handler, "codegraph_node", json!({ "symbol": "run" }));
    call(
        &handler,
        "codegraph_node",
        json!({ "symbol": "linkme::trace_detail" }),
    );
    call(
        &handler,
        "codegraph_callers",
        json!({ "symbol": "jsonish::from_str" }),
    );
    call(
        &handler,
        "codegraph_impact",
        json!({ "symbol": "sqlish::Connection::open" }),
    );
    call(&handler, "codegraph_explore", json!({ "query": "run" }));
    cg.close();
    let after = (
        snapshot(&store),
        snapshot(&linked_index),
        snapshot(&machine.cargo_home),
    );
    assert_eq!(after.0, before.0, "shards are only read");
    assert_eq!(after.1, before.1, "no -wal/-shm beside the linked index");
    assert_eq!(after.2, before.2, "dependency sources are only read");
}

#[tokio::test(flavor = "multi_thread")]
async fn explore_names_what_the_answer_calls_in_other_graphs() {
    let machine = machine().await;
    let (cg, handler) = tools_at(&machine, &machine.app);
    let result = call(&handler, "codegraph_explore", json!({ "query": "run" }));
    let payload = result.structured_content.clone().unwrap();
    let external = payload["external"].as_array().expect("external rows");
    assert!(
        external
            .iter()
            .any(|row| row["graph"] == "sqlish@0.3.0" && row["symbol"] == "Connection::prepare"),
        "{payload:#}"
    );
    assert!(
        external.iter().all(|row| row["from"] == "run"),
        "{payload:#}"
    );
    assert!(external.len() <= 8);
    conforms(
        &payload,
        &success_schema("codegraph_explore", "explore"),
        "explore",
    );
    cg.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn projects_view_lists_and_shows_links_dependencies_and_graphs() {
    let machine = machine().await;
    let (cg, handler) = tools_at(&machine, &machine.app);
    let list = call(&handler, "codegraph_projects", json!({}));
    let payload = list.structured_content.clone().unwrap();
    assert_eq!(payload["total"], 2, "{payload:#}");
    conforms(
        &payload,
        &success_schema("codegraph_projects", "projects"),
        "projects",
    );

    let show = call(&handler, "codegraph_projects", json!({ "project": "app" }));
    let payload = show.structured_content.clone().unwrap();
    assert_eq!(payload["links"][0]["project"], "linkme", "{payload:#}");
    assert_eq!(payload["dependencies"]["withShard"], 6, "{payload:#}");
    let graphs: Vec<&str> = payload["graphs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["graph"].as_str().unwrap())
        .collect();
    assert!(
        graphs.contains(&"sqlish@0.3.0") && graphs.contains(&"linkme"),
        "{graphs:?}"
    );
    conforms(
        &payload,
        &success_schema("codegraph_projects", "project"),
        "project",
    );

    let linked = call(
        &handler,
        "codegraph_projects",
        json!({ "project": "linkme" }),
    );
    let payload = linked.structured_content.clone().unwrap();
    assert_eq!(payload["usedBy"][0]["project"], "app", "{payload:#}");
    cg.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn search_covers_linked_projects_read_only() {
    let machine = machine().await;
    let linked_index = machine.linked.join(".codegraph");
    let before = snapshot(&linked_index);
    let (cg, handler) = tools_at(&machine, &machine.app);
    let result = call(
        &handler,
        "codegraph_search",
        json!({ "query": "trace_detail", "projects": "linked" }),
    );
    let payload = result.structured_content.clone().unwrap();
    let linked_root = fs::canonicalize(&machine.linked).unwrap();
    assert!(
        payload["results"].as_array().unwrap().iter().any(|hit| {
            hit["name"] == "trace_detail"
                && hit["project"] == linked_root.to_string_lossy().as_ref()
        }),
        "{payload:#}"
    );
    conforms(
        &payload,
        &success_schema("codegraph_search", "search"),
        "search",
    );

    // By name, and every registered project.
    for scope in [json!(["linkme"]), json!("all")] {
        let result = call(
            &handler,
            "codegraph_search",
            json!({ "query": "Field", "projects": scope }),
        );
        let hits = result.structured_content.unwrap()["results"].clone();
        assert!(
            hits.as_array()
                .unwrap()
                .iter()
                .any(|hit| hit["name"] == "Field"),
            "{hits:#}"
        );
    }
    cg.close();
    assert_eq!(
        snapshot(&linked_index),
        before,
        "searching creates nothing there"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spent_budget_degrades_instead_of_failing() {
    let machine = machine().await;
    let (cg, handler) = tools_at(&machine, &machine.app);
    let mut options = FederationOptions::at(machine.federation_home());
    options.deadline = std::time::Duration::ZERO;
    handler.set_federation_options(options);
    let text = call(
        &handler,
        "codegraph_callees",
        json!({ "symbol": "run", "limit": 50 }),
    )
    .text()
    .to_string();
    assert!(
        text.contains(
            "- trace_detail (function) - linkme src/lib.rs:2 (target not available: out of time)"
        ),
        "{text}"
    );
    // The project's own answer is unaffected.
    assert!(
        text.contains("- make_thing (function) - src/lib.rs:"),
        "{text}"
    );
    cg.close();
}
