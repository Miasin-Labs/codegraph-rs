//! Rust `recv.method(..)` and `Type::assoc(..)` calls: resolved on the type
//! the receiver or path names, never guessed onto a same-named method of
//! another type.

use super::{Fixture, make_ref, match_reference, node};
use crate::resolution::types::{ResolvedBy, UnresolvedRef};
use crate::types::{EdgeKind, Language, Node, NodeKind};

const FILE: &str = "src/main.rs";

fn rust_node(kind: NodeKind, qualified: &str, file: &str, line: u32) -> Node {
    let name = qualified.rsplit("::").next().unwrap_or(qualified);
    node(
        &format!("{}:{file}:{qualified}", kind.as_str()),
        kind,
        name,
        qualified,
        file,
        Language::Rust,
        line,
        line + 2,
    )
}

fn with_signature(mut node: Node, signature: &str) -> Node {
    node.signature = Some(signature.into());
    node
}

fn method(qualified: &str, file: &str, line: u32, signature: &str) -> Node {
    with_signature(
        rust_node(NodeKind::Method, qualified, file, line),
        signature,
    )
}

fn id(kind: NodeKind, qualified: &str, file: &str) -> String {
    format!("{}:{file}:{qualified}", kind.as_str())
}

/// The types and methods the call sites below may or may not mean.
fn project() -> Vec<Node> {
    vec![
        rust_node(NodeKind::Struct, "FrontierIter", "src/frontier.rs", 1),
        method(
            "FrontierIter::next",
            "src/frontier.rs",
            10,
            "(&mut self) -> Option<u32>",
        ),
        rust_node(NodeKind::Struct, "Graph", "src/graph.rs", 1),
        method("Graph::new", "src/graph.rs", 5, "() -> Self"),
        method(
            "Graph::open",
            "src/graph.rs",
            10,
            "(path: &Path) -> io::Result<Self>",
        ),
        method(
            "Graph::get",
            "src/graph.rs",
            15,
            "(&self, id: u32) -> Option<&u32>",
        ),
        method(
            "Graph::get_outgoing_edges",
            "src/graph.rs",
            20,
            "(&self, id: u32) -> Vec<u32>",
        ),
        rust_node(NodeKind::Struct, "Cache", "src/cache.rs", 1),
        method(
            "Cache::get",
            "src/cache.rs",
            5,
            "(&self, key: &str) -> Option<&u32>",
        ),
        rust_node(NodeKind::Struct, "OrderedNodeMap", "src/map.rs", 1),
        method("OrderedNodeMap::new", "src/map.rs", 5, "() -> Self"),
        rust_node(NodeKind::Struct, "IndexOptions", "src/options.rs", 1),
        rust_node(NodeKind::Struct, "ContextOptions", "src/options.rs", 10),
        method(
            "ContextOptions::default",
            "src/options.rs",
            12,
            "() -> Self",
        ),
        rust_node(NodeKind::Struct, "Node", "src/types.rs", 1),
        rust_node(NodeKind::Struct, "Walker", "src/walker.rs", 1),
        method("Walker::walk", "src/walker.rs", 5, "(&self)"),
        method("Walker::exit", "src/walker.rs", 10, "(&self)"),
        rust_node(NodeKind::Struct, "MatchQuality", "src/quality.rs", 1),
        method(
            "MatchQuality::is_match",
            "src/quality.rs",
            5,
            "(&self) -> bool",
        ),
        rust_node(NodeKind::Trait, "Target", "src/target.rs", 1),
        method("Target::install", "src/target.rs", 3, "(&self)"),
        rust_node(NodeKind::Struct, "ClaudeTarget", "src/target.rs", 10),
        method("ClaudeTarget::install", "src/target.rs", 12, "(&self)"),
        with_signature(
            rust_node(NodeKind::Function, "find_target", "src/target.rs", 20),
            "(id: &str) -> Option<Box<dyn Target>>",
        ),
    ]
}

/// A `use` declaration of `file`, as the index stores it.
fn use_decl(file: &str, text: &str, line: u32) -> Node {
    with_signature(
        rust_node(NodeKind::Import, &format!("use_{line}"), file, line),
        text,
    )
}

/// The project plus `source` as `file`, whose one fn `caller` runs from its
/// `fn` line to the end of the file.
fn fixture_with(file: &str, source: &str, extra: Vec<Node>) -> Fixture {
    let (start, header) = source
        .lines()
        .enumerate()
        .find(|(_, line)| line.trim_start().starts_with("fn caller"))
        .expect("source defines `caller`");
    let signature =
        &header[header.find('(').expect("params")..header.rfind('{').unwrap_or(header.len())];
    let mut caller = with_signature(
        rust_node(NodeKind::Function, "caller", file, start as u32 + 1),
        signature.trim(),
    );
    caller.end_line = source.lines().count() as u32;
    let mut nodes = project();
    nodes.push(caller);
    nodes.extend(extra);
    let mut fixture = Fixture::new(nodes);
    fixture.files.insert(file.into(), source.into());
    fixture
}

/// The call named `needle` (`chars.next`) at the place `source` writes it.
fn call(file: &str, source: &str, needle: &str) -> UnresolvedRef {
    let (index, line) = source
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(&format!("{needle}(")))
        .expect("needle written in source");
    UnresolvedRef {
        line: index as u32 + 1,
        column: line.find(needle).expect("needle on its line") as u32,
        ..make_ref(needle, EdgeKind::Calls, 0, file, Language::Rust)
    }
}

fn target(fixture: &Fixture, source: &str, needle: &str) -> Option<String> {
    match_reference(&call(FILE, source, needle), fixture).map(|resolved| resolved.target_node_id)
}

/// `chars.next()` runs `Chars::next`; the project's only `next` is not a
/// candidate for a receiver whose type is not known.
#[test]
fn std_method_on_an_unknown_receiver_stays_unresolved() {
    let source = "fn caller(s: &str) {\n    let mut chars = s.chars();\n    chars.next();\n}\n";
    let fixture = fixture_with(FILE, source, Vec::new());
    assert_eq!(target(&fixture, source, "chars.next"), None);
}

#[test]
fn typed_parameters_resolve_on_their_type() {
    let source = "fn caller(graph: &mut Graph, cache: Box<Cache>) {\n    graph.get(1);\n    cache.get(\"k\");\n}\n";
    let fixture = fixture_with(FILE, source, Vec::new());
    let resolved = match_reference(&call(FILE, source, "graph.get"), &fixture)
        .expect("`graph: &mut Graph` resolves");
    assert_eq!(
        resolved.target_node_id,
        id(NodeKind::Method, "Graph::get", "src/graph.rs")
    );
    assert_eq!(resolved.resolved_by, ResolvedBy::InstanceMethod);
    assert_eq!(resolved.confidence, 0.9);
    assert_eq!(
        target(&fixture, source, "cache.get"),
        Some(id(NodeKind::Method, "Cache::get", "src/cache.rs"))
    );
}

#[test]
fn constructed_locals_resolve_on_the_constructed_type() {
    let source = "fn caller(path: &Path) -> io::Result<()> {\n    let made = Graph::new();\n    made.get(1);\n    let opened = Graph::open(path)?;\n    opened.get(2);\n    let literal = Graph {\n        nodes: vec![],\n    };\n    literal.get(3);\n    let borrowed = &literal;\n    borrowed.get(4);\n    let annotated: Graph = build();\n    annotated.get(5);\n    let deferred;\n    deferred = Graph::new();\n    deferred.get(6);\n    Ok(())\n}\n";
    let fixture = fixture_with(FILE, source, Vec::new());
    for receiver in [
        "made",
        "opened",
        "literal",
        "borrowed",
        "annotated",
        "deferred",
    ] {
        assert_eq!(
            target(&fixture, source, &format!("{receiver}.get")),
            Some(id(NodeKind::Method, "Graph::get", "src/graph.rs")),
            "`{receiver}` is a `Graph`"
        );
    }
}

/// `if let Some(t) = find_target(id)`: the callee's `Option<Box<dyn Target>>`
/// unwraps and derefs to the trait.
#[test]
fn unwrapping_bindings_read_the_wrapped_type() {
    let source = "fn caller(id: &str) {\n    let Some(target) = find_target(id) else {\n        return;\n    };\n    target.install();\n    if let Some(other) = find_target(id) {\n        other.install();\n    }\n}\n";
    let fixture = fixture_with(FILE, source, Vec::new());
    for needle in ["target.install", "other.install"] {
        assert_eq!(
            target(&fixture, source, needle),
            Some(id(NodeKind::Method, "Target::install", "src/target.rs")),
            "{needle}"
        );
    }
}

/// A type from outside the project runs none of the project's methods,
/// not even a project-unique name like `walk`, and not even when the
/// project has its own type of the same name (`tree_sitter::Node`).
#[test]
fn external_receivers_do_not_run_project_methods() {
    let source = "use std::collections::HashMap;\nuse tree_sitter::Node;\n\nfn caller(node: Node) {\n    let map: HashMap<String, u32> = HashMap::new();\n    map.get(\"k\");\n    let text = String::from(\"k\");\n    text.get(0);\n    node.walk();\n    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(\"a\").unwrap());\n    RE.is_match(\"a\");\n}\n";
    let uses = vec![
        use_decl(FILE, "use std::collections::HashMap;", 1),
        use_decl(FILE, "use tree_sitter::Node;", 2),
    ];
    let fixture = fixture_with(FILE, source, uses);
    for needle in ["map.get", "text.get", "node.walk", "RE.is_match"] {
        assert_eq!(target(&fixture, source, needle), None, "{needle}");
    }
}

/// A nearer binding that does not spell the type hides the typed one.
#[test]
fn shadowing_bindings_end_the_search() {
    let source = "fn caller(graph: &Graph, cache: &Cache, items: Vec<u32>) {\n    let graph = graph.nodes();\n    graph.get(1);\n    for cache in items {\n        cache.get(\"k\");\n    }\n}\n";
    let fixture = fixture_with(FILE, source, Vec::new());
    assert_eq!(target(&fixture, source, "graph.get"), None);
    assert_eq!(target(&fixture, source, "cache.get"), None);
}

/// Project-specific names on an unknown receiver keep their fallbacks.
#[test]
fn project_specific_methods_still_reach_the_fallbacks() {
    let source =
        "fn caller() {\n    let service = make();\n    service.get_outgoing_edges(1);\n}\n";
    let fixture = fixture_with(FILE, source, Vec::new());
    assert_eq!(
        target(&fixture, source, "service.get_outgoing_edges"),
        Some(id(
            NodeKind::Method,
            "Graph::get_outgoing_edges",
            "src/graph.rs"
        ))
    );
}

/// `Type::assoc(..)` names its type: a type the project does not define
/// resolves to nothing, whatever same-named method shares a word with it.
#[test]
fn type_paths_resolve_only_on_the_named_project_type() {
    let source = "use crate::graph::Graph as G;\n\nfn caller() {\n    HashMap::new();\n    process::exit(1);\n    IndexOptions::default();\n    G::new();\n}\n";
    let uses = vec![use_decl(FILE, "use crate::graph::Graph as G;", 1)];
    let fixture = fixture_with(FILE, source, uses);
    for needle in ["HashMap::new", "process::exit", "IndexOptions::default"] {
        assert_eq!(target(&fixture, source, needle), None, "{needle}");
    }
    assert_eq!(
        target(&fixture, source, "G::new"),
        Some(id(NodeKind::Method, "Graph::new", "src/graph.rs"))
    );
}

/// Two integration-test fixtures define `Fx`; the one the file glob-imports
/// (and shares a directory with) is meant.
#[test]
fn same_named_types_resolve_to_the_nearest_definition() {
    let file = "tests/resolution/case.rs";
    let source = "use crate::fixture::*;\n\nfn caller() {\n    let fx = Fx::new();\n    fx.write(\"a\", \"b\");\n}\n";
    let extra = vec![
        use_decl(file, "use crate::fixture::*;", 1),
        rust_node(NodeKind::Struct, "Fx", "tests/common/jvm.rs", 1),
        method("Fx::new", "tests/common/jvm.rs", 3, "() -> Self"),
        method(
            "Fx::write",
            "tests/common/jvm.rs",
            6,
            "(&self, rel: &str, content: &str)",
        ),
        rust_node(NodeKind::Struct, "Fx", "tests/resolution/fixture.rs", 1),
        method("Fx::new", "tests/resolution/fixture.rs", 3, "() -> Fx"),
        method(
            "Fx::write",
            "tests/resolution/fixture.rs",
            6,
            "(&self, rel: &str, content: &str)",
        ),
    ];
    let fixture = fixture_with(file, source, extra);
    let resolved = match_reference(&call(file, source, "fx.write"), &fixture)
        .map(|resolved| resolved.target_node_id);
    assert_eq!(
        resolved,
        Some(id(
            NodeKind::Method,
            "Fx::write",
            "tests/resolution/fixture.rs"
        ))
    );
}
