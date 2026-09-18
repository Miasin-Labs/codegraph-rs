use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use super::*;
use crate::edges::{EdgeData, EdgeKind};
use crate::graph::CodeGraph;
use crate::ir::{BinOpKind, IrFunction, IrOp, Operand, Var};
use crate::nodes::{NodeData, NodeId, NodeKind, Span, Visibility};

/// `Param(name)` of a function analysed with [`analyze`].
fn param(f: &IrFunction, name: &str) -> AbstractLocation {
    AbstractLocation::param(standalone_owner(f), name)
}

fn copy(dst: &str, src: &str) -> IrOp {
    IrOp::Assign {
        dst: Var::new(dst),
        src: Operand::var(src),
    }
}

fn alloc(dst: &str) -> IrOp {
    IrOp::Call {
        dst: Some(Var::new(dst)),
        callee: "new".into(),
        args: vec![],
    }
}

fn read(dst: &str, base: &str, field: &str) -> IrOp {
    IrOp::FieldRead {
        dst: Var::new(dst),
        base: Operand::var(base),
        field: field.into(),
    }
}

fn write(base: &str, field: &str, src: Operand) -> IrOp {
    IrOp::FieldWrite {
        base: Operand::var(base),
        field: field.into(),
        src,
    }
}

#[test]
fn param_seeds_with_self_location() {
    let mut f = IrFunction::new("f");
    f.params.push(Var::new("x"));
    let pts = analyze(&f);
    let set = pts.pts_of(&Var::new("x")).expect("param tracked");
    assert!(set.contains(&param(&f, "x")));
}

#[test]
fn copy_propagates_pts_set() {
    // y = x; → pts(y) ⊇ pts(x)
    let mut f = IrFunction::new("f");
    f.params.push(Var::new("x"));
    f.push(copy("y", "x"));
    let pts = analyze(&f);
    let x_set = pts.pts_of(&Var::new("x")).unwrap().clone();
    let y_set = pts.pts_of(&Var::new("y")).unwrap();
    assert!(x_set.is_subset(y_set), "pts(y) should ⊇ pts(x)");
}

#[test]
fn aliasing_through_copy() {
    // y = x;  z = x;  → may_alias(y, z) == true
    let mut f = IrFunction::new("f");
    f.params.push(Var::new("x"));
    f.push(copy("y", "x"));
    f.push(copy("z", "x"));
    let pts = analyze(&f);
    assert!(pts.may_alias(&Var::new("y"), &Var::new("z")));
}

#[test]
fn distinct_calls_get_distinct_heap_sites() {
    // a = call(); b = call();  → !may_alias(a, b)
    let mut f = IrFunction::new("f");
    f.push(alloc("a"));
    f.push(alloc("b"));
    let pts = analyze(&f);
    assert!(!pts.may_alias(&Var::new("a"), &Var::new("b")));
}

#[test]
fn field_write_then_read_preserves_value() {
    // o = call();
    // o.f = x;
    // y = o.f;
    // → pts(y) should overlap pts(x)
    let mut f = IrFunction::new("f");
    f.params.push(Var::new("x"));
    f.push(alloc("o"));
    f.push(write("o", "f", Operand::var("x")));
    f.push(read("y", "o", "f"));
    let pts = analyze(&f);
    let x_set = pts.pts_of(&Var::new("x")).unwrap().clone();
    let y_set = pts.pts_of(&Var::new("y")).unwrap();
    assert!(
        x_set.iter().any(|loc| y_set.contains(loc)),
        "pts(y) should contain at least one location from pts(x); got y={:?} x={:?}",
        y_set,
        x_set,
    );
}

#[test]
fn distinct_fields_dont_alias() {
    // o = call();
    // o.f = x;
    // o.g = y;     (different field)
    // a = o.f;
    // b = o.g;
    // → !may_alias(a, b)
    let mut f = IrFunction::new("f");
    f.params.push(Var::new("x"));
    f.params.push(Var::new("y"));
    f.push(alloc("o"));
    f.push(write("o", "f", Operand::var("x")));
    f.push(write("o", "g", Operand::var("y")));
    f.push(read("a", "o", "f"));
    f.push(read("b", "o", "g"));
    let pts = analyze(&f);
    assert!(
        !pts.may_alias(&Var::new("a"), &Var::new("b")),
        "field-sensitive analysis should not alias a (from o.f) with b (from o.g)"
    );
}

#[test]
fn binop_result_is_anonymous_literal() {
    let mut f = IrFunction::new("f");
    f.params.push(Var::new("x"));
    f.push(IrOp::BinOp {
        dst: Var::new("y"),
        lhs: Operand::var("x"),
        op: BinOpKind::Add,
        rhs: Operand::constant("1"),
    });
    let pts = analyze(&f);
    let y_set = pts.pts_of(&Var::new("y")).unwrap();
    assert!(
        y_set
            .iter()
            .any(|l| matches!(l, AbstractLocation::Literal { .. })),
        "binop result should yield a Literal location"
    );
}

#[test]
fn cyclic_field_store_terminates() {
    // node.next = node;  (self-referential — bounded by MAX_FIELD_DEPTH)
    let mut f = IrFunction::new("f");
    f.push(alloc("node"));
    f.push(write("node", "next", Operand::var("node")));
    // If this loops forever the test runner will time out;
    // termination is the assertion.
    assert!(analyze(&f).converged);
}

#[test]
fn cyclic_traversal_is_k_limited_and_converges() {
    // cur = cur.left; cur = cur.right — a traversal over two fields. Paths
    // grow until MAX_FIELD_DEPTH, where the summary cell is its own field.
    let mut f = IrFunction::new("walk");
    f.params.push(Var::new("cur"));
    f.push(read("__t0", "cur", "left"));
    f.push(copy("cur", "__t0"));
    f.push(read("__t1", "cur", "right"));
    f.push(copy("cur", "__t1"));
    let pts = analyze(&f);
    assert!(pts.converged);
    let cur = pts.pts_of(&Var::new("cur")).unwrap();
    // Every left/right path of length 0..=MAX_FIELD_DEPTH: 2^(k+1) - 1.
    assert_eq!(cur.len(), (1 << (MAX_FIELD_DEPTH + 1)) - 1);
    assert!(cur.iter().all(|loc| loc.depth() <= MAX_FIELD_DEPTH));
}

/// `a{len} = a{len-1}; …; a2 = a1; a1 = p`: every copy is listed before the
/// copy that defines its source, the worst order for a forward pass.
fn reversed_copy_chain(len: usize) -> IrFunction {
    let mut f = IrFunction::new("chain");
    f.params.push(Var::new("p"));
    for i in (1..=len).rev() {
        let src = if i == 1 {
            "p".to_owned()
        } else {
            format!("a{}", i - 1)
        };
        f.push(copy(&format!("a{i}"), &src));
    }
    f
}

#[test]
fn reversed_copy_chain_reaches_the_source() {
    // The old worklist re-queued every op on each change and gave up after
    // 32·n pops, leaving pts(a40) empty.
    for len in [40, 80] {
        let f = reversed_copy_chain(len);
        let pts = analyze(&f);
        assert!(pts.converged, "chain of {len}");
        let expected = BTreeSet::from([param(&f, "p")]);
        for i in 1..=len {
            assert_eq!(
                pts.pts_of(&Var::new(format!("a{i}"))),
                Some(&expected),
                "pts(a{i}) in a reversed chain of {len}",
            );
        }
    }
}

#[test]
fn exhausted_budget_is_reported_not_silent() {
    let f = reversed_copy_chain(10);
    let owner = standalone_owner(&f);
    // p's own location plus three copies fit; the rest does not.
    let cut = analyze_with_budget(owner.clone(), &f, 4);
    assert!(!cut.converged);
    assert!(cut.pts_of(&Var::new("a10")).is_none_or(BTreeSet::is_empty));

    let full = analyze_with_budget(owner, &f, 11);
    assert!(full.converged);
    assert_eq!(full.pts_of(&Var::new("a10")).map(BTreeSet::len), Some(1));
}

/// Ops exercising every constraint: copies (incl. a temp), allocation
/// sites, literals, field writes through aliases, placeholder reads of a
/// parameter's field, and a cyclic store.
fn mixed_ops() -> Vec<IrOp> {
    vec![
        copy("a", "p"),
        alloc("o"),
        write("o", "f", Operand::var("a")),
        read("b", "o", "f"),
        copy("c", "b"),
        write("o", "g", Operand::constant("\"lit\"")),
        read("d", "o", "g"),
        copy("r", "o"),
        write("r", "h", Operand::var("q")),
        read("e", "o", "h"),
        read("n", "p", "next"),
        write("n", "next", Operand::var("n")),
        read("m", "n", "next"),
        copy("n", "m"),
        IrOp::BinOp {
            dst: Var::new("s"),
            lhs: Operand::var("c"),
            op: BinOpKind::Add,
            rhs: Operand::constant("1"),
        },
        copy("__t0", "q"),
        IrOp::Assign {
            dst: Var::new("u"),
            src: Operand::Temp(0),
        },
        copy("w", "e"),
    ]
}

/// A permutation of `0..len` from a fixed-seed LCG (Fisher–Yates).
fn shuffled(len: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    let mut state = seed;
    for i in (1..len).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let j = usize::try_from(state >> 33).unwrap() % (i + 1);
        order.swap(i, j);
    }
    order
}

/// Rename allocation/literal sites from positions in a shuffled body back
/// to the original op indices (`original[pos]` is the op at `pos`).
fn unshuffle(loc: &AbstractLocation, original: &[usize]) -> AbstractLocation {
    crate::ensure_sufficient_stack(|| match loc {
        AbstractLocation::Heap { owner, site } => {
            AbstractLocation::heap(owner.clone(), original[*site])
        }
        AbstractLocation::Literal { owner, site } => {
            AbstractLocation::literal(owner.clone(), original[*site])
        }
        AbstractLocation::Param { .. } => loc.clone(),
        AbstractLocation::Field(base, name) => {
            AbstractLocation::field(unshuffle(base, original), name.clone())
        }
    })
}

type Canonical = (
    BTreeMap<Var, BTreeSet<AbstractLocation>>,
    BTreeMap<AbstractLocation, BTreeSet<AbstractLocation>>,
);

/// Analyse `ops` in the given order and express the result in terms of the
/// original op indices.
fn analyze_in_order(ops: &[IrOp], order: &[usize]) -> Canonical {
    let mut f = IrFunction::new("mixed");
    f.params.push(Var::new("p"));
    f.params.push(Var::new("q"));
    for &idx in order {
        f.push(ops[idx].clone());
    }
    let pts = analyze(&f);
    assert!(pts.converged);
    let set = |locs: &BTreeSet<AbstractLocation>| -> BTreeSet<AbstractLocation> {
        locs.iter().map(|loc| unshuffle(loc, order)).collect()
    };
    let vars = pts
        .vars
        .iter()
        .map(|(var, locs)| (var.clone(), set(locs)))
        .collect();
    let fields = pts
        .fields
        .iter()
        .map(|(cell, locs)| (unshuffle(cell, order), set(locs)))
        .collect();
    (vars, fields)
}

#[test]
fn op_order_does_not_change_the_result() {
    let ops = mixed_ops();
    let identity: Vec<usize> = (0..ops.len()).collect();
    let baseline = analyze_in_order(&ops, &identity);

    let reversed: Vec<usize> = identity.iter().rev().copied().collect();
    let mut orders = vec![reversed];
    orders.extend((1..=6).map(|seed| shuffled(ops.len(), seed)));
    for order in &orders {
        assert_eq!(analyze_in_order(&ops, order), baseline, "order {order:?}");
    }

    // And the baseline is the fixpoint, not just a stable answer.
    let owner = standalone_owner(&IrFunction::new("mixed"));
    let p = AbstractLocation::param(owner.clone(), "p");
    let q = AbstractLocation::param(owner.clone(), "q");
    let (vars, _) = &baseline;
    assert!(vars[&Var::new("c")].contains(&p), "o.f = a = p; c = o.f");
    assert!(vars[&Var::new("w")].contains(&q), "r = o; r.h = q; w = o.h");
    assert!(vars[&Var::new("u")].contains(&q), "__t0 = q; u = temp 0");
    assert!(vars[&Var::new("d")].contains(&AbstractLocation::literal(owner, 5)));
}

// ─── Interprocedural tests ───────────────────────────────────────────────────

fn mk_span() -> Span {
    Span {
        file: PathBuf::from("test.rs"),
        start_line: 1,
        start_col: 0,
        end_line: 1,
        end_col: 0,
        byte_range: 0..0,
    }
}

fn mk_node_data(name: &str) -> NodeData {
    NodeData {
        id: NodeId::new("test.rs", name, NodeKind::Function),
        kind: NodeKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: PathBuf::from("test.rs"),
        span: mk_span(),
        visibility: Visibility::Private,
        metadata: HashMap::new(),
        birth_revision: 0,
        last_modified_revision: 0,
        complexity: None,
        cfg: None,
        dataflow: None,
    }
}

fn calls_edge() -> EdgeData {
    EdgeData {
        kind: EdgeKind::Calls,
        source_span: mk_span(),
        weight: 1.0,
    }
}

/// A graph + IR map over `functions`, with a `Calls` edge per `calls` pair
/// (indices into `functions`). Returns the node ids in `functions` order.
fn program(
    functions: Vec<IrFunction>,
    calls: &[(usize, usize)],
) -> (CodeGraph, HashMap<NodeId, IrFunction>, Vec<NodeId>) {
    let mut graph = CodeGraph::new();
    let ids: Vec<NodeId> = functions
        .iter()
        .map(|f| graph.add_node(mk_node_data(&f.name)))
        .collect();
    for &(caller, callee) in calls {
        graph
            .add_edge(&ids[caller], &ids[callee], calls_edge())
            .unwrap();
    }
    let ir_map = ids.iter().cloned().zip(functions).collect();
    (graph, ir_map, ids)
}

fn call(dst: Option<&str>, callee: &str, args: &[&str]) -> IrOp {
    IrOp::Call {
        dst: dst.map(Var::new),
        callee: callee.into(),
        args: args.iter().map(|a| Operand::var(*a)).collect(),
    }
}

#[test]
fn interprocedural_return_flows_to_caller() {
    // source() returns a param-location; main calls source() and
    // assigns to `x`. x's pts should contain source's return location.
    let mut source_ir = IrFunction::new("source");
    source_ir.params.push(Var::new("seed"));
    source_ir.push(IrOp::Return {
        value: Some(Operand::var("seed")),
    });

    let mut main_ir = IrFunction::new("main");
    main_ir.push(call(Some("x"), "source", &[]));

    let (graph, ir_map, ids) = program(vec![source_ir, main_ir], &[(1, 0)]);
    let tables = analyze_interprocedural(&graph, &ir_map);
    let x_pts = tables[&ids[1]].pts_of(&Var::new("x")).expect("x tracked");

    // source's return is `seed` → Param(source, seed); it should flow to x.
    let seed = AbstractLocation::param(ids[0].clone(), "seed");
    assert!(
        x_pts.contains(&seed),
        "expected {seed:?} in pts(x), got {x_pts:?}"
    );
}

#[test]
fn interprocedural_arg_flows_to_callee_param() {
    // main has param `data`, calls sink(data). sink's param `x` pts
    // should include main's Param(data) location.
    let mut main_ir = IrFunction::new("main");
    main_ir.params.push(Var::new("data"));
    main_ir.push(call(None, "sink", &["data"]));

    let mut sink_ir = IrFunction::new("sink");
    sink_ir.params.push(Var::new("x"));

    let (graph, ir_map, ids) = program(vec![main_ir, sink_ir], &[(0, 1)]);
    let tables = analyze_interprocedural(&graph, &ir_map);
    let x_pts = tables[&ids[1]].pts_of(&Var::new("x")).expect("x tracked");

    let data = AbstractLocation::param(ids[0].clone(), "data");
    assert!(
        x_pts.contains(&data),
        "expected {data:?} in pts(x), got {x_pts:?}"
    );
}

#[test]
fn interprocedural_transitive_source_to_sink() {
    // source(taint) returns taint; sink(x) receives;
    // main: tmp = source(); sink(tmp);
    let mut source_ir = IrFunction::new("source");
    source_ir.params.push(Var::new("taint"));
    source_ir.push(IrOp::Return {
        value: Some(Operand::var("taint")),
    });

    let mut sink_ir = IrFunction::new("sink");
    sink_ir.params.push(Var::new("x"));

    let mut main_ir = IrFunction::new("main");
    main_ir.push(call(Some("tmp"), "source", &[]));
    main_ir.push(call(None, "sink", &["tmp"]));

    let (graph, ir_map, ids) = program(vec![source_ir, sink_ir, main_ir], &[(2, 0), (2, 1)]);
    let tables = analyze_interprocedural(&graph, &ir_map);
    let x_pts = tables[&ids[1]].pts_of(&Var::new("x")).expect("x tracked");

    let taint = AbstractLocation::param(ids[0].clone(), "taint");
    assert!(
        x_pts.contains(&taint),
        "transitive: expected {taint:?} in sink's pts(x), got {x_pts:?}",
    );
}

#[test]
fn interprocedural_seeds_are_re_solved_through_local_copies() {
    // main: tmp = source(); y = tmp; sink(y);   sink(x): z = x;
    // Seeds arrive after each body was first solved; the copies `y = tmp`
    // and `z = x` must re-run for taint to reach sink's `z`.
    let mut source_ir = IrFunction::new("source");
    source_ir.params.push(Var::new("taint"));
    source_ir.push(IrOp::Return {
        value: Some(Operand::var("taint")),
    });

    let mut sink_ir = IrFunction::new("sink");
    sink_ir.params.push(Var::new("x"));
    sink_ir.push(copy("z", "x"));

    let mut main_ir = IrFunction::new("main");
    main_ir.push(call(Some("tmp"), "source", &[]));
    main_ir.push(copy("y", "tmp"));
    main_ir.push(call(None, "sink", &["y"]));

    let (graph, ir_map, ids) = program(vec![source_ir, sink_ir, main_ir], &[(2, 0), (2, 1)]);
    let tables = analyze_interprocedural(&graph, &ir_map);
    let z_pts = tables[&ids[1]].pts_of(&Var::new("z")).expect("z tracked");

    let taint = AbstractLocation::param(ids[0].clone(), "taint");
    assert!(
        z_pts.contains(&taint),
        "expected {taint:?} in pts(z), got {z_pts:?}"
    );
}

#[test]
fn interprocedural_long_call_chain_reaches_fixpoint() {
    // f0(p0) calls f1(p0), f1(p1) calls f2(p1), … — 20 hops, more than the
    // old 8-round cap could carry.
    const LEN: usize = 20;
    let functions: Vec<IrFunction> = (0..LEN)
        .map(|i| {
            let mut f = IrFunction::new(format!("f{i}"));
            let param = format!("p{i}");
            if i + 1 < LEN {
                f.push(call(None, &format!("f{}", i + 1), &[param.as_str()]));
            }
            f.params.push(Var::new(param));
            f
        })
        .collect();
    let calls: Vec<(usize, usize)> = (0..LEN - 1).map(|i| (i, i + 1)).collect();
    let (graph, ir_map, ids) = program(functions, &calls);

    let tables = analyze_interprocedural(&graph, &ir_map);
    assert!(tables.values().all(|t| t.converged));
    let origin = AbstractLocation::param(ids[0].clone(), "p0");
    let last = tables[&ids[LEN - 1]]
        .pts_of(&Var::new(format!("p{}", LEN - 1)))
        .unwrap();
    assert!(
        last.contains(&origin),
        "p0 should reach f{}: {last:?}",
        LEN - 1
    );
}

#[test]
fn same_named_roots_in_different_functions_stay_distinct() {
    // f(x) { a = new(); b = 1 } and g(x) { a = new(); b = 1 } with no call
    // between them share parameter names and op indices, but no location.
    let body = |name: &str| {
        let mut f = IrFunction::new(name);
        f.params.push(Var::new("x"));
        f.push(alloc("a"));
        f.push(IrOp::Assign {
            dst: Var::new("b"),
            src: Operand::constant("1"),
        });
        f
    };
    let (graph, ir_map, ids) = program(vec![body("f"), body("g")], &[]);
    let tables = analyze_interprocedural(&graph, &ir_map);
    let locations = |id: &NodeId| -> BTreeSet<AbstractLocation> {
        tables[id].vars.values().flatten().cloned().collect()
    };
    let (f_locs, g_locs) = (locations(&ids[0]), locations(&ids[1]));
    assert_eq!(f_locs.len(), 3);
    assert!(f_locs.is_disjoint(&g_locs), "{f_locs:?} vs {g_locs:?}");
}
