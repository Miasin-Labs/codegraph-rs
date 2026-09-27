use super::*;
use crate::ir::lower_tests::lower;

/// The op index of the call to `callee` (the `n`-th, 0-based).
fn call_at(func: &IrFunction, callee: &str, n: usize) -> usize {
    func.body
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, IrOp::Call { callee: c, .. } if c == callee))
        .nth(n)
        .unwrap_or_else(|| panic!("no call {callee} #{n}"))
        .0
}

/// Source lines of the definitions of `var` reaching the call to `callee`.
fn reaching_lines(func: &IrFunction, rd: &ReachingDefs, callee: &str, var: &str) -> Vec<u32> {
    let at = call_at(func, callee, 0);
    let place = rd.place_of(&Operand::var(var)).unwrap();
    let mut lines: Vec<u32> = rd
        .reaching(at, &place)
        .into_iter()
        .map(|id| match rd.defs[id].origin {
            DefOrigin::Op(op) => func.span(op).line,
            DefOrigin::Entry => 0,
            DefOrigin::Extra(i) => 1000 + i as u32,
        })
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

fn solve(src: &str) -> (IrFunction, ReachingDefs) {
    let func = lower("java", src);
    let rd = ReachingDefs::compute(&func, &[], Options::default());
    (func, rd)
}

#[test]
fn a_reassignment_kills_the_earlier_definition() {
    let (func, rd) = solve(
        "class A { void f(java.util.Map m) {\n\
         String bar = (String) m.get(\"keyB\");\n\
         bar = (String) m.get(\"keyA\");\n\
         sink(bar);\n\
         } }",
    );
    assert_eq!(reaching_lines(&func, &rd, "sink", "bar"), vec![3]);
}

#[test]
fn both_branches_reach_the_join() {
    let (func, rd) = solve(
        "class A { void f(boolean c) {\n\
         String x;\n\
         if (c) { x = a(); }\n\
         else { x = b(); }\n\
         sink(x);\n\
         } }",
    );
    assert_eq!(reaching_lines(&func, &rd, "sink", "x"), vec![3, 4]);
}

#[test]
fn a_loop_carries_its_definitions_around() {
    let (func, rd) = solve(
        "class A { void f(boolean c) {\n\
         String x = a();\n\
         while (c) {\n\
           sink(x);\n\
           x = b();\n\
         }\n\
         } }",
    );
    assert_eq!(reaching_lines(&func, &rd, "sink", "x"), vec![2, 5]);
}

#[test]
fn break_and_continue_leave_the_loop_body() {
    let (func, rd) = solve(
        "class A { void f(boolean c) {\n\
         String x = a();\n\
         for (int i = 0; i < 3; i++) {\n\
           if (c) { x = b(); break; }\n\
           x = d();\n\
         }\n\
         sink(x);\n\
         } }",
    );
    assert_eq!(reaching_lines(&func, &rd, "sink", "x"), vec![2, 4, 5]);
}

#[test]
fn field_paths_are_killed_by_their_own_or_a_prefix_write() {
    let (func, rd) = solve(
        "class A { void f(Req req) {\n\
         req.p.x = a();\n\
         req.p.y = b();\n\
         sink(req.p.x);\n\
         req.p = d();\n\
         sink2(req.p.x);\n\
         req.p.x = e();\n\
         sink3(req.p);\n\
         } }",
    );
    // Only `req.p.x` itself (and the entry def of `req`, a prefix) reach.
    let first = call_at(&func, "sink", 0);
    let args = match &func.body[first] {
        IrOp::Call { args, .. } => args.clone(),
        _ => unreachable!(),
    };
    let place = rd.place_of(&args[0]).unwrap();
    assert_eq!(place.to_string(), "req.p.x");
    let lines = |at: usize, place: &Place| -> Vec<u32> {
        let mut lines: Vec<u32> = rd
            .reaching(at, place)
            .into_iter()
            .map(|id| match rd.defs[id].origin {
                DefOrigin::Op(op) => func.span(op).line,
                _ => 0,
            })
            .collect();
        lines.sort_unstable();
        lines
    };
    assert_eq!(lines(first, &place), vec![0, 2]);
    // `req.p = d()` replaced the whole `p`: `req.p.x = a()` is dead.
    assert_eq!(lines(call_at(&func, "sink2", 0), &place), vec![0, 5]);
    // Reading `req.p` sees the write below it too.
    let whole = Place {
        base: Var::new("req"),
        fields: vec!["p".into()],
    };
    assert_eq!(lines(call_at(&func, "sink3", 0), &whole), vec![0, 5, 7]);
}

#[test]
fn constant_keys_are_separate_elements() {
    let (func, rd) = solve(
        "class A { void f() {\n\
         String[] a = new String[2];\n\
         a[0] = x();\n\
         a[1] = y();\n\
         sink(a[1]);\n\
         a[i] = z();\n\
         sink2(a[1]);\n\
         } }",
    );
    let at = call_at(&func, "sink", 0);
    let place = Place {
        base: Var::new("a"),
        fields: vec!["[1]".into()],
    };
    let lines = |at: usize| -> Vec<u32> {
        let mut lines: Vec<u32> = rd
            .reaching(at, &place)
            .into_iter()
            .filter_map(|id| match rd.defs[id].origin {
                DefOrigin::Op(op) => Some(func.span(op).line),
                _ => None,
            })
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    };
    assert_eq!(lines(at), vec![2, 4]);
    // An unknown element may be `a[1]`: a weak write that adds.
    assert_eq!(lines(call_at(&func, "sink2", 0)), vec![2, 4, 6]);
}

#[test]
fn constant_conditions_prune_dead_branches() {
    let (func, rd) = solve(
        "class A { void f(String param) {\n\
         String bar;\n\
         int num = 86;\n\
         if ((7 * 42) - num > 200) bar = \"safe\";\n\
         else bar = param;\n\
         sink(bar);\n\
         String y = param;\n\
         if (false) { y = \"dead\"; }\n\
         sink2(y);\n\
         } }",
    );
    assert_eq!(reaching_lines(&func, &rd, "sink", "bar"), vec![4]);
    assert_eq!(reaching_lines(&func, &rd, "sink2", "y"), vec![7]);
    // Without pruning, both reach.
    let unpruned = ReachingDefs::compute(
        &func,
        &[],
        Options {
            prune_constant_branches: false,
        },
    );
    assert_eq!(reaching_lines(&func, &unpruned, "sink", "bar"), vec![4, 5]);
}

#[test]
fn a_switch_on_a_constant_takes_one_arm() {
    let (func, rd) = solve(
        "class A { void f(String param) {\n\
         String bar;\n\
         String guess = \"ABC\";\n\
         char target = guess.charAt(1);\n\
         switch (target) {\n\
           case 'A': bar = param; break;\n\
           case 'B': bar = \"bob\"; break;\n\
           case 'C':\n\
           case 'D': bar = param; break;\n\
           default: bar = \"uncle\"; break;\n\
         }\n\
         sink(bar);\n\
         } }",
    );
    assert_eq!(reaching_lines(&func, &rd, "sink", "bar"), vec![7]);
    // An unknown scrutinee may take any arm, the default included.
    let (func, rd) = solve(
        "class A { void f(String param, char target) {\n\
         String bar;\n\
         switch (target) {\n\
           case 'A': bar = param; break;\n\
           case 'B': bar = \"bob\"; break;\n\
           default: bar = \"uncle\"; break;\n\
         }\n\
         sink(bar);\n\
         } }",
    );
    assert_eq!(reaching_lines(&func, &rd, "sink", "bar"), vec![4, 5, 6]);
}

#[test]
fn dead_code_is_unreachable() {
    let (func, rd) = solve(
        "class A { void f(String p) {\n\
         if (true) { return; }\n\
         sink(p);\n\
         } }",
    );
    assert!(!rd.is_reachable(call_at(&func, "sink", 0)));
}

#[test]
fn a_method_call_may_update_its_receiver() {
    let (func, rd) = solve(
        "class A { void f(String p) {\n\
         StringBuilder sb = new StringBuilder();\n\
         sb.append(p);\n\
         sink(sb);\n\
         } }",
    );
    // The call's weak definition joins the declaration.
    assert_eq!(reaching_lines(&func, &rd, "sink", "sb"), vec![2, 3]);
}

#[test]
fn extra_definitions_model_out_parameters() {
    let func = lower(
        "c",
        "void f() {\n\
         char buf[10] = \"\";\n\
         fgets(buf, 10, stdin);\n\
         system(buf);\n\
         }",
    );
    let fgets = call_at(&func, "fgets", 0);
    let extras = [ExtraDef {
        after: Some(fgets),
        place: Place::var(Var::new("buf")),
        strong: false,
    }];
    let rd = ReachingDefs::compute(&func, &extras, Options::default());
    assert_eq!(reaching_lines(&func, &rd, "system", "buf"), vec![2, 1000]);
}

#[test]
fn def_use_lists_each_read() {
    let (func, rd) = solve(
        "class A { void f(String p) {\n\
         String q = p + \"x\";\n\
         } }",
    );
    let binop = func
        .body
        .iter()
        .position(|op| matches!(op, IrOp::BinOp { .. }))
        .unwrap();
    let uses = rd.uses(&func, binop);
    assert_eq!(uses.len(), 1, "the constant reads nothing: {uses:?}");
    assert_eq!(uses[0].0, Place::var(Var::new("p")));
    assert_eq!(rd.defs[uses[0].1[0]].origin, DefOrigin::Entry);
}
