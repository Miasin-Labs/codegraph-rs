//! Resolution pipeline integration tests.
//!
//! Ports the full-pipeline suites of `__tests__/resolution.test.ts` that the
//! import/match agents left to the stitch owner (Framework Detection,
//! Integration Tests, tsconfig path aliases, re-export chain following, the
//! C/C++ end-to-end include case), the `getApplicableFrameworks` suite from
//! `__tests__/frameworks.test.ts`, the resolution half of
//! `__tests__/object-literal-methods.test.ts`, and the resolution parts of
//! `__tests__/pr19-improvements.test.ts` (Best-Candidate Resolution +
//! Resolution Warm Caches).
//!
//! The TS suites drive `CodeGraph.init` + `indexAll`; the extraction
//! orchestrator is still in flight, so these fixtures insert the
//! extraction-shaped nodes/files the TS extractors would produce (same ids,
//! kinds, qualified-name schemes) over REAL files in a tempdir and REAL
//! SQLite — no mocks. The resolution side (import maps, alias/workspace/
//! go.mod loading, barrel chases, name matching, edge persistence) runs the
//! real production code end-to-end via `create_resolver` +
//! `resolve_and_persist_batched`.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use codegraph::db::{DatabaseConnection, QueryBuilder};
use codegraph::resolution::frameworks::{
    detect_frameworks,
    get_all_framework_resolvers,
    get_applicable_frameworks,
};
use codegraph::resolution::import_resolver::clear_cpp_include_dir_cache;
use codegraph::resolution::{
    FrameworkResolver,
    ImportMapping,
    ReferenceResolver,
    ResolutionContext,
    ResolvedRef,
    UnresolvedRef,
    create_resolver,
};
use codegraph::types::{Edge, EdgeKind, FileRecord, Language, Node, NodeKind, UnresolvedReference};
use tempfile::{TempDir, tempdir};

// =============================================================================
// Fixture helpers
// =============================================================================

struct Fx {
    _dir: TempDir,
    root: PathBuf,
    conn: DatabaseConnection,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let conn = DatabaseConnection::initialize(root.join(".codegraph").join("codegraph.db"))
            .expect("initialize db");
        Fx {
            _dir: dir,
            root,
            conn,
        }
    }

    fn q(&self) -> QueryBuilder {
        QueryBuilder::new(self.conn.get_db().expect("db"))
    }

    fn resolver(&self) -> ReferenceResolver {
        create_resolver(self.root.to_string_lossy().to_string(), self.q())
    }

    /// Write a REAL file under the project root (creates parent dirs).
    fn write(&self, rel: &str, content: &str) {
        let p = self.root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("mkdir -p");
        }
        fs::write(p, content).expect("write fixture file");
    }

    /// Track a file in the `files` table (what indexing does) so
    /// `warm_caches`' known-files set sees it.
    fn track(&self, q: &QueryBuilder, path: &str, language: Language) {
        q.upsert_file(&FileRecord {
            path: path.to_string(),
            content_hash: "test".to_string(),
            language,
            size: 1,
            modified_at: 1,
            indexed_at: 1,
            node_count: 0,
            errors: None,
        })
        .expect("upsert file");
    }
}

fn node(
    id: &str,
    kind: NodeKind,
    name: &str,
    qualified_name: &str,
    file_path: &str,
    language: Language,
    start_line: u32,
    end_line: u32,
) -> Node {
    Node::new(
        id,
        kind,
        name,
        qualified_name,
        file_path,
        language,
        start_line,
        end_line,
    )
}

fn exported(mut n: Node) -> Node {
    n.is_exported = Some(true);
    n
}

fn uref(
    from: &str,
    name: &str,
    kind: EdgeKind,
    line: u32,
    file_path: &str,
    language: Language,
) -> UnresolvedReference {
    UnresolvedReference {
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: kind,
        line,
        column: 0,
        file_path: Some(file_path.to_string()),
        language: Some(language),
        candidates: None,
    }
}

fn incoming(q: &QueryBuilder, id: &str, kind: EdgeKind) -> Vec<Edge> {
    q.get_incoming_edges(id, Some(&[kind]))
        .expect("incoming edges")
}

fn outgoing(q: &QueryBuilder, id: &str, kind: EdgeKind) -> Vec<Edge> {
    q.get_outgoing_edges(id, Some(&[kind]), None)
        .expect("outgoing edges")
}

fn source_files(q: &QueryBuilder, edges: &[Edge]) -> Vec<String> {
    edges
        .iter()
        .filter_map(|e| q.get_node_by_id(&e.source).ok().flatten())
        .map(|n| n.file_path)
        .collect()
}

// =============================================================================
// Mock ResolutionContext for the framework-detection cases (mirrors the TS
// inline object-literal contexts)
// =============================================================================

#[derive(Default)]
struct MockCtx {
    files: Vec<String>,
    contents: HashMap<String, String>,
    existing: Vec<String>,
    root: String,
}

impl ResolutionContext for MockCtx {
    fn get_nodes_in_file(&self, _: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_name(&self, _: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_qualified_name(&self, _: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_kind(&self, _: NodeKind) -> Vec<Node> {
        Vec::new()
    }
    fn file_exists(&self, p: &str) -> bool {
        self.existing.iter().any(|f| f == p)
    }
    fn read_file(&self, p: &str) -> Option<String> {
        self.contents.get(p).cloned()
    }
    fn get_project_root(&self) -> &str {
        &self.root
    }
    fn get_all_files(&self) -> Vec<String> {
        self.files.clone()
    }
    fn get_nodes_by_lower_name(&self, _: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_import_mappings(&self, _: &str, _: Language) -> Vec<ImportMapping> {
        Vec::new()
    }
}

// =============================================================================
// Framework Detection (resolution.test.ts)
// =============================================================================

#[test]
fn detects_react_framework() {
    let ctx = MockCtx {
        files: vec!["package.json".into(), "src/App.tsx".into()],
        contents: HashMap::from([(
            "package.json".to_string(),
            r#"{"dependencies":{"react":"^18.0.0"}}"#.to_string(),
        )]),
        existing: vec![],
        root: "/test".into(),
    };
    let frameworks = detect_frameworks(&ctx);
    assert!(frameworks.iter().any(|f| f.name() == "react"));
}

#[test]
fn detects_express_framework() {
    let ctx = MockCtx {
        files: vec!["package.json".into(), "src/app.js".into()],
        contents: HashMap::from([(
            "package.json".to_string(),
            r#"{"dependencies":{"express":"^4.18.0"}}"#.to_string(),
        )]),
        existing: vec![],
        root: "/test".into(),
    };
    let frameworks = detect_frameworks(&ctx);
    assert!(frameworks.iter().any(|f| f.name() == "express"));
}

#[test]
fn detects_laravel_framework() {
    let ctx = MockCtx {
        files: vec!["artisan".into(), "app/Http/Kernel.php".into()],
        contents: HashMap::new(),
        existing: vec!["artisan".into()],
        root: "/test".into(),
    };
    let frameworks = detect_frameworks(&ctx);
    assert!(frameworks.iter().any(|f| f.name() == "laravel"));
}

#[test]
fn returns_all_framework_resolvers() {
    let resolvers = get_all_framework_resolvers();
    assert!(!resolvers.is_empty());
    assert!(resolvers.iter().any(|r| r.name() == "react"));
    assert!(resolvers.iter().any(|r| r.name() == "express"));
    assert!(resolvers.iter().any(|r| r.name() == "laravel"));
}

#[test]
fn framework_registry_preserves_ts_registration_order() {
    // The TS FRAMEWORK_RESOLVERS array order is load-bearing: detection
    // results and per-reference strategy order iterate it.
    let names: Vec<String> = get_all_framework_resolvers()
        .iter()
        .map(|r| r.name().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "laravel",
            "drupal",
            "express",
            "nestjs",
            "react",
            "svelte",
            "vue",
            "django",
            "flask",
            "fastapi",
            "rails",
            "spring",
            "play",
            "go",
            "rust",
            "aspnet",
            "swiftui",
            "uikit",
            "vapor",
            "swift-objc-bridge",
            "react-native-bridge",
            "expo-modules",
            "fabric-view",
            "salesforce",
        ]
    );
}

// =============================================================================
// getApplicableFrameworks (frameworks.test.ts)
// =============================================================================

struct FakeFw {
    name: &'static str,
    langs: Option<&'static [Language]>,
}

impl FrameworkResolver for FakeFw {
    fn name(&self) -> &str {
        self.name
    }
    fn languages(&self) -> Option<&[Language]> {
        self.langs
    }
    fn detect(&self, _: &dyn ResolutionContext) -> bool {
        true
    }
    fn resolve(&self, _: &UnresolvedRef, _: &dyn ResolutionContext) -> Option<ResolvedRef> {
        None
    }
}

fn fake_fws() -> Vec<Box<dyn FrameworkResolver>> {
    static PY: [Language; 1] = [Language::Python];
    static JS: [Language; 2] = [Language::Javascript, Language::Typescript];
    vec![
        Box::new(FakeFw {
            name: "py",
            langs: Some(&PY),
        }),
        Box::new(FakeFw {
            name: "js",
            langs: Some(&JS),
        }),
        Box::new(FakeFw {
            name: "any",
            langs: None,
        }),
    ]
}

#[test]
fn get_applicable_frameworks_filters_by_language() {
    let fws = fake_fws();
    let result = get_applicable_frameworks(&fws, Language::Python);
    let names: Vec<&str> = result.iter().map(|r| r.name()).collect();
    assert_eq!(names, vec!["py", "any"]);
}

#[test]
fn get_applicable_frameworks_returns_universal_only_when_no_match() {
    let fws = fake_fws();
    let result = get_applicable_frameworks(&fws, Language::Rust);
    let names: Vec<&str> = result.iter().map(|r| r.name()).collect();
    assert_eq!(names, vec!["any"]);
}

// =============================================================================
// Integration Tests (resolution.test.ts)
// =============================================================================

#[test]
fn creates_resolver_that_detects_react_from_project() {
    let fx = Fx::new();
    fx.write(
        "package.json",
        r#"{"name":"test","dependencies":{"react":"^18.0.0"}}"#,
    );
    fx.write(
        "src/utils.ts",
        "export function formatDate(date: Date): string {\n  return date.toISOString();\n}\n",
    );
    fx.write("src/main.ts", "import { formatDate } from './utils';\n");

    let resolver = fx.resolver();
    let frameworks = resolver.get_detected_frameworks();
    assert!(frameworks.contains(&"react".to_string()));
}

#[test]
fn resolves_references_after_indexing() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/helper.ts",
        "export function helperFunction(): void {\n  console.log('helper');\n}",
    );
    fx.write(
        "src/main.ts",
        "import { helperFunction } from './helper';\n\nfunction main(): void {\n  helperFunction();\n}",
    );
    fx.track(&q, "src/helper.ts", Language::Typescript);
    fx.track(&q, "src/main.ts", Language::Typescript);

    let helper = exported(node(
        "func:src/helper.ts:helperFunction:1",
        NodeKind::Function,
        "helperFunction",
        "src/helper.ts::helperFunction",
        "src/helper.ts",
        Language::Typescript,
        1,
        3,
    ));
    let main = node(
        "func:src/main.ts:main:3",
        NodeKind::Function,
        "main",
        "src/main.ts::main",
        "src/main.ts",
        Language::Typescript,
        3,
        5,
    );
    q.insert_nodes(&[helper.clone(), main.clone()]).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &main.id,
        "helperFunction",
        EdgeKind::Calls,
        4,
        "src/main.ts",
        Language::Typescript,
    )])
    .unwrap();

    let resolver = fx.resolver();
    let result = resolver.resolve_and_persist_batched(None, None).unwrap();

    // TS assertion: should have attempted resolution.
    assert!(result.stats.total >= 1);
    // Port validation: the import-based call edge landed.
    let callers = incoming(&q, &helper.id, EdgeKind::Calls);
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].source, main.id);
}

#[test]
fn promotes_calls_to_instantiates_when_target_is_a_class_python() {
    // Python has no `new` keyword — `Foo()` is the standard instantiation
    // syntax. Extraction emits a `calls` ref; resolution promotes it to
    // `instantiates` once the target is known to be a class.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/app.py",
        "class UserService:\n    def __init__(self):\n        self.db = None\n\ndef bootstrap():\n    return UserService()\n",
    );
    fx.track(&q, "src/app.py", Language::Python);

    let class = node(
        "class:src/app.py:UserService:1",
        NodeKind::Class,
        "UserService",
        "src/app.py::UserService",
        "src/app.py",
        Language::Python,
        1,
        3,
    );
    let init = node(
        "method:src/app.py:UserService.__init__:2",
        NodeKind::Method,
        "__init__",
        "src/app.py::UserService::__init__",
        "src/app.py",
        Language::Python,
        2,
        3,
    );
    let bootstrap = node(
        "func:src/app.py:bootstrap:5",
        NodeKind::Function,
        "bootstrap",
        "src/app.py::bootstrap",
        "src/app.py",
        Language::Python,
        5,
        6,
    );
    q.insert_nodes(&[class.clone(), init, bootstrap.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &bootstrap.id,
        "UserService",
        EdgeKind::Calls,
        6,
        "src/app.py",
        Language::Python,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let out = q.get_outgoing_edges(&bootstrap.id, None, None).unwrap();
    let instantiates: Vec<&Edge> = out
        .iter()
        .filter(|e| e.kind == EdgeKind::Instantiates)
        .collect();
    assert_eq!(instantiates.len(), 1, "promoted instantiates edge expected");
    assert_eq!(instantiates[0].target, class.id);
    // Same edge must NOT also appear as a `calls` edge — promotion replaces
    // the kind, doesn't duplicate.
    let calls_to_user_service: Vec<&Edge> = out
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.target == instantiates[0].target)
        .collect();
    assert!(calls_to_user_service.is_empty());
}

#[test]
fn resolves_go_cross_package_qualified_calls_via_go_mod_388() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write("go.mod", "module github.com/example/myproject\n\ngo 1.21\n");
    fx.write(
        "pkga/conv.go",
        "package pkga\nfunc Convert(x int) int { return x * 2 }\n",
    );
    fx.write(
        "pkgb/conv.go",
        "package pkgb\nfunc Convert(x int) int { return x + 1 }\n",
    );
    fx.write(
        "pkgc/use.go",
        "package pkgc\n\nimport \"github.com/example/myproject/pkga\"\n\nfunc UsePkga() {\n  pkga.Convert(5)\n}\n",
    );
    for f in ["pkga/conv.go", "pkgb/conv.go", "pkgc/use.go"] {
        fx.track(&q, f, Language::Go);
    }

    // Same-name exported function in two packages — only the imported one
    // should resolve.
    let convert_a = exported(node(
        "func:pkga/conv.go:Convert:2",
        NodeKind::Function,
        "Convert",
        "pkga::Convert",
        "pkga/conv.go",
        Language::Go,
        2,
        2,
    ));
    let convert_b = exported(node(
        "func:pkgb/conv.go:Convert:2",
        NodeKind::Function,
        "Convert",
        "pkgb::Convert",
        "pkgb/conv.go",
        Language::Go,
        2,
        2,
    ));
    let use_pkga = exported(node(
        "func:pkgc/use.go:UsePkga:5",
        NodeKind::Function,
        "UsePkga",
        "pkgc::UsePkga",
        "pkgc/use.go",
        Language::Go,
        5,
        7,
    ));
    q.insert_nodes(&[convert_a.clone(), convert_b, use_pkga.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &use_pkga.id,
        "pkga.Convert",
        EdgeKind::Calls,
        6,
        "pkgc/use.go",
        Language::Go,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let call_edges = outgoing(&q, &use_pkga.id, EdgeKind::Calls);
    assert_eq!(call_edges.len(), 1);
    let target = q.get_node_by_id(&call_edges[0].target).unwrap().unwrap();
    assert_eq!(target.name, "Convert");
    // Critical: the resolver must pick the imported pkga's Convert, not pkgb's.
    assert_eq!(target.file_path.replace('\\', "/"), "pkga/conv.go");
}

#[test]
fn resolves_go_aliased_imports_across_packages_388() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write("go.mod", "module github.com/example/myproject\n\ngo 1.21\n");
    fx.write(
        "pkgb/lib.go",
        "package pkgb\nfunc Compute(x int) int { return x }\n",
    );
    fx.write(
        "pkgd/use.go",
        "package pkgd\n\nimport (\n  \"fmt\"\n  alias \"github.com/example/myproject/pkgb\"\n)\n\nfunc UseAliased() {\n  fmt.Println(\"hi\")\n  alias.Compute(3)\n}\n",
    );
    fx.track(&q, "pkgb/lib.go", Language::Go);
    fx.track(&q, "pkgd/use.go", Language::Go);

    let compute = exported(node(
        "func:pkgb/lib.go:Compute:2",
        NodeKind::Function,
        "Compute",
        "pkgb::Compute",
        "pkgb/lib.go",
        Language::Go,
        2,
        2,
    ));
    let use_aliased = exported(node(
        "func:pkgd/use.go:UseAliased:8",
        NodeKind::Function,
        "UseAliased",
        "pkgd::UseAliased",
        "pkgd/use.go",
        Language::Go,
        8,
        11,
    ));
    q.insert_nodes(&[compute.clone(), use_aliased.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[
        uref(
            &use_aliased.id,
            "fmt.Println",
            EdgeKind::Calls,
            9,
            "pkgd/use.go",
            Language::Go,
        ),
        uref(
            &use_aliased.id,
            "alias.Compute",
            EdgeKind::Calls,
            10,
            "pkgd/use.go",
            Language::Go,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    // fmt.Println is stdlib — must stay external. alias.Compute must resolve.
    let calls = outgoing(&q, &use_aliased.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 1);
    let target = q.get_node_by_id(&calls[0].target).unwrap().unwrap();
    assert_eq!(target.name, "Compute");
    assert_eq!(target.file_path.replace('\\', "/"), "pkgb/lib.go");
}

#[test]
fn resolves_go_cross_package_calls_from_nested_module_root() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "go/go.mod",
        "module github.com/dolthub/dolt/go\n\ngo 1.24\n",
    );
    fx.write(
        "go/libraries/doltcore/diff/calc.go",
        "package diff\nfunc Calculate(x int) int { return x * 2 }\n",
    );
    fx.write(
        "go/cmd/dolt/diff/calc.go",
        "package diff\nfunc Calculate(x int) int { return x + 1 }\n",
    );
    fx.write(
        "go/cmd/dolt/main.go",
        "package main\n\nimport \"github.com/dolthub/dolt/go/libraries/doltcore/diff\"\n\nfunc main() {\n  diff.Calculate(5)\n}\n",
    );
    for f in [
        "go/libraries/doltcore/diff/calc.go",
        "go/cmd/dolt/diff/calc.go",
        "go/cmd/dolt/main.go",
    ] {
        fx.track(&q, f, Language::Go);
    }

    let imported_calculate = exported(node(
        "func:go/libraries/doltcore/diff/calc.go:Calculate:2",
        NodeKind::Function,
        "Calculate",
        "diff::Calculate",
        "go/libraries/doltcore/diff/calc.go",
        Language::Go,
        2,
        2,
    ));
    let nearby_distractor = exported(node(
        "func:go/cmd/dolt/diff/calc.go:Calculate:2",
        NodeKind::Function,
        "Calculate",
        "diff::Calculate",
        "go/cmd/dolt/diff/calc.go",
        Language::Go,
        2,
        2,
    ));
    let main_fn = exported(node(
        "func:go/cmd/dolt/main.go:main:5",
        NodeKind::Function,
        "main",
        "main::main",
        "go/cmd/dolt/main.go",
        Language::Go,
        5,
        7,
    ));
    q.insert_nodes(&[
        imported_calculate.clone(),
        nearby_distractor,
        main_fn.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &main_fn.id,
        "diff.Calculate",
        EdgeKind::Calls,
        6,
        "go/cmd/dolt/main.go",
        Language::Go,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let call_edges = outgoing(&q, &main_fn.id, EdgeKind::Calls);
    assert_eq!(call_edges.len(), 1);
    let target = q.get_node_by_id(&call_edges[0].target).unwrap().unwrap();
    assert_eq!(
        target.file_path.replace('\\', "/"),
        "go/libraries/doltcore/diff/calc.go"
    );
}

#[test]
fn type_alias_object_shape_members_resolve_method_calls_359() {
    // `recorder.stop()` (recorder: RecorderHandle) must attach to
    // `RecorderHandle::stop`, not the look-alike class method in a sibling
    // directory.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "voice/recorder.ts",
        "export type RecorderHandle = {\n  wavPath: string;\n  stop: () => Promise<{ ok: true }>;\n};\n",
    );
    fx.write(
        "voice/controller.ts",
        "import type { RecorderHandle } from \"./recorder\";\nexport async function finaliseRecording(recorder: RecorderHandle) {\n  return await recorder.stop();\n}\n",
    );
    fx.write(
        "codegraph/stdio-client.ts",
        "export class StdioMcpClient {\n  private stopped = false;\n  async stop(): Promise<void> { this.stopped = true; }\n}\n",
    );
    fx.track(&q, "voice/recorder.ts", Language::Typescript);
    fx.track(&q, "voice/controller.ts", Language::Typescript);
    fx.track(&q, "codegraph/stdio-client.ts", Language::Typescript);

    // type_alias produces member nodes (property/method) — #359. The TS test
    // asserts qualifiedName === 'RecorderHandle::stop' exactly.
    let alias = exported(node(
        "type:voice/recorder.ts:RecorderHandle:1",
        NodeKind::TypeAlias,
        "RecorderHandle",
        "voice/recorder.ts::RecorderHandle",
        "voice/recorder.ts",
        Language::Typescript,
        1,
        4,
    ));
    let wav_path = node(
        "prop:voice/recorder.ts:RecorderHandle.wavPath:2",
        NodeKind::Property,
        "wavPath",
        "RecorderHandle::wavPath",
        "voice/recorder.ts",
        Language::Typescript,
        2,
        2,
    );
    // Function-typed property surfaces as a `method` node, not `property`.
    let handle_stop = node(
        "method:voice/recorder.ts:RecorderHandle.stop:3",
        NodeKind::Method,
        "stop",
        "RecorderHandle::stop",
        "voice/recorder.ts",
        Language::Typescript,
        3,
        3,
    );
    let finalise = exported(node(
        "func:voice/controller.ts:finaliseRecording:2",
        NodeKind::Function,
        "finaliseRecording",
        "voice/controller.ts::finaliseRecording",
        "voice/controller.ts",
        Language::Typescript,
        2,
        4,
    ));
    let client = exported(node(
        "class:codegraph/stdio-client.ts:StdioMcpClient:1",
        NodeKind::Class,
        "StdioMcpClient",
        "codegraph/stdio-client.ts::StdioMcpClient",
        "codegraph/stdio-client.ts",
        Language::Typescript,
        1,
        4,
    ));
    let client_stop = node(
        "method:codegraph/stdio-client.ts:StdioMcpClient.stop:3",
        NodeKind::Method,
        "stop",
        "StdioMcpClient::stop",
        "codegraph/stdio-client.ts",
        Language::Typescript,
        3,
        3,
    );
    q.insert_nodes(&[
        alias,
        wav_path,
        handle_stop.clone(),
        finalise.clone(),
        client,
        client_stop.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &finalise.id,
        "recorder.stop",
        EdgeKind::Calls,
        3,
        "voice/controller.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    assert_eq!(handle_stop.kind, NodeKind::Method);
    let handle_callers = incoming(&q, &handle_stop.id, EdgeKind::Calls);
    let client_callers = incoming(&q, &client_stop.id, EdgeKind::Calls);
    assert!(
        !handle_callers.is_empty(),
        "RecorderHandle::stop should have a caller"
    );
    // The class method must have NO callers — voice/'s call must NOT
    // mis-attribute.
    assert!(client_callers.is_empty());
}

#[test]
fn java_import_disambiguates_same_name_classes_across_modules_314() {
    let fx = Fx::new();
    let q = fx.q();
    let dao = "dao/src/main/java/com/example/dao/converter/FooConverter.java";
    let service = "service/src/main/java/com/example/service/converter/FooConverter.java";
    let web = "web/src/main/java/com/example/web/Handler.java";
    fx.write(
        dao,
        "package com.example.dao.converter;\npublic class FooConverter { public String convert(String x) { return \"dao:\" + x; } }\n",
    );
    fx.write(
        service,
        "package com.example.service.converter;\npublic class FooConverter { public String convert(String x) { return \"svc:\" + x; } }\n",
    );
    // The caller imports the SERVICE version.
    fx.write(
        web,
        "package com.example.web;\n\nimport com.example.service.converter.FooConverter;\n\npublic class Handler {\n  private FooConverter fooConverter;\n  public String use() { return fooConverter.convert(\"input\"); }\n}\n",
    );
    for f in [dao, service, web] {
        fx.track(&q, f, Language::Java);
    }

    let dao_class = exported(node(
        "class:dao:FooConverter:2",
        NodeKind::Class,
        "FooConverter",
        "com.example.dao.converter::FooConverter",
        dao,
        Language::Java,
        2,
        2,
    ));
    let dao_convert = node(
        "method:dao:FooConverter.convert:2",
        NodeKind::Method,
        "convert",
        "com.example.dao.converter::FooConverter::convert",
        dao,
        Language::Java,
        2,
        2,
    );
    let service_class = exported(node(
        "class:service:FooConverter:2",
        NodeKind::Class,
        "FooConverter",
        "com.example.service.converter::FooConverter",
        service,
        Language::Java,
        2,
        2,
    ));
    let service_convert = node(
        "method:service:FooConverter.convert:2",
        NodeKind::Method,
        "convert",
        "com.example.service.converter::FooConverter::convert",
        service,
        Language::Java,
        2,
        2,
    );
    let handler_class = exported(node(
        "class:web:Handler:5",
        NodeKind::Class,
        "Handler",
        "com.example.web::Handler",
        web,
        Language::Java,
        5,
        8,
    ));
    let mut field = node(
        "field:web:Handler.fooConverter:6",
        NodeKind::Field,
        "fooConverter",
        "com.example.web::Handler::fooConverter",
        web,
        Language::Java,
        6,
        6,
    );
    field.signature = Some("FooConverter fooConverter".to_string());
    let use_method = node(
        "method:web:Handler.use:7",
        NodeKind::Method,
        "use",
        "com.example.web::Handler::use",
        web,
        Language::Java,
        7,
        7,
    );
    q.insert_nodes(&[
        dao_class,
        dao_convert,
        service_class,
        service_convert.clone(),
        handler_class,
        field,
        use_method.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &use_method.id,
        "fooConverter.convert",
        EdgeKind::Calls,
        7,
        web,
        Language::Java,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let calls = outgoing(&q, &use_method.id, EdgeKind::Calls);
    assert!(!calls.is_empty());
    let target = q.get_node_by_id(&calls[0].target).unwrap().unwrap();
    assert_eq!(target.name, "convert");
    // The import must trump candidate order — even though dao is
    // lexically first.
    assert_eq!(target.id, service_convert.id);
    assert_eq!(target.file_path.replace('\\', "/"), service);
}

#[test]
fn csharp_type_references_resolve_to_dto_classes_381() {
    // Extraction-side #381 produces `references` refs from method returns/
    // params, properties and fields; this pins the RESOLUTION of those refs
    // to the DTO classes.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/Dtos.cs",
        "namespace MyApp;\npublic class SessionInfoDto { public string Id { get; set; } = \"\"; }\npublic class UserDto { public string Name { get; set; } = \"\"; }\n",
    );
    fx.write(
        "src/Service.cs",
        "using System.Threading.Tasks;\nnamespace MyApp;\npublic class DataExporter\n{\n  public SessionInfoDto Build(UserDto user, SessionInfoDto session) { return session; }\n  public Task<SessionInfoDto> BuildAsync(UserDto user) { return Task.FromResult(new SessionInfoDto()); }\n  public SessionInfoDto Latest { get; set; } = new();\n  private UserDto _cached;\n}\n",
    );
    fx.track(&q, "src/Dtos.cs", Language::Csharp);
    fx.track(&q, "src/Service.cs", Language::Csharp);

    let session_dto = exported(node(
        "class:src/Dtos.cs:SessionInfoDto:2",
        NodeKind::Class,
        "SessionInfoDto",
        "MyApp::SessionInfoDto",
        "src/Dtos.cs",
        Language::Csharp,
        2,
        2,
    ));
    let user_dto = exported(node(
        "class:src/Dtos.cs:UserDto:3",
        NodeKind::Class,
        "UserDto",
        "MyApp::UserDto",
        "src/Dtos.cs",
        Language::Csharp,
        3,
        3,
    ));
    let exporter = exported(node(
        "class:src/Service.cs:DataExporter:3",
        NodeKind::Class,
        "DataExporter",
        "MyApp::DataExporter",
        "src/Service.cs",
        Language::Csharp,
        3,
        9,
    ));
    let build = node(
        "method:src/Service.cs:DataExporter.Build:5",
        NodeKind::Method,
        "Build",
        "MyApp::DataExporter::Build",
        "src/Service.cs",
        Language::Csharp,
        5,
        5,
    );
    let build_async = node(
        "method:src/Service.cs:DataExporter.BuildAsync:6",
        NodeKind::Method,
        "BuildAsync",
        "MyApp::DataExporter::BuildAsync",
        "src/Service.cs",
        Language::Csharp,
        6,
        6,
    );
    let latest = node(
        "prop:src/Service.cs:DataExporter.Latest:7",
        NodeKind::Property,
        "Latest",
        "MyApp::DataExporter::Latest",
        "src/Service.cs",
        Language::Csharp,
        7,
        7,
    );
    let cached = node(
        "field:src/Service.cs:DataExporter._cached:8",
        NodeKind::Field,
        "_cached",
        "MyApp::DataExporter::_cached",
        "src/Service.cs",
        Language::Csharp,
        8,
        8,
    );
    q.insert_nodes(&[
        session_dto.clone(),
        user_dto.clone(),
        exporter,
        build.clone(),
        build_async.clone(),
        latest.clone(),
        cached.clone(),
    ])
    .unwrap();
    // SessionInfoDto: Build return, Build param, BuildAsync return (inside
    // Task<>), Latest property. UserDto: Build param, BuildAsync param,
    // _cached field.
    q.insert_unresolved_refs_batch(&[
        uref(
            &build.id,
            "SessionInfoDto",
            EdgeKind::References,
            5,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &build.id,
            "UserDto",
            EdgeKind::References,
            5,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &build.id,
            "SessionInfoDto",
            EdgeKind::References,
            5,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &build_async.id,
            "SessionInfoDto",
            EdgeKind::References,
            6,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &build_async.id,
            "UserDto",
            EdgeKind::References,
            6,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &latest.id,
            "SessionInfoDto",
            EdgeKind::References,
            7,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &cached.id,
            "UserDto",
            EdgeKind::References,
            8,
            "src/Service.cs",
            Language::Csharp,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let session_incoming = incoming(&q, &session_dto.id, EdgeKind::References);
    let user_incoming = incoming(&q, &user_dto.id, EdgeKind::References);
    assert!(
        session_incoming.len() >= 4,
        "got {}",
        session_incoming.len()
    );
    assert!(user_incoming.len() >= 3, "got {}", user_incoming.len());
}

#[test]
fn go_leaves_stdlib_calls_external() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write("go.mod", "module github.com/example/myproject\n\ngo 1.21\n");
    fx.write(
        "main.go",
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n  fmt.Println(\"hi\")\n}\n",
    );
    fx.track(&q, "main.go", Language::Go);

    let main_fn = node(
        "func:main.go:main:5",
        NodeKind::Function,
        "main",
        "main::main",
        "main.go",
        Language::Go,
        5,
        7,
    );
    q.insert_nodes(std::slice::from_ref(&main_fn)).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &main_fn.id,
        "fmt.Println",
        EdgeKind::Calls,
        6,
        "main.go",
        Language::Go,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    // No spurious in-project edge — fmt.* must stay unresolved/external.
    let calls = outgoing(&q, &main_fn.id, EdgeKind::Calls);
    assert!(calls.is_empty());
}

// =============================================================================
// tsconfig path aliases (resolution.test.ts)
// =============================================================================

#[test]
fn resolves_aliased_import_to_alias_mapped_file() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/utils/format.ts",
        "export function pickMe(): number { return 1; }\n",
    );
    fx.write(
        "src/legacy/format.ts",
        "export function pickMe(): number { return 99; }\n",
    );
    fx.write(
        "src/main.ts",
        "import { pickMe } from '@utils/format';\nexport function go(): number { return pickMe(); }\n",
    );
    fx.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":"./src","paths":{"@utils/*":["utils/*"]}}}"#,
    );
    for f in ["src/utils/format.ts", "src/legacy/format.ts", "src/main.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    let utils_pick = exported(node(
        "func:src/utils/format.ts:pickMe:1",
        NodeKind::Function,
        "pickMe",
        "src/utils/format.ts::pickMe",
        "src/utils/format.ts",
        Language::Typescript,
        1,
        1,
    ));
    let legacy_pick = exported(node(
        "func:src/legacy/format.ts:pickMe:1",
        NodeKind::Function,
        "pickMe",
        "src/legacy/format.ts::pickMe",
        "src/legacy/format.ts",
        Language::Typescript,
        1,
        1,
    ));
    let go_fn = exported(node(
        "func:src/main.ts:go:2",
        NodeKind::Function,
        "go",
        "src/main.ts::go",
        "src/main.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[utils_pick.clone(), legacy_pick.clone(), go_fn.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &go_fn.id,
        "pickMe",
        EdgeKind::Calls,
        2,
        "src/main.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let utils_callers = incoming(&q, &utils_pick.id, EdgeKind::Calls);
    let legacy_callers = incoming(&q, &legacy_pick.id, EdgeKind::Calls);
    assert!(!utils_callers.is_empty());
    assert!(source_files(&q, &utils_callers).contains(&"src/main.ts".to_string()));
    // The legacy node should NOT have a caller from src/main.ts — the alias
    // correctly picked the utils version.
    assert!(!source_files(&q, &legacy_callers).contains(&"src/main.ts".to_string()));
}

#[test]
fn falls_back_gracefully_when_tsconfig_is_absent() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write("src/a.ts", "export function aFn(): void {}\n");
    fx.write(
        "src/b.ts",
        "import { aFn } from './a';\nexport function bFn(): void { aFn(); }\n",
    );
    fx.track(&q, "src/a.ts", Language::Typescript);
    fx.track(&q, "src/b.ts", Language::Typescript);

    let a_fn = exported(node(
        "func:src/a.ts:aFn:1",
        NodeKind::Function,
        "aFn",
        "src/a.ts::aFn",
        "src/a.ts",
        Language::Typescript,
        1,
        1,
    ));
    let b_fn = exported(node(
        "func:src/b.ts:bFn:2",
        NodeKind::Function,
        "bFn",
        "src/b.ts::bFn",
        "src/b.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[a_fn.clone(), b_fn.clone()]).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &b_fn.id,
        "aFn",
        EdgeKind::Calls,
        2,
        "src/b.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let callers = incoming(&q, &a_fn.id, EdgeKind::Calls);
    assert!(source_files(&q, &callers).contains(&"src/b.ts".to_string()));
}

// =============================================================================
// re-export chain following (resolution.test.ts)
// =============================================================================

#[test]
fn chases_a_3_hop_barrel_chain_wildcard_named_declaration() {
    // main.ts → all.ts (wildcard) → index.ts (named) → auth.ts (declaration).
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/services/auth.ts",
        "export function signIn(): void {}\n",
    );
    fx.write(
        "src/services/index.ts",
        "export { signIn } from './auth';\n",
    );
    fx.write("src/all.ts", "export * from './services/index';\n");
    fx.write(
        "src/main.ts",
        "import { signIn } from './all';\nexport function go(): void { signIn(); }\n",
    );
    for f in [
        "src/services/auth.ts",
        "src/services/index.ts",
        "src/all.ts",
        "src/main.ts",
    ] {
        fx.track(&q, f, Language::Typescript);
    }

    let sign_in = exported(node(
        "func:src/services/auth.ts:signIn:1",
        NodeKind::Function,
        "signIn",
        "src/services/auth.ts::signIn",
        "src/services/auth.ts",
        Language::Typescript,
        1,
        1,
    ));
    let go_fn = exported(node(
        "func:src/main.ts:go:2",
        NodeKind::Function,
        "go",
        "src/main.ts::go",
        "src/main.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[sign_in.clone(), go_fn.clone()]).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &go_fn.id,
        "signIn",
        EdgeKind::Calls,
        2,
        "src/main.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let callers = incoming(&q, &sign_in.id, EdgeKind::Calls);
    assert!(source_files(&q, &callers).contains(&"src/main.ts".to_string()));
}

#[test]
fn follows_a_renamed_named_re_export() {
    // `export { signIn as login } from './auth'` — the chase looks up
    // `signIn` upstream even though the importer asked for `login`.
    let fx = Fx::new();
    let q = fx.q();
    fx.write("src/auth.ts", "export function signIn(): void {}\n");
    fx.write(
        "src/index.ts",
        "export { signIn as login } from './auth';\n",
    );
    fx.write(
        "src/main.ts",
        "import { login } from './index';\nexport function go(): void { login(); }\n",
    );
    for f in ["src/auth.ts", "src/index.ts", "src/main.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    let sign_in = exported(node(
        "func:src/auth.ts:signIn:1",
        NodeKind::Function,
        "signIn",
        "src/auth.ts::signIn",
        "src/auth.ts",
        Language::Typescript,
        1,
        1,
    ));
    let go_fn = exported(node(
        "func:src/main.ts:go:2",
        NodeKind::Function,
        "go",
        "src/main.ts::go",
        "src/main.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[sign_in.clone(), go_fn.clone()]).unwrap();
    // `login` has NO declaration anywhere — only the import-mapping escape in
    // the resolver's pre-filter lets this ref through.
    q.insert_unresolved_refs_batch(&[uref(
        &go_fn.id,
        "login",
        EdgeKind::Calls,
        2,
        "src/main.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let callers = incoming(&q, &sign_in.id, EdgeKind::Calls);
    assert!(source_files(&q, &callers).contains(&"src/main.ts".to_string()));
}

#[test]
fn follows_default_re_export_of_a_svelte_component_629() {
    // `export { default as Foo } from './RealButton.svelte'` — alias differs
    // from the component's real name so only the import-chase (with the
    // component-node preference for default exports) can connect the edge.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/lib/RealButton.svelte",
        "<script lang=\"ts\">\n  export let label: string = '';\n</script>\n\n<button>{label}</button>\n",
    );
    fx.write(
        "src/lib/index.ts",
        "export { default as Foo } from './RealButton.svelte';\n",
    );
    fx.write(
        "src/Bar.svelte",
        "<script lang=\"ts\">\n  import { Foo } from './lib';\n</script>\n\n<Foo />\n",
    );
    fx.track(&q, "src/lib/RealButton.svelte", Language::Svelte);
    fx.track(&q, "src/lib/index.ts", Language::Typescript);
    fx.track(&q, "src/Bar.svelte", Language::Svelte);

    let real_button = exported(node(
        "component:src/lib/RealButton.svelte:RealButton:1",
        NodeKind::Component,
        "RealButton",
        "src/lib/RealButton.svelte::RealButton",
        "src/lib/RealButton.svelte",
        Language::Svelte,
        1,
        5,
    ));
    let bar = exported(node(
        "component:src/Bar.svelte:Bar:1",
        NodeKind::Component,
        "Bar",
        "src/Bar.svelte::Bar",
        "src/Bar.svelte",
        Language::Svelte,
        1,
        5,
    ));
    q.insert_nodes(&[real_button.clone(), bar.clone()]).unwrap();
    // The svelte extractor emits `<Foo />` markup usage as a `references` ref
    // from the consumer component node.
    q.insert_unresolved_refs_batch(&[uref(
        &bar.id,
        "Foo",
        EdgeKind::References,
        5,
        "src/Bar.svelte",
        Language::Svelte,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let callers = incoming(&q, &real_button.id, EdgeKind::References);
    assert!(source_files(&q, &callers).contains(&"src/Bar.svelte".to_string()));
}

#[test]
fn resolves_bare_directory_import_to_index_ts_629() {
    // `import { helper } from '.'` (and './') must map to the directory's
    // index.ts before the rename chase can run.
    let fx = Fx::new();
    let q = fx.q();
    fx.write("src/util.ts", "export function realHelper(): void {}\n");
    fx.write(
        "src/index.ts",
        "export { realHelper as helper } from './util';\n",
    );
    fx.write(
        "src/main.ts",
        "import { helper } from '.';\nexport function go(): void { helper(); }\n",
    );
    fx.write(
        "src/main2.ts",
        "import { helper } from './';\nexport function go2(): void { helper(); }\n",
    );
    for f in ["src/util.ts", "src/index.ts", "src/main.ts", "src/main2.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    let real_helper = exported(node(
        "func:src/util.ts:realHelper:1",
        NodeKind::Function,
        "realHelper",
        "src/util.ts::realHelper",
        "src/util.ts",
        Language::Typescript,
        1,
        1,
    ));
    let go_fn = exported(node(
        "func:src/main.ts:go:2",
        NodeKind::Function,
        "go",
        "src/main.ts::go",
        "src/main.ts",
        Language::Typescript,
        2,
        2,
    ));
    let go2_fn = exported(node(
        "func:src/main2.ts:go2:2",
        NodeKind::Function,
        "go2",
        "src/main2.ts::go2",
        "src/main2.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[real_helper.clone(), go_fn.clone(), go2_fn.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[
        uref(
            &go_fn.id,
            "helper",
            EdgeKind::Calls,
            2,
            "src/main.ts",
            Language::Typescript,
        ),
        uref(
            &go2_fn.id,
            "helper",
            EdgeKind::Calls,
            2,
            "src/main2.ts",
            Language::Typescript,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let callers = incoming(&q, &real_helper.id, EdgeKind::Calls);
    let files = source_files(&q, &callers);
    assert!(files.contains(&"src/main.ts".to_string()));
    assert!(files.contains(&"src/main2.ts".to_string()));
}

#[test]
fn resolves_workspace_package_subpath_barrel_629() {
    // bun/npm/pnpm workspace: `@scope/ui/widgets` → the `ui` member's
    // widgets/ subdir index, which re-exports a .svelte component under an
    // alias that defeats the name-matcher.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "package.json",
        "{\n  \"name\": \"root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}",
    );
    fx.write(
        "packages/ui/package.json",
        "{\n  \"name\": \"@scope/ui\",\n  \"version\": \"1.0.0\"\n}",
    );
    fx.write(
        "packages/ui/widgets/Widget.svelte",
        "<script lang=\"ts\">\n  export let label: string = '';\n</script>\n\n<button>{label}</button>\n",
    );
    fx.write(
        "packages/ui/widgets/index.ts",
        "export { default as Thing } from './Widget.svelte';\n",
    );
    fx.write(
        "app/App.svelte",
        "<script lang=\"ts\">\n  import { Thing } from '@scope/ui/widgets';\n</script>\n\n<Thing />\n",
    );
    fx.track(&q, "packages/ui/widgets/Widget.svelte", Language::Svelte);
    fx.track(&q, "packages/ui/widgets/index.ts", Language::Typescript);
    fx.track(&q, "app/App.svelte", Language::Svelte);

    let widget = exported(node(
        "component:packages/ui/widgets/Widget.svelte:Widget:1",
        NodeKind::Component,
        "Widget",
        "packages/ui/widgets/Widget.svelte::Widget",
        "packages/ui/widgets/Widget.svelte",
        Language::Svelte,
        1,
        5,
    ));
    let app = exported(node(
        "component:app/App.svelte:App:1",
        NodeKind::Component,
        "App",
        "app/App.svelte::App",
        "app/App.svelte",
        Language::Svelte,
        1,
        5,
    ));
    q.insert_nodes(&[widget.clone(), app.clone()]).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &app.id,
        "Thing",
        EdgeKind::References,
        5,
        "app/App.svelte",
        Language::Svelte,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let callers = incoming(&q, &widget.id, EdgeKind::References);
    assert!(source_files(&q, &callers).contains(&"app/App.svelte".to_string()));
}

#[test]
fn resolves_barrel_import_from_vue_sfc_script_block_629() {
    // The barrel renames `realRun` → `run` so only the import-chase (not the
    // name-matcher) can connect the call from the .vue consumer.
    let fx = Fx::new();
    let q = fx.q();
    fx.write("src/util.ts", "export function realRun(): void {}\n");
    fx.write("src/index.ts", "export { realRun as run } from './util';\n");
    fx.write(
        "src/App.vue",
        "<script lang=\"ts\">\nimport { run } from './';\nexport default { mounted() { run(); } };\n</script>\n<template><div/></template>\n",
    );
    fx.track(&q, "src/util.ts", Language::Typescript);
    fx.track(&q, "src/index.ts", Language::Typescript);
    fx.track(&q, "src/App.vue", Language::Vue);

    let real_run = exported(node(
        "func:src/util.ts:realRun:1",
        NodeKind::Function,
        "realRun",
        "src/util.ts::realRun",
        "src/util.ts",
        Language::Typescript,
        1,
        1,
    ));
    let app = exported(node(
        "component:src/App.vue:App:1",
        NodeKind::Component,
        "App",
        "src/App.vue::App",
        "src/App.vue",
        Language::Vue,
        1,
        5,
    ));
    q.insert_nodes(&[real_run.clone(), app.clone()]).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &app.id,
        "run",
        EdgeKind::Calls,
        3,
        "src/App.vue",
        Language::Vue,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let callers = incoming(&q, &real_run.id, EdgeKind::Calls);
    assert!(source_files(&q, &callers).contains(&"src/App.vue".to_string()));
}

#[test]
fn follows_vue_component_in_template_through_default_re_export_barrel_629() {
    // Vue analogue of the Svelte case: leaf is a `.vue` component
    // re-exported under an alias; the consumer uses it ONLY in markup.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/lib/Widget.vue",
        "<script setup lang=\"ts\">\ndefineProps<{ label?: string }>();\n</script>\n<template><button>x</button></template>\n",
    );
    fx.write(
        "src/lib/index.ts",
        "export { default as Thing } from './Widget.vue';\n",
    );
    fx.write(
        "src/App.vue",
        "<script setup lang=\"ts\">\nimport { Thing } from './lib';\n</script>\n<template>\n  <Thing />\n</template>\n",
    );
    fx.track(&q, "src/lib/Widget.vue", Language::Vue);
    fx.track(&q, "src/lib/index.ts", Language::Typescript);
    fx.track(&q, "src/App.vue", Language::Vue);

    let widget = exported(node(
        "component:src/lib/Widget.vue:Widget:1",
        NodeKind::Component,
        "Widget",
        "src/lib/Widget.vue::Widget",
        "src/lib/Widget.vue",
        Language::Vue,
        1,
        4,
    ));
    let app = exported(node(
        "component:src/App.vue:App:1",
        NodeKind::Component,
        "App",
        "src/App.vue::App",
        "src/App.vue",
        Language::Vue,
        1,
        6,
    ));
    q.insert_nodes(&[widget.clone(), app.clone()]).unwrap();
    // Template-tag extraction emits `<Thing />` as a `references` ref.
    q.insert_unresolved_refs_batch(&[uref(
        &app.id,
        "Thing",
        EdgeKind::References,
        5,
        "src/App.vue",
        Language::Vue,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let callers = incoming(&q, &widget.id, EdgeKind::References);
    assert!(source_files(&q, &callers).contains(&"src/App.vue".to_string()));
}

// =============================================================================
// C/C++ include resolution, end-to-end (resolution.test.ts)
// =============================================================================

#[test]
fn connects_include_to_real_header_file_via_include_dir_scan() {
    clear_cpp_include_dir_cache();
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "include/utils.h",
        "#ifndef UTILS_H\n#define UTILS_H\nint add(int, int);\n#endif\n",
    );
    fx.write(
        "src/main.cpp",
        "#include \"utils.h\"\n#include <vector>\nint main(){ return add(1,2); }\n",
    );
    fx.track(&q, "include/utils.h", Language::C);
    fx.track(&q, "src/main.cpp", Language::Cpp);

    // Extraction emits #include refs from the FILE node, with the include
    // path as the reference name and kind `imports`.
    let header_file = node(
        "file:include/utils.h",
        NodeKind::File,
        "utils.h",
        "include/utils.h",
        "include/utils.h",
        Language::C,
        1,
        4,
    );
    let main_file = node(
        "file:src/main.cpp",
        NodeKind::File,
        "main.cpp",
        "src/main.cpp",
        "src/main.cpp",
        Language::Cpp,
        1,
        3,
    );
    q.insert_nodes(&[header_file.clone(), main_file.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[
        uref(
            &main_file.id,
            "utils.h",
            EdgeKind::Imports,
            1,
            "src/main.cpp",
            Language::Cpp,
        ),
        uref(
            &main_file.id,
            "vector",
            EdgeKind::Imports,
            2,
            "src/main.cpp",
            Language::Cpp,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();
    clear_cpp_include_dir_cache();

    // The `#include "utils.h"` edge should target the real `include/utils.h`
    // file node — not a floating `import` node living inside main.cpp.
    let imports = outgoing(&q, &main_file.id, EdgeKind::Imports);
    let resolved_to_header = imports.iter().any(|e| e.target == header_file.id);
    assert!(
        resolved_to_header,
        "main.cpp → include/utils.h imports edge missing"
    );
    // `<vector>` should NOT produce a file edge — it's a stdlib header.
    let stdlib_edge = imports.iter().any(|e| {
        q.get_node_by_id(&e.target)
            .ok()
            .flatten()
            .map(|n| n.file_path.ends_with("vector"))
            .unwrap_or(false)
    });
    assert!(!stdlib_edge);
}

// =============================================================================
// object-literal method resolution, end-to-end (object-literal-methods.test.ts)
// =============================================================================

#[test]
fn resolves_callers_of_store_actions_across_files() {
    // Resolution half of the Zustand store case: extraction makes the
    // object-literal actions real `function` nodes; every call form reduces
    // to a bare-name ref that the exact-name matcher connects.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "package.json",
        "{\"name\":\"t\",\"dependencies\":{\"zustand\":\"^4\"}}\n",
    );
    fx.write(
        "store.ts",
        "import { create } from 'zustand'\ninterface S { fetchUser(): Promise<void>; reset(): void }\nexport const useStore = create<S>((set, get) => ({\n  fetchUser: async () => { get().reset() },\n  reset: () => set({}),\n}))\n",
    );
    fx.write(
        "caller.ts",
        "import { useStore } from './store'\nexport async function loginFlow() {\n  const { fetchUser } = useStore.getState()\n  await fetchUser()\n}\nexport function hardReset() {\n  useStore.getState().reset()\n}\n",
    );
    fx.track(&q, "store.ts", Language::Typescript);
    fx.track(&q, "caller.ts", Language::Typescript);

    let use_store = exported(node(
        "var:store.ts:useStore:3",
        NodeKind::Variable,
        "useStore",
        "store.ts::useStore",
        "store.ts",
        Language::Typescript,
        3,
        6,
    ));
    let fetch_user = node(
        "func:store.ts:fetchUser:4",
        NodeKind::Function,
        "fetchUser",
        "store.ts::fetchUser",
        "store.ts",
        Language::Typescript,
        4,
        4,
    );
    let reset = node(
        "func:store.ts:reset:5",
        NodeKind::Function,
        "reset",
        "store.ts::reset",
        "store.ts",
        Language::Typescript,
        5,
        5,
    );
    let login_flow = exported(node(
        "func:caller.ts:loginFlow:2",
        NodeKind::Function,
        "loginFlow",
        "caller.ts::loginFlow",
        "caller.ts",
        Language::Typescript,
        2,
        5,
    ));
    let hard_reset = exported(node(
        "func:caller.ts:hardReset:6",
        NodeKind::Function,
        "hardReset",
        "caller.ts::hardReset",
        "caller.ts",
        Language::Typescript,
        6,
        8,
    ));
    q.insert_nodes(&[
        use_store,
        fetch_user.clone(),
        reset.clone(),
        login_flow.clone(),
        hard_reset.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[
        // Destructured-then-bare call: loginFlow -> fetchUser
        uref(
            &login_flow.id,
            "fetchUser",
            EdgeKind::Calls,
            4,
            "caller.ts",
            Language::Typescript,
        ),
        // Chained getState() call reduces to bare `reset`
        uref(
            &hard_reset.id,
            "reset",
            EdgeKind::Calls,
            7,
            "caller.ts",
            Language::Typescript,
        ),
        // In-store sibling: fetchUser -> reset (get().reset())
        uref(
            &fetch_user.id,
            "reset",
            EdgeKind::Calls,
            4,
            "store.ts",
            Language::Typescript,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let fetch_user_callers: Vec<String> = incoming(&q, &fetch_user.id, EdgeKind::Calls)
        .iter()
        .filter_map(|e| q.get_node_by_id(&e.source).ok().flatten())
        .map(|n| n.name)
        .collect();
    assert!(fetch_user_callers.contains(&"loginFlow".to_string()));

    let reset_callers: Vec<String> = incoming(&q, &reset.id, EdgeKind::Calls)
        .iter()
        .filter_map(|e| q.get_node_by_id(&e.source).ok().flatten())
        .map(|n| n.name)
        .collect();
    assert!(reset_callers.contains(&"hardReset".to_string()));
    assert!(reset_callers.contains(&"fetchUser".to_string()));
}

// =============================================================================
// pr19-improvements.test.ts — resolution parts
// =============================================================================

#[test]
fn warm_caches_and_resolve_completes() {
    // "Resolution Warm Caches" — resolveReferences internally warms caches
    // and completes without error.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/a.ts",
        "export function myFunc(): void {}\nexport function otherFunc(): void { myFunc(); }\n",
    );
    fx.track(&q, "src/a.ts", Language::Typescript);

    let my_func = exported(node(
        "func:src/a.ts:myFunc:1",
        NodeKind::Function,
        "myFunc",
        "src/a.ts::myFunc",
        "src/a.ts",
        Language::Typescript,
        1,
        1,
    ));
    let other_func = exported(node(
        "func:src/a.ts:otherFunc:2",
        NodeKind::Function,
        "otherFunc",
        "src/a.ts::otherFunc",
        "src/a.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[my_func.clone(), other_func.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &other_func.id,
        "myFunc",
        EdgeKind::Calls,
        2,
        "src/a.ts",
        Language::Typescript,
    )])
    .unwrap();

    let resolver = fx.resolver();
    resolver.warm_caches();
    let result = resolver.resolve_and_persist_batched(None, None).unwrap();

    assert!(result.stats.total >= 1);
    assert_eq!(result.stats.resolved, 1);
    // The post-resolution callback-synthesis pass always records its count.
    assert!(result.stats.by_method.contains_key("callback-synthesis"));
    // Resolved row was deleted from unresolved_refs (metrics accuracy).
    assert_eq!(q.get_unresolved_references_count().unwrap(), 0);
}

#[test]
fn resolve_one_skips_builtins_best_candidate_api() {
    // "Best-Candidate Resolution" — TS only asserted resolveOne exists on
    // the prototype; in Rust that's a compile-time fact, so exercise the
    // built-in short-circuit through the public API instead.
    let fx = Fx::new();
    let resolver = fx.resolver();
    let r = UnresolvedRef {
        from_node_id: "caller".to_string(),
        reference_name: "console".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 1,
        column: 0,
        file_path: "src/a.ts".to_string(),
        language: Language::Typescript,
        candidates: None,
    };
    assert!(resolver.resolve_one(&r).is_none());
}

#[test]
fn resolve_one_skips_jvm_namespace_segments_but_not_types() {
    let fx = Fx::new();
    let q = fx.q();
    let caller = node(
        "method:src/Main.java:Main::run:1",
        NodeKind::Method,
        "run",
        "src/Main.java::Main::run",
        "src/Main.java",
        Language::Java,
        1,
        5,
    );
    let target = node(
        "class:src/com/example/Builder.java:Builder:1",
        NodeKind::Class,
        "Builder",
        "src/com/example/Builder.java::Builder",
        "src/com/example/Builder.java",
        Language::Java,
        1,
        20,
    );
    q.insert_nodes(&[caller, target]).unwrap();

    let resolver = fx.resolver();
    resolver.warm_caches();
    let package_ref = UnresolvedRef {
        from_node_id: "method:src/Main.java:Main::run:1".to_string(),
        reference_name: "org".to_string(),
        reference_kind: EdgeKind::References,
        line: 1,
        column: 0,
        file_path: "src/Main.java".to_string(),
        language: Language::Java,
        candidates: None,
    };
    assert!(resolver.resolve_one(&package_ref).is_none());

    let type_ref = UnresolvedRef {
        reference_name: "Builder".to_string(),
        ..package_ref
    };
    let resolved = resolver.resolve_one(&type_ref).expect("type resolves");
    assert_eq!(
        resolved.target_node_id,
        "class:src/com/example/Builder.java:Builder:1"
    );

    let external_call = UnresolvedRef {
        from_node_id: "method:src/Main.java:Main::run:1".to_string(),
        reference_name: "assertEquals".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 2,
        column: 0,
        file_path: "src/Main.java".to_string(),
        language: Language::Kotlin,
        candidates: None,
    };
    assert!(resolver.resolve_one(&external_call).is_none());

    let stdlib_import = UnresolvedRef {
        reference_name: "java.util.List".to_string(),
        reference_kind: EdgeKind::Imports,
        language: Language::Java,
        ..external_call.clone()
    };
    assert!(resolver.resolve_one(&stdlib_import).is_none());

    let stdlib_type = UnresolvedRef {
        reference_name: "String".to_string(),
        reference_kind: EdgeKind::References,
        language: Language::Java,
        ..external_call
    };
    assert!(resolver.resolve_one(&stdlib_type).is_none());
}

#[test]
fn resolve_one_keeps_project_classes_that_match_jvm_stdlib_names() {
    let fx = Fx::new();
    let q = fx.q();
    let caller = node(
        "method:src/Main.java:Main::run:1",
        NodeKind::Method,
        "run",
        "src/Main.java::Main::run",
        "src/Main.java",
        Language::Java,
        1,
        5,
    );
    let local_string = node(
        "class:src/String.java:String:1",
        NodeKind::Class,
        "String",
        "src/String.java::String",
        "src/String.java",
        Language::Java,
        1,
        20,
    );
    q.insert_nodes(&[caller, local_string]).unwrap();

    let resolver = fx.resolver();
    resolver.warm_caches();
    let type_ref = UnresolvedRef {
        from_node_id: "method:src/Main.java:Main::run:1".to_string(),
        reference_name: "String".to_string(),
        reference_kind: EdgeKind::References,
        line: 2,
        column: 0,
        file_path: "src/Main.java".to_string(),
        language: Language::Java,
        candidates: None,
    };
    let resolved = resolver
        .resolve_one(&type_ref)
        .expect("local String resolves");
    assert_eq!(resolved.target_node_id, "class:src/String.java:String:1");
}

#[test]
fn resolve_one_skips_c_stdlib_calls_unless_declared_locally() {
    let fx = Fx::new();
    let q = fx.q();
    let caller = node(
        "func:src/main.c:main:1",
        NodeKind::Function,
        "main",
        "src/main.c::main",
        "src/main.c",
        Language::C,
        1,
        5,
    );
    q.insert_nodes(&[caller]).unwrap();

    let resolver = fx.resolver();
    resolver.warm_caches();
    let stdlib_call = UnresolvedRef {
        from_node_id: "func:src/main.c:main:1".to_string(),
        reference_name: "printf".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 2,
        column: 0,
        file_path: "src/main.c".to_string(),
        language: Language::C,
        candidates: None,
    };
    assert!(resolver.resolve_one(&stdlib_call).is_none());

    let fx = Fx::new();
    let q = fx.q();
    let caller = node(
        "func:src/main.c:main:1",
        NodeKind::Function,
        "main",
        "src/main.c::main",
        "src/main.c",
        Language::C,
        1,
        5,
    );
    let local_printf = node(
        "func:src/main.c:printf:7",
        NodeKind::Function,
        "printf",
        "src/main.c::printf",
        "src/main.c",
        Language::C,
        7,
        9,
    );
    q.insert_nodes(&[caller, local_printf]).unwrap();

    let resolver = fx.resolver();
    resolver.warm_caches();
    let resolved = resolver
        .resolve_one(&stdlib_call)
        .expect("declared local printf resolves");
    assert_eq!(resolved.target_node_id, "func:src/main.c:printf:7");
}

// =============================================================================
// Progress reporting + resolve_all shape (resolver-internal contracts the TS
// suite exercised through cg.resolveReferences)
// =============================================================================

#[test]
fn resolve_all_reports_progress_and_stats() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write("src/x.ts", "export function target(): void {}\n");
    fx.track(&q, "src/x.ts", Language::Typescript);

    let target = exported(node(
        "func:src/x.ts:target:1",
        NodeKind::Function,
        "target",
        "src/x.ts::target",
        "src/x.ts",
        Language::Typescript,
        1,
        1,
    ));
    let caller = exported(node(
        "func:src/x.ts:caller:2",
        NodeKind::Function,
        "caller",
        "src/x.ts::caller",
        "src/x.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[target.clone(), caller.clone()]).unwrap();

    let refs = vec![
        uref(
            &caller.id,
            "target",
            EdgeKind::Calls,
            2,
            "src/x.ts",
            Language::Typescript,
        ),
        uref(
            &caller.id,
            "nothingHasThisName",
            EdgeKind::Calls,
            2,
            "src/x.ts",
            Language::Typescript,
        ),
    ];

    let resolver = fx.resolver();
    let mut calls: Vec<(usize, usize)> = Vec::new();
    let mut cb = |current: usize, total: usize| calls.push((current, total));
    let result = resolver.resolve_all(&refs, Some(&mut cb));

    assert_eq!(result.stats.total, 2);
    assert_eq!(result.stats.resolved, 1);
    assert_eq!(result.stats.unresolved, 1);
    assert_eq!(result.unresolved[0].reference_name, "nothingHasThisName");
    assert_eq!(*result.stats.by_method.get("exact-match").unwrap(), 1);
    // Final progress report is always (total, total).
    assert_eq!(calls.last(), Some(&(2, 2)));

    // Denormalized fields missing → resolver back-fills from the source node.
    let bare = UnresolvedReference {
        from_node_id: caller.id.clone(),
        reference_name: "target".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 2,
        column: 0,
        file_path: None,
        language: None,
        candidates: None,
    };
    let result = resolver.resolve_all(std::slice::from_ref(&bare), None);
    assert_eq!(result.stats.resolved, 1);
    assert_eq!(result.resolved[0].original.file_path, "src/x.ts");
    assert_eq!(result.resolved[0].original.language, Language::Typescript);
}
