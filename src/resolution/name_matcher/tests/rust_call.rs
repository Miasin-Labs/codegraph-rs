//! Rust calls decided by their syntax: a bare call never runs a method, a
//! local's namesake, or a prelude value's namesake; a method call runs a
//! method; and no std method name is guessed onto a receiver whose type is
//! unknown.

use super::{Fixture, make_ref, match_reference, node};
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{EdgeKind, Language, Node, NodeKind, receiver_dropped_metadata};

const FILE: &str = "src/run.rs";

fn id(kind: NodeKind, qualified: &str, file: &str) -> String {
    format!("{}:{file}:{qualified}", kind.as_str())
}

fn rust_node(kind: NodeKind, qualified: &str, file: &str, line: u32) -> Node {
    let name = qualified.rsplit("::").next().unwrap_or(qualified);
    node(
        &id(kind, qualified, file),
        kind,
        name,
        qualified,
        file,
        Language::Rust,
        line,
        line + 2,
    )
}

/// A `use` declaration of `file`, as the index stores it.
fn use_decl(file: &str, text: &str, line: u32) -> Node {
    let mut import = rust_node(NodeKind::Import, &format!("use_{line}"), file, line);
    import.signature = Some(text.into());
    import
}

/// Symbols elsewhere in the project named like the calls below.
fn project() -> Vec<Node> {
    vec![
        rust_node(NodeKind::Method, "Daemon::stop", "src/daemon.rs", 5),
        rust_node(NodeKind::Function, "stop", "src/stop.rs", 1),
        rust_node(NodeKind::Function, "probe", "src/probe.rs", 1),
        rust_node(NodeKind::Function, "helper", "src/helpers.rs", 1),
        rust_node(NodeKind::Method, "Guard::drop", "src/guard.rs", 5),
        rust_node(NodeKind::Function, "drop", "src/util.rs", 1),
        rust_node(NodeKind::EnumMember, "Anchor::Ok", "src/anchor.rs", 3),
        rust_node(NodeKind::TypeAlias, "Location::Err", "src/location.rs", 3),
        rust_node(NodeKind::EnumMember, "Kind::Leaf", "src/kind.rs", 3),
        rust_node(
            NodeKind::Method,
            "ModuleLocation::parent",
            "src/layout.rs",
            5,
        ),
        rust_node(NodeKind::Struct, "TypedEdge", "src/edge.rs", 1),
        rust_node(NodeKind::Method, "TypedEdge::into_inner", "src/edge.rs", 5),
        rust_node(NodeKind::Field, "Graph::outgoing", "src/graph.rs", 3),
        rust_node(NodeKind::Function, "outgoing", "src/walk.rs", 1),
        rust_node(
            NodeKind::Method,
            "Graph::get_outgoing_edges",
            "src/graph.rs",
            10,
        ),
    ]
}

/// The project plus `source` as [`FILE`], whose fns are read from their
/// `fn` lines: each runs to the line before the next top-level item, and
/// one inside an `impl` block is a method of it.
fn project_with(source: &str, extra: Vec<Node>) -> Fixture {
    let lines: Vec<&str> = source.lines().collect();
    let mut nodes = project();
    let mut owner: Option<&str> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("impl ") {
            owner = rest.split_whitespace().next();
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("fn ") else {
            continue;
        };
        let name = &rest[..rest.find(['(', '<']).expect("fn params")];
        let indent = line.len() - trimmed.len();
        // The fn's last line is the one closing it at its own indentation.
        let end = lines[index..]
            .iter()
            .position(|later| later.len() > indent && later[indent..].starts_with('}'))
            .map_or(index, |offset| index + offset);
        let end = if rest.trim_end().ends_with('}') {
            index
        } else {
            end
        };
        let (kind, qualified) = match owner {
            Some(owner) if indent > 0 => (NodeKind::Method, format!("{owner}::{name}")),
            _ => (NodeKind::Function, name.to_string()),
        };
        let mut function = rust_node(kind, &qualified, FILE, index as u32 + 1);
        function.end_line = end as u32 + 1;
        let params = &rest[rest.find('(').unwrap()..];
        function.signature = Some(
            params[..params.rfind('{').unwrap_or(params.len())]
                .trim()
                .into(),
        );
        nodes.push(function);
        if indent == 0 {
            owner = None;
        }
    }
    nodes.extend(extra);
    let mut fixture = Fixture::new(nodes);
    fixture.files.insert(FILE.into(), source.into());
    fixture
}

/// The call of `name` (`probe`, `chars.next`) written on the first line of
/// `source` containing `marker`, as extraction records it: at the start of
/// the call, from the innermost fn around it.
fn call(fixture: &Fixture, source: &str, marker: &str, name: &str) -> UnresolvedRef {
    at_call(fixture, source, marker, name, &format!("{name}("))
}

/// `self.name(…)` on the line holding `marker`: a bare `name` anchored at
/// `self`.
fn self_call(fixture: &Fixture, source: &str, marker: &str, name: &str) -> UnresolvedRef {
    at_call(fixture, source, marker, name, "self.")
}

fn at_call(
    fixture: &Fixture,
    source: &str,
    marker: &str,
    name: &str,
    written: &str,
) -> UnresolvedRef {
    let (index, line) = source
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(marker))
        .expect("marker written in source");
    let line_number = index as u32 + 1;
    let column = line.find(written).expect("call on the marked line") as u32;
    let caller = fixture
        .get_nodes_in_file(FILE)
        .into_iter()
        .filter(|node| {
            matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && node.start_line <= line_number
                && node.end_line >= line_number
        })
        .max_by_key(|node| node.start_line)
        .expect("call inside a fn");
    UnresolvedRef {
        from_node_id: caller.id,
        line: line_number,
        column,
        ..make_ref(name, EdgeKind::Calls, 0, FILE, Language::Rust)
    }
}

fn target(fixture: &Fixture, reference: &UnresolvedRef) -> Option<String> {
    match_reference(reference, fixture).map(|resolved| resolved.target_node_id)
}

/// `a.b().name()`, recorded as its bare method name.
fn dropped(name: &str) -> UnresolvedRef {
    UnresolvedRef {
        metadata: Some(receiver_dropped_metadata()),
        ..make_ref(name, EdgeKind::Calls, 5, "src/main.rs", Language::Rust)
    }
}

#[test]
fn bare_calls_never_run_methods() {
    let source = "\
fn caller() {
    helper(); // free
    halt(); // method only
}
impl Other {
    fn helper(&self) {}
}
";
    let halt = rust_node(NodeKind::Method, "Daemon::halt", "src/daemon.rs", 9);
    let fixture = project_with(source, vec![halt]);
    // The same-file method `Other::helper` is nearer, but a bare call runs
    // the free fn.
    assert_eq!(
        target(&fixture, &call(&fixture, source, "// free", "helper")),
        Some(id(NodeKind::Function, "helper", "src/helpers.rs"))
    );
    // Only a method is named `halt`.
    assert_eq!(
        target(&fixture, &call(&fixture, source, "// method only", "halt")),
        None
    );
}

#[test]
fn bare_calls_of_locals_resolve_to_nothing() {
    let source = "\
fn caller(stop: &dyn Fn() -> bool) {
    if stop() {
        return;
    }
    let probe = |n: u8| n + 1;
    probe(1);
    for helper in helpers {
        helper(); // loop variable
    }
}
";
    let fixture = project_with(source, Vec::new());
    for (marker, name) in [
        ("if stop", "stop"),
        ("probe(1)", "probe"),
        ("// loop variable", "helper"),
    ] {
        assert_eq!(
            target(&fixture, &call(&fixture, source, marker, name)),
            None,
            "{name}() names a local"
        );
    }
}

#[test]
fn locals_out_of_scope_do_not_shadow() {
    let source = "\
fn caller(root: &str) {
    {
        let probe = 1;
    }
    probe(2);
    let helper = helper(3);
    for stop in stops {
        run(stop);
    }
    stop();
    let lens = names.iter().map(|outgoing| outgoing.len()).count();
    outgoing(lens);
}
";
    let fixture = project_with(source, Vec::new());
    for (marker, name, expected) in [
        ("probe(2)", "probe", "src/probe.rs"),
        ("let helper", "helper", "src/helpers.rs"),
        ("    stop();", "stop", "src/stop.rs"),
        ("outgoing(lens)", "outgoing", "src/walk.rs"),
    ] {
        assert_eq!(
            target(&fixture, &call(&fixture, source, marker, name)),
            Some(id(NodeKind::Function, name, expected)),
            "{name}() is out of the local's scope"
        );
    }
}

#[test]
fn nested_fns_are_callable_bare_inside_their_fn() {
    let source = "\
impl Resolver {
    fn rank(&self) -> u8 {
        fn helper(kind: u8) -> u8 { kind }
        helper(1) // nested
    }
}
fn caller() {
    helper(2); // outside
}
";
    let fixture = project_with(source, Vec::new());
    assert_eq!(
        target(&fixture, &call(&fixture, source, "// nested", "helper")),
        Some(id(NodeKind::Method, "Resolver::helper", FILE))
    );
    assert_eq!(
        target(&fixture, &call(&fixture, source, "// outside", "helper")),
        Some(id(NodeKind::Function, "helper", "src/helpers.rs"))
    );
}

#[test]
fn self_calls_run_methods_never_fields() {
    let source = "\
impl Graph {
    fn walk(&self) {
        self.outgoing();
    }
}
";
    let method = rust_node(NodeKind::Method, "Graph::outgoing", "src/graph/walk.rs", 20);
    let fixture = project_with(source, vec![method]);
    assert_eq!(
        target(
            &fixture,
            &self_call(&fixture, source, "self.outgoing", "outgoing")
        ),
        Some(id(NodeKind::Method, "Graph::outgoing", "src/graph/walk.rs"))
    );
}

#[test]
fn prelude_values_stay_std() {
    let source = "\
fn caller(guard: Guard) -> Result<u8, String> {
    drop(guard);
    if guard.failed {
        return Err(String::new());
    }
    Ok(1)
}
";
    let fixture = project_with(source, Vec::new());
    for (marker, name) in [("drop(guard)", "drop"), ("Err(", "Err"), ("Ok(1)", "Ok")] {
        assert_eq!(
            target(&fixture, &call(&fixture, source, marker, name)),
            None,
            "{name} is the prelude's"
        );
    }
}

#[test]
fn prelude_names_resolve_when_the_file_brings_them_into_scope() {
    let source = "\
fn drop(value: u8) {}
fn caller() -> Anchor {
    drop(1);
    Ok(2)
}
";
    let glob = use_decl(FILE, "use crate::anchor::Anchor::*;", 1);
    let fixture = project_with(source, vec![glob]);
    assert_eq!(
        target(&fixture, &call(&fixture, source, "drop(1)", "drop")),
        Some(id(NodeKind::Function, "drop", FILE))
    );
    assert_eq!(
        target(&fixture, &call(&fixture, source, "Ok(2)", "Ok")),
        Some(id(NodeKind::EnumMember, "Anchor::Ok", "src/anchor.rs"))
    );
}

#[test]
fn tuple_variants_need_a_use() {
    let without = "fn caller() {\n    Leaf(1);\n}\n";
    let fixture_without = project_with(without, Vec::new());
    assert_eq!(
        target(
            &fixture_without,
            &call(&fixture_without, without, "Leaf(1)", "Leaf")
        ),
        None
    );
    let named = use_decl(FILE, "use crate::kind::Kind::{Branch, Leaf};", 1);
    let fixture_named = project_with(without, vec![named]);
    let fn_local = "fn caller() {\n    use Kind::*;\n    Leaf(1);\n}\n";
    let fixture_local = project_with(fn_local, Vec::new());
    for (fixture, source) in [(&fixture_named, without), (&fixture_local, fn_local)] {
        assert_eq!(
            target(fixture, &call(fixture, source, "Leaf(1)", "Leaf")),
            Some(id(NodeKind::EnumMember, "Kind::Leaf", "src/kind.rs"))
        );
    }
}

#[test]
fn method_call_syntax_never_names_fields_or_free_fns() {
    let fixture = project_with("fn caller() {}\n", Vec::new());
    assert_eq!(target(&fixture, &dropped("outgoing")), None);
    assert_eq!(
        target(&fixture, &dropped("get_outgoing_edges")),
        Some(id(
            NodeKind::Method,
            "Graph::get_outgoing_edges",
            "src/graph.rs"
        ))
    );
}

/// Names std defines beyond the old hand-picked list are not guessed onto
/// a receiver of unknown type, dropped or kept; a typed receiver still
/// resolves on its type.
#[test]
fn generated_std_method_names_gate_unknown_receivers() {
    let fixture = project_with("fn caller() {}\n", Vec::new());
    assert_eq!(target(&fixture, &dropped("parent")), None);
    let source = "\
fn caller(edge: TypedEdge) -> u8 {
    let value = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    edge.into_inner()
}
";
    let fixture = project_with(source, Vec::new());
    assert_eq!(
        target(
            &fixture,
            &call(
                &fixture,
                source,
                "poisoned.into_inner",
                "poisoned.into_inner"
            )
        ),
        None
    );
    assert_eq!(
        target(
            &fixture,
            &call(&fixture, source, "edge.into_inner", "edge.into_inner")
        ),
        Some(id(NodeKind::Method, "TypedEdge::into_inner", "src/edge.rs"))
    );
}

/// `self.graph.get(1)` reaches resolution as a dropped-receiver `get`; the
/// field's declared type decides it, as for a typed local, whether the call
/// is parsed (anchored at `self`) or sits in a macro's arguments (anchored
/// at the method).
#[test]
fn self_field_receivers_resolve_on_the_field_type() {
    let source = "\
struct Holder {
    graph: Arc<Graph>,
    names: Vec<String>,
}
impl Holder {
    fn walk(&self) {
        self.graph.get(1);
        assert!(self.graph.get(2).is_some());
        self.names.get_outgoing_edges(3);
    }
}
";
    let extra = vec![
        rust_node(NodeKind::Struct, "Holder", FILE, 1),
        rust_node(NodeKind::Field, "Holder::graph", FILE, 2),
        rust_node(NodeKind::Field, "Holder::names", FILE, 3),
        rust_node(NodeKind::Struct, "Graph", "src/graph.rs", 1),
        rust_node(NodeKind::Method, "Graph::get", "src/graph.rs", 20),
    ];
    let fixture = project_with(source, extra);
    let dropped_at = |marker: &str, written: &str, name: &str| UnresolvedRef {
        metadata: Some(receiver_dropped_metadata()),
        ..at_call(&fixture, source, marker, name, written)
    };
    let graph_get = Some(id(NodeKind::Method, "Graph::get", "src/graph.rs"));
    assert_eq!(
        target(&fixture, &dropped_at("self.graph.get(1)", "self.", "get")),
        graph_get
    );
    assert_eq!(
        target(&fixture, &dropped_at("self.graph.get(2)", "get(2)", "get")),
        graph_get
    );
    // `Vec` runs no project method, however project-specific the name.
    assert_eq!(
        target(
            &fixture,
            &dropped_at("self.names", "self.", "get_outgoing_edges")
        ),
        None
    );
}

/// A bare-named reference of `kind` from the first fn of `source`.
fn reference(fixture: &Fixture, name: &str, kind: EdgeKind) -> UnresolvedRef {
    let caller = fixture
        .get_nodes_in_file(FILE)
        .into_iter()
        .find(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
        .expect("a fn in the source");
    UnresolvedRef {
        from_node_id: caller.id,
        ..make_ref(name, kind, 2, FILE, Language::Rust)
    }
}

#[test]
fn a_bare_type_never_names_an_enum_variant_out_of_scope() {
    // `path: &Path`, `name: String`: std's types, not `Named::Path` or
    // `TomlValue::String`, which only a `use` would bring into scope.
    let variants = vec![
        rust_node(NodeKind::EnumMember, "Named::Path", "src/named.rs", 3),
        rust_node(NodeKind::EnumMember, "TomlValue::String", "src/toml.rs", 3),
    ];
    let source = "fn open(path: &Path, name: String) {\n}\n";
    let fixture = project_with(source, variants.clone());
    for name in ["Path", "String"] {
        assert_eq!(
            target(&fixture, &reference(&fixture, name, EdgeKind::References)),
            None,
            "{name}"
        );
    }

    // Imported, the variant is in scope.
    let source = "fn open(path: &Path) {\n}\n";
    let fixture = project_with(
        source,
        [
            variants,
            vec![use_decl(FILE, "use crate::named::Named::*;", 1)],
        ]
        .concat(),
    );
    assert_eq!(
        target(&fixture, &reference(&fixture, "Path", EdgeKind::References)),
        Some(id(NodeKind::EnumMember, "Named::Path", "src/named.rs"))
    );
}

#[test]
fn impl_and_derive_targets_are_traits() {
    // `#[derive(Default, Eq)]` with only same-named enum variants in the
    // project: std's traits, so nothing.
    let others = vec![
        rust_node(NodeKind::EnumMember, "Mode::Default", "src/mode.rs", 3),
        rust_node(NodeKind::EnumMember, "BinOp::Eq", "src/op.rs", 3),
    ];
    let source = "fn build() {\n}\n";
    let fixture = project_with(source, others.clone());
    for name in ["Default", "Eq"] {
        assert_eq!(
            target(&fixture, &reference(&fixture, name, EdgeKind::Implements)),
            None,
            "{name}"
        );
    }

    // A project trait of the name is the target.
    let fixture = project_with(
        source,
        [
            others,
            vec![rust_node(NodeKind::Trait, "Visitor", "src/visit.rs", 1)],
        ]
        .concat(),
    );
    assert_eq!(
        target(
            &fixture,
            &reference(&fixture, "Visitor", EdgeKind::Implements)
        ),
        Some(id(NodeKind::Trait, "Visitor", "src/visit.rs"))
    );
}

#[test]
fn prelude_names_in_references_stay_std_unless_imported() {
    // `None` as a value and `Result` as a type are std's, even with a
    // project constant `None` or a `Result` alias elsewhere.
    let namesakes = vec![
        rust_node(NodeKind::Constant, "None", "src/aggregate.rs", 3),
        rust_node(NodeKind::TypeAlias, "Result", "src/error.rs", 3),
    ];
    let source = "fn run() {\n}\n";
    let fixture = project_with(source, namesakes.clone());
    for name in ["None", "Result"] {
        assert_eq!(
            target(&fixture, &reference(&fixture, name, EdgeKind::References)),
            None,
            "{name}"
        );
    }

    // `use crate::error::Result;` brings the alias in.
    let fixture = project_with(
        source,
        [
            namesakes,
            vec![use_decl(FILE, "use crate::error::Result;", 1)],
        ]
        .concat(),
    );
    assert_eq!(
        target(
            &fixture,
            &reference(&fixture, "Result", EdgeKind::References)
        ),
        Some(id(NodeKind::TypeAlias, "Result", "src/error.rs"))
    );
}
