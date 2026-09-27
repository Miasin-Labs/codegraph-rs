use super::check::check_rules;
use super::compile::{LoadError, RuleSet};
use super::semantics::IndexSemantics;
use super::{BUILTIN_RULES, scan};
use crate::analyze::bugs::{BugsOptions, CallSite, FnSpan, Project};

fn load(yaml: &str) -> RuleSet {
    RuleSet::load(&[], &[("t.yaml".to_string(), yaml.to_string())], false)
}

/// The one load error of `yaml`.
fn error(yaml: &str) -> LoadError {
    let set = load(yaml);
    assert_eq!(set.errors.len(), 1, "{:?}", set.errors);
    set.errors[0].clone()
}

/// `(example label, passed)` of the only rule in `yaml`.
fn outcomes(yaml: &str) -> Vec<(String, bool)> {
    let set = load(yaml);
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    let report = check_rules(&set);
    report.rules[0]
        .examples
        .iter()
        .map(|e| (e.example.clone(), e.passed))
        .collect()
}

fn all_pass(yaml: &str) {
    let set = load(yaml);
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    let report = check_rules(&set);
    for rule in &report.rules {
        for example in &rule.examples {
            assert!(example.passed, "{}: {}", rule.id, example.explain());
        }
    }
}

#[test]
fn rules_builtin_examples_pass() {
    let set = RuleSet::load(&[], &[], true);
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    assert!(set.rules.len() >= 40, "{} builtin rules", set.rules.len());
    let report = check_rules(&set);
    let failures: Vec<String> = report
        .rules
        .iter()
        .flat_map(|rule| {
            rule.examples
                .iter()
                .filter(|e| !e.passed)
                .map(move |e| format!("{}: {}", rule.id, e.explain()))
        })
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
    // Every builtin rule has good examples too.
    for rule in &set.rules {
        assert!(
            rule.examples.iter().any(|e| !e.bad),
            "{} has no good example",
            rule.id
        );
    }
    assert_eq!(BUILTIN_RULES.len(), 8);
}

#[test]
fn unknown_keys_are_errors_with_line_and_suggestion() {
    let e = error("id: a\nlanguag: rust\ncheck-patterns:\n  - query: \"(identifier) @i\"\n");
    assert_eq!(e.line, Some(2));
    assert!(e.message.contains("unknown field `languag`"), "{e}");
    assert!(e.message.contains("did you mean `language`?"), "{e}");
    assert!(!e.message.contains("check patterns"), "aliases elided: {e}");

    // Inside a pattern and a predicate too.
    let e = error(
        "id: a\nlanguage: rust\ncheck-patterns:\n  - query: \"(identifier) @i\"\n    wher: []\n",
    );
    assert_eq!(e.line, Some(5));
    assert!(e.message.contains("did you mean `where`?"), "{e}");
    let e = error(
        "id: a\nlanguage: rust\ncheck-patterns:\n  - query: \"(identifier) @i\"\n    where:\n      - capture: i\n        regexp: x\n",
    );
    assert_eq!(e.line, Some(7));
    assert!(e.message.contains("did you mean `regex`?"), "{e}");
}

#[test]
fn a_bad_document_does_not_hide_the_others() {
    let set = load(
        "id: a\nbogus: 1\n---\nid: b\nlanguage: rust\ncheck-patterns:\n  - query: \"(identifier) @i\"\nexamples:\n  bad: [\"fn f() {}\"]\n",
    );
    assert_eq!(set.errors.len(), 1);
    assert_eq!(set.rules.len(), 1);
    assert_eq!(set.rules[0].id, "b");
    assert_eq!(set.rules[0].line, 4);

    // A list of rules in one document.
    let set = load(
        "- id: a\n  language: rust\n  check-patterns: {query: \"(identifier) @i\"}\n  examples: {bad: [\"fn f() {}\"]}\n- id: b\n  language: rust\n  check-patterns: {query: \"(identifier) @i\"}\n  examples: {bad: [\"fn f() {}\"]}\n",
    );
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    assert_eq!(
        set.rules.iter().map(|r| r.line).collect::<Vec<_>>(),
        vec![1, 5]
    );
}

#[test]
fn semantic_errors_name_rule_pattern_and_line() {
    // Examples are required.
    let e = error("id: a\nlanguage: rust\ncheck-patterns:\n  - query: \"(identifier) @i\"\n");
    assert_eq!(e.rule.as_deref(), Some("a"));
    assert!(e.message.contains("no `examples`"), "{e}");

    // Both backends, or neither.
    let e = error(
        "id: a\ncheck-patterns:\n  - name: p\n    pattern: \"{ f(); }\"\n    query: \"(x) @x\"\nexamples: {bad: [\"\"]}\n",
    );
    assert!(e.message.contains("check-patterns[0] `p`"), "{e}");
    assert!(e.message.contains("both `pattern`"), "{e}");
    assert_eq!(e.line, Some(3));

    // A query needs a language; a weggli pattern only runs on C/C++.
    let e =
        error("id: a\ncheck-patterns:\n  - query: \"(identifier) @i\"\nexamples: {bad: [\"\"]}\n");
    assert!(e.message.contains("needs `language:`"), "{e}");
    let e = error(
        "id: a\nlanguage: rust\ncheck-patterns:\n  - pattern: \"{ f(); }\"\nexamples: {bad: [\"\"]}\n",
    );
    assert!(e.message.contains("C and C++ only"), "{e}");
    let e = error(
        "id: a\nlanguage: klingon\ncheck-patterns:\n  - query: \"(x) @x\"\nexamples: {bad: [\"\"]}\n",
    );
    assert!(e.message.contains("unknown language `klingon`"), "{e}");
    assert!(e.message.contains("rust"), "{e}");
    assert_eq!(e.line, Some(2));

    // A weggli syntax error points into the block scalar.
    let e = error(
        "id: a\ncheck-patterns:\n  - pattern: |\n      {\n        memcpy(a, b;\n      }\nexamples: {bad: [\"\"]}\n",
    );
    assert!(e.message.contains("does not parse"), "{e}");
    assert_eq!(e.line, Some(5), "{e}");

    // A query error names the node kind and suggests one.
    let e = error(
        "id: a\nlanguage: rust\ncheck-patterns:\n  - query: |\n      (call_expresion) @c\nexamples: {bad: [\"\"]}\n",
    );
    assert!(
        e.message.contains("`call_expresion` is not a node kind"),
        "{e}"
    );
    assert!(e.message.contains("did you mean: call_expression"), "{e}");
    assert_eq!(e.line, Some(5));

    // Unsupported query predicates go to `where`.
    let e = error(
        "id: a\nlanguage: rust\ncheck-patterns:\n  - query: '((identifier) @i (#resolves? @i \"x\"))'\nexamples: {bad: [\"\"]}\n",
    );
    assert!(
        e.message.contains("unsupported predicate `#resolves?`"),
        "{e}"
    );

    // Captures used by where / at / message / regex must exist.
    let e = error(
        "id: a\nlanguage: rust\ncheck-patterns:\n  - query: \"(identifier) @i\"\n    where:\n      - capture: j\n        regex: x\nexamples: {bad: [\"\"]}\n",
    );
    assert!(
        e.message
            .contains("where[0]: `capture: j` names no capture"),
        "{e}"
    );
    assert!(e.message.contains("captures: i"), "{e}");
    assert_eq!(e.line, Some(6));
    let e = error(
        "id: a\nlanguage: rust\ncheck-patterns:\n  - query: \"(identifier) @i\"\n    at: k\nexamples: {bad: [\"\"]}\n",
    );
    assert!(e.message.contains("`at: k` names no capture"), "{e}");
    let e = error(
        "id: a\nlanguage: rust\nmessage: \"{nope} bad\"\ncheck-patterns:\n  - query: \"(identifier) @i\"\nexamples: {bad: [\"\"]}\n",
    );
    assert!(e.message.contains("`{nope}`"), "{e}");
    assert_eq!(e.line, Some(3));
    let e = error(
        "id: a\ncheck-patterns:\n  - pattern: \"{ $f(); }\"\n    regex: g=^x$\nexamples: {bad: [\"\"]}\n",
    );
    assert!(e.message.contains("constrains `$g`"), "{e}");
    assert!(e.message.contains("$f"), "{e}");
    let e = error(
        "id: a\ncheck-patterns:\n  - pattern: \"{ $f(); }\"\n    regex: \"f=(\"\nexamples: {bad: [\"\"]}\n",
    );
    assert!(e.message.contains("does not compile"), "{e}");

    // One test per `where` entry.
    let e = error(
        "id: a\nlanguage: rust\ncheck-patterns:\n  - query: \"(identifier) @i\"\n    where:\n      - capture: i\n        regex: x\n        not-regex: y\nexamples: {bad: [\"\"]}\n",
    );
    assert!(e.message.contains("each `where` entry is one of"), "{e}");

    // Ids are unique across a set.
    let set = load(
        "id: a\nlanguage: rust\ncheck-patterns: {query: \"(identifier) @i\"}\nexamples: {bad: [\"\"]}\n---\nid: a\nlanguage: rust\ncheck-patterns: {query: \"(identifier) @i\"}\nexamples: {bad: [\"\"]}\n",
    );
    assert_eq!(set.errors.len(), 1);
    assert!(set.errors[0].message.contains("already loaded"));
    assert_eq!(set.errors[0].line, Some(6));
}

#[test]
fn check_explains_what_matched_and_what_was_rejected() {
    let set = load(
        r#"
id: unwrap-outside-closure
language: rust
check-patterns:
  - name: unwrap
    query: "((call_expression function: (field_expression field: (field_identifier) @m)) @call (#eq? @m \"unwrap\"))"
    where:
      - not-inside: "(closure_expression) @c"
examples:
  bad:
    - "fn f(x: Option<u8>) { let g = || x.unwrap(); }"
  good:
    - "fn f(x: Option<u8>) { x.unwrap(); }"
"#,
    );
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    let report = check_rules(&set);
    assert!(!report.ok());
    assert_eq!(report.failed, 1);
    let rule = &report.rules[0];
    let bad = &rule.examples[0];
    assert!(!bad.passed);
    let text = bad.explain();
    assert!(text.contains("bad[0] did not match"), "{text}");
    assert!(
        text.contains("`unwrap` matched at example line 1"),
        "{text}"
    );
    assert!(text.contains("where[0] (not-inside"), "{text}");
    assert!(text.contains("it is inside `|| x.unwrap()`"), "{text}");
    let good = &rule.examples[1];
    assert!(!good.passed);
    assert!(
        good.explain()
            .contains("good[0] matched check-patterns[0] `unwrap` at example line 1"),
        "{}",
        good.explain()
    );
    assert_eq!(bad.line, Some(11));

    // A syntax error in an example is noted, not fatal.
    let set = load(
        "id: a\nlanguage: rust\ncheck-patterns: {query: \"(identifier) @i\"}\nexamples: {bad: [\"fn f( {\"]}\n",
    );
    let report = check_rules(&set);
    let example = &report.rules[0].examples[0];
    assert!(
        example
            .syntax_error
            .as_deref()
            .is_some_and(|e| e.contains("syntax error")),
        "{example:?}"
    );
}

#[test]
fn capture_regex_predicates() {
    all_pass(
        r#"
id: a
language: python
check-patterns:
  - query: "((call function: (identifier) @fn arguments: (argument_list . (_) @arg)) @call)"
    where:
      - capture: fn
        regex: "^(eval|exec)$"
      - capture: arg
        not-regex: "^\"[^\"]*\"$"
examples:
  bad: ["eval(user)\n", "exec(code)\n"]
  good: ["eval(\"1\")\n", "print(user)\n"]
"#,
    );
}

#[test]
fn resolves_to_uses_the_example_resolution_map() {
    let yaml = r#"
id: send-unchecked
language: rust
check-patterns:
  - query: "((call_expression function: (field_expression field: (field_identifier) @m)) @call (#eq? @m \"send\"))"
    where:
      - capture: m
        resolves-to: "^reqwest::.*::send$"
examples:
  bad:
    - code: "fn f(c: Client) { c.get(u).send(); }"
      resolves: {send: "reqwest::RequestBuilder::send"}
  good:
    - "fn f(tx: Sender<u8>) { tx.send(1); }"
    - code: "fn f(tx: Sender<u8>) { tx.send(1); }"
      resolves: {tx.send: "std::sync::mpsc::Sender::send"}
"#;
    assert_eq!(
        outcomes(yaml),
        vec![
            ("bad[0]".to_string(), true),
            ("good[0]".to_string(), true),
            ("good[1]".to_string(), true)
        ]
    );
    // And the negation.
    all_pass(
        r#"
id: send-not-reqwest
language: rust
check-patterns:
  - query: "((call_expression function: (field_expression field: (field_identifier) @m)) @call (#eq? @m \"send\"))"
    where:
      - capture: m
        not-resolves-to: "^reqwest::"
examples:
  bad: ["fn f(tx: Sender<u8>) { tx.send(1); }"]
  good:
    - code: "fn f(c: Client) { c.send(); }"
      resolves: {send: "reqwest::RequestBuilder::send"}
"#,
    );
}

#[test]
fn enclosing_function_predicates() {
    all_pass(
        r#"
id: open-without-close
language: go
check-patterns:
  - query: "((call_expression function: (selector_expression field: (field_identifier) @m)) @call (#eq? @m \"Open\"))"
    where:
      - enclosing-function:
          calls-not: "(^|\\.)Close$"
          name-regex: "^[a-z]"
          is-test: false
examples:
  bad:
    - |
      package p
      func load() { f, _ := os.Open("x"); use(f) }
  good:
    - |
      package p
      func load() { f, _ := os.Open("x"); defer f.Close() }
    - |
      package p
      func Load() { f, _ := os.Open("x"); use(f) }
    - |
      package p
      func TestLoad(t *testing.T) { f, _ := os.Open("x"); use(f) }
"#,
    );
    all_pass(
        r#"
id: lock-with-unlock
language: [javascript, typescript]
check-patterns:
  - query: "((call_expression function: (member_expression property: (property_identifier) @m)) @call (#eq? @m \"lock\"))"
    where:
      - enclosing-function:
          calls: "unlock$"
examples:
  bad: ["function f(m) { m.lock(); work(); m.unlock(); }"]
  good: ["function f(m) { m.lock(); work(); }"]
"#,
    );
    // Rust test functions are recognised by attribute and module.
    all_pass(
        r##"
id: unwrap-in-tests
language: rust
check-patterns:
  - query: "((call_expression function: (field_expression field: (field_identifier) @m)) @call (#eq? @m \"unwrap\"))"
    where:
      - enclosing-function: {is-test: true}
examples:
  bad:
    - "#[test]\nfn works() { x().unwrap(); }"
    - "mod tests { fn helper() { x().unwrap(); } }"
  good: ["fn works() { x().unwrap(); }"]
"##,
    );
}

#[test]
fn inside_and_not_inside() {
    all_pass(
        r#"
id: unwrap-in-loop
language: rust
check-patterns:
  - query: "((call_expression function: (field_expression field: (field_identifier) @m)) @call (#eq? @m \"unwrap\"))"
    where:
      - inside: "[(for_expression) (while_expression) (loop_expression)] @loop"
      - capture: call
        not-inside: "(closure_expression) @c"
examples:
  bad: ["fn f(v: Vec<Option<u8>>) { for x in v { x.unwrap(); } }"]
  good:
    - "fn f(x: Option<u8>) { x.unwrap(); }"
    - "fn f(v: Vec<Option<u8>>) { for x in v { let g = || x.unwrap(); } }"
"#,
    );
}

#[test]
fn query_backend_on_several_languages() {
    all_pass(
        r#"
id: todo-panics
language: [rust, python, javascript, typescript, go, java]
check-patterns:
  - name: rust-todo
    language: rust
    query: "((macro_invocation macro: (identifier) @m) @call (#match? @m \"^(todo|unimplemented)$\"))"
  - name: python-not-implemented
    language: python
    query: "((raise_statement (call function: (identifier) @m)) @call (#eq? @m \"NotImplementedError\"))"
  - name: js-throw-todo
    language: [javascript, typescript]
    query: "((throw_statement (new_expression arguments: (arguments (string) @m))) @call (#match? @m \"(?i)todo\"))"
  - name: go-panic
    language: go
    query: "((call_expression function: (identifier) @m arguments: (argument_list (interpreted_string_literal) @msg)) @call (#eq? @m \"panic\") (#match? @msg \"(?i)todo\"))"
  - name: java-unsupported
    language: java
    query: "((throw_statement (object_creation_expression type: (type_identifier) @m)) @call (#eq? @m \"UnsupportedOperationException\"))"
message: "{m}: unfinished code"
examples:
  bad:
    - "fn f() { todo!() }"
    - code: "def f():\n    raise NotImplementedError()\n"
      language: python
    - code: "function f() { throw new Error('TODO'); }"
      language: javascript
    - code: "function f(): void { throw new Error('todo: later'); }"
      language: typescript
    - code: "package p\nfunc f() { panic(\"TODO\") }\n"
      language: go
    - code: "class A { void f() { throw new UnsupportedOperationException(); } }"
      language: java
  good:
    - "fn f() { unreachable!() }"
    - code: "def f():\n    raise ValueError()\n"
      language: python
    - code: "function f() { throw new Error('bad input'); }"
      language: javascript
    - code: "package p\nfunc f() { panic(err) }\n"
      language: go
"#,
    );
}

#[test]
fn weggli_patterns_with_regex_unique_and_limit() {
    all_pass(
        r#"
id: memcpy-same-size
check-patterns:
  - pattern: "{ $f($a, $b, $n); }"
    regex: ["f=^mem(cpy|move)$"]
    unique: true
examples:
  bad: ["void f(char *d, char *s, int n) { memcpy(d, s, n); }"]
  good:
    - "void f(char *d, int n) { memcpy(d, d, n); }"
    - "void f(char *d, char *s, int n) { strncpy(d, s, n); }"
"#,
    );
    // `limit`: one finding per function.
    let set = load(
        "id: a\ncheck-patterns:\n  - pattern: \"{ free($p); }\"\n    limit: true\nexamples: {bad: [\"void f(char *a, char *b) { free(a); free(b); }\"]}\n",
    );
    let report = check_rules(&set);
    assert_eq!(report.rules[0].examples[0].matches.len(), 1);
    let set = load(
        "id: a\ncheck-patterns:\n  - pattern: \"{ free($p); }\"\nexamples: {bad: [\"void f(char *a, char *b) { free(a); free(b); }\"]}\n",
    );
    let report = check_rules(&set);
    assert_eq!(report.rules[0].examples[0].matches.len(), 2);
}

#[test]
fn ignore_patterns_drop_the_matches_they_cover() {
    all_pass(
        r#"
id: free-twice
language: c
check-patterns:
  - pattern: "{ free($p); not: $p = _; free($p); }"
ignore-patterns:
  - pattern: "{ if (_) { free($p); } else { free($p); } }"
examples:
  bad: ["void f(char *p) { free(p); free(p); }"]
  good: ["void f(char *p) { if (x) { free(p); } else { free(p); } }"]
"#,
    );
    let set = load(
        r#"
id: free-twice
language: c
check-patterns:
  - pattern: "{ free($p); not: $p = _; free($p); }"
ignore-patterns:
  - name: branches
    pattern: "{ if (_) { free($p); } else { free($p); } }"
examples:
  bad: ["void f(char *p) { if (x) { free(p); } else { free(p); } }"]
"#,
    );
    let report = check_rules(&set);
    let explained = report.rules[0].examples[0].explain();
    assert!(
        explained.contains("dropped by ignore-patterns[0] `branches`"),
        "{explained}"
    );
}

#[test]
fn messages_interpolate_captures() {
    let set = load(
        "id: a\nlanguage: rust\nmessage: \"{m} on {function}; {nothing}\"\ncheck-patterns:\n  - query: \"((call_expression function: (field_expression field: (field_identifier) @m)) @call)\"\n    message: \"called `{m}` in {function}\"\nexamples: {bad: [\"fn f() { x.go(); }\"]}\n",
    );
    // `{nothing}` is captured by no pattern: an error.
    assert_eq!(set.errors.len(), 1, "{:?}", set.errors);
    assert!(set.errors[0].message.contains("`{nothing}`"));
}

/// A project on disk plus the index facts `Project::from_parts` takes.
fn project(
    files: &[(&str, &str)],
    functions: Vec<FnSpan>,
    calls: Vec<CallSite>,
) -> (tempfile::TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let names = files.iter().map(|(path, _)| path.to_string()).collect();
    let project = Project::from_parts(dir.path(), names, functions, calls);
    (dir, project)
}

fn span(id: &str, name: &str, file: &str, lines: (u32, u32)) -> FnSpan {
    FnSpan {
        id: id.to_string(),
        name: name.to_string(),
        qualified_name: format!("crate::{name}"),
        kind: "function".to_string(),
        file: file.to_string(),
        start_line: lines.0,
        end_line: lines.1,
        start_col: 0,
        signature: None,
        return_type: None,
        is_test: false,
    }
}

fn call(caller: &FnSpan, line: u32, col: u32, callee: &str, qualified: &str) -> CallSite {
    CallSite {
        caller_id: caller.id.clone(),
        caller: caller.qualified_name.clone(),
        file: caller.file.clone(),
        line,
        col,
        callee_id: format!("id:{qualified}"),
        callee_name: callee.to_string(),
        callee_qualified: qualified.to_string(),
        callee_kind: "method".to_string(),
        callee_signature: None,
        callee_return_type: None,
        callee_file: "src/client.rs".to_string(),
        in_test: false,
    }
}

#[test]
fn index_predicates_read_resolved_call_sites() {
    let source = "fn run(c: &Client, tx: &Sender) {\n    c.send();\n    tx.send();\n}\n\nfn checked(c: &Client) {\n    c.send();\n    validate();\n}\n";
    let run = span("f1", "run", "src/lib.rs", (1, 4));
    let checked = span("f2", "checked", "src/lib.rs", (6, 9));
    let calls = vec![
        call(&run, 2, 4, "send", "Client::send"),
        call(&run, 3, 4, "send", "Sender::send"),
        call(&checked, 7, 4, "send", "Client::send"),
        call(&checked, 8, 4, "validate", "crate::validate"),
    ];
    let (_dir, project) = project(&[("src/lib.rs", source)], vec![run.clone(), checked], calls);
    let rules = load(
        r#"
id: unvalidated-client-send
language: rust
check-patterns:
  - query: "((call_expression function: (field_expression field: (field_identifier) @m)) @call (#eq? @m \"send\"))"
    where:
      - capture: m
        resolves-to: "^Client::send$"
      - enclosing-function:
          calls-not: "::validate$"
message: "{m} without validate() in {function}"
examples:
  bad:
    - code: "fn f(c: Client) { c.send(); }"
      resolves: {send: "Client::send"}
"#,
    );
    assert!(rules.errors.is_empty(), "{:?}", rules.errors);
    let semantics = IndexSemantics::for_tests(&project);
    let found = scan(&project, &semantics, &rules, &BugsOptions::default()).findings;
    assert_eq!(found.len(), 1, "{found:#?}");
    let finding = &found[0];
    assert_eq!((finding.file.as_str(), finding.line), ("src/lib.rs", 2));
    assert_eq!(finding.function.as_deref(), Some("crate::run"));
    assert_eq!(finding.message, "send without validate() in run");
    assert!(
        finding
            .evidence
            .iter()
            .any(|e| e.note == "`m` resolves to Client::send"),
        "{finding:?}"
    );
    assert_eq!(finding.rule, "unvalidated-client-send");
}

#[test]
fn an_alias_without_an_anchor_is_a_load_error_not_a_crash() {
    let lone = error("description: *x\n");
    assert!(lone.message.contains("no anchor"), "{lone:?}");

    // Anchors do not cross `---`: the second document fails, the first loads.
    let set = load(
        "id: &x a\nlanguage: rust\ncheck-patterns:\n  - query: (identifier) @i\n\
         examples: {bad: ['fn f() { x; }'], good: ['']}\n---\nid: b\ndescription: *x\n",
    );
    assert_eq!(set.errors.len(), 1, "{:?}", set.errors);
    assert!(
        set.errors[0].message.contains("no anchor"),
        "{:?}",
        set.errors
    );
    assert_eq!(set.errors[0].line, Some(8), "{:?}", set.errors);
    assert_eq!(set.rules.len(), 1);
}
