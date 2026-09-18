use std::collections::HashSet;

use tree_sitter::{Language, Parser, Tree};

use super::*;

// ─── Harness ─────────────────────────────────────────────────────────────────

const ENTRY: u32 = 0;
const EXIT: u32 = 1;

fn grammar(lang: &str) -> Language {
    match lang {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        other => panic!("no test grammar for {other}"),
    }
}

fn parse(lang: &str, src: &str) -> Tree {
    let mut parser = Parser::new();
    parser.set_language(&grammar(lang)).unwrap();
    parser.parse(src, None).unwrap()
}

/// A function's name: its `name` field, or (C/C++) the identifier at the
/// bottom of its declarator chain.
fn function_name(node: TsNode<'_>, src: &str) -> String {
    let text = |node: TsNode<'_>| node.utf8_text(src.as_bytes()).unwrap().to_owned();
    let mut current = node;
    loop {
        if let Some(name) = current.child_by_field_name("name") {
            return text(name);
        }
        match current.child_by_field_name("declarator") {
            Some(next) if next.kind() == "identifier" => return text(next),
            Some(next) => current = next,
            None => return String::new(),
        }
    }
}

/// Build the CFG of every function in `src`, asserting each validates.
fn all_cfgs(lang: &str, src: &str) -> Vec<(String, FunctionCfg)> {
    let tree = parse(lang, src);
    let rules = CfgRules::for_language(lang).unwrap();
    let mut functions = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if rules.function_nodes.contains(&node.kind()) {
            functions.push(node);
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    functions
        .into_iter()
        .map(|function| {
            let name = function_name(function, src);
            let cfg = build_cfg(function, src.as_bytes(), lang)
                .unwrap_or_else(|| panic!("{lang} `{name}`: no CFG"));
            if let Err(violation) = cfg.validate() {
                panic!("{lang} `{name}`: {violation}\n{}", cfg.format_summary());
            }
            (name, cfg)
        })
        .collect()
}

/// The validated CFG of the function called `name`.
fn cfg_of(lang: &str, src: &str, name: &str) -> FunctionCfg {
    all_cfgs(lang, src)
        .into_iter()
        .find(|(found, _)| found == name)
        .map(|(_, cfg)| cfg)
        .unwrap_or_else(|| panic!("{lang}: no function `{name}`"))
}

/// The tightest block labelled `label` whose lines cover `line`.
fn block(cfg: &FunctionCfg, label: &str, line: u32) -> u32 {
    cfg.blocks
        .iter()
        .filter(|b| b.label == label && b.start_line <= line && line <= b.end_line)
        .min_by_key(|b| b.end_line - b.start_line)
        .map(|b| b.id)
        .unwrap_or_else(|| panic!("no `{label}` block at L{line}:\n{}", cfg.format_summary()))
}

fn blocks_labelled(cfg: &FunctionCfg, label: &str) -> Vec<u32> {
    cfg.blocks
        .iter()
        .filter(|b| b.label == label)
        .map(|b| b.id)
        .collect()
}

fn has_edge(cfg: &FunctionCfg, from: u32, to: u32, kind: CfgEdgeKind) -> bool {
    cfg.edges
        .iter()
        .any(|e| e.from == from && e.to == to && e.kind == kind)
}

fn edge_targets(cfg: &FunctionCfg, from: u32, kind: CfgEdgeKind) -> Vec<u32> {
    cfg.edges
        .iter()
        .filter(|e| e.from == from && e.kind == kind)
        .map(|e| e.to)
        .collect()
}

fn reaches(cfg: &FunctionCfg, from: u32, to: u32) -> bool {
    let mut seen = HashSet::from([from]);
    let mut stack = vec![from];
    while let Some(block) = stack.pop() {
        if block == to {
            return true;
        }
        for edge in cfg.edges.iter().filter(|e| e.from == block) {
            if seen.insert(edge.to) {
                stack.push(edge.to);
            }
        }
    }
    false
}

/// Kinds of the edges entering EXIT.
fn exit_edge_kinds(cfg: &FunctionCfg) -> HashSet<CfgEdgeKind> {
    cfg.edges
        .iter()
        .filter(|e| e.to == EXIT)
        .map(|e| e.kind)
        .collect()
}

fn covers_line(cfg: &FunctionCfg, line: u32) -> bool {
    cfg.blocks
        .iter()
        .any(|b| b.kind != CfgBlockKind::Exit && b.start_line <= line && line <= b.end_line)
}

// ─── Fixtures (line numbers are load-bearing) ────────────────────────────────

const RUST_SRC: &str = r#"fn else_return(x: i32) -> i32 {
    if x > 0 {
        let a = 1;
    } else {
        return 7;
    }
    let after = 2;
    after
}

fn arm_return(o: Option<i32>) -> i32 {
    match o {
        Some(v) => { let w = v; }
        None => { return 0; }
    }
    let after = 3;
    after
}

fn try_op(s: &str) -> Result<i32, E> {
    let v = parse(s)?;
    let after = v + 1;
    Ok(after)
}

fn let_else_return(o: Option<i32>) -> i32 {
    let v = match o { Some(v) => v, None => return 0 };
    v
}

fn labeled(n: i32) {
    'outer: for i in 0..n {
        for j in 0..n {
            if j == 2 { break 'outer; }
        }
        let tail = i;
    }
    let after = 1;
}

fn let_else_continue(xs: &[Option<i32>]) -> i32 {
    let mut sum = 0;
    for x in xs {
        let Some(v) = x else { continue };
        sum += v;
    }
    sum
}

fn closure_return_stays_local(xs: &[i32]) -> Vec<i32> {
    let ys = xs.iter().map(|x| { return x + 1; }).collect();
    ys
}

fn diverges() -> ! {
    loop {}
}

fn labeled_block(c: bool) {
    'blk: {
        if c { break 'blk; }
        work();
    }
    done();
}
"#;

const TS_SRC: &str = r#"function tsElse(x: number): number {
  if (x > 0) {
    a();
  } else {
    return 1;
  }
  const after = 2;
  return after;
}
function tsThrow(x: number): number {
  if (x) {
    throw new Error("x");
  }
  return 5;
}
function tsTry(x: number): number {
  try {
    a();
    if (x) {
      throw new Error("x");
    }
    b();
  } catch (e) {
    return 1;
  } finally {
    done();
  }
  return 2;
}
function tsFinallyOnly(): void {
  try {
    a();
  } finally {
    b();
  }
  c();
}
function tsLabeled(n: number): void {
  outer: for (let i = 0; i < n; i++) {
    for (let j = 0; j < n; j++) {
      if (j === i) continue outer;
      inner();
    }
    tail();
  }
}
function tsSwitch(x: number): number {
  switch (x) {
    case 1:
      a();
    case 2:
      b();
      break;
    default:
      return 0;
  }
  return 1;
}
"#;

const PY_SRC: &str = r#"def elif_chain(x):
    if x == 1:
        a = 1
    elif x == 2:
        return 2
    else:
        return 3
    after = 4
    return after

def raiser(x):
    try:
        y = f(x)
        raise ValueError()
    except ValueError:
        y = 0
    finally:
        cleanup()
    return y

def with_ret(p):
    with open(p) as f:
        return f.read()
    unreachable_tail = 1

def long_chain(x):
    if x == 1:
        r = "a"
    elif x == 2:
        r = "b"
    elif x == 3:
        r = "c"
    else:
        r = "d"
    return r

def matcher(x):
    match x:
        case 1:
            return "one"
    return "other"

def matcher_wild(x):
    match x:
        case 1:
            return "one"
        case _:
            return "any"

def loop_else(xs):
    for x in xs:
        if x:
            break
    else:
        return "none"
    return "found"

def break_in_match(xs):
    for x in xs:
        match x:
            case 0:
                break
        use(x)
    return 1
"#;

const C_SRC: &str = r#"int sw(int x) {
    switch (x) {
        case 1: a(); break;
        default: b(); break;
    }
    return 0;
}

int sw_in_loop(int n) {
    for (int i = 0; i < n; i++) {
        switch (i) {
            case 1: a(); break;
        }
        c();
    }
    return 1;
}

int nodefault(int x) {
    switch (x) {
        case 1: return 1;
    }
    return 0;
}

int fallthrough(int x) {
    switch (x) {
        case 1: a();
        case 2: b(); break;
        default: c();
    }
    return 2;
}

int bare(int x) {
    if (x) return 1; else return 2;
}

int forever(void) {
    for (;;) {
        if (poll()) return 3;
    }
}
"#;

const GO_SRC: &str = "package main

func goReturn(x int) int {
\tif x > 0 {
\t\treturn 1
\t}
\treturn 0
}

func goSwitch(x int) int {
\tswitch x {
\tcase 1:
\t\ta()
\t\tfallthrough
\tcase 2:
\t\tb()
\tcase 3:
\t\tc()
\t}
\treturn 0
}

func goLabeled(n int) {
outer:
\tfor i := 0; i < n; i++ {
\t\tfor j := 0; j < n; j++ {
\t\t\tif j == i {
\t\t\t\tcontinue outer
\t\t\t}
\t\t}
\t\ttail()
\t}
}

func goForever() int {
\tfor {
\t\tif poll() {
\t\t\treturn 1
\t\t}
\t}
}
";

const JAVA_SRC: &str = r#"class J {
    int labeled(int[] xs) {
        outer:
        while (ok()) {
            while (more()) {
                if (stop()) break outer;
                step();
            }
            tail();
        }
        return 0;
    }

    int sw(int x) {
        switch (x) {
            case 1:
                a();
            case 2:
                b();
                break;
            default:
                return 3;
        }
        return 1;
    }

    int tryThrow(int x) {
        try {
            if (x > 0) throw new IllegalStateException();
            work();
        } catch (IllegalStateException e) {
            return -1;
        }
        return 0;
    }

    int arrow(int x) {
        switch (x) {
            case 1 -> a();
            default -> b();
        }
        return 0;
    }
}
"#;

const CPP_SRC: &str = r#"int cpp(int x) {
    try {
        if (x) throw 1;
        work();
    } catch (int e) {
        return e;
    }
    auto f = [](int y) { return y; };
    return f(x);
}
"#;

const PHP_SRC: &str = r#"<?php
function php_chain($x) {
    if ($x == 1) {
        return 1;
    } elseif ($x == 2) {
        return 2;
    } else {
        return 3;
    }
}
function php_switch($x) {
    switch ($x) {
        case 1:
            a();
            break;
    }
    return 0;
}
function php_throw($x) {
    $y = $x ?? throw new Exception("x");
    return $y;
}
"#;

const FIXTURES: &[(&str, &str)] = &[
    ("rust", RUST_SRC),
    ("typescript", TS_SRC),
    ("python", PY_SRC),
    ("c", C_SRC),
    ("go", GO_SRC),
    ("java", JAVA_SRC),
    ("cpp", CPP_SRC),
    ("php", PHP_SRC),
];

// ─── Invariants ──────────────────────────────────────────────────────────────

#[test]
fn every_fixture_function_validates() {
    for &(lang, src) in FIXTURES {
        let cfgs = all_cfgs(lang, src);
        assert!(!cfgs.is_empty(), "{lang}: no functions found");
    }
}

#[test]
fn validate_rejects_unreachable_exit() {
    // The shape the old builder produced for `sw.c`: both `break`s dangled
    // and the join after the switch had no predecessor.
    let block = |id, label: &str, kind| CfgBlock {
        id,
        label: label.to_owned(),
        start_line: 1,
        end_line: 1,
        kind,
    };
    let edge = |from, to, kind| CfgEdge { from, to, kind };
    let cfg = FunctionCfg {
        blocks: vec![
            block(0, "ENTRY", CfgBlockKind::Entry),
            block(1, "EXIT", CfgBlockKind::Exit),
            block(2, "break", CfgBlockKind::Normal),
            block(3, "after_match", CfgBlockKind::Normal),
            block(4, "return", CfgBlockKind::Normal),
        ],
        edges: vec![
            edge(0, 2, CfgEdgeKind::Normal),
            edge(3, 4, CfgEdgeKind::Normal),
            edge(4, 1, CfgEdgeKind::Return),
        ],
    };
    assert_eq!(cfg.validate(), Err(CfgViolation::ExitUnreachable));
}

#[test]
fn validate_rejects_block_without_terminator() {
    let cfg = FunctionCfg {
        blocks: vec![
            CfgBlock {
                id: 0,
                label: "ENTRY".into(),
                start_line: 1,
                end_line: 1,
                kind: CfgBlockKind::Entry,
            },
            CfgBlock {
                id: 1,
                label: "EXIT".into(),
                start_line: 3,
                end_line: 3,
                kind: CfgBlockKind::Exit,
            },
            CfgBlock {
                id: 2,
                label: "stmt".into(),
                start_line: 2,
                end_line: 2,
                kind: CfgBlockKind::Normal,
            },
        ],
        edges: vec![CfgEdge {
            from: 0,
            to: 2,
            kind: CfgEdgeKind::Normal,
        }],
    };
    assert_eq!(
        cfg.validate(),
        Err(CfgViolation::MissingTerminator {
            block: 2,
            label: "stmt".into()
        })
    );

    let mut dangling = cfg;
    dangling.edges.push(CfgEdge {
        from: 2,
        to: 9,
        kind: CfgEdgeKind::Normal,
    });
    assert_eq!(
        dangling.validate(),
        Err(CfgViolation::DanglingEdge { from: 2, to: 9 })
    );
}

#[test]
fn divergent_function_has_no_exit_edges_and_validates() {
    let cfg = cfg_of("rust", RUST_SRC, "diverges");
    assert!(exit_edge_kinds(&cfg).is_empty(), "{}", cfg.format_summary());
    assert!(blocks_labelled(&cfg, "after_loop").is_empty());
}

// ─── Bug 1: returns nested in blocks ─────────────────────────────────────────

#[test]
fn rust_else_block_return_reaches_exit() {
    let cfg = cfg_of("rust", RUST_SRC, "else_return");
    let ret = block(&cfg, "return", 5);
    assert!(has_edge(&cfg, ret, EXIT, CfgEdgeKind::Return));
    let after = block(&cfg, "stmt", 7);
    assert!(reaches(&cfg, ENTRY, after));
    assert!(
        !reaches(&cfg, block(&cfg, "else", 5), after),
        "else fell through"
    );
}

#[test]
fn rust_match_arm_block_return_reaches_exit() {
    let cfg = cfg_of("rust", RUST_SRC, "arm_return");
    let ret = block(&cfg, "return", 14);
    assert!(has_edge(&cfg, ret, EXIT, CfgEdgeKind::Return));
    let none_arm = block(&cfg, "case", 14);
    assert!(!reaches(&cfg, none_arm, block(&cfg, "stmt", 16)));
    assert!(reaches(
        &cfg,
        block(&cfg, "case", 13),
        block(&cfg, "stmt", 16)
    ));
}

#[test]
fn typescript_else_block_return_reaches_exit() {
    let cfg = cfg_of("typescript", TS_SRC, "tsElse");
    assert!(has_edge(
        &cfg,
        block(&cfg, "return", 5),
        EXIT,
        CfgEdgeKind::Return
    ));
    assert!(!reaches(
        &cfg,
        block(&cfg, "else", 5),
        block(&cfg, "stmt", 7)
    ));
}

#[test]
fn python_with_body_return_reaches_exit_and_tail_is_dead() {
    let cfg = cfg_of("python", PY_SRC, "with_ret");
    assert!(has_edge(
        &cfg,
        block(&cfg, "return", 23),
        EXIT,
        CfgEdgeKind::Return
    ));
    assert!(!covers_line(&cfg, 24), "dead tail was modelled");
    assert_eq!(exit_edge_kinds(&cfg), HashSet::from([CfgEdgeKind::Return]));
}

#[test]
fn c_unbraced_branches_are_statements() {
    let cfg = cfg_of("c", C_SRC, "bare");
    assert_eq!(blocks_labelled(&cfg, "return").len(), 2);
    assert_eq!(exit_edge_kinds(&cfg), HashSet::from([CfgEdgeKind::Return]));
}

#[test]
fn go_statement_lists_are_walked() {
    // tree-sitter-go wraps every block's statements in a `statement_list`.
    let cfg = cfg_of("go", GO_SRC, "goReturn");
    assert!(has_edge(
        &cfg,
        block(&cfg, "return", 5),
        EXIT,
        CfgEdgeKind::Return
    ));
    assert_eq!(exit_edge_kinds(&cfg), HashSet::from([CfgEdgeKind::Return]));
}

// ─── Bug 2: every alternative of an elif chain ───────────────────────────────

#[test]
fn python_elif_chain_keeps_final_else() {
    let cfg = cfg_of("python", PY_SRC, "elif_chain");
    let if_block = block(&cfg, "if", 2);
    let elif = block(&cfg, "elif", 4);
    let else_block = block(&cfg, "else", 7);
    assert!(has_edge(&cfg, if_block, elif, CfgEdgeKind::BranchFalse));
    assert!(has_edge(&cfg, elif, else_block, CfgEdgeKind::BranchFalse));
    assert!(has_edge(
        &cfg,
        block(&cfg, "return", 5),
        EXIT,
        CfgEdgeKind::Return
    ));
    assert!(has_edge(
        &cfg,
        block(&cfg, "return", 7),
        EXIT,
        CfgEdgeKind::Return
    ));
    assert!(!reaches(&cfg, elif, block(&cfg, "stmt", 8)));
}

#[test]
fn python_long_elif_chain_has_three_alternatives() {
    let cfg = cfg_of("python", PY_SRC, "long_chain");
    assert_eq!(blocks_labelled(&cfg, "elif").len(), 2);
    assert_eq!(blocks_labelled(&cfg, "else").len(), 1);
    let ret = block(&cfg, "return", 35);
    for line in [28, 30, 32, 34] {
        let arm = block(&cfg, "stmt", line);
        assert!(reaches(&cfg, ENTRY, arm), "L{line} unreachable");
        assert!(reaches(&cfg, arm, ret), "L{line} does not reach the return");
    }
}

#[test]
fn php_elseif_chain_is_an_elif_chain() {
    let cfg = cfg_of("php", PHP_SRC, "php_chain");
    assert_eq!(blocks_labelled(&cfg, "elif").len(), 1);
    assert_eq!(blocks_labelled(&cfg, "return").len(), 3);
    assert_eq!(exit_edge_kinds(&cfg), HashSet::from([CfgEdgeKind::Return]));
}

// ─── Bug 3: switch break targets and `otherwise` edges ───────────────────────

#[test]
fn c_switch_breaks_leave_the_switch() {
    let cfg = cfg_of("c", C_SRC, "sw");
    let after = block(&cfg, "after_match", 5);
    for brk in blocks_labelled(&cfg, "break") {
        assert_eq!(edge_targets(&cfg, brk, CfgEdgeKind::Break), vec![after]);
    }
    assert!(reaches(&cfg, ENTRY, EXIT));
    let switch = block(&cfg, "match", 2);
    assert!(
        edge_targets(&cfg, switch, CfgEdgeKind::BranchFalse).is_empty(),
        "a switch with a default has no otherwise edge"
    );
}

#[test]
fn c_switch_without_default_has_otherwise_edge() {
    let cfg = cfg_of("c", C_SRC, "nodefault");
    let switch = block(&cfg, "match", 20);
    let after = block(&cfg, "after_match", 22);
    assert!(has_edge(&cfg, switch, after, CfgEdgeKind::BranchFalse));
    assert!(reaches(&cfg, ENTRY, block(&cfg, "return", 23)));
}

#[test]
fn c_break_in_switch_inside_loop_stays_in_the_loop() {
    let cfg = cfg_of("c", C_SRC, "sw_in_loop");
    let brk = block(&cfg, "break", 12);
    let targets = edge_targets(&cfg, brk, CfgEdgeKind::Break);
    assert_eq!(targets, vec![block(&cfg, "after_match", 13)]);
    assert!(reaches(&cfg, brk, block(&cfg, "stmt", 14)));
}

#[test]
fn c_cases_fall_through_until_break() {
    let cfg = cfg_of("c", C_SRC, "fallthrough");
    let case_two = block(&cfg, "case", 29);
    assert!(has_edge(
        &cfg,
        block(&cfg, "stmt", 28),
        case_two,
        CfgEdgeKind::Normal
    ));
    let after = block(&cfg, "after_match", 31);
    assert!(has_edge(
        &cfg,
        block(&cfg, "stmt", 30),
        after,
        CfgEdgeKind::Normal
    ));
    assert!(!has_edge(
        &cfg,
        block(&cfg, "match", 27),
        after,
        CfgEdgeKind::BranchFalse
    ));
}

#[test]
fn typescript_switch_falls_through_and_breaks_out() {
    let cfg = cfg_of("typescript", TS_SRC, "tsSwitch");
    assert!(has_edge(
        &cfg,
        block(&cfg, "stmt", 50),
        block(&cfg, "case", 51),
        CfgEdgeKind::Normal
    ));
    let after = block(&cfg, "after_match", 56);
    assert_eq!(
        edge_targets(&cfg, block(&cfg, "break", 53), CfgEdgeKind::Break),
        vec![after]
    );
}

#[test]
fn java_switch_groups_fall_through_but_rules_do_not() {
    let groups = cfg_of("java", JAVA_SRC, "sw");
    assert!(has_edge(
        &groups,
        block(&groups, "stmt", 17),
        block(&groups, "case", 18),
        CfgEdgeKind::Normal
    ));
    let rules = cfg_of("java", JAVA_SRC, "arrow");
    assert!(!reaches(
        &rules,
        block(&rules, "stmt", 39),
        block(&rules, "case", 40)
    ));
}

#[test]
fn go_cases_fall_through_only_explicitly() {
    let cfg = cfg_of("go", GO_SRC, "goSwitch");
    let case_two = block(&cfg, "case", 15);
    let case_three = block(&cfg, "case", 17);
    assert!(has_edge(
        &cfg,
        block(&cfg, "stmt", 13),
        case_two,
        CfgEdgeKind::Normal
    ));
    assert!(!has_edge(
        &cfg,
        block(&cfg, "stmt", 16),
        case_three,
        CfgEdgeKind::Normal
    ));
    let after = block(&cfg, "after_match", 19);
    assert!(has_edge(
        &cfg,
        block(&cfg, "match", 11),
        after,
        CfgEdgeKind::BranchFalse
    ));
}

#[test]
fn python_match_otherwise_edge_needs_no_wildcard() {
    let open = cfg_of("python", PY_SRC, "matcher");
    let switch = block(&open, "match", 38);
    assert_eq!(
        edge_targets(&open, switch, CfgEdgeKind::BranchFalse).len(),
        1
    );
    assert!(reaches(&open, ENTRY, block(&open, "return", 41)));

    let closed = cfg_of("python", PY_SRC, "matcher_wild");
    assert!(blocks_labelled(&closed, "after_match").is_empty());
    assert_eq!(
        exit_edge_kinds(&closed),
        HashSet::from([CfgEdgeKind::Return])
    );
}

#[test]
fn python_break_in_match_leaves_the_loop() {
    let cfg = cfg_of("python", PY_SRC, "break_in_match");
    let brk = block(&cfg, "break", 62);
    assert_eq!(
        edge_targets(&cfg, brk, CfgEdgeKind::Break),
        vec![block(&cfg, "after_loop", 63)]
    );
    assert!(!reaches(&cfg, brk, block(&cfg, "stmt", 63)));
}

#[test]
fn php_switch_break_and_otherwise() {
    let cfg = cfg_of("php", PHP_SRC, "php_switch");
    let after = block(&cfg, "after_match", 16);
    assert_eq!(
        edge_targets(&cfg, block(&cfg, "break", 15), CfgEdgeKind::Break),
        vec![after]
    );
    assert!(has_edge(
        &cfg,
        block(&cfg, "match", 12),
        after,
        CfgEdgeKind::BranchFalse
    ));
}

// ─── Bug 4: labeled break/continue ───────────────────────────────────────────

#[test]
fn rust_labeled_break_leaves_the_named_loop() {
    let cfg = cfg_of("rust", RUST_SRC, "labeled");
    let brk = block(&cfg, "break", 34);
    let targets = edge_targets(&cfg, brk, CfgEdgeKind::Break);
    assert_eq!(targets, vec![block(&cfg, "after_loop", 37)]);
    assert!(
        !reaches(&cfg, brk, block(&cfg, "stmt", 36)),
        "break hit the inner loop"
    );
    assert!(reaches(&cfg, brk, block(&cfg, "stmt", 38)));
}

#[test]
fn rust_labeled_block_break_skips_the_rest() {
    let cfg = cfg_of("rust", RUST_SRC, "labeled_block");
    let brk = block(&cfg, "break", 61);
    let after = block(&cfg, "after_label", 63);
    assert_eq!(edge_targets(&cfg, brk, CfgEdgeKind::Break), vec![after]);
    assert!(!reaches(&cfg, brk, block(&cfg, "stmt", 62)));
    assert!(reaches(&cfg, after, block(&cfg, "stmt", 64)));
}

#[test]
fn typescript_labeled_continue_targets_the_named_loop() {
    let cfg = cfg_of("typescript", TS_SRC, "tsLabeled");
    let cont = block(&cfg, "continue", 41);
    let outer = block(&cfg, "loop_header", 39);
    assert_eq!(edge_targets(&cfg, cont, CfgEdgeKind::Continue), vec![outer]);
}

#[test]
fn java_labeled_break_leaves_the_named_loop() {
    let cfg = cfg_of("java", JAVA_SRC, "labeled");
    let brk = block(&cfg, "break", 6);
    assert_eq!(
        edge_targets(&cfg, brk, CfgEdgeKind::Break),
        vec![block(&cfg, "after_loop", 10)]
    );
    assert!(!reaches(&cfg, brk, block(&cfg, "stmt", 9)));
}

#[test]
fn go_labeled_continue_targets_the_named_loop() {
    let cfg = cfg_of("go", GO_SRC, "goLabeled");
    let cont = block(&cfg, "continue", 28);
    assert_eq!(
        edge_targets(&cfg, cont, CfgEdgeKind::Continue),
        vec![block(&cfg, "loop_header", 25)]
    );
}

// ─── Bug 5: abrupt exits ─────────────────────────────────────────────────────

#[test]
fn rust_question_mark_is_a_conditional_return() {
    let cfg = cfg_of("rust", RUST_SRC, "try_op");
    let stmt = block(&cfg, "stmt", 21);
    assert!(has_edge(&cfg, stmt, EXIT, CfgEdgeKind::Return));
    // The `?` ends its block; the happy path continues in the next one.
    let next = block(&cfg, "stmt", 22);
    assert_ne!(stmt, next);
    assert!(has_edge(&cfg, stmt, next, CfgEdgeKind::Normal));
}

#[test]
fn rust_return_inside_let_match_is_a_return_edge() {
    let cfg = cfg_of("rust", RUST_SRC, "let_else_return");
    assert!(has_edge(
        &cfg,
        block(&cfg, "stmt", 27),
        EXIT,
        CfgEdgeKind::Return
    ));
}

#[test]
fn rust_let_else_continue_is_a_continue_edge() {
    let cfg = cfg_of("rust", RUST_SRC, "let_else_continue");
    let header = block(&cfg, "loop_header", 43);
    let stmt = block(&cfg, "stmt", 44);
    assert!(has_edge(&cfg, stmt, header, CfgEdgeKind::Continue));
}

#[test]
fn rust_closure_return_is_not_a_function_exit() {
    let cfg = cfg_of("rust", RUST_SRC, "closure_return_stays_local");
    assert_eq!(exit_edge_kinds(&cfg), HashSet::from([CfgEdgeKind::Normal]));
}

#[test]
fn typescript_throw_outside_try_exits_the_function() {
    let cfg = cfg_of("typescript", TS_SRC, "tsThrow");
    let throw = block(&cfg, "throw", 12);
    assert_eq!(
        edge_targets(&cfg, throw, CfgEdgeKind::Exception),
        vec![EXIT]
    );
    assert!(!reaches(&cfg, throw, block(&cfg, "return", 14)));
}

#[test]
fn typescript_throw_inside_try_reaches_the_catch() {
    let cfg = cfg_of("typescript", TS_SRC, "tsTry");
    let catch = block(&cfg, "catch", 24);
    let throw = block(&cfg, "throw", 20);
    assert_eq!(
        edge_targets(&cfg, throw, CfgEdgeKind::Exception),
        vec![catch]
    );
    // Statements inside the try body may throw too, not just its header.
    assert!(has_edge(
        &cfg,
        block(&cfg, "stmt", 18),
        catch,
        CfgEdgeKind::Exception
    ));
    let finally = block(&cfg, "finally", 26);
    assert!(reaches(&cfg, block(&cfg, "stmt", 22), finally));
    assert!(reaches(&cfg, finally, block(&cfg, "return", 28)));
}

#[test]
fn typescript_finally_without_catch_rethrows() {
    let cfg = cfg_of("typescript", TS_SRC, "tsFinallyOnly");
    let finally = block(&cfg, "finally", 34);
    assert!(has_edge(
        &cfg,
        block(&cfg, "stmt", 32),
        finally,
        CfgEdgeKind::Exception
    ));
    let finally_body = block(&cfg, "stmt", 34);
    assert!(has_edge(&cfg, finally_body, EXIT, CfgEdgeKind::Exception));
    assert!(reaches(&cfg, finally_body, block(&cfg, "stmt", 36)));
}

#[test]
fn python_raise_in_try_reaches_except_then_finally() {
    let cfg = cfg_of("python", PY_SRC, "raiser");
    let except = block(&cfg, "catch", 16);
    let raise = block(&cfg, "throw", 14);
    assert_eq!(
        edge_targets(&cfg, raise, CfgEdgeKind::Exception),
        vec![except]
    );
    assert!(has_edge(
        &cfg,
        block(&cfg, "stmt", 13),
        except,
        CfgEdgeKind::Exception
    ));
    let finally = block(&cfg, "finally", 18);
    assert!(reaches(&cfg, except, finally));
    assert!(reaches(&cfg, finally, block(&cfg, "return", 19)));
}

#[test]
fn python_loop_else_runs_only_without_break() {
    let cfg = cfg_of("python", PY_SRC, "loop_else");
    let brk = block(&cfg, "break", 53);
    let else_block = block(&cfg, "else", 55);
    assert!(!reaches(&cfg, brk, else_block));
    assert!(reaches(&cfg, block(&cfg, "loop_header", 51), else_block));
    assert!(reaches(&cfg, brk, block(&cfg, "return", 56)));
}

#[test]
fn java_throw_inside_try_reaches_the_catch() {
    let cfg = cfg_of("java", JAVA_SRC, "tryThrow");
    let catch = block(&cfg, "catch", 32);
    assert_eq!(
        edge_targets(&cfg, block(&cfg, "throw", 29), CfgEdgeKind::Exception),
        vec![catch]
    );
}

#[test]
fn cpp_throw_reaches_catch_and_lambda_return_stays_local() {
    let cfg = cfg_of("cpp", CPP_SRC, "cpp");
    let catch = block(&cfg, "catch", 6);
    assert_eq!(
        edge_targets(&cfg, block(&cfg, "throw", 3), CfgEdgeKind::Exception),
        vec![catch]
    );
    assert!(!has_edge(
        &cfg,
        block(&cfg, "stmt", 8),
        EXIT,
        CfgEdgeKind::Return
    ));
}

#[test]
fn php_throw_expression_exits_the_function() {
    let cfg = cfg_of("php", PHP_SRC, "php_throw");
    let stmt = block(&cfg, "stmt", 20);
    assert!(has_edge(&cfg, stmt, EXIT, CfgEdgeKind::Exception));
    assert!(reaches(&cfg, stmt, block(&cfg, "return", 21)));
}

#[test]
fn headerless_loops_never_exit_through_the_header() {
    for (lang, src, name, header_line) in
        [("c", C_SRC, "forever", 40), ("go", GO_SRC, "goForever", 36)]
    {
        let cfg = cfg_of(lang, src, name);
        let header = block(&cfg, "loop", header_line);
        assert!(edge_targets(&cfg, header, CfgEdgeKind::BranchFalse).is_empty());
        assert_eq!(exit_edge_kinds(&cfg), HashSet::from([CfgEdgeKind::Return]));
    }
}

// ─── Baseline shapes ─────────────────────────────────────────────────────────

fn parse_rust(src: &str) -> Tree {
    parse("rust", src)
}

fn find_function(tree: &Tree) -> TsNode<'_> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() == "function_item" {
            return child;
        }
    }
    panic!("no function_item found in source");
}

#[test]
fn cfg_simple_function_no_branches() {
    let src = r#"
fn foo() {
    let x = 1;
    let y = 2;
    let z = x + y;
}
"#;
    let tree = parse_rust(src);
    let func = find_function(&tree);
    let cfg = build_cfg(func, src.as_bytes(), "rust").unwrap();

    // Should have ENTRY, EXIT, and one statement block.
    assert!(
        cfg.blocks.len() >= 3,
        "expected >=3 blocks, got {}",
        cfg.blocks.len()
    );
    assert_eq!(cfg.blocks[0].kind, CfgBlockKind::Entry);
    assert_eq!(cfg.blocks[1].kind, CfgBlockKind::Exit);

    // At least 2 Normal edges: ENTRY→stmt, stmt→EXIT.
    let normal_edges: Vec<_> = cfg
        .edges
        .iter()
        .filter(|e| e.kind == CfgEdgeKind::Normal)
        .collect();
    assert!(
        normal_edges.len() >= 2,
        "expected >=2 normal edges, got {}",
        normal_edges.len()
    );
}

#[test]
fn cfg_if_else() {
    let src = r#"
fn foo(x: i32) {
    if x > 0 {
        let a = 1;
    } else {
        let b = 2;
    }
}
"#;
    let tree = parse_rust(src);
    let func = find_function(&tree);
    let cfg = build_cfg(func, src.as_bytes(), "rust").unwrap();

    // Should have a Branch block.
    let branch_blocks: Vec<_> = cfg
        .blocks
        .iter()
        .filter(|b| b.kind == CfgBlockKind::Branch)
        .collect();
    assert!(
        !branch_blocks.is_empty(),
        "expected at least one Branch block"
    );

    // Should have BranchFalse edge (from branch to else path).
    let false_edges: Vec<_> = cfg
        .edges
        .iter()
        .filter(|e| e.kind == CfgEdgeKind::BranchFalse)
        .collect();
    assert!(
        !false_edges.is_empty(),
        "expected at least one BranchFalse edge"
    );
    assert!(cfg.edges.iter().any(|e| e.kind == CfgEdgeKind::BranchTrue));
}

#[test]
fn cfg_for_loop() {
    let src = r#"
fn foo() {
    for i in 0..10 {
        let x = i;
    }
}
"#;
    let tree = parse_rust(src);
    let func = find_function(&tree);
    let cfg = build_cfg(func, src.as_bytes(), "rust").unwrap();

    // Should have a Loop block.
    let loop_blocks: Vec<_> = cfg
        .blocks
        .iter()
        .filter(|b| b.kind == CfgBlockKind::Loop)
        .collect();
    assert!(!loop_blocks.is_empty(), "expected at least one Loop block");

    // Should have a LoopBack edge.
    let loopback_edges: Vec<_> = cfg
        .edges
        .iter()
        .filter(|e| e.kind == CfgEdgeKind::LoopBack)
        .collect();
    assert!(
        !loopback_edges.is_empty(),
        "expected at least one LoopBack edge"
    );
}

#[test]
fn cfg_while_loop() {
    let src = r#"
fn foo() {
    let mut x = 0;
    while x < 10 {
        x += 1;
    }
}
"#;
    let tree = parse_rust(src);
    let func = find_function(&tree);
    let cfg = build_cfg(func, src.as_bytes(), "rust").unwrap();

    // Should have a Loop block.
    let loop_blocks: Vec<_> = cfg
        .blocks
        .iter()
        .filter(|b| b.kind == CfgBlockKind::Loop)
        .collect();
    assert!(!loop_blocks.is_empty(), "expected at least one Loop block");

    // Should have a LoopBack edge.
    let loopback_edges: Vec<_> = cfg
        .edges
        .iter()
        .filter(|e| e.kind == CfgEdgeKind::LoopBack)
        .collect();
    assert!(
        !loopback_edges.is_empty(),
        "expected at least one LoopBack edge"
    );

    // Should have BranchFalse edge (condition false → after loop).
    let false_edges: Vec<_> = cfg
        .edges
        .iter()
        .filter(|e| e.kind == CfgEdgeKind::BranchFalse)
        .collect();
    assert!(
        !false_edges.is_empty(),
        "expected BranchFalse edge for while condition"
    );
}

#[test]
fn cfg_match_expression() {
    let src = r#"
fn foo(x: i32) {
    match x {
        1 => { let a = 1; }
        2 => { let b = 2; }
        _ => { let c = 3; }
    }
}
"#;
    let tree = parse_rust(src);
    let func = find_function(&tree);
    let cfg = build_cfg(func, src.as_bytes(), "rust").unwrap();

    // Should have a Branch block for the match.
    let branch_blocks: Vec<_> = cfg
        .blocks
        .iter()
        .filter(|b| b.kind == CfgBlockKind::Branch)
        .collect();
    assert!(!branch_blocks.is_empty(), "expected Branch block for match");

    // The branch block should have multiple outgoing edges (one per arm).
    let branch_id = branch_blocks[0].id;
    let outgoing: Vec<_> = cfg.edges.iter().filter(|e| e.from == branch_id).collect();
    assert!(
        outgoing.len() >= 3,
        "expected >=3 edges from match branch, got {}",
        outgoing.len()
    );
}

#[test]
fn cfg_return_statement() {
    let src = r#"
fn foo(x: i32) -> i32 {
    if x > 0 {
        return x;
    }
    0
}
"#;
    let tree = parse_rust(src);
    let func = find_function(&tree);
    let cfg = build_cfg(func, src.as_bytes(), "rust").unwrap();

    // Should have a Return edge to EXIT.
    let return_edges: Vec<_> = cfg
        .edges
        .iter()
        .filter(|e| e.kind == CfgEdgeKind::Return)
        .collect();
    assert!(
        !return_edges.is_empty(),
        "expected at least one Return edge"
    );

    // The Return edge should point to the EXIT block (id=1).
    assert!(
        return_edges.iter().any(|e| e.to == 1),
        "Return edge should point to EXIT (id=1)"
    );
}

#[test]
fn cfg_nested_if_in_loop() {
    let src = r#"
fn foo() {
    for i in 0..10 {
        if i > 5 {
            let x = i;
        }
    }
}
"#;
    let tree = parse_rust(src);
    let func = find_function(&tree);
    let cfg = build_cfg(func, src.as_bytes(), "rust").unwrap();

    // Should have both Loop and Branch blocks.
    let loop_blocks: Vec<_> = cfg
        .blocks
        .iter()
        .filter(|b| b.kind == CfgBlockKind::Loop)
        .collect();
    let branch_blocks: Vec<_> = cfg
        .blocks
        .iter()
        .filter(|b| b.kind == CfgBlockKind::Branch)
        .collect();

    assert!(!loop_blocks.is_empty(), "expected Loop block");
    assert!(
        !branch_blocks.is_empty(),
        "expected Branch block inside loop"
    );

    // Should have both LoopBack and BranchFalse edges.
    assert!(cfg.edges.iter().any(|e| e.kind == CfgEdgeKind::LoopBack));
    assert!(cfg.edges.iter().any(|e| e.kind == CfgEdgeKind::BranchFalse));
}

#[test]
fn cfg_rust_loop_with_break() {
    let src = r#"
fn foo() {
    loop {
        break;
    }
}
"#;
    let tree = parse_rust(src);
    let func = find_function(&tree);
    let cfg = build_cfg(func, src.as_bytes(), "rust").unwrap();

    // Should have a Loop block (for infinite loop).
    let loop_blocks: Vec<_> = cfg
        .blocks
        .iter()
        .filter(|b| b.kind == CfgBlockKind::Loop)
        .collect();
    assert!(!loop_blocks.is_empty(), "expected Loop block for `loop`");

    // Should have a Break edge.
    let break_edges: Vec<_> = cfg
        .edges
        .iter()
        .filter(|e| e.kind == CfgEdgeKind::Break)
        .collect();
    assert!(!break_edges.is_empty(), "expected Break edge");
}

#[test]
fn cfg_returns_none_for_non_function() {
    let src = r#"
struct Foo {
    x: i32,
}
"#;
    let tree = parse_rust(src);
    let root = tree.root_node();
    let mut cursor = root.walk();
    let struct_node = root
        .named_children(&mut cursor)
        .find(|c| c.kind() == "struct_item")
        .unwrap();

    let result = build_cfg(struct_node, src.as_bytes(), "rust");
    assert!(result.is_none());
}

#[test]
fn cfg_continue_statement() {
    let src = r#"
fn foo() {
    for i in 0..10 {
        if i == 5 {
            continue;
        }
        let x = i;
    }
}
"#;
    let tree = parse_rust(src);
    let func = find_function(&tree);
    let cfg = build_cfg(func, src.as_bytes(), "rust").unwrap();

    // Should have a Continue edge pointing back to loop header.
    let continue_edges: Vec<_> = cfg
        .edges
        .iter()
        .filter(|e| e.kind == CfgEdgeKind::Continue)
        .collect();
    assert!(!continue_edges.is_empty(), "expected Continue edge");

    // The Continue edge target should be a Loop block.
    for ce in &continue_edges {
        let target = &cfg.blocks[ce.to as usize];
        assert_eq!(
            target.kind,
            CfgBlockKind::Loop,
            "Continue should target a Loop block"
        );
    }
}
