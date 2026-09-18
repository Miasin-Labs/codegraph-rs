//! Method calls and path calls carry argument, receiver and return flow;
//! the callee text's qualifier picks between same-named targets.

use std::collections::HashMap;

use super::{calls_edge, mk_function_node};
use crate::graph::CodeGraph;
use crate::ir::{IrFunction, IrOp, Operand, Var, lower_for_language};
use crate::nodes::{NodeId, NodeKind};
use crate::points_to::{
    AbstractLocation,
    CallBinding,
    PointsToTable,
    analyze_interprocedural,
    bind_call_sites,
};

/// A graph + IR map over `(file, qualified name, ir)` triples, with a
/// `Calls` edge per `calls` pair. Returns the node ids in input order.
fn program(
    functions: Vec<(&str, &str, IrFunction)>,
    calls: &[(usize, usize)],
) -> (CodeGraph, HashMap<NodeId, IrFunction>, Vec<NodeId>) {
    let mut graph = CodeGraph::new();
    let mut ir_map = HashMap::new();
    let mut ids = Vec::new();
    for (file, qualified, ir) in functions {
        let id = graph.add_node(mk_function_node(file, qualified));
        ir_map.insert(id.clone(), ir);
        ids.push(id);
    }
    for &(caller, callee) in calls {
        graph
            .add_edge(&ids[caller], &ids[callee], calls_edge())
            .unwrap();
    }
    (graph, ir_map, ids)
}

fn function(name: &str, params: &[&str]) -> IrFunction {
    let mut f = IrFunction::new(name);
    f.params = params.iter().map(|p| Var::new(*p)).collect();
    f
}

fn method(name: &str, params: &[&str]) -> IrFunction {
    let mut f = function(name, params);
    f.receiver = Some(Var::new("self"));
    f
}

/// `dst = callee(args)` / `dst = receiver.callee(args)`.
fn call(dst: Option<&str>, callee: &str, receiver: Option<&str>, args: &[&str]) -> IrOp {
    IrOp::Call {
        dst: dst.map(Var::new),
        callee: callee.into(),
        receiver: receiver.map(Operand::var),
        args: args.iter().map(|a| Operand::var(*a)).collect(),
    }
}

/// An allocation whose callee text names no function, so it never binds.
fn fresh(dst: &str) -> IrOp {
    call(Some(dst), "<alloc>", None, &[])
}

fn ret(var: &str) -> IrOp {
    IrOp::Return {
        value: Some(Operand::var(var)),
    }
}

fn pts<'t>(
    tables: &'t HashMap<NodeId, PointsToTable>,
    id: &NodeId,
    var: &str,
) -> &'t std::collections::BTreeSet<AbstractLocation> {
    tables[id]
        .pts_of(&Var::new(var))
        .unwrap_or_else(|| panic!("{var} untracked"))
}

fn param(owner: &NodeId, name: &str) -> AbstractLocation {
    AbstractLocation::param(owner.clone(), name)
}

/// `main(data) { obj = alloc; obj.put(data) }` calling
/// `Store::put(&mut self, v) { self.last = v }`.
fn method_call_program() -> (CodeGraph, HashMap<NodeId, IrFunction>, Vec<NodeId>) {
    let mut main = function("main", &["data"]);
    main.push(fresh("obj"));
    main.push(call(None, "obj.put", Some("obj"), &["data"]));
    let mut put = method("put", &["v"]);
    put.push(IrOp::FieldWrite {
        base: Operand::var("self"),
        field: "last".into(),
        src: Operand::var("v"),
    });
    program(
        vec![("test.rs", "main", main), ("test.rs", "Store::put", put)],
        &[(0, 1)],
    )
}

#[test]
fn method_call_argument_flows_to_first_parameter() {
    let (graph, ir_map, ids) = method_call_program();
    let tables = analyze_interprocedural(&graph, &ir_map);
    let v = pts(&tables, &ids[1], "v");
    assert!(v.contains(&param(&ids[0], "data")), "pts(v) = {v:?}");
    assert_eq!(
        bind_call_sites(&graph, &ir_map),
        [CallBinding {
            caller: ids[0].clone(),
            op: 1,
            callee: ids[1].clone(),
        }]
    );
}

#[test]
fn method_call_receiver_flows_to_self() {
    let (graph, ir_map, ids) = method_call_program();
    let tables = analyze_interprocedural(&graph, &ir_map);
    let obj = AbstractLocation::heap(ids[0].clone(), 0);
    let receiver = pts(&tables, &ids[1], "self");
    assert!(receiver.contains(&obj), "pts(self) = {receiver:?}");
    assert!(
        !receiver.contains(&param(&ids[0], "data")),
        "the argument must not land in the receiver: {receiver:?}"
    );
    // The store through `self` lands in the caller's object.
    let cell = AbstractLocation::field(obj, "last");
    let stored = tables[&ids[1]].field_pts(&cell).expect("self.last stored");
    assert!(stored.contains(&param(&ids[0], "data")), "{stored:?}");
}

#[test]
fn path_call_carries_arguments_and_return() {
    // main(x) { y = Foo::new(x) }   Foo::new(v) { s = alloc; s.v = v; return s }
    let mut main = function("main", &["x"]);
    main.push(call(Some("y"), "Foo::new", None, &["x"]));
    let mut new = function("new", &["v"]);
    new.push(fresh("s"));
    new.push(IrOp::FieldWrite {
        base: Operand::var("s"),
        field: "v".into(),
        src: Operand::var("v"),
    });
    new.push(ret("s"));
    let (graph, ir_map, ids) = program(
        vec![("test.rs", "main", main), ("test.rs", "Foo::new", new)],
        &[(0, 1)],
    );
    let tables = analyze_interprocedural(&graph, &ir_map);

    let v = pts(&tables, &ids[1], "v");
    assert!(v.contains(&param(&ids[0], "x")), "pts(v) = {v:?}");
    let y = pts(&tables, &ids[0], "y");
    let built = AbstractLocation::heap(ids[1].clone(), 0);
    assert!(y.contains(&built), "Foo::new's object reaches y: {y:?}");
}

#[test]
fn same_named_callees_are_told_apart_by_qualifier() {
    // main(a, b) { ra = A::new(a); rb = B::new(b) }
    // A::new(p) { return p }   B::new(q) { return q }
    let mut main = function("main", &["a", "b"]);
    main.push(call(Some("ra"), "A::new", None, &["a"]));
    main.push(call(Some("rb"), "B::<u8>::new", None, &["b"]));
    let mut a_new = function("new", &["p"]);
    a_new.push(ret("p"));
    let mut b_new = function("new", &["q"]);
    b_new.push(ret("q"));
    let (graph, ir_map, ids) = program(
        vec![
            ("test.rs", "main", main),
            ("test.rs", "A::new", a_new),
            ("test.rs", "B::new", b_new),
        ],
        &[(0, 1), (0, 2)],
    );

    let bound: Vec<(usize, &NodeId)> = bind_call_sites(&graph, &ir_map)
        .iter()
        .map(|b| {
            (
                b.op,
                &ids[ids.iter().position(|id| *id == b.callee).unwrap()],
            )
        })
        .collect();
    assert_eq!(bound, [(0, &ids[1]), (1, &ids[2])]);

    let tables = analyze_interprocedural(&graph, &ir_map);
    let (a, b) = (param(&ids[0], "a"), param(&ids[0], "b"));
    let p = pts(&tables, &ids[1], "p");
    let q = pts(&tables, &ids[2], "q");
    assert!(p.contains(&a) && !p.contains(&b), "pts(A::new.p) = {p:?}");
    assert!(q.contains(&b) && !q.contains(&a), "pts(B::new.q) = {q:?}");
    let ra = pts(&tables, &ids[0], "ra");
    let rb = pts(&tables, &ids[0], "rb");
    assert!(ra.contains(&a) && !ra.contains(&b), "pts(ra) = {ra:?}");
    assert!(rb.contains(&b) && !rb.contains(&a), "pts(rb) = {rb:?}");
}

#[test]
fn bare_name_call_still_carries_flow() {
    // main(x) { y = helper(x) }   helper(v) { return v }
    let mut main = function("main", &["x"]);
    main.push(call(Some("y"), "helper", None, &["x"]));
    let mut helper = function("helper", &["v"]);
    helper.push(ret("v"));
    let (graph, ir_map, ids) = program(
        vec![("test.rs", "main", main), ("test.rs", "helper", helper)],
        &[(0, 1)],
    );
    let tables = analyze_interprocedural(&graph, &ir_map);
    let x = param(&ids[0], "x");
    assert!(pts(&tables, &ids[1], "v").contains(&x));
    assert!(pts(&tables, &ids[0], "y").contains(&x));
    // A turbofish does not hide the name.
    let mut generic = function("main", &["x"]);
    generic.push(call(Some("y"), "helper::<u8>", None, &["x"]));
    let mut helper = function("helper", &["v"]);
    helper.push(ret("v"));
    let (graph, ir_map, _) = program(
        vec![("test.rs", "main", generic), ("test.rs", "helper", helper)],
        &[(0, 1)],
    );
    assert_eq!(bind_call_sites(&graph, &ir_map).len(), 1);
}

#[test]
fn ambiguous_call_binds_nothing_and_own_type_wins() {
    // A::go(&self, other, x) { self.run(x); other.run(x) }, edges to A::run
    // and B::run: `self.run` is A's own; `other` could be either.
    let mut go = method("go", &["other", "x"]);
    go.push(call(None, "self.run", Some("self"), &["x"]));
    go.push(call(None, "other.run", Some("other"), &["x"]));
    let (graph, ir_map, ids) = program(
        vec![
            ("test.rs", "A::go", go),
            ("test.rs", "A::run", method("run", &["a"])),
            ("test.rs", "B::run", method("run", &["b"])),
        ],
        &[(0, 1), (0, 2)],
    );
    let bindings = bind_call_sites(&graph, &ir_map);
    assert_eq!(
        bindings,
        [CallBinding {
            caller: ids[0].clone(),
            op: 0,
            callee: ids[1].clone(),
        }]
    );
    let tables = analyze_interprocedural(&graph, &ir_map);
    let x = param(&ids[0], "x");
    assert!(pts(&tables, &ids[1], "a").contains(&x));
    assert!(!pts(&tables, &ids[2], "b").contains(&x));
}

#[test]
fn bare_call_prefers_a_sibling_in_the_callers_scope() {
    // fn outer() { fn walk(d) { walk(d) } } next to fn other() { fn walk(d) }:
    // the recursive bare `walk` is outer's. From a free function, a bare
    // `walk` fits both and binds neither.
    let mut walk = function("walk", &["d"]);
    walk.push(call(None, "walk", None, &["d"]));
    let mut main = function("main", &["d"]);
    main.push(call(None, "walk", None, &["d"]));
    let (graph, ir_map, ids) = program(
        vec![
            ("test.rs", "outer::walk", walk),
            ("test.rs", "other::walk", function("walk", &["e"])),
            ("test.rs", "main", main),
        ],
        &[(0, 0), (0, 1), (2, 0), (2, 1)],
    );
    assert_eq!(
        bind_call_sites(&graph, &ir_map),
        [CallBinding {
            caller: ids[0].clone(),
            op: 0,
            callee: ids[0].clone(),
        }]
    );
}

#[test]
fn path_qualifier_naming_another_owner_does_not_bind() {
    // main() { v = Vec::new() } — the only edge target is Foo::new.
    let mut main = function("main", &[]);
    main.push(call(Some("v"), "Vec::new", None, &[]));
    let mut new = function("new", &[]);
    new.push(fresh("s"));
    new.push(ret("s"));
    let (graph, ir_map, ids) = program(
        vec![("test.rs", "main", main), ("test.rs", "Foo::new", new)],
        &[(0, 1)],
    );
    assert!(bind_call_sites(&graph, &ir_map).is_empty());
    let tables = analyze_interprocedural(&graph, &ir_map);
    let v = pts(&tables, &ids[0], "v");
    assert!(
        !v.contains(&AbstractLocation::heap(ids[1].clone(), 0)),
        "{v:?}"
    );
}

#[test]
fn delegation_through_a_self_field_does_not_bind_to_the_own_type() {
    // Wrapper::get(&self, k) { self.inner.get(k) } — name-only resolution
    // links the op to Wrapper::get itself; the field is some other type.
    let mut get = method("get", &["k"]);
    get.push(IrOp::FieldRead {
        dst: Var::new("__t0"),
        base: Operand::var("self"),
        field: "inner".into(),
    });
    get.push(call(Some("r"), "self.inner.get", Some("__t0"), &["k"]));
    let (graph, ir_map, _) = program(vec![("test.rs", "Wrapper::get", get)], &[(0, 0)]);
    assert!(bind_call_sites(&graph, &ir_map).is_empty());
}

#[test]
fn bare_call_does_not_bind_to_a_function_of_a_type() {
    // impl Graph { fn open(root) { open(root) } } — the bare call is the
    // free `open`; the only edge (name-only resolution) is to Graph::open.
    let mut open = function("open", &["root"]);
    open.push(call(Some("g"), "open", None, &["root"]));
    let (mut graph, ir_map, _) = program(vec![("test.rs", "Graph::open", open)], &[(0, 0)]);
    let mut owner = mk_function_node("test.rs", "Graph");
    owner.kind = NodeKind::Struct;
    owner.id = NodeId::new("test.rs", "Graph", NodeKind::Struct);
    graph.add_node(owner);
    assert!(bind_call_sites(&graph, &ir_map).is_empty());
}

#[test]
fn bare_call_binds_a_function_nested_in_a_method() {
    // impl Graph { fn walk(&self) { fn dfs(n) { dfs(n) } dfs(1) } } — the
    // nested `dfs` is named `Graph::dfs` but is reachable by bare name.
    let mut dfs = function("dfs", &["n"]);
    dfs.push(call(None, "dfs", None, &["n"]));
    let mut graph = CodeGraph::new();
    let mut ir_map = HashMap::new();
    let mut ids = Vec::new();
    for (qualified, bytes, ir) in [
        ("Graph::walk", 0..100, method("walk", &[])),
        ("Graph::dfs", 20..60, dfs),
    ] {
        let mut node = mk_function_node("test.rs", qualified);
        node.span.byte_range = bytes;
        ids.push(graph.add_node(node));
        ir_map.insert(ids[ids.len() - 1].clone(), ir);
    }
    let mut owner = mk_function_node("test.rs", "Graph");
    owner.kind = NodeKind::Struct;
    owner.id = NodeId::new("test.rs", "Graph", NodeKind::Struct);
    graph.add_node(owner);
    graph.add_edge(&ids[1], &ids[1], calls_edge()).unwrap();
    assert_eq!(
        bind_call_sites(&graph, &ir_map),
        [CallBinding {
            caller: ids[1].clone(),
            op: 0,
            callee: ids[1].clone(),
        }]
    );
}

#[test]
fn path_call_to_a_method_passes_first_argument_as_receiver() {
    // main(data) { obj = alloc; Store::put(obj, data) }
    let mut main = function("main", &["data"]);
    main.push(fresh("obj"));
    main.push(call(None, "Store::put", None, &["obj", "data"]));
    let (graph, ir_map, ids) = program(
        vec![
            ("test.rs", "main", main),
            ("test.rs", "Store::put", method("put", &["v"])),
        ],
        &[(0, 1)],
    );
    let tables = analyze_interprocedural(&graph, &ir_map);
    let receiver = pts(&tables, &ids[1], "self");
    let v = pts(&tables, &ids[1], "v");
    assert!(receiver.contains(&AbstractLocation::heap(ids[0].clone(), 0)));
    assert!(!receiver.contains(&param(&ids[0], "data")), "{receiver:?}");
    assert!(v.contains(&param(&ids[0], "data")), "{v:?}");
}

#[test]
fn class_qualified_call_passes_explicit_receiver() {
    // Python `Base.init(self, x)` from Derived.init(self, x).
    let mut derived = method("init", &["x"]);
    derived.push(call(None, "Base.init", Some("Base"), &["self", "x"]));
    let (graph, ir_map, ids) = program(
        vec![
            ("test.py", "Derived::init", derived),
            ("test.py", "Base::init", method("init", &["y"])),
        ],
        &[(0, 1)],
    );
    let tables = analyze_interprocedural(&graph, &ir_map);
    let receiver = pts(&tables, &ids[1], "self");
    let y = pts(&tables, &ids[1], "y");
    assert!(receiver.contains(&param(&ids[0], "self")), "{receiver:?}");
    assert!(y.contains(&param(&ids[0], "x")) && !y.contains(&param(&ids[0], "self")));
}

#[test]
fn member_call_to_a_package_function_drops_the_receiver() {
    // Go `pkg.Parse(src)` calling the free function Parse in pkg/parse.go.
    let mut main = function("main", &["src"]);
    main.push(call(Some("out"), "pkg.Parse", Some("pkg"), &["src"]));
    let mut parse = function("Parse", &["text"]);
    parse.push(ret("text"));
    let (graph, ir_map, ids) = program(
        vec![
            ("cmd/main.go", "main", main),
            ("pkg/parse.go", "Parse", parse),
        ],
        &[(0, 1)],
    );
    let tables = analyze_interprocedural(&graph, &ir_map);
    let src = param(&ids[0], "src");
    assert!(pts(&tables, &ids[1], "text").contains(&src));
    assert!(pts(&tables, &ids[0], "out").contains(&src));
}

/// Lowered from real Rust source: a path call to a constructor and a method
/// call on its result both carry flow.
#[test]
fn lowered_rust_method_and_path_calls_carry_flow() {
    let src = "\
struct Store { last: i32 }
impl Store {
    fn new(seed: i32) -> Store { return seed; }
    fn put(&mut self, v: i32) -> i32 { self.last = v; return self.last; }
}
fn run(data: i32) {
    let s = Store::new(data);
    let r = s.put(&data);
}
";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let mut lowered = HashMap::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_item" {
            let ir = lower_for_language("rust", node, src).unwrap();
            lowered.insert(ir.name.clone(), ir);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    let put = lowered.remove("put").unwrap();
    assert_eq!(put.receiver, Some(Var::new("self")));
    assert_eq!(put.params, [Var::new("v")]);
    let (graph, ir_map, ids) = program(
        vec![
            ("store.rs", "run", lowered.remove("run").unwrap()),
            ("store.rs", "Store::new", lowered.remove("new").unwrap()),
            ("store.rs", "Store::put", put),
        ],
        &[(0, 1), (0, 2)],
    );
    assert_eq!(bind_call_sites(&graph, &ir_map).len(), 2);

    let tables = analyze_interprocedural(&graph, &ir_map);
    let data = param(&ids[0], "data");
    assert!(pts(&tables, &ids[1], "seed").contains(&data));
    // `s` holds what Store::new returned; `s.put(..)` passes it as `self`.
    let s = pts(&tables, &ids[0], "s");
    assert!(s.contains(&data), "pts(s) = {s:?}");
    assert!(pts(&tables, &ids[2], "self").is_superset(s));
    assert!(pts(&tables, &ids[2], "v").contains(&data));
    assert!(pts(&tables, &ids[0], "r").contains(&data));
}
