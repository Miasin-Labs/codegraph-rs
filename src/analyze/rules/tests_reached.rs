//! `reached-from`: matches in functions an entry point reaches, over the
//! index (`Project::from_parts`) and in `--check` examples (`reached`).

use super::check::check_rules;
use super::compile::RuleSet;
use super::scan;
use super::semantics::IndexSemantics;
use crate::analyze::bugs::{
    BugsOptions,
    CallSite,
    Finding,
    FnSpan,
    LibraryCall,
    Project,
    RouteHandler,
};

fn load(yaml: &str) -> RuleSet {
    let set = RuleSet::load(&[], &[("t.yaml".to_string(), yaml.to_string())], false);
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    set
}

/// The load error of `yaml`'s only rule.
fn load_error(yaml: &str) -> String {
    let set = RuleSet::load(&[], &[("t.yaml".to_string(), yaml.to_string())], false);
    assert_eq!(set.errors.len(), 1, "{:?}", set.errors);
    set.errors[0].message.clone()
}

fn span(id: &str, qualified: &str, kind: &str, lines: (u32, u32)) -> FnSpan {
    FnSpan {
        id: id.to_string(),
        name: qualified.rsplit("::").next().unwrap().to_string(),
        qualified_name: qualified.to_string(),
        kind: kind.to_string(),
        file: "src/lib.rs".to_string(),
        start_line: lines.0,
        end_line: lines.1,
        start_col: 0,
        signature: Some("()".to_string()),
        return_type: None,
        is_test: false,
    }
}

fn call(caller: &FnSpan, line: u32, callee: &FnSpan) -> CallSite {
    CallSite {
        caller_id: caller.id.clone(),
        caller: caller.qualified_name.clone(),
        file: caller.file.clone(),
        line,
        col: 4,
        callee_id: callee.id.clone(),
        callee_name: callee.name.clone(),
        callee_qualified: callee.qualified_name.clone(),
        callee_kind: callee.kind.clone(),
        callee_signature: None,
        callee_return_type: None,
        callee_file: callee.file.clone(),
        callee_line: callee.start_line,
        in_test: false,
    }
}

const SOURCE: &str = "\
pub async fn reindex() {
    extract();
}

fn extract() {
    Command::new(\"pdftotext\").output();
}

fn cli() {
    Command::new(\"pdftotext\").output();
}

impl Feed {
    fn new() -> Self {
        Command::new(\"pdftotext\").output();
    }

    fn fetch(&self) {
        extract();
    }
}

async fn serve(l: TcpListener) {
    loop {
        let (s, _) = l.accept().await.unwrap();
        session(s);
    }
}

fn session(s: TcpStream) {
    Command::new(\"pdftotext\").output();
}
";

/// The functions of [`SOURCE`], its calls, and a route to `reindex`.
fn project() -> (tempfile::TempDir, Project) {
    let reindex = span("reindex", "reindex", "function", (1, 3));
    let extract = span("extract", "extract", "function", (5, 7));
    let cli = span("cli", "cli", "function", (9, 11));
    let new = span("new", "Feed::new", "method", (14, 16));
    let fetch = span("fetch", "Feed::fetch", "method", (18, 20));
    let serve = span("serve", "serve", "function", (23, 28));
    let session = span("session", "session", "function", (30, 32));
    let calls = vec![
        call(&reindex, 2, &extract),
        call(&fetch, 19, &extract),
        call(&serve, 26, &session),
    ];
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), SOURCE).unwrap();
    let project = Project::from_parts(
        dir.path(),
        vec!["src/lib.rs".to_string()],
        vec![reindex, extract, cli, new, fetch.clone(), serve, session],
        calls,
    )
    .with_entries(
        vec![
            RouteHandler {
                handler_id: "reindex".into(),
                route: "POST /reindex".into(),
                file: "src/main.rs".into(),
                line: 40,
            },
            // `fetch` serves `GET /feed`.
            RouteHandler {
                handler_id: "fetch".into(),
                route: "GET /feed".into(),
                file: "src/main.rs".into(),
                line: 41,
            },
        ],
        &[],
    )
    .with_listener_calls(vec![LibraryCall {
        caller_id: "serve".into(),
        callee: "l.accept".into(),
        file: "src/lib.rs".into(),
        line: 25,
    }]);
    (dir, project)
}

/// A rule reporting every `.output()`, with `predicate` as its `where`.
fn rule(predicate: &str) -> String {
    format!(
        r#"
id: waits
language: rust
check-patterns:
  - query: "((call_expression function: (field_expression field: (field_identifier) @m)) @call (#eq? @m \"output\"))"
    at: call
    where:
{predicate}
examples:
  bad:
    - code: "fn f() {{ Command::new(\"x\").output(); }}"
      reached: true
"#
    )
}

fn run(yaml: &str) -> Vec<Finding> {
    let (_dir, project) = project();
    let rules = load(yaml);
    let semantics = IndexSemantics::for_tests(&project);
    let mut found = scan(&project, &semantics, &rules, &BugsOptions::default()).findings;
    found.sort_by_key(|f| f.line);
    found
}

fn lines(found: &[Finding]) -> Vec<u32> {
    found.iter().map(|f| f.line).collect()
}

#[test]
fn reached_from_keeps_what_an_entry_reaches_with_the_path() {
    let found = run(&rule("      - reached-from: route"));
    // `extract` (reached from both routes: the nearest, first listed wins);
    // `Feed::new`, `cli` and the listener's `session` are not route code.
    assert_eq!(lines(&found), [6], "{found:#?}");
    let notes: Vec<(&str, u32, &str)> = found[0]
        .evidence
        .iter()
        .map(|e| (e.file.as_str(), e.line, e.note.as_str()))
        .collect();
    assert!(
        notes.contains(&(
            "src/lib.rs",
            6,
            "reached from route `POST /reindex` (`reindex`), 1 call away"
        )),
        "{notes:#?}"
    );
    assert!(
        notes.contains(&(
            "src/main.rs",
            40,
            "entry point: route `POST /reindex` (`reindex`)"
        )),
        "{notes:#?}"
    );
    assert!(
        notes.contains(&("src/lib.rs", 2, "`reindex` calls `extract`")),
        "{notes:#?}"
    );
    assert_eq!(found[0].confidence, 0.6);

    // `server` is every server kind: the listener's session joins.
    let found = run(&rule("      - reached-from: server"));
    assert_eq!(lines(&found), [6, 31], "{found:#?}");
    assert!(
        found[1]
            .evidence
            .iter()
            .any(|e| e.note == "reached from listener `serve` (calls `l.accept`), 1 call away"),
        "{found:#?}"
    );
    let found = run(&rule("      - reached-from: [listener, message]"));
    assert_eq!(lines(&found), [31], "{found:#?}");
}

#[test]
fn via_type_counts_a_method_whose_type_is_reached() {
    let found = run(&rule(
        "      - reached-from: server\n        via-type: true",
    ));
    assert_eq!(lines(&found), [6, 15, 31], "{found:#?}");
    assert!(
        found[1].evidence.iter().any(|e| e.note
            == "reached through its type: `Feed::fetch` is reached from route `GET /feed` \
                (`Feed::fetch`)"),
        "{found:#?}"
    );
}

#[test]
fn unreached_confidence_keeps_the_rest_ranked_lower() {
    let found = run(&rule(
        "      - reached-from: server\n        unreached-confidence: 0.2",
    ));
    let ranked: Vec<(u32, f64)> = found.iter().map(|f| (f.line, f.confidence)).collect();
    assert_eq!(ranked, [(6, 0.6), (10, 0.2), (15, 0.2), (31, 0.6)]);
    assert!(
        found[1].evidence.iter().any(|e| e.note
            == "ranked lower: no route/extractor/listener/message entry point reaches the \
                function (within 8 resolved calls)"),
        "{found:#?}"
    );
}

#[test]
fn examples_state_reach_with_reached() {
    // `reached: true` is every kind, a list names kinds, and no `reached`
    // (or `false`) is not reached.
    let yaml = r#"
id: waits
language: rust
check-patterns:
  - query: "((call_expression function: (field_expression field: (field_identifier) @m)) @call (#eq? @m \"output\"))"
    where:
      - reached-from: [route, listener]
examples:
  bad:
    - code: "fn f() { Command::new(\"x\").output(); }"
      reached: true
    - code: "fn f() { Command::new(\"x\").output(); }"
      reached: [message, listener]
    - code: "fn f() { Command::new(\"x\").output(); }"
      reached: route
  good:
    - "fn f() { Command::new(\"x\").output(); }"
    - code: "fn f() { Command::new(\"x\").output(); }"
      reached: false
    - code: "fn f() { Command::new(\"x\").output(); }"
      reached: public-api
"#;
    let set = load(yaml);
    let report = check_rules(&set);
    for example in &report.rules[0].examples {
        assert!(example.passed, "{}", example.explain());
    }
    // A failing one says which predicate turned it down, and why.
    let failing = yaml.replace("reached: route", "reached: extractor");
    let report = check_rules(&load(&failing));
    let bad = &report.rules[0].examples[2];
    assert!(!bad.passed);
    assert!(
        bad.explain().contains(
            "where[0] (reached-from: [route, listener]) rejected it: no route/listener entry \
             point reaches the function"
        ),
        "{}",
        bad.explain()
    );
}

#[test]
fn reached_from_load_errors_say_what_is_wrong() {
    let base = rule("      - reached-from: servers");
    assert!(
        load_error(&base).contains(
            "`reached-from` unknown entry kind `servers` (one of: server, route, extractor, \
             listener, message, public-api)"
        ),
        "{}",
        load_error(&base)
    );
    let alone = rule("      - unreached-confidence: 0.3");
    assert!(
        load_error(&alone).contains("`unreached-confidence` goes with `reached-from`"),
        "{}",
        load_error(&alone)
    );
    let range = rule("      - reached-from: route\n        unreached-confidence: 3");
    assert!(
        load_error(&range).contains("must be between 0 and 1, not 3"),
        "{}",
        load_error(&range)
    );
    let capture = rule("      - reached-from: route\n        capture: m");
    assert!(
        load_error(&capture).contains("`reached-from` takes no `capture`"),
        "{}",
        load_error(&capture)
    );
    let ignore = format!(
        "{}ignore-patterns:\n  - query: \"(identifier) @i\"\n    where:\n      - reached-from: \
         route\n        unreached-confidence: 0.1\n",
        rule("      - reached-from: route")
    );
    assert!(
        load_error(&ignore).contains("belongs on a check-pattern or a taint sink"),
        "{}",
        load_error(&ignore)
    );
    let example =
        rule("      - reached-from: route").replace("reached: true", "reached: sometimes");
    assert!(
        load_error(&example).contains("`reached`: unknown entry kind `sometimes`"),
        "{}",
        load_error(&example)
    );
}
