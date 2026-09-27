//! Each rule, per language: the bug shape fires, the look-alikes that are
//! fine do not.

use super::*;
use crate::analyze::bugs::FnSpan;
use crate::extraction::create_parser;
use crate::types::Language;

fn lint(language: Language, source: &str) -> Vec<(&'static str, u32)> {
    let rules = rules::for_language(language).expect("rules");
    let mut parser = create_parser(language).expect("parser");
    let tree = parser.parse(source, None).expect("tree");
    let mut out: Vec<_> = lint_source(rules, source, tree.root_node(), &|_| false)
        .into_iter()
        .map(|raw| (raw.rule, raw.line))
        .collect();
    out.sort();
    out
}

fn rule_lines(found: &[(&'static str, u32)], rule: &str) -> Vec<u32> {
    found
        .iter()
        .filter(|(r, _)| *r == rule)
        .map(|(_, line)| *line)
        .collect()
}

// ---------------------------------------------------------------- loops

#[test]
fn rust_loop_without_progress_fires_and_progressing_loops_do_not() {
    let source = r#"
fn stuck(n: usize) -> usize {
    let mut i = 0;
    let mut total = 0;
    while i < n {
        total += 2;
    }
    total
}
fn counts(n: usize) {
    let mut i = 0;
    while i < n { i += 1; }
}
fn polls(done: &mut bool) {
    while !*done { poll(done); }
}
fn method(v: &mut Vec<u8>) {
    let mut len = 0;
    while len < 3 { v.push(1); len = v.len(); }
}
fn breaks(n: usize) {
    let i = 0;
    while i < n { if ready() { break; } }
}
fn tries(n: usize) -> Result<(), E> {
    let i = 0;
    while i < n { step()?; }
    Ok(())
}
fn receiver(&mut self) {
    while self.pos < self.len { self.advance(); }
}
fn macro_exit(n: usize) {
    let i = 0;
    while i < n { bail!("stop"); }
}
fn infinite() {
    while true { work(); }
}
fn with_let(it: &mut I) {
    while let Some(x) = it.next() { use_it(x); }
}
fn call_in_condition(q: &Q) {
    while q.is_empty() { wait(); }
}
fn borrowed(mut n: usize) {
    while n > 0 { shrink(&mut n); }
}
fn async_wait(flag: bool) {
    let f = flag;
    while f { sleep().await; }
}
"#;
    let found = lint(Language::Rust, source);
    assert_eq!(rule_lines(&found, "loop-no-progress"), vec![5]);
}

#[test]
fn rust_closure_capturing_the_condition_suppresses_the_loop_rule() {
    let source = r#"
fn f() {
    let mut i = 0;
    let bump = || i += 1;
    while i < 10 { other(); }
}
"#;
    assert!(rule_lines(&lint(Language::Rust, source), "loop-no-progress").is_empty());
}

#[test]
fn ts_loop_rule_needs_locals_and_gives_up_on_fields_behind_calls() {
    let source = r#"
function stuck(items: number[]) {
  let i = 0;
  while (i < items.length) {
    console.log(items[0]);
  }
}
function progress(items: number[]) {
  let i = 0;
  while (i < 10) { i++; }
}
function field() {
  while (this.running) { tick(); }
}
function global() {
  while (running) { tick(); }
}
function awaits() {
  let done = false;
  while (!done) { await next(); }
}
function nodeWalk(node) {
  while (node !== null) {
    visit(other);
  }
}
function fieldPathCall(node) {
  while (node.next !== null) {
    visit(other);
  }
}
function doWhile() {
  let k = 0;
  do { total += 1; } while (k < 3);
}
function throwing(x) {
  while (x > 0) { throw new Error("no"); }
}
"#;
    let found = lint(Language::Typescript, source);
    // `items.length` behind `console.log(...)`: a field path with a call — skipped.
    assert_eq!(rule_lines(&found, "loop-no-progress"), vec![23, 34]);
}

#[test]
fn python_go_java_loops() {
    let py = r#"
def stuck(n):
    i = 0
    while i < n:
        print(n)
def moves(node):
    while node:
        node = node.next
def global_flag():
    while running:
        step()
def spins(self):
    while not self.done:
        pass
"#;
    // `print(n)` names `n` in a call: skipped, conservatively.
    assert!(rule_lines(&lint(Language::Python, py), "loop-no-progress").is_empty());
    let py2 = r#"
def stuck(n):
    i = 0
    total = 0
    while i < n:
        total += 1
"#;
    assert_eq!(
        rule_lines(&lint(Language::Python, py2), "loop-no-progress"),
        vec![5]
    );

    let go = r#"
package p
func stuck(n int) int {
	i := 0
	t := 0
	for i < n {
		t++
	}
	return t
}
func ok(n int) {
	for i := 0; i < n; i++ {
	}
	j := 0
	for j < n {
		j++
	}
	for {
		work()
	}
}
func goroutine() {
	done := false
	go func() { done = true }()
	for !done {
	}
}
func pointer() {
	n := 3
	p := &n
	for n > 0 {
		*p--
	}
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Go, go), "loop-no-progress"),
        vec![6]
    );

    let java = r#"
class A {
  int stuck(int n) {
    int i = 0;
    int t = 0;
    while (i < n) { t++; }
    return t;
  }
  void field() {
    while (!ready) { lock.wait(); }
  }
  void ok(int n) {
    int i = 0;
    while (i < n) { i += 1; }
  }
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Java, java), "loop-no-progress"),
        vec![6]
    );
}

// ---------------------------------------------------------------- branches

#[test]
fn identical_if_else_branches_fire_across_languages() {
    let rust = r#"
fn f(a: bool) -> i32 {
    if a { compute(1) } else { compute(1) }
}
fn g(a: bool) -> i32 {
    if a { compute(1) } else { compute(2) }
}
fn h(a: bool, b: bool) {
    if a { x() } else if b { y() } else { y() }
}
fn empty(a: bool) {
    if a {} else {}
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Rust, rust), "identical-branches"),
        vec![3, 9]
    );

    let ts = r#"
function f(a) {
  if (a) { run(1); } else { run(1); }
  if (a) { run(1); } else { run(2); }
  const v = a ? x.y : x.y;
  const w = a ? x.y : x.z;
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Typescript, ts), "identical-branches"),
        vec![3, 5]
    );

    let py = r#"
def f(a, b):
    if a:
        run(1)
    elif b:
        run(2)
    else:
        run(2)
    v = x if a else x
    if a:
        run(1)  # one
    else:
        run(1)  # two
"#;
    assert_eq!(
        rule_lines(&lint(Language::Python, py), "identical-branches"),
        vec![3, 9, 10]
    );

    let go = r#"
package p
func f(a bool) {
	if a {
		run(1)
	} else {
		run(1)
	}
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Go, go), "identical-branches"),
        vec![4]
    );

    let java = r#"
class A { void f(boolean a) { if (a) { run(1); } else { run(1); } int v = a ? x.y : x.y; int w = a ? 1 : 1; } }
"#;
    assert_eq!(
        rule_lines(&lint(Language::Java, java), "identical-branches"),
        vec![2, 2]
    );
}

#[test]
fn identical_arms_fire_when_the_body_names_the_other_arms_case() {
    let rust = r#"
fn f(k: Kind) -> R {
    match k {
        Kind::Plus => Op::Plus,
        Kind::Minus => Op::Plus,
        Kind::Times => apply(state, "times"),
        Kind::Divide => apply(state, "times"),
        Kind::Left | Kind::Right => shift(state),
        Kind::Up => shift(state),
        Kind::Some(v) => convert(v, "some"),
        Kind::Other(v) => convert(v, "some"),
        Kind::Gone => unreachable!("gone"),
        Kind::Lost => unreachable!("gone"),
        Kind::Mode => Op::Mode,
        Kind::Star => Op::Mode,
        _ => Op::Plus,
    }
}
enum Op { Plus, Minus, Mode }
fn minus() -> Op { Op::Minus }
"#;
    // `Divide => apply(state, "times")`: the case is only in a string, and
    // `Star => Op::Mode`: no `Op::Star` exists — neither is a proven slip.
    assert_eq!(
        rule_lines(&lint(Language::Rust, rust), "identical-branches"),
        vec![5]
    );

    let ts = r#"
function f(k) {
  switch (k) {
    case "open": openFile(a); break;
    case "close": openFile(a); break;
    case "save": run(a); break;
    case "load": run(a); break;
    case 3:
    case 4: return 1;
    case "fail": throw new Error("fail");
    case "boom": throw new Error("fail");
    default: openFile(a); break;
  }
}
function closeFile(a) {}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Typescript, ts), "identical-branches"),
        vec![5]
    );

    let go = r#"
package p
func f(k string, v any) {
	switch k {
	case "open":
		openFile(a)
	case "close":
		openFile(a)
	}
	switch x := v.(type) {
	case int:
		showInt(x)
	case string:
		showInt(x)
	}
}
func closeFile(a int) {}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Go, go), "identical-branches"),
        vec![7]
    );

    let java = r#"
class A { void f(Kind k) {
  switch (k) {
    case OPEN: openFile(a); break;
    case CLOSE: openFile(a); break;
  }
  switch (k) {
    case OPEN -> openFile(a);
    case CLOSE -> closeFile(a);
  }
} }
"#;
    assert_eq!(
        rule_lines(&lint(Language::Java, java), "identical-branches"),
        vec![5]
    );

    let py = r#"
def f(k):
    match k:
        case "open":
            open_file(a)
        case "close":
            open_file(a)
        case Point(x=px):
            show_point(px)
        case Line(x=px):
            show_point(px)
        case _:
            open_file(a)
def close_file(a):
    pass
"#;
    assert_eq!(
        rule_lines(&lint(Language::Python, py), "identical-branches"),
        vec![6]
    );
}

// ---------------------------------------------------------------- comparisons

#[test]
fn self_comparison_fires_on_pure_operands_only() {
    let rust = r#"
fn f(a: &S, x: f64) -> bool {
    let d = x - x;
    let nan = x != x;
    let calls = next() == next();
    let other = a.b == a.c;
    a.b == a.b || (a.ok && a.ok)
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Rust, rust), "self-comparison"),
        vec![3, 7, 7]
    );

    let ts = r#"
function f(a) {
  if (a.b === a.b) {}
  if (a !== a) {}
  if (a === a) {}
  if (a[i] < a[i]) {}
  if (f() === f()) {}
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Typescript, ts), "self-comparison"),
        vec![3, 6]
    );

    let py = "def f(a, x):\n    return a.b == a.b or x != x or 1 == 1 or (a < a < b)\n";
    assert_eq!(
        rule_lines(&lint(Language::Python, py), "self-comparison"),
        vec![2]
    );

    let go = "package p\nfunc f(a T) bool { return a.n == a.n || a != a || a == a }\n";
    assert_eq!(
        rule_lines(&lint(Language::Go, go), "self-comparison"),
        vec![2]
    );

    let java = "class A { boolean f(int a) { return a >= a; } }\n";
    assert_eq!(
        rule_lines(&lint(Language::Java, java), "self-comparison"),
        vec![1]
    );
}

#[test]
fn constant_conditions_skip_bare_switches_loops_and_constants() {
    let rust = r#"
fn f(x: bool) {
    if 1 == 1 { a(); }
    if false && x { b(); }
    if x || true { c(); }
    if true { d(); }
    if false { e(); }
    if cfg!(windows) { g(); }
    if DEBUG && x { h(); }
    if !true { i(); }
    while true { j(); }
    if Self::MAX > 3 { k(); }
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Rust, rust), "constant-condition"),
        vec![3, 4, 5]
    );

    let ts = r#"
function f(x) {
  if (1 === 1) {}
  const v = false && x ? 1 : 2;
  if (0) {}
  do {} while (false);
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Typescript, ts), "constant-condition"),
        vec![3, 4]
    );

    let py = "def f(x):\n    if 0:\n        a()\n    elif False and x:\n        b()\n    if 2 > 1:\n        c()\n";
    assert_eq!(
        rule_lines(&lint(Language::Python, py), "constant-condition"),
        vec![4, 6]
    );
}

// ---------------------------------------------------------------- stores

#[test]
fn rust_dead_store_rules() {
    let source = r#"
fn dead() {
    let mut x = compute(1);
    x = compute(2);
    use_it(x);
}
fn read_between() {
    let mut x = compute(1);
    log(x);
    x = compute(2);
}
fn read_in_value() {
    let mut x = compute(1);
    x = x + compute(2);
}
fn default_first() {
    let mut x = Vec::new();
    x = compute(2);
    let mut y = 0;
    y = compute(3);
    let mut z = None;
    z = compute(4);
}
fn shadowed() {
    let x = compute(1);
    let x = compute(2);
}
fn format_read() {
    let mut x = compute(1);
    println!("{x}");
    x = compute(2);
}
fn loop_between() {
    let mut x = compute(1);
    for i in 0..3 { other(i); }
    x = compute(2);
}
fn cfg_between() {
    let mut x = compute(1);
    #[cfg(windows)]
    x = compute(2);
}
fn underscore() {
    let mut _g = lock(1);
    _g = lock(2);
}
fn assigned_twice(a: i32) {
    let mut x;
    x = compute(a);
    x = compute(3);
}
"#;
    let found = lint(Language::Rust, source);
    assert_eq!(rule_lines(&found, "dead-store"), vec![3, 49]);
}

#[test]
fn dead_store_other_languages() {
    let ts = r#"
function f() {
  let x = compute(1);
  x = compute(2);
  let y = compute(1);
  g(() => y);
  y = compute(2);
  let z = compute(1);
  try {
    z = compute(3);
    z = compute(4);
  } catch (e) { log(z); }
}
x = compute(1);
x = compute(2);
"#;
    assert_eq!(
        rule_lines(&lint(Language::Typescript, ts), "dead-store"),
        vec![3]
    );

    let py = r#"
def f():
    x = compute(1)
    x = compute(2)
    y = None
    y = compute(2)
    z = compute(1)
    z = compute(z)
def g():
    global w
    w = compute(1)
    w = compute(2)
"#;
    assert_eq!(
        rule_lines(&lint(Language::Python, py), "dead-store"),
        vec![3]
    );

    let go = r#"
package p
func f() error {
	a, err := first()
	b, err := second()
	if err != nil {
		return err
	}
	use(a, b)
	c := 0
	c = compute()
	return nil
}
func g() {
	v := compute(1)
	p := &v
	v = compute(2)
	use(p)
}
"#;
    assert_eq!(rule_lines(&lint(Language::Go, go), "dead-store"), vec![4]);

    let java = r#"
class A {
  int f() {
    int x = compute(1);
    x = compute(2);
    int y = compute(1);
    y += compute(2);
    this.z = compute(1);
    this.z = compute(2);
    return x + y;
  }
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Java, java), "dead-store"),
        vec![4]
    );
}

// ---------------------------------------------------------------- driver

#[test]
fn detect_reports_lint_findings_with_their_function() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.rs"),
        "fn f(a: i32) -> i32 {\n    a - a\n}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("b.md"), "# notes\n").unwrap();
    let span = FnSpan {
        id: "f".into(),
        name: "f".into(),
        qualified_name: "crate::f".into(),
        kind: "function".into(),
        file: "a.rs".into(),
        start_line: 1,
        end_line: 3,
        start_col: 0,
        signature: None,
        return_type: None,
        is_test: false,
    };
    let mut project = Project::from_parts(
        dir.path(),
        vec!["a.rs".into(), "b.md".into()],
        vec![span],
        Vec::new(),
    );
    let findings = detect(&mut project);
    assert_eq!(findings.len(), 1);
    let finding = &findings[0];
    assert_eq!(finding.detector, Detector::Lint);
    assert_eq!(finding.rule, "self-comparison");
    assert_eq!((finding.line, finding.col), (2, 4));
    assert_eq!(finding.function.as_deref(), Some("crate::f"));
}

#[test]
fn constructors_are_defaults_but_computing_calls_are_not() {
    let ts = r#"
function f() {
  let m = new Map();
  m = build();
  let n = first();
  n = second();
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Typescript, ts), "dead-store"),
        vec![5]
    );
}

/// Measurement harness: every rule over a source tree, read-only, no index.
/// `CODEGRAPH_LINT_CORPUS=<dir> cargo test -p codegraph-rs --lib
/// lint::tests::lint_corpus -- --ignored --nocapture` prints one
/// tab-separated line per finding outside test files.
#[test]
#[ignore]
fn lint_corpus() {
    let Ok(root) = std::env::var("CODEGRAPH_LINT_CORPUS") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    // Pass 1: the sources, and every word in them standing in for the
    // index's symbol names.
    let mut sources = Vec::new();
    let mut symbols = std::collections::HashSet::new();
    for entry in ignore::WalkBuilder::new(&root)
        .hidden(true)
        .build()
        .flatten()
    {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(&root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !entry.file_type().is_some_and(|t| t.is_file()) || rel.contains("node_modules/") {
            continue;
        }
        let language = crate::extraction::detect_language(&rel, None);
        if rules::for_language(language).is_none() {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        if is_bundle(&source) {
            continue;
        }
        symbols.extend(syntax::split_words(&source).map(str::to_string));
        if !crate::search::is_test_source_file(&rel) {
            sources.push((rel, language, source));
        }
    }
    let known = |name: &str| symbols.contains(name);
    for (rel, language, source) in &sources {
        let rules = rules::for_language(*language).expect("rules");
        let Some(tree) = create_parser(*language).and_then(|mut p| p.parse(source, None)) else {
            continue;
        };
        for raw in lint_source(rules, source, tree.root_node(), &known) {
            println!("LINT\t{}\t{}:{}\t{}", raw.rule, rel, raw.line, raw.message);
        }
    }
}

#[test]
fn text_a_grammar_keeps_off_its_children_still_counts() {
    // `format_specifier` holds `+.2g` as its own text.
    let py = "def f(x, s):\n    return f\"{x:+.2g}\" if s else f\"{x:.2g}\"\n";
    assert!(rule_lines(&lint(Language::Python, py), "identical-branches").is_empty());
}

#[test]
fn typescript_narrowing_branches_are_not_identical() {
    let ts =
        "function f(c) {\n  return typeof c === \"string\" ? c.length > 0 : c.length > 0;\n}\n";
    assert!(rule_lines(&lint(Language::Typescript, ts), "identical-branches").is_empty());
}

#[test]
fn typed_and_placeholder_literals_are_defaults() {
    let ts = r#"
function f(a) {
  let body = {} as Data;
  body = build(a);
  let res = { drives: [], next: undefined };
  res = fetch(a);
  let v = compute(a) as number;
  v = compute(2);
}
"#;
    assert_eq!(
        rule_lines(&lint(Language::Typescript, ts), "dead-store"),
        vec![7]
    );
}

#[test]
fn messages_quote_code_on_one_line() {
    assert_eq!(
        spaced(&[
            "adj", "[", "u", "]", ".", "push", "(", "v", ")", ";", "n", "+=", "1"
        ]),
        "adj[u].push(v); n += 1"
    );
    assert_eq!(
        spaced(&["{", "f", "(", "a", ",", "b", ")", "}"]),
        "{ f(a, b) }"
    );
}
