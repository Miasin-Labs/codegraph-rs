//! Integration tests for the "systems ecosystems" framework resolvers
//! (Go/Gin, Rust/Axum/Actix, Cargo workspaces, Swift/Vapor, Swift↔ObjC bridge).
//!
//! Ports the matching cases from:
//! - `__tests__/frameworks.test.ts` — `goResolver.extract`,
//!   `rustResolver.extract`, `rustResolver.resolve cargo workspace crates`,
//!   `vaporResolver.extract`, and the go/rust/vapor cases of
//!   "framework extractors ignore commented-out routes".
//! - `__tests__/swift-objc-bridge-resolver.test.ts` — the full suite
//!   (detect / claimsReference / both resolve directions). The TS file's
//!   in-memory `ResolutionContext` fixture is reproduced as a Rust struct
//!   implementing the trait (it is a test fixture, not a DB mock).
//!
//! DEFERRED (depend on modules still in flight — see
//! `notes/frameworks-systems.md`):
//! - `__tests__/gin-middleware-chain.test.ts` — end-to-end through
//!   `CodeGraph.init`/`indexAll` and the `gin-middleware-chain`
//!   synthesizer in `callback-synthesizer.ts` (resolution core + public
//!   API wiring, both owned by other agents).
//! - `__tests__/frameworks-integration.test.ts` "Go gRPC stub→impl
//!   synthesis" — same end-to-end dependency (and the synthesis lives in
//!   the callback synthesizer, not the go framework resolver).

use std::collections::{HashMap, HashSet};

use codegraph::resolution::frameworks::go::GoResolver;
use codegraph::resolution::frameworks::rust::RustResolver;
use codegraph::resolution::frameworks::swift::VaporResolver;
use codegraph::resolution::frameworks::swift_objc::SwiftObjcBridgeResolver;
use codegraph::resolution::types::{
    FrameworkResolver,
    ImportMapping,
    ResolutionContext,
    ResolvedBy,
    UnresolvedRef,
};
use codegraph::types::{EdgeKind, Language, Node, NodeKind};

// =============================================================================
// Fixture ResolutionContext (mirrors the TS test mocks)
// =============================================================================

#[derive(Default)]
struct TestContext {
    nodes: Vec<Node>,
    /// Readable file contents (TS `readFile` mock).
    files: HashMap<String, String>,
    /// Paths reported by `file_exists` / `get_all_files`.
    existing: HashSet<String>,
    /// `list_directories` fixture.
    dirs: HashMap<String, Vec<String>>,
    root: String,
}

impl TestContext {
    fn new() -> Self {
        TestContext {
            root: "/test".to_string(),
            ..Default::default()
        }
    }

    /// TS swift-objc fixture: `makeContext(nodes, fileContents)` — the
    /// existing-file set is derived from the nodes' file paths.
    fn from_nodes(nodes: Vec<Node>, file_contents: &[(&str, &str)]) -> Self {
        let mut ctx = TestContext::new();
        ctx.existing = nodes.iter().map(|n| n.file_path.clone()).collect();
        ctx.files = file_contents
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ctx.nodes = nodes;
        ctx
    }
}

impl ResolutionContext for TestContext {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.file_path == file_path)
            .cloned()
            .collect()
    }
    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.name == name)
            .cloned()
            .collect()
    }
    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.qualified_name == qualified_name)
            .cloned()
            .collect()
    }
    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.kind == kind)
            .cloned()
            .collect()
    }
    fn file_exists(&self, file_path: &str) -> bool {
        self.existing.contains(file_path) || self.files.contains_key(file_path)
    }
    fn read_file(&self, file_path: &str) -> Option<String> {
        self.files.get(file_path).cloned()
    }
    fn get_project_root(&self) -> &str {
        &self.root
    }
    fn get_all_files(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .existing
            .iter()
            .chain(self.files.keys())
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        all.sort();
        all
    }
    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.name.to_lowercase() == lower_name)
            .cloned()
            .collect()
    }
    fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
        Vec::new()
    }
    fn list_directories(&self, relative_path: &str) -> Vec<String> {
        self.dirs.get(relative_path).cloned().unwrap_or_default()
    }
}

fn make_module_node(id: &str, name: &str, file_path: &str) -> Node {
    Node::new(
        id,
        NodeKind::Module,
        name,
        format!("{file_path}::{name}"),
        file_path,
        Language::Rust,
        1,
        1,
    )
}

/// TS `method(name, language, filePath, startLine = 10)` helper.
fn method(name: &str, language: Language, file_path: &str, start_line: u32) -> Node {
    Node::new(
        format!("{}:{file_path}:{name}:{start_line}", language.as_str()),
        NodeKind::Method,
        name,
        format!("{file_path}::{name}"),
        file_path,
        language,
        start_line,
        start_line + 5,
    )
}

/// TS `ref(name, language, filePath)` helper.
fn make_ref(name: &str, language: Language, file_path: &str) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: format!("caller:{file_path}"),
        reference_name: name.to_string(),
        reference_kind: EdgeKind::Calls,
        line: 1,
        column: 0,
        file_path: file_path.to_string(),
        language,
        candidates: None,
        metadata: None,
    }
}

// =============================================================================
// goResolver.extract (frameworks.test.ts)
// =============================================================================

#[test]
fn go_extracts_route_from_r_get() {
    let src = "r.GET(\"/users\", listUsers)\n";
    let result = GoResolver.extract("main.go", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "listUsers");
}

#[test]
fn go_extracts_route_from_router_handle_func() {
    let src = "router.HandleFunc(\"/items\", createItem)\n";
    let result = GoResolver.extract("main.go", src).unwrap();
    assert_eq!(result.references[0].reference_name, "createItem");
}

#[test]
fn go_extracts_gorilla_mux_handle_func_on_subrouter_ignoring_chained_methods() {
    // `s` is a PathPrefix().Subrouter() var — any receiver is matched; the
    // trailing .Methods("GET") doesn't break the handler capture.
    let src = "s.HandleFunc(\"/users/{id}\", listUsers).Methods(\"GET\")\n";
    let result = GoResolver.extract("routes.go", src).unwrap();
    assert_eq!(result.references[0].reference_name, "listUsers");
}

#[test]
fn go_skips_commented_out_router_method_calls() {
    let src = "\n// r.GET(\"/fake\", fakeHandler)\n/* r.POST(\"/also-fake\", anotherHandler) */\nr.GET(\"/real\", listUsers)\n";
    let result = GoResolver.extract("main.go", src).unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["GET /real"]);
    let refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(refs, vec!["listUsers"]);
}

// =============================================================================
// rustResolver.extract (frameworks.test.ts)
// =============================================================================

#[test]
fn rust_extracts_route_from_axum_route_with_get() {
    let src = "let app = Router::new().route(\"/users\", get(list_users));\n";
    let result = RustResolver::new().extract("main.rs", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "list_users");
}

#[test]
fn rust_extracts_every_method_from_chained_axum_route() {
    let src = "let app = Router::new().route(\"/user\", get(get_current_user).put(update_user));\n";
    let result = RustResolver::new().extract("main.rs", src).unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["GET /user", "PUT /user"]);
    let refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(refs, vec!["get_current_user", "update_user"]);
}

#[test]
fn rust_extracts_multiline_axum_route_with_namespaced_handler() {
    let src = "\nlet app = Router::new()\n    .route(\n        \"/articles/feed\",\n        get(listing::feed_articles),\n    );\n";
    let result = RustResolver::new().extract("main.rs", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /articles/feed");
    assert_eq!(result.references[0].reference_name, "feed_articles");
}

#[test]
fn rust_extracts_actix_web_resource_route_method_to() {
    let src = "App::new().service(web::resource(\"/user/{id}\").route(web::get().to(get_user)))\n";
    let result = RustResolver::new().extract("main.rs", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /user/{id}");
    assert_eq!(result.references[0].reference_name, "get_user");
}

#[test]
fn rust_extracts_actix_web_resource_direct_to_all_methods() {
    let src = "App::new().service(web::resource(\"/\").to(index))\n";
    let result = RustResolver::new().extract("main.rs", src).unwrap();
    assert_eq!(result.nodes[0].name, "ANY /");
    assert_eq!(result.references[0].reference_name, "index");
}

#[test]
fn rust_extracts_actix_app_level_route() {
    let src = "App::new().route(\"/health\", web::get().to(health_check))\n";
    let result = RustResolver::new().extract("main.rs", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /health");
    assert_eq!(result.references[0].reference_name, "health_check");
}

#[test]
fn rust_skips_commented_out_route_calls_including_nested_block_comments() {
    let src = "\n// .route(\"/fake\", get(fake_handler))\n/* outer /* inner .route(\"/inner-fake\", get(x)) */ still .route(\"/outer-fake\", get(y)) */\nlet app = Router::new().route(\"/real\", get(list_users));\n";
    let result = RustResolver::new().extract("main.rs", src).unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["GET /real"]);
    let refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(refs, vec!["list_users"]);
}

// =============================================================================
// rustResolver.resolve cargo workspace crates (frameworks.test.ts)
// =============================================================================

fn workspace_ref(name: &str, file_path: &str) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: format!("fn:{file_path}:other:1"),
        reference_name: name.to_string(),
        reference_kind: EdgeKind::References,
        line: 1,
        column: 1,
        file_path: file_path.to_string(),
        language: Language::Rust,
        candidates: None,
        metadata: None,
    }
}

#[test]
fn resolves_crate_name_from_workspace_member_lib_rs() {
    let workspace_cargo =
        "\n[workspace]\nmembers = [\"crates/mytool-core\", \"crates/mytool-fetcher\"]\n";
    let core_cargo = "\n[package]\nname = \"mytool-core\"\nversion = \"0.1.0\"\n";

    let lib_node = make_module_node(
        "module:crates/mytool-core/src/lib.rs:mytool_core:1",
        "mytool_core",
        "crates/mytool-core/src/lib.rs",
    );

    let mut ctx = TestContext::new();
    ctx.nodes = vec![lib_node.clone()];
    ctx.files
        .insert("Cargo.toml".into(), workspace_cargo.into());
    ctx.files
        .insert("crates/mytool-core/Cargo.toml".into(), core_cargo.into());
    ctx.existing = [
        "Cargo.toml",
        "crates/mytool-core/Cargo.toml",
        "crates/mytool-core/src/lib.rs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let reference = UnresolvedRef {
        from_node_id: "fn:crates/mytool-fetcher/src/main.rs:main:1".into(),
        reference_name: "mytool_core".into(),
        reference_kind: EdgeKind::References,
        line: 1,
        column: 1,
        file_path: "crates/mytool-fetcher/src/main.rs".into(),
        language: Language::Rust,
        candidates: None,
        metadata: None,
    };

    let result = RustResolver::new().resolve(&reference, &ctx).unwrap();
    assert_eq!(result.target_node_id, lib_node.id);
    assert_eq!(result.resolved_by, ResolvedBy::Framework);
    // Workspace-manifest hits are unambiguous and must beat name-matcher's
    // self-file matches (0.7) so cross-crate `imports` edges materialize.
    assert!(result.confidence >= 0.9);
}

#[test]
fn resolves_crate_name_from_workspace_member_main_rs_when_lib_rs_is_absent() {
    let workspace_cargo = "\n[workspace]\nmembers = [\n  \"crates/mytool-runner\",\n]\n";
    let runner_cargo = "\n[package]\nname = \"mytool-runner\"\nversion = \"0.1.0\"\n";

    let main_node = make_module_node(
        "module:crates/mytool-runner/src/main.rs:mytool_runner:1",
        "mytool_runner",
        "crates/mytool-runner/src/main.rs",
    );

    let mut ctx = TestContext::new();
    ctx.nodes = vec![main_node.clone()];
    ctx.files
        .insert("Cargo.toml".into(), workspace_cargo.into());
    ctx.files.insert(
        "crates/mytool-runner/Cargo.toml".into(),
        runner_cargo.into(),
    );
    ctx.existing = [
        "Cargo.toml",
        "crates/mytool-runner/Cargo.toml",
        "crates/mytool-runner/src/main.rs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let reference = workspace_ref("mytool_runner", "crates/mytool-runner/src/main.rs");

    let result = RustResolver::new().resolve(&reference, &ctx).unwrap();
    assert_eq!(result.target_node_id, main_node.id);
    assert_eq!(result.resolved_by, ResolvedBy::Framework);
}

#[test]
fn resolves_crate_name_when_members_uses_a_glob() {
    let workspace_cargo = "\n[workspace]\nmembers = [\"crates/*\"]\n";
    let foo_cargo = "\n[package]\nname = \"mytool-foo\"\nversion = \"0.1.0\"\n";
    let bar_cargo = "\n[package]\nname = \"mytool-bar\"\nversion = \"0.1.0\"\n";

    let foo_lib = make_module_node(
        "module:crates/mytool-foo/src/lib.rs:mytool_foo:1",
        "mytool_foo",
        "crates/mytool-foo/src/lib.rs",
    );
    let bar_lib = make_module_node(
        "module:crates/mytool-bar/src/lib.rs:mytool_bar:1",
        "mytool_bar",
        "crates/mytool-bar/src/lib.rs",
    );

    let mut ctx = TestContext::new();
    ctx.nodes = vec![foo_lib.clone(), bar_lib.clone()];
    ctx.files
        .insert("Cargo.toml".into(), workspace_cargo.into());
    ctx.files
        .insert("crates/mytool-foo/Cargo.toml".into(), foo_cargo.into());
    ctx.files
        .insert("crates/mytool-bar/Cargo.toml".into(), bar_cargo.into());
    ctx.existing = [
        "Cargo.toml",
        "crates/mytool-foo/Cargo.toml",
        "crates/mytool-bar/Cargo.toml",
        "crates/mytool-foo/src/lib.rs",
        "crates/mytool-bar/src/lib.rs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    ctx.dirs.insert(".".into(), vec!["crates".into()]);
    ctx.dirs.insert(
        "crates".into(),
        vec!["mytool-foo".into(), "mytool-bar".into()],
    );
    ctx.dirs
        .insert("crates/mytool-foo".into(), vec!["src".into()]);
    ctx.dirs
        .insert("crates/mytool-bar".into(), vec!["src".into()]);

    let foo_ref = workspace_ref("mytool_foo", "crates/mytool-bar/src/lib.rs");
    let bar_ref = workspace_ref("mytool_bar", "crates/mytool-foo/src/lib.rs");

    let resolver = RustResolver::new();
    assert_eq!(
        resolver.resolve(&foo_ref, &ctx).unwrap().target_node_id,
        foo_lib.id
    );
    assert_eq!(
        resolver.resolve(&bar_ref, &ctx).unwrap().target_node_id,
        bar_lib.id
    );
}

#[test]
fn resolves_crate_name_when_members_uses_a_name_glob_at_root() {
    let workspace_cargo = "\n[workspace]\nmembers = [\"helix-*\"]\n";
    let core_cargo = "\n[package]\nname = \"helix-core\"\nversion = \"0.1.0\"\n";

    let core_lib = make_module_node(
        "module:helix-core/src/lib.rs:helix_core:1",
        "helix_core",
        "helix-core/src/lib.rs",
    );

    let mut ctx = TestContext::new();
    ctx.nodes = vec![core_lib.clone()];
    ctx.files
        .insert("Cargo.toml".into(), workspace_cargo.into());
    ctx.files
        .insert("helix-core/Cargo.toml".into(), core_cargo.into());
    ctx.existing = [
        "Cargo.toml",
        "helix-core/Cargo.toml",
        "helix-core/src/lib.rs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    ctx.dirs.insert(
        ".".into(),
        vec!["helix-core".into(), "docs".into(), "target".into()],
    );
    ctx.dirs.insert("helix-core".into(), vec!["src".into()]);

    let reference = workspace_ref("helix_core", "helix-core/src/lib.rs");

    assert_eq!(
        RustResolver::new()
            .resolve(&reference, &ctx)
            .unwrap()
            .target_node_id,
        core_lib.id
    );
}

// =============================================================================
// vaporResolver.extract (frameworks.test.ts)
// =============================================================================

#[test]
fn vapor_extracts_route_from_app_get_with_use() {
    let src = "app.get(\"users\", use: listUsers)\n";
    let result = VaporResolver.extract("routes.swift", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "listUsers");
}

#[test]
fn vapor_extracts_grouped_route_collection_routes_with_group_prefix() {
    let src = "\nfunc boot(routes: RoutesBuilder) throws {\n    let todos = routes.grouped(\"todos\")\n    todos.get(use: index)\n    todos.post(use: create)\n    todos.group(\":todoID\") { todo in\n        todo.delete(use: delete)\n    }\n}\n";
    let result = VaporResolver.extract("TodoController.swift", src).unwrap();
    let mut names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["DELETE /todos/:todoID", "GET /todos", "POST /todos"]
    );
    let mut refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    refs.sort();
    assert_eq!(refs, vec!["create", "delete", "index"]);
}

#[test]
fn vapor_handles_use_self_handler_and_non_string_path_segments() {
    let src = "router.get(\"users\", User.parameter, \"edit\", use: self.editUserHandler)\n";
    let result = VaporResolver.extract("UserController.swift", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /users/edit");
    assert_eq!(result.references[0].reference_name, "editUserHandler");
}

#[test]
fn vapor_ignores_non_route_get_calls_that_lack_use() {
    let src = "let host = Environment.get(\"DATABASE_HOST\") ?? \"localhost\"\n";
    let result = VaporResolver.extract("configure.swift", src).unwrap();
    assert_eq!(result.nodes.len(), 0);
}

#[test]
fn vapor_skips_commented_out_app_method_calls() {
    let src = "\n// app.get(\"fake\", use: fakeHandler)\n/* app.post(\"also-fake\", use: anotherHandler) */\napp.get(\"real\", use: listUsers)\n";
    let result = VaporResolver.extract("routes.swift", src).unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["GET /real"]);
    let refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(refs, vec!["listUsers"]);
}

// =============================================================================
// swiftObjcBridgeResolver (swift-objc-bridge-resolver.test.ts)
// =============================================================================

#[test]
fn bridge_detect_returns_true_when_both_swift_and_m_files_exist() {
    let ctx = TestContext::from_nodes(
        vec![
            method("foo", Language::Swift, "A.swift", 10),
            method("bar", Language::Objc, "B.m", 10),
        ],
        &[],
    );
    assert!(SwiftObjcBridgeResolver::new().detect(&ctx));
}

#[test]
fn bridge_detect_returns_false_when_only_swift_files_exist() {
    let ctx = TestContext::from_nodes(vec![method("foo", Language::Swift, "A.swift", 10)], &[]);
    assert!(!SwiftObjcBridgeResolver::new().detect(&ctx));
}

#[test]
fn bridge_detect_returns_true_when_swift_and_mm_exist() {
    let ctx = TestContext::from_nodes(
        vec![
            method("foo", Language::Swift, "A.swift", 10),
            method("bar", Language::Objc, "B.mm", 10),
        ],
        &[],
    );
    assert!(SwiftObjcBridgeResolver::new().detect(&ctx));
}

#[test]
fn bridge_claims_selector_shape_names() {
    let resolver = SwiftObjcBridgeResolver::new();
    assert!(resolver.claims_reference("fooWithBar:"));
    assert!(resolver.claims_reference("tableView:didSelectRowAtIndexPath:"));
    assert!(resolver.claims_reference("setName:"));
}

#[test]
fn bridge_does_not_claim_bare_names() {
    let resolver = SwiftObjcBridgeResolver::new();
    assert!(!resolver.claims_reference("foo"));
    assert!(!resolver.claims_reference("init"));
}

#[test]
fn bridge_resolves_swift_call_to_cocoa_style_objc_method() {
    // Swift writes `cache.fetchEntry(forKey: "x")` → ref name `fetchEntry`.
    // ObjC method is `fetchEntryForKey:` (preposition-prefix shape).
    // `fetchEntry` is project-specific (not in the generic-names blocklist
    // that filters init/count/description/etc. to avoid Cocoa noise).
    let objc_target = method("fetchEntryForKey:", Language::Objc, "Cache.m", 10);
    let ctx = TestContext::from_nodes(vec![objc_target.clone()], &[]);
    let result = SwiftObjcBridgeResolver::new()
        .resolve(
            &make_ref("fetchEntry", Language::Swift, "Caller.swift"),
            &ctx,
        )
        .expect("should resolve");
    assert_eq!(result.target_node_id, objc_target.id);
    assert_eq!(result.resolved_by, ResolvedBy::Framework);
    assert_eq!(result.confidence, 0.6);
}

#[test]
fn bridge_does_not_bridge_generic_cocoa_names() {
    // Bridging Swift `init()` calls to arbitrary ObjC `init*:` methods is
    // noise — every NSObject subclass has them. The regular name-matcher
    // handles `init` on its own.
    let objc_init = method("initWithFrame:", Language::Objc, "View.m", 10);
    let ctx = TestContext::from_nodes(vec![objc_init], &[]);
    let result = SwiftObjcBridgeResolver::new()
        .resolve(&make_ref("init", Language::Swift, "Caller.swift"), &ctx);
    assert!(result.is_none());
}

#[test]
fn bridge_resolves_bridged_with_form() {
    // Swift `play(song:)` → ObjC `playWithSong:`
    let objc_target = method("playWithSong:", Language::Objc, "Player.m", 10);
    let ctx = TestContext::from_nodes(vec![objc_target.clone()], &[]);
    let result = SwiftObjcBridgeResolver::new()
        .resolve(&make_ref("play", Language::Swift, "Caller.swift"), &ctx)
        .expect("should resolve");
    assert_eq!(result.target_node_id, objc_target.id);
}

#[test]
fn bridge_returns_none_when_no_matching_objc_method_exists() {
    let ctx = TestContext::from_nodes(
        vec![method("unrelated:thing:", Language::Objc, "X.m", 10)],
        &[],
    );
    let result = SwiftObjcBridgeResolver::new().resolve(
        &make_ref("completelyDifferent", Language::Swift, "Caller.swift"),
        &ctx,
    );
    assert!(result.is_none());
}

#[test]
fn bridge_resolves_objc_selector_to_objc_exposed_swift_method() {
    // Swift @objc export of `func animate(xAxisDuration:, yAxisDuration:)`
    // produces ObjC selector `animateWithXAxisDuration:yAxisDuration:`
    // (always "With" insertion on first explicit label).
    let swift_target = method("animate", Language::Swift, "Chart.swift", 10);
    let content = format!(
        "{}@objc open func animate(xAxisDuration: Double, yAxisDuration: Double) {{}}\n",
        "\n".repeat(8)
    );
    let ctx = TestContext::from_nodes(vec![swift_target.clone()], &[("Chart.swift", &content)]);
    let result = SwiftObjcBridgeResolver::new()
        .resolve(
            &make_ref(
                "animateWithXAxisDuration:yAxisDuration:",
                Language::Objc,
                "Caller.m",
            ),
            &ctx,
        )
        .expect("should resolve");
    assert_eq!(result.target_node_id, swift_target.id);
    assert_eq!(result.resolved_by, ResolvedBy::Framework);
}

#[test]
fn bridge_does_not_resolve_when_swift_method_is_not_objc_exposed() {
    let swift_target = method("animate", Language::Swift, "Chart.swift", 10);
    // Plain `func` without @objc — bridge correctly skips it
    let content = format!(
        "{}func animate(xAxisDuration: Double, yAxisDuration: Double) {{}}\n",
        "\n".repeat(8)
    );
    let ctx = TestContext::from_nodes(vec![swift_target], &[("Chart.swift", &content)]);
    let result = SwiftObjcBridgeResolver::new().resolve(
        &make_ref(
            "animateWithXAxisDuration:yAxisDuration:",
            Language::Objc,
            "Caller.m",
        ),
        &ctx,
    );
    assert!(result.is_none());
}

#[test]
fn bridge_resolves_init_selectors_to_swift_init() {
    let swift_target = method("init", Language::Swift, "MyClass.swift", 10);
    let content = format!(
        "{}@objc init(name: String, age: Int) {{}}\n",
        "\n".repeat(8)
    );
    let ctx = TestContext::from_nodes(vec![swift_target.clone()], &[("MyClass.swift", &content)]);
    let result = SwiftObjcBridgeResolver::new()
        .resolve(
            &make_ref("initWithName:age:", Language::Objc, "Caller.m"),
            &ctx,
        )
        .expect("should resolve");
    assert_eq!(result.target_node_id, swift_target.id);
}

#[test]
fn bridge_returns_none_for_selectors_with_no_derivable_existing_candidates() {
    let ctx = TestContext::from_nodes(vec![], &[]);
    let result = SwiftObjcBridgeResolver::new().resolve(
        &make_ref("someUnknownThing:", Language::Objc, "Caller.m"),
        &ctx,
    );
    assert!(result.is_none());
}
