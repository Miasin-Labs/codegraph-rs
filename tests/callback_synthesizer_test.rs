//! Integration tests for the callback synthesizer.
//!
//! Ports `__tests__/closure-collection-synthesizer.test.ts` (the end-to-end
//! closure-collection case) plus port-validation coverage for the other
//! synthesis channels driven through `synthesize_callback_edges`.
//!
//! NOTE: `__tests__/pr19-improvements.test.ts` was checked for synthesizer
//! cases — none of its suites target `synthesizeCallbackEdges` (the other
//! channel tests live in framework-owned suites: fabric-view, rn-event-channel,
//! gin-middleware-chain, frameworks-integration).
//!
//! The TS test drives the full `CodeGraph.init` + `indexAll` pipeline; that
//! facade isn't ported yet, so the fixture inserts the nodes/edges extraction
//! would produce (same shape as `tests/db_test.rs` does) over REAL files in a
//! tempdir and REAL SQLite — no mocks.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use codegraph::db::{DatabaseConnection, QueryBuilder};
use codegraph::resolution::callback_synthesizer::synthesize_callback_edges;
use codegraph::resolution::types::{ImportMapping, ResolutionContext};
use codegraph::types::{Edge, EdgeKind, Language, Node, NodeKind};
use tempfile::tempdir;

/// Test ResolutionContext backed by the real QueryBuilder + real files.
struct TestCtx {
    root: PathBuf,
    root_str: String,
    files: Vec<String>,
    q: QueryBuilder,
}

impl ResolutionContext for TestCtx {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.q.get_nodes_by_file(file_path).unwrap_or_default()
    }
    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.q.get_nodes_by_name(name).unwrap_or_default()
    }
    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.q
            .get_nodes_by_qualified_name_exact(qualified_name)
            .unwrap_or_default()
    }
    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.q.get_nodes_by_kind(kind).unwrap_or_default()
    }
    fn file_exists(&self, file_path: &str) -> bool {
        self.root.join(file_path).exists()
    }
    fn read_file(&self, file_path: &str) -> Option<String> {
        fs::read_to_string(self.root.join(file_path)).ok()
    }
    fn get_project_root(&self) -> &str {
        &self.root_str
    }
    fn get_all_files(&self) -> Vec<String> {
        self.files.clone()
    }
    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.q
            .get_nodes_by_lower_name(lower_name)
            .unwrap_or_default()
    }
    fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
        Vec::new()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    _conn: DatabaseConnection,
    ctx: TestCtx,
}

fn setup(files: &[(&str, &str)]) -> Fixture {
    let dir = tempdir().expect("tempdir");
    for (name, content) in files {
        fs::write(dir.path().join(name), content).expect("write fixture file");
    }
    let conn =
        DatabaseConnection::initialize(dir.path().join("codegraph.db")).expect("initialize db");
    let q = QueryBuilder::new(conn.get_db().expect("get_db"));
    let ctx = TestCtx {
        root: dir.path().to_path_buf(),
        root_str: dir.path().to_string_lossy().to_string(),
        files: files.iter().map(|(n, _)| n.to_string()).collect(),
        q,
    };
    Fixture {
        _dir: dir,
        _conn: conn,
        ctx,
    }
}

fn node(
    id: &str,
    kind: NodeKind,
    name: &str,
    file: &str,
    lang: Language,
    start: u32,
    end: u32,
) -> Node {
    Node::new(
        id,
        kind,
        name,
        format!("{}::{}", file, name),
        file,
        lang,
        start,
        end,
    )
}

/// All synthesized edges of one channel, joined with source/target names —
/// mirrors the TS test's `json_extract` SQL.
struct SynthRow {
    source_name: String,
    source_kind: String,
    target_name: String,
    field: Option<String>,
    registered_at: Option<String>,
}

fn synth_rows(fx: &Fixture, synthesized_by: &str) -> Vec<SynthRow> {
    let db = fx._conn.get_db().expect("get_db");
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT s.name source_name, s.kind source_kind, t.name target_name,
                    json_extract(e.metadata,'$.field') field,
                    json_extract(e.metadata,'$.registeredAt') registeredAt
             FROM edges e
             JOIN nodes s ON s.id = e.source
             JOIN nodes t ON t.id = e.target
             WHERE json_extract(e.metadata,'$.synthesizedBy') = ?",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([synthesized_by], |row| {
            Ok(SynthRow {
                source_name: row.get(0)?,
                source_kind: row.get(1)?,
                target_name: row.get(2)?,
                field: row.get(3)?,
                registered_at: row.get(4)?,
            })
        })
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

// =============================================================================
// __tests__/closure-collection-synthesizer.test.ts
// =============================================================================

/// End-to-end synthesizer test for closure-collection dynamic dispatch.
///
/// A method appends a closure to a collection property; another method iterates
/// that property *invoking each element* (`coll.forEach { $0() }`) — a dynamic
/// dispatch tree-sitter can't resolve, so a flow into the dispatcher dead-ends
/// before the registered closures. This is Alamofire's request-validation shape.
///
/// Verify the synthesizer (1) links the dispatcher → each same-named registrar
/// across files/classes, (2) handles both the Swift `prop.write { $0.append }`
/// and the direct `prop.append(...)` registrar forms, (3) surfaces the wiring
/// site, and (4) does NOT fire on a `.forEach` that doesn't invoke its element
/// (the closure-invoke is the precision gate — a plain collection is skipped).
#[test]
fn links_dispatcher_to_registrars_across_files_both_append_forms_and_skips_non_invoked_collections()
{
    let request_swift = "class Request {\n    var validators: [() -> Void] = []\n    var handlers: [() -> Void] = []\n    var names: [String] = []\n\n    func didCompleteTask() {\n        let validators = validators\n        validators.forEach { $0() }\n    }\n\n    func runHandlers() {\n        handlers.forEach { $0() }\n    }\n\n    func printNames() {\n        names.forEach { print($0) }\n    }\n}\n";
    let data_request_swift = "class DataRequest: Request {\n    func validate(_ validation: @escaping () -> Void) -> Self {\n        let validator: () -> Void = { validation() }\n        validators.write { $0.append(validator) }\n        return self\n    }\n\n    func onEvent(_ handler: @escaping () -> Void) {\n        handlers.append(handler)\n    }\n\n    func addName(_ n: String) {\n        names.append(n)\n    }\n}\n";

    let fx = setup(&[
        ("Request.swift", request_swift),
        ("DataRequest.swift", data_request_swift),
    ]);

    // Nodes extraction would produce (line ranges match the files above).
    let sw = Language::Swift;
    fx.ctx
        .q
        .insert_nodes(&[
            node(
                "c-request",
                NodeKind::Class,
                "Request",
                "Request.swift",
                sw,
                1,
                18,
            ),
            node(
                "m-didCompleteTask",
                NodeKind::Method,
                "didCompleteTask",
                "Request.swift",
                sw,
                6,
                9,
            ),
            node(
                "m-runHandlers",
                NodeKind::Method,
                "runHandlers",
                "Request.swift",
                sw,
                11,
                13,
            ),
            node(
                "m-printNames",
                NodeKind::Method,
                "printNames",
                "Request.swift",
                sw,
                15,
                17,
            ),
            node(
                "c-datarequest",
                NodeKind::Class,
                "DataRequest",
                "DataRequest.swift",
                sw,
                1,
                15,
            ),
            node(
                "m-validate",
                NodeKind::Method,
                "validate",
                "DataRequest.swift",
                sw,
                2,
                6,
            ),
            node(
                "m-onEvent",
                NodeKind::Method,
                "onEvent",
                "DataRequest.swift",
                sw,
                8,
                10,
            ),
            node(
                "m-addName",
                NodeKind::Method,
                "addName",
                "DataRequest.swift",
                sw,
                12,
                14,
            ),
        ])
        .expect("insert nodes");
    fx.ctx
        .q
        .insert_edges(&[
            Edge::new("c-request", "m-didCompleteTask", EdgeKind::Contains),
            Edge::new("c-request", "m-runHandlers", EdgeKind::Contains),
            Edge::new("c-request", "m-printNames", EdgeKind::Contains),
            Edge::new("c-datarequest", "m-validate", EdgeKind::Contains),
            Edge::new("c-datarequest", "m-onEvent", EdgeKind::Contains),
            Edge::new("c-datarequest", "m-addName", EdgeKind::Contains),
            Edge::new("c-datarequest", "c-request", EdgeKind::Extends),
        ])
        .expect("insert edges");

    let count = synthesize_callback_edges(&fx.ctx.q, &fx.ctx).expect("synthesize");
    assert!(count > 0);

    let rows = synth_rows(&fx, "closure-collection");
    assert!(!rows.is_empty());

    // Every edge originates from a dispatcher method and is a real `calls` hop.
    assert!(rows.iter().all(|r| r.source_kind == "method"));

    // The validators flow: didCompleteTask → validate, captured via the Swift
    // Protected `prop.write { $0.append }` form, wiring site surfaced.
    let validators_edge = rows
        .iter()
        .find(|r| r.field.as_deref() == Some("validators") && r.target_name == "validate")
        .expect("validators edge exists");
    assert_eq!(validators_edge.source_name, "didCompleteTask");
    let registered_at = validators_edge.registered_at.as_deref().unwrap();
    let re = regex_lite_match(registered_at);
    assert!(
        re,
        "registeredAt should match DataRequest.swift:<line>, got {registered_at}"
    );

    // The handlers flow: runHandlers → onEvent, via the direct `prop.append`
    // form — proves both registrar shapes are covered.
    let handlers_edge = rows
        .iter()
        .find(|r| r.field.as_deref() == Some("handlers") && r.target_name == "onEvent")
        .expect("handlers edge exists");
    assert_eq!(handlers_edge.source_name, "runHandlers");

    // Precision gate: `names.forEach { print($0) }` does NOT invoke its element,
    // so `names` is not a closure collection — no edge, and addName is never a target.
    assert!(!rows.iter().any(|r| r.field.as_deref() == Some("names")));
    assert!(!rows.iter().any(|r| r.target_name == "addName"));
}

/// TS assertion `expect(registeredAt).toMatch(/DataRequest\.swift:\d+/)`.
fn regex_lite_match(s: &str) -> bool {
    match s.strip_prefix("DataRequest.swift:") {
        Some(rest) => !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

// =============================================================================
// Port-validation coverage for the other channels
// =============================================================================

/// Phase 1 field-backed observer channel: registrar + dispatcher share a field
/// in the same file; the registration call site names the callback; synthesize
/// dispatcher → callback with via/field/registeredAt metadata.
#[test]
fn field_channel_links_dispatcher_to_named_callback() {
    let scene_js = "class Scene {\n  onUpdate(cb) {\n    this.callbacks.add(cb);\n  }\n  triggerUpdate() {\n    for (const cb of this.callbacks) cb();\n  }\n}\n";
    let app_js = "class App {\n  attach() {\n    scene.onUpdate(this.triggerRender);\n  }\n  triggerRender() {\n    return 1;\n  }\n}\n";

    let fx = setup(&[("scene.js", scene_js), ("app.js", app_js)]);
    let js = Language::Javascript;
    fx.ctx
        .q
        .insert_nodes(&[
            node(
                "m-onUpdate",
                NodeKind::Method,
                "onUpdate",
                "scene.js",
                js,
                2,
                4,
            ),
            node(
                "m-triggerUpdate",
                NodeKind::Method,
                "triggerUpdate",
                "scene.js",
                js,
                5,
                7,
            ),
            node("m-attach", NodeKind::Method, "attach", "app.js", js, 2, 4),
            node(
                "m-triggerRender",
                NodeKind::Method,
                "triggerRender",
                "app.js",
                js,
                5,
                7,
            ),
        ])
        .expect("insert nodes");
    // The static registration call edge (attach → onUpdate at the wiring line).
    let mut call = Edge::new("m-attach", "m-onUpdate", EdgeKind::Calls);
    call.line = Some(3);
    fx.ctx.q.insert_edges(&[call]).expect("insert call edge");

    synthesize_callback_edges(&fx.ctx.q, &fx.ctx).expect("synthesize");

    let db = fx._conn.get_db().unwrap();
    let (via, field, registered_at): (String, String, String) = db
        .conn()
        .query_row(
            "SELECT json_extract(e.metadata,'$.via'),
                    json_extract(e.metadata,'$.field'),
                    json_extract(e.metadata,'$.registeredAt')
             FROM edges e
             WHERE e.source = 'm-triggerUpdate' AND e.target = 'm-triggerRender'
               AND e.provenance = 'heuristic'
               AND json_extract(e.metadata,'$.synthesizedBy') = 'callback'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("synthesized callback edge exists");
    assert_eq!(via, "onUpdate");
    assert_eq!(field, "callbacks");
    assert_eq!(registered_at, "app.js:3");
}

/// Phase 2 EventEmitter channel: `.on('evt', namedHandler)` ↔ `.emit('evt')`.
#[test]
fn event_emitter_links_emit_site_to_named_handler() {
    let emitter_js = "class Bus {\n  send() {\n    this.bus.emit('mount', 1);\n  }\n}\n";
    let listener_js = "function setup(bus) {\n  bus.on('mount', function onmount() {\n    return 2;\n  });\n}\nfunction onmount() {\n  return 3;\n}\n";

    let fx = setup(&[("emitter.js", emitter_js), ("listener.js", listener_js)]);
    let js = Language::Javascript;
    fx.ctx
        .q
        .insert_nodes(&[
            node("m-send", NodeKind::Method, "send", "emitter.js", js, 2, 4),
            node(
                "f-setup",
                NodeKind::Function,
                "setup",
                "listener.js",
                js,
                1,
                5,
            ),
            node(
                "f-onmount",
                NodeKind::Function,
                "onmount",
                "listener.js",
                js,
                6,
                8,
            ),
        ])
        .expect("insert nodes");

    synthesize_callback_edges(&fx.ctx.q, &fx.ctx).expect("synthesize");

    let db = fx._conn.get_db().unwrap();
    let (event, registered_at): (String, String) = db
        .conn()
        .query_row(
            "SELECT json_extract(e.metadata,'$.event'),
                    json_extract(e.metadata,'$.registeredAt')
             FROM edges e
             WHERE e.source = 'm-send' AND e.target = 'f-onmount'
               AND json_extract(e.metadata,'$.synthesizedBy') = 'event-emitter'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("synthesized event-emitter edge exists");
    assert_eq!(event, "mount");
    assert_eq!(registered_at, "listener.js:2");
}

#[test]
fn gin_registration_prefilter_accepts_whitespace_before_paren() {
    let server_go = "package main\nfunc Next(c *Context) { c.handlers[c.index](c) }\nfunc setup(router *Engine) { router.GET (\"/x\", authMiddleware, handleX) }\nfunc authMiddleware(c *Context) {}\nfunc handleX(c *Context) {}\n";
    let fx = setup(&[("server.go", server_go)]);
    fx.ctx
        .q
        .insert_nodes(&[
            node(
                "m-next",
                NodeKind::Method,
                "Next",
                "server.go",
                Language::Go,
                2,
                2,
            ),
            node(
                "f-auth",
                NodeKind::Function,
                "authMiddleware",
                "server.go",
                Language::Go,
                4,
                4,
            ),
            node(
                "f-handle",
                NodeKind::Function,
                "handleX",
                "server.go",
                Language::Go,
                5,
                5,
            ),
        ])
        .expect("insert nodes");

    synthesize_callback_edges(&fx.ctx.q, &fx.ctx).expect("synthesize");

    let rows = synth_rows(&fx, "gin-middleware-chain");
    let targets: HashSet<&str> = rows.iter().map(|row| row.target_name.as_str()).collect();
    assert!(targets.contains("authMiddleware"), "got {targets:?}");
    assert!(targets.contains("handleX"), "got {targets:?}");
}

#[test]
fn rn_event_channel_ignores_unrelated_jvm_emitters() {
    let native_java = "class Bus { void send() { eventBus.emit(\"shared\", body); } }\n";
    let listener_js = "function setup(bus) { bus.addListener(\"shared\", handleShared); }\nfunction handleShared() { return 1; }\n";
    let fx = setup(&[("Bus.java", native_java), ("listener.js", listener_js)]);
    fx.ctx
        .q
        .insert_nodes(&[
            node(
                "m-send",
                NodeKind::Method,
                "send",
                "Bus.java",
                Language::Java,
                1,
                1,
            ),
            node(
                "f-handle",
                NodeKind::Function,
                "handleShared",
                "listener.js",
                Language::Javascript,
                2,
                2,
            ),
        ])
        .expect("insert nodes");

    synthesize_callback_edges(&fx.ctx.q, &fx.ctx).expect("synthesize");

    assert!(synth_rows(&fx, "rn-event-channel").is_empty());
}

/// Phase 4 React re-render: a sibling method calling `this.setState(` is
/// bridged to the class's `render` method.
#[test]
fn react_render_bridges_set_state_to_render() {
    let app_tsx = "class App {\n  handleClick() {\n    this.setState({ a: 1 });\n  }\n  render() {\n    return null;\n  }\n  helper() {\n    return 2;\n  }\n}\n";

    let fx = setup(&[("App.tsx", app_tsx)]);
    let ts = Language::Tsx;
    fx.ctx
        .q
        .insert_nodes(&[
            node("c-app", NodeKind::Class, "App", "App.tsx", ts, 1, 11),
            node(
                "m-handleClick",
                NodeKind::Method,
                "handleClick",
                "App.tsx",
                ts,
                2,
                4,
            ),
            node("m-render", NodeKind::Method, "render", "App.tsx", ts, 5, 7),
            node("m-helper", NodeKind::Method, "helper", "App.tsx", ts, 8, 10),
        ])
        .expect("insert nodes");
    fx.ctx
        .q
        .insert_edges(&[
            Edge::new("c-app", "m-handleClick", EdgeKind::Contains),
            Edge::new("c-app", "m-render", EdgeKind::Contains),
            Edge::new("c-app", "m-helper", EdgeKind::Contains),
        ])
        .expect("insert edges");

    synthesize_callback_edges(&fx.ctx.q, &fx.ctx).expect("synthesize");

    let edges = fx
        .ctx
        .q
        .get_outgoing_edges("m-handleClick", Some(&[EdgeKind::Calls]), Some("heuristic"))
        .expect("edges");
    assert_eq!(edges.len(), 1);
    let e = &edges[0];
    assert_eq!(e.target, "m-render");
    let meta = e.metadata.as_ref().expect("metadata");
    assert_eq!(meta.get("synthesizedBy").unwrap(), "react-render");
    assert_eq!(meta.get("via").unwrap(), "setState");
    assert_eq!(meta.get("registeredAt").unwrap(), "App.tsx:5");

    // helper() doesn't call setState → no bridge.
    let helper_edges = fx
        .ctx
        .q
        .get_outgoing_edges("m-helper", Some(&[EdgeKind::Calls]), Some("heuristic"))
        .expect("edges");
    assert!(helper_edges.is_empty());
}

/// Phase 5 JSX child rendering: a render method returning `<Child/>` links to
/// the Child component node; lowercase tags and unresolved names are skipped.
#[test]
fn jsx_child_links_parent_to_capitalized_resolved_children() {
    let app_tsx = "class App {\n  render() {\n    return <div><StaticCanvas a={1}/><unknownTag/><Missing/></div>;\n  }\n}\n";
    let canvas_tsx = "function StaticCanvas() {\n  return <canvas/>;\n}\n";

    let fx = setup(&[("App.tsx", app_tsx), ("StaticCanvas.tsx", canvas_tsx)]);
    let ts = Language::Tsx;
    fx.ctx
        .q
        .insert_nodes(&[
            node("m-render", NodeKind::Method, "render", "App.tsx", ts, 2, 4),
            node(
                "f-canvas",
                NodeKind::Function,
                "StaticCanvas",
                "StaticCanvas.tsx",
                ts,
                1,
                3,
            ),
        ])
        .expect("insert nodes");

    synthesize_callback_edges(&fx.ctx.q, &fx.ctx).expect("synthesize");

    let edges = fx
        .ctx
        .q
        .get_outgoing_edges("m-render", Some(&[EdgeKind::Calls]), Some("heuristic"))
        .expect("edges");
    let targets: HashSet<&str> = edges.iter().map(|e| e.target.as_str()).collect();
    assert!(targets.contains("f-canvas"));
    // `Missing` resolves to nothing, `unknownTag` is lowercase — only one edge.
    assert_eq!(edges.len(), 1);
    let meta = edges[0].metadata.as_ref().unwrap();
    assert_eq!(meta.get("synthesizedBy").unwrap(), "jsx-render");
    assert_eq!(meta.get("via").unwrap(), "StaticCanvas");
}

/// Merged-pass dedup: the same source>target pair produced twice is inserted
/// once, and the returned count reflects the deduped set. The Vue channel is
/// the one place a single channel can emit the same pair twice (its per-channel
/// dedup key includes `synthesizedBy`): a kebab-case child tag AND an event
/// binding resolving to the same function node. The merged pass keeps the
/// first (jsx-render — channel emit order), exactly like the TS
/// `${e.source}>${e.target}` seen-set.
#[test]
fn synthesize_returns_deduped_count_and_inserts_heuristic_edges() {
    let comp_vue = "<template>\n  <el-button @click=\"ElButton\">go</el-button>\n</template>\n<script>\nexport default {}\n</script>\n";
    let el_button_ts = "export function ElButton() {\n  return 1;\n}\n";

    let fx = setup(&[("Comp.vue", comp_vue), ("ElButton.ts", el_button_ts)]);
    fx.ctx
        .q
        .insert_nodes(&[
            node(
                "comp-1",
                NodeKind::Component,
                "Comp",
                "Comp.vue",
                Language::Vue,
                1,
                6,
            ),
            node(
                "f-elbutton",
                NodeKind::Function,
                "ElButton",
                "ElButton.ts",
                Language::Typescript,
                1,
                3,
            ),
        ])
        .expect("insert nodes");

    let count = synthesize_callback_edges(&fx.ctx.q, &fx.ctx).expect("synthesize");
    assert_eq!(count, 1); // kebab child + @click handler → same pair, deduped

    let db = fx._conn.get_db().unwrap();
    let (edge_count, synthesized_by): (i64, String) = db
        .conn()
        .query_row(
            "SELECT count(*), json_extract(min(metadata),'$.synthesizedBy')
             FROM edges WHERE provenance = 'heuristic'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(edge_count, 1);
    // First-emitted wins: the kebab-child jsx-render edge, not vue-handler.
    assert_eq!(synthesized_by, "jsx-render");
}
