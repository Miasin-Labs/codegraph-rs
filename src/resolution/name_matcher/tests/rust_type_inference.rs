//! Rust type inference behind `recv.m()` resolution, pinned directly: what a
//! written type path names ([`resolve_type`] and the [`RustType`] it
//! returns), and what a receiver local or chained receiver expression is
//! typed as ([`infer_rust_receiver_type`], [`infer_rust_chain_type`]).
//!
//! `rust_receiver.rs` covers the same code end to end through
//! `match_reference`; these tests sit a hop or two from it, so a change in
//! a lookup shows up here first.

use super::{Fixture, make_ref, node};
use crate::resolution::name_matcher::receiver::{
    RustType,
    infer_rust_chain_type,
    infer_rust_receiver_type,
    resolve_type,
};
use crate::resolution::types::UnresolvedRef;
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
        line,
    )
}

fn method(qualified: &str, file: &str, line: u32, signature: &str) -> Node {
    let mut node = rust_node(NodeKind::Method, qualified, file, line);
    node.signature = Some(signature.into());
    node
}

fn id(kind: NodeKind, qualified: &str, file: &str) -> String {
    format!("{}:{file}:{qualified}", kind.as_str())
}

/// A `use` declaration of `file`, as the index stores it.
fn use_decl(file: &str, text: &str, line: u32) -> Node {
    let mut node = rust_node(NodeKind::Import, &format!("use_{line}"), file, line);
    node.signature = Some(text.into());
    node
}

/// A reference written in [`FILE`] (no enclosing fn).
fn reference_here() -> UnresolvedRef {
    make_ref("m", EdgeKind::Calls, 1, FILE, Language::Rust)
}

/// `nodes` plus the source text of each `(file, text)`.
fn context_of(nodes: Vec<Node>, files: &[(&str, &str)]) -> Fixture {
    let mut fixture = Fixture::new(nodes);
    for (file, text) in files {
        fixture.files.insert((*file).into(), (*text).into());
    }
    fixture
}

/// The type `path` names when written in [`FILE`].
fn resolved(path: &str, fixture: &Fixture) -> RustType {
    resolve_type(path, FILE, &reference_here(), fixture)
}

fn graph_project() -> Vec<Node> {
    vec![
        rust_node(NodeKind::Struct, "Graph", "src/graph.rs", 1),
        method("Graph::new", "src/graph.rs", 5, "() -> Self"),
        method(
            "Graph::node",
            "src/graph.rs",
            10,
            "(&self, id: u32) -> Option<Node>",
        ),
        method("Graph::edges", "src/graph.rs", 15, "(&self) -> Vec<u32>"),
        method("Graph::walker", "src/graph.rs", 20, "(&self) -> Walker"),
        rust_node(NodeKind::Struct, "Walker", "src/walker.rs", 1),
        method("Walker::walk", "src/walker.rs", 5, "(&self)"),
        rust_node(NodeKind::Struct, "Node", "src/node.rs", 1),
        method("Node::kind", "src/node.rs", 5, "(&self) -> u8"),
    ]
}

// ---------------------------------------------------------------------------
// `resolve_type` and `RustType`
// ---------------------------------------------------------------------------

#[test]
fn a_bare_name_the_project_defines_is_a_project_type() {
    let fixture = context_of(graph_project(), &[]);
    let graph = resolved("Graph", &fixture);
    assert_eq!(graph.name, "Graph");
    assert!(graph.is_project_type(&fixture));
    assert_eq!(graph.external_crate(), None);
    // A type with no external path answers with its own name.
    assert_eq!(graph.external_path(), ["Graph".to_string()]);
}

#[test]
fn a_type_the_project_does_not_define_is_not_a_project_type() {
    let fixture = context_of(graph_project(), &[]);
    let unknown = resolved("Frobnicator", &fixture);
    assert_eq!(unknown.name, "Frobnicator");
    assert!(!unknown.is_project_type(&fixture));
    assert_eq!(unknown.external_crate(), None);
}

/// Only a Rust struct, enum, union or trait (or a type alias, below) makes a name a
/// project type: a fn of that name, or a class in another language, does
/// not.
#[test]
fn only_rust_type_definitions_make_a_project_type() {
    let mut nodes = vec![
        rust_node(NodeKind::Function, "Widget", "src/widget.rs", 1),
        node(
            "class:app.py:Gadget",
            NodeKind::Class,
            "Gadget",
            "Gadget",
            "app.py",
            Language::Python,
            1,
            2,
        ),
    ];
    for (kind, name) in [
        (NodeKind::Enum, "Colour"),
        (NodeKind::Union, "Bits"),
        (NodeKind::Trait, "Walk"),
    ] {
        nodes.push(rust_node(kind, name, "src/kinds.rs", 1));
    }
    let fixture = context_of(nodes, &[]);
    assert!(!resolved("Widget", &fixture).is_project_type(&fixture));
    assert!(!resolved("Gadget", &fixture).is_project_type(&fixture));
    for name in ["Colour", "Bits", "Walk"] {
        assert!(
            resolved(name, &fixture).is_project_type(&fixture),
            "{name} is a project type"
        );
    }
}

/// `use tree_sitter::Node;` makes `Node` the dependency's type even though
/// the project defines its own `Node`: it carries its crate and path, is no
/// project type, and runs none of the project `Node`'s methods.
#[test]
fn a_used_external_type_carries_its_crate_and_runs_no_project_method() {
    let mut nodes = graph_project();
    nodes.push(use_decl(FILE, "use tree_sitter::Node;", 1));
    let fixture = context_of(nodes, &[]);
    let node = resolved("Node", &fixture);
    assert_eq!(node.name, "Node");
    assert_eq!(node.external_crate(), Some("tree_sitter"));
    assert_eq!(node.external_path(), ["Node".to_string()]);
    assert!(!node.is_project_type(&fixture));

    let kinds = fixture_nodes_named(&fixture, "kind");
    assert_eq!(
        node.method("kind", &kinds, &reference_here(), &fixture),
        None
    );

    // Without the `use`, the same name is the project's `Node`.
    let project = context_of(graph_project(), &[]);
    let own = resolved("Node", &project);
    assert!(own.is_project_type(&project));
    let kinds = fixture_nodes_named(&project, "kind");
    assert_eq!(
        own.method("kind", &kinds, &reference_here(), &project)
            .map(|node| node.id.clone()),
        Some(id(NodeKind::Method, "Node::kind", "src/node.rs"))
    );
}

fn fixture_nodes_named(fixture: &Fixture, name: &str) -> Vec<Node> {
    use crate::resolution::types::ResolutionContext;
    fixture.get_nodes_by_name(name)
}

/// A path's first segment names the crate, the rest its path inside it, so
/// `regex::Regex` and `regex::bytes::Regex` stay apart.
#[test]
fn an_external_path_keeps_its_crate_and_in_crate_path() {
    let fixture = context_of(Vec::new(), &[]);
    let bytes = resolved("regex::bytes::Regex", &fixture);
    assert_eq!(bytes.name, "Regex");
    assert_eq!(bytes.external_crate(), Some("regex"));
    assert_eq!(
        bytes.external_path(),
        ["bytes".to_string(), "Regex".to_string()]
    );
    let top = resolved("regex::Regex", &fixture);
    assert_eq!(top.external_path(), ["Regex".to_string()]);
    assert_ne!(bytes, top);
    assert!(!bytes.is_project_type(&fixture));
}

/// `use serde_json as json;` then `json::Value`: the alias is followed to
/// the crate it names.
#[test]
fn a_renamed_crate_use_is_followed_to_the_crate() {
    let fixture = context_of(vec![use_decl(FILE, "use serde_json as json;", 1)], &[]);
    let value = resolved("json::Value", &fixture);
    assert_eq!(value.external_crate(), Some("serde_json"));
    assert_eq!(value.external_path(), ["Value".to_string()]);
}

/// An external type's methods are project methods only when no project
/// type shares its name (an extension trait implemented for `Regex`).
#[test]
fn an_external_type_runs_an_extension_method_only_without_a_project_namesake() {
    let nodes = vec![
        use_decl(FILE, "use regex::Regex;", 1),
        method("Regex::shout", "src/ext.rs", 3, "(&self) -> String"),
    ];
    let fixture = context_of(nodes.clone(), &[]);
    let regex = resolved("Regex", &fixture);
    assert_eq!(regex.external_crate(), Some("regex"));
    let shouts = fixture_nodes_named(&fixture, "shout");
    assert_eq!(
        regex
            .method("shout", &shouts, &reference_here(), &fixture)
            .map(|node| node.id.clone()),
        Some(id(NodeKind::Method, "Regex::shout", "src/ext.rs"))
    );

    let mut shadowed = nodes;
    shadowed.push(rust_node(NodeKind::Struct, "Regex", "src/regex.rs", 1));
    let fixture = context_of(shadowed, &[]);
    let regex = resolved("Regex", &fixture);
    let shouts = fixture_nodes_named(&fixture, "shout");
    assert_eq!(
        regex.method("shout", &shouts, &reference_here(), &fixture),
        None
    );
}

/// `crate::a::Graph` picks the method in module `a` over a same-named
/// type's elsewhere; a bare `Graph` with no `use` is placed by name only.
#[test]
fn a_crate_path_prefers_the_named_module() {
    let nodes = vec![
        rust_node(NodeKind::Struct, "Graph", "src/a.rs", 1),
        method("Graph::load", "src/a.rs", 3, "() -> Self"),
        rust_node(NodeKind::Struct, "Graph", "src/b.rs", 1),
        method("Graph::load", "src/b.rs", 3, "() -> Self"),
    ];
    let fixture = context_of(nodes, &[]);
    let loads = fixture_nodes_named(&fixture, "load");
    for (path, file) in [
        ("crate::a::Graph", "src/a.rs"),
        ("crate::b::Graph", "src/b.rs"),
    ] {
        let graph = resolved(path, &fixture);
        assert!(graph.is_project_type(&fixture), "{path}");
        assert_eq!(graph.external_crate(), None, "{path}");
        assert_eq!(
            graph
                .method("load", &loads, &reference_here(), &fixture)
                .map(|node| node.file_path.clone()),
            Some(file.to_string()),
            "{path}"
        );
    }
    // Placed by module, the two paths are different types.
    assert_ne!(
        resolved("crate::a::Graph", &fixture),
        resolved("crate::b::Graph", &fixture)
    );
}

/// `use crate::b::*;` puts module `b` first among same-named definitions.
#[test]
fn a_glob_import_prefers_the_globbed_module() {
    let nodes = vec![
        use_decl(FILE, "use crate::b::*;", 1),
        rust_node(NodeKind::Struct, "Graph", "src/a.rs", 1),
        method("Graph::load", "src/a.rs", 3, "() -> Self"),
        rust_node(NodeKind::Struct, "Graph", "src/b.rs", 1),
        method("Graph::load", "src/b.rs", 3, "() -> Self"),
    ];
    let fixture = context_of(nodes, &[]);
    let graph = resolved("Graph", &fixture);
    let loads = fixture_nodes_named(&fixture, "load");
    assert_eq!(
        graph
            .method("load", &loads, &reference_here(), &fixture)
            .map(|node| node.file_path.clone()),
        Some("src/b.rs".to_string())
    );
}

/// `type Db = Graph;` is followed to `Graph`, through alias chains, and an
/// alias naming an external type makes the alias external.
#[test]
fn type_aliases_are_followed_to_what_they_name() {
    let mut nodes = graph_project();
    nodes.extend([
        rust_node(NodeKind::TypeAlias, "Db", "src/alias.rs", 1),
        rust_node(NodeKind::TypeAlias, "Store", "src/alias.rs", 2),
        rust_node(NodeKind::TypeAlias, "Table", "src/alias.rs", 3),
        rust_node(NodeKind::TypeAlias, "Shared", "src/alias.rs", 4),
    ]);
    let source = "pub type Db = Graph;\npub type Store = Db;\npub type Table = std::collections::HashMap<String, u32>;\npub type Shared = std::sync::Arc<Graph>;\n";
    let fixture = context_of(nodes, &[("src/alias.rs", source)]);

    for alias in ["Db", "Store", "Shared"] {
        let ty = resolved(alias, &fixture);
        assert_eq!(ty.name, "Graph", "{alias}");
        assert!(ty.is_project_type(&fixture), "{alias}");
    }
    let table = resolved("Table", &fixture);
    assert_eq!(table.name, "HashMap");
    assert_eq!(table.external_crate(), Some("std"));
    assert_eq!(
        table.external_path(),
        ["collections".to_string(), "HashMap".to_string()]
    );
    assert!(!table.is_project_type(&fixture));
}

/// An alias to a tuple or slice is not followed (the alias is the type),
/// and an alias cycle ends after a bounded number of hops.
#[test]
fn structural_aliases_stay_and_alias_cycles_end() {
    let nodes = vec![
        rust_node(NodeKind::TypeAlias, "Pair", "src/alias.rs", 1),
        rust_node(NodeKind::TypeAlias, "Ping", "src/alias.rs", 2),
        rust_node(NodeKind::TypeAlias, "Pong", "src/alias.rs", 3),
    ];
    let source = "type Pair = (u32, u32);\ntype Ping = Pong;\ntype Pong = Ping;\n";
    let fixture = context_of(nodes, &[("src/alias.rs", source)]);

    let pair = resolved("Pair", &fixture);
    assert_eq!(pair.name, "Pair");
    assert!(pair.is_project_type(&fixture));

    let ping = resolved("Ping", &fixture);
    assert!(
        matches!(ping.name.as_str(), "Ping" | "Pong"),
        "{}",
        ping.name
    );
    assert_eq!(ping.external_crate(), None);
}

/// An alias whose source text is not available is not followed.
#[test]
fn an_alias_without_source_is_the_type_itself() {
    let mut nodes = graph_project();
    nodes.push(rust_node(NodeKind::TypeAlias, "Db", "src/alias.rs", 1));
    let fixture = context_of(nodes, &[]);
    let db = resolved("Db", &fixture);
    assert_eq!(db.name, "Db");
    assert!(db.is_project_type(&fixture));
}

// ---------------------------------------------------------------------------
// Inference at a call site
// ---------------------------------------------------------------------------

/// `source` as [`FILE`], whose one fn `caller` (or method `Graph::caller`
/// when `owner` is set) spans the whole file.
fn site_fixture(source: &str, owner: Option<&str>, extra: Vec<Node>) -> (Fixture, String) {
    let (start, header) = source
        .lines()
        .enumerate()
        .find(|(_, line)| line.trim_start().starts_with("fn caller"))
        .expect("source defines `caller`");
    let signature = &header[header.find('(').expect("params")..header.rfind('{').unwrap()];
    let (kind, qualified) = match owner {
        Some(owner) => (NodeKind::Method, format!("{owner}::caller")),
        None => (NodeKind::Function, "caller".to_string()),
    };
    let mut caller = rust_node(kind, &qualified, FILE, start as u32 + 1);
    caller.signature = Some(signature.trim().into());
    caller.end_line = source.lines().count() as u32;
    let caller_id = caller.id.clone();
    let mut nodes = graph_project();
    nodes.push(caller);
    nodes.extend(extra);
    (context_of(nodes, &[(FILE, source)]), caller_id)
}

/// A call reference at the place `source` writes `needle(`.
fn call_at(source: &str, needle: &str, caller: &str) -> UnresolvedRef {
    let (index, line) = source
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(&format!("{needle}(")))
        .expect("needle written in source");
    UnresolvedRef {
        from_node_id: caller.into(),
        line: index as u32 + 1,
        column: line.find(needle).expect("needle on its line") as u32,
        ..make_ref(needle, EdgeKind::Calls, 0, FILE, Language::Rust)
    }
}

fn receiver_type(source: &str, owner: Option<&str>, receiver: &str) -> Option<RustType> {
    let (fixture, caller) = site_fixture(source, owner, Vec::new());
    let reference = call_at(source, &format!("{receiver}.walk"), &caller);
    infer_rust_receiver_type(receiver, &reference, &fixture)
}

fn chain_type(source: &str, owner: Option<&str>, chain: &str, needle: &str) -> Option<RustType> {
    let (fixture, caller) = site_fixture(source, owner, Vec::new());
    let reference = call_at(source, needle, &caller);
    infer_rust_chain_type(chain, &reference, &fixture)
}

#[test]
fn a_typed_parameter_is_its_written_type() {
    let source = "fn caller(walker: &Walker) {\n    walker.walk();\n}\n";
    let ty = receiver_type(source, None, "walker").expect("typed parameter");
    assert_eq!(ty.name, "Walker");
}

/// Inside `impl Graph`, a local annotated `Self` is a `Graph`.
#[test]
fn self_is_the_enclosing_methods_owner() {
    let source = "fn caller(&self) {\n    let copy: Self = make();\n    copy.walk();\n}\n";
    let ty = receiver_type(source, Some("Graph"), "copy").expect("`Self` is the owner");
    assert_eq!(ty.name, "Graph");
    let (fixture, _) = site_fixture(source, Some("Graph"), Vec::new());
    assert!(ty.is_project_type(&fixture));

    // Outside any impl, `Self` means nothing.
    assert_eq!(receiver_type(source, None, "copy"), None);
}

/// A generic parameter `T` is unknown, not a type: it never becomes a
/// project type, so no project method is looked up on it.
#[test]
fn a_generic_parameter_is_not_a_project_type() {
    let source = "fn caller<T: Walk>(item: T) {\n    item.walk();\n}\n";
    let (fixture, _) = site_fixture(source, None, Vec::new());
    let ty = receiver_type(source, None, "item");
    assert!(
        ty.as_ref().is_none_or(|ty| !ty.is_project_type(&fixture)),
        "{ty:?}"
    );
}

/// The nearest binding decides: a `for` item over a slice is the slice's
/// element, and a nearer binding that does not spell a type shadows an
/// earlier typed one, so the search ends with no answer.
#[test]
fn the_nearest_binding_shadows_an_earlier_typed_one() {
    let source = "fn caller(walker: &Walker, items: &[u32]) {\n    for walker in items {\n        walker.walk();\n    }\n}\n";
    let ty = receiver_type(source, None, "walker").expect("slice item");
    assert_eq!(ty.name, "u32");

    let source = "fn caller(walker: &Walker) {\n    for walker in unknown() {\n        walker.walk();\n    }\n}\n";
    assert_eq!(receiver_type(source, None, "walker"), None);
}

#[test]
fn a_constructed_local_is_the_constructed_type() {
    let source = "fn caller() {\n    let made = Graph::new();\n    made.walk();\n}\n";
    let ty = receiver_type(source, None, "made").expect("`Graph::new()` returns Self");
    assert_eq!(ty.name, "Graph");
}

/// Each link of a chain is typed on the type reached so far: a project
/// method's declared return type, `Self` meaning its owner.
#[test]
fn a_chain_is_typed_link_by_link() {
    let source = "fn caller(graph: &Graph) {\n    graph.walker().walk();\n}\n";
    let ty = chain_type(source, None, "graph.walker()", "walk").expect("typed chain");
    assert_eq!(ty.name, "Walker");

    let source = "fn caller() {\n    Graph::new().walker().walk();\n}\n";
    let ty = chain_type(source, None, "Graph::new().walker()", "walk").expect("typed chain");
    assert_eq!(ty.name, "Walker");

    // `(graph)` is `graph`.
    let source = "fn caller(graph: &Graph) {\n    (graph).walker().walk();\n}\n";
    let ty = chain_type(source, None, "(graph).walker()", "walk").expect("paren head");
    assert_eq!(ty.name, "Walker");
}

/// `self` heads a chain as the enclosing method's owner.
#[test]
fn a_self_headed_chain_starts_at_the_owner() {
    let source = "fn caller(&self) {\n    self.walker().walk();\n}\n";
    let ty = chain_type(source, Some("Graph"), "self.walker()", "walk").expect("self chain");
    assert_eq!(ty.name, "Walker");
}

/// The first link whose type is not known ends the chain: nothing after it
/// is guessed.
#[test]
fn an_unknown_link_ends_the_chain() {
    let source = "fn caller(graph: &Graph) {\n    graph.missing().walker().walk();\n}\n";
    assert_eq!(
        chain_type(source, None, "graph.missing().walker()", "walk"),
        None
    );
    // An unknown head ends it too.
    let source = "fn caller() {\n    mystery.walker().walk();\n}\n";
    assert_eq!(chain_type(source, None, "mystery.walker()", "walk"), None);
}

/// A spelled link (`.collect::<T>()`, `as T`) fixes the type whatever the
/// links before it were.
#[test]
fn a_spelled_link_fixes_the_chain_type() {
    let source = "fn caller(items: &[u32]) {\n    items.iter().collect::<Walker>().walk();\n}\n";
    let ty =
        chain_type(source, None, "items.iter().collect::<Walker>()", "walk").expect("spelled type");
    assert_eq!(ty.name, "Walker");
}

/// `Option<Node>` unwrapped by `?`/`unwrap()` is the project's `Node`; a
/// project method returning a std type ends in that std type, which is no
/// project type.
#[test]
fn std_wrappers_open_and_std_types_are_not_project_types() {
    let source = "fn caller(graph: &Graph) {\n    graph.node(1).unwrap().kind();\n}\n";
    let (fixture, caller) = site_fixture(source, None, Vec::new());
    let reference = call_at(source, "kind", &caller);
    let ty = infer_rust_chain_type("graph.node(1).unwrap()", &reference, &fixture)
        .expect("unwrapped Option<Node>");
    assert_eq!(ty.name, "Node");
    assert!(ty.is_project_type(&fixture));

    let source = "fn caller(graph: &Graph) {\n    graph.edges().len();\n}\n";
    let (fixture, caller) = site_fixture(source, None, Vec::new());
    let reference = call_at(source, "len", &caller);
    let ty = infer_rust_chain_type("graph.edges()", &reference, &fixture);
    assert!(
        ty.as_ref().is_none_or(|ty| !ty.is_project_type(&fixture)),
        "{ty:?}"
    );
}
