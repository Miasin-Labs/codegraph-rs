use std::path::PathBuf;

use super::*;
use crate::history::event::CallResult;

fn call(tool: &str) -> RawToolCall {
    RawToolCall {
        native_id: "id".into(),
        tool: tool.into(),
        cwd: Some("/repo".into()),
        ..Default::default()
    }
}

fn bash(cmd: &str) -> RawToolCall {
    RawToolCall {
        command: Some(cmd.into()),
        ..call("Bash")
    }
}

fn touches(act: &Activity) -> Vec<(String, &'static str)> {
    act.touches
        .iter()
        .map(|t| (t.path.to_string_lossy().into_owned(), t.op.as_str()))
        .collect()
}

fn pairs(v: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    v.iter().map(|(p, o)| ((*p).to_owned(), *o)).collect()
}

#[test]
fn structured_tools_touch_their_files() {
    let read = Activity::of(&RawToolCall {
        file_path: Some("src/a.rs".into()),
        line: Some(40),
        ..call("Read")
    });
    assert_eq!(
        read.touches,
        [Touch {
            path: PathBuf::from("/repo/src/a.rs"),
            op: Op::Read,
            line: Some(40)
        }]
    );
    let patch = Activity::of(&RawToolCall {
        extra_paths: vec!["/repo/a.rs".into(), "b.rs".into()],
        ..call("apply_patch")
    });
    assert_eq!(
        touches(&patch),
        pairs(&[("/repo/a.rs", "e"), ("/repo/b.rs", "e")])
    );
    let grep = Activity::of(&RawToolCall {
        pattern: Some(r"fn\s+handle_recall|ToolHandler::execute".into()),
        search_path: Some("src/mcp/tools/mod.rs".into()),
        ..call("Grep")
    });
    assert_eq!(grep.idents, ["handle_recall", "ToolHandler", "execute"]);
    assert_eq!(
        touches(&grep),
        pairs(&[("/repo/src/mcp/tools/mod.rs", "s")])
    );
}

#[test]
fn codegraph_calls_record_symbols_and_code_words_only() {
    let node = Activity::of(&RawToolCall {
        symbols: vec!["HistoryDb::open".into(), "the".into()],
        query: Some("how does ingest_source call the ToolCallSource trait".into()),
        file_path: Some("/repo/src/history/store.rs".into()),
        ..call("mcp__codegraph__codegraph_node")
    });
    assert_eq!(
        node.idents,
        ["HistoryDb", "open", "ingest_source", "ToolCallSource"]
    );
    assert_eq!(
        touches(&node),
        pairs(&[("/repo/src/history/store.rs", "r")])
    );
    // opencode's server-prefixed spelling is the same tool.
    let oc = Activity::of(&RawToolCall {
        symbols: vec!["Foo".into()],
        ..call("codegraph_codegraph_search")
    });
    assert_eq!(oc.idents, ["Foo"]);
}

#[test]
fn shell_reads_edits_and_searches_follow_cd() {
    let act = Activity::of(&bash(
        "cd /repo/src && cat a.rs ../README.md | head -n 5 && sed -n '1,20p' b.rs && \
         sed -i 's/x/y/' c.rs && rg -n 'parse_rfc3339|HistoryDb' history/mod.rs > /tmp/out.txt \
         2>&1; echo hi >> notes.md",
    ));
    assert_eq!(
        touches(&act),
        pairs(&[
            ("/repo/src/a.rs", "r"),
            ("/repo/README.md", "r"),
            ("/repo/src/b.rs", "r"),
            ("/repo/src/c.rs", "e"),
            ("/tmp/out.txt", "e"),
            ("/repo/src/history/mod.rs", "s"),
            ("/repo/src/notes.md", "e"),
        ])
    );
    assert_eq!(act.idents, ["parse_rfc3339", "HistoryDb"]);
    assert_eq!(act.outcome, None);
}

#[test]
fn an_edit_beats_a_read_of_the_same_file() {
    let act = Activity::of(&bash("cat a.rs && sed -i 's/a/b/' a.rs && grep -n x a.rs"));
    assert_eq!(touches(&act), pairs(&[("/repo/a.rs", "e")]));
}

#[test]
fn redacted_text_never_yields_identifiers() {
    let act = Activity::of(&bash(concat!(
        "grep -r 'gh",
        "p_abcdefghijklmnopqrstuvwxyz0123' ."
    )));
    assert!(act.idents.is_empty(), "{:?}", act.idents);
    let act = Activity::of(&RawToolCall {
        pattern: Some("API_TOKEN=sk-ant-abcdefghijklmnopqrstu".into()),
        ..call("Grep")
    });
    assert!(act.idents.is_empty(), "{:?}", act.idents);
}

fn outcome(cmd: &str, result: Option<CallResult>) -> Option<Outcome> {
    Activity::of(&RawToolCall {
        result,
        ..bash(cmd)
    })
    .outcome
}

#[test]
fn builds_tests_and_commits_get_masked_templates() {
    let o = outcome(
        "cd /repo && cargo test -p codegraph-rs --lib history:: 2>&1 | tail -20",
        None,
    )
    .unwrap();
    assert_eq!(o.kind, OutcomeKind::Test);
    assert_eq!(o.template, "cargo test -p … --lib …");
    assert_eq!(o.ok, None);
    let o = outcome("git commit -m 'fix: the secret thing'", None).unwrap();
    assert_eq!(
        (o.kind, o.template.as_str()),
        (OutcomeKind::Commit, "git commit -m …")
    );
    let o = outcome("git -C /repo -c user.name=x commit -qm 'wip'", None).unwrap();
    assert_eq!(o.kind, OutcomeKind::Commit);
    let o = outcome("npm run test:unit -- --watch=false", None).unwrap();
    assert_eq!(
        (o.kind, o.template.as_str()),
        (OutcomeKind::Test, "npm run test:unit -- --watch=…")
    );
    let o = outcome("cargo +nightly fmt --all --check", None).unwrap();
    assert_eq!(o.template, "cargo +nightly fmt --all --check");
    // The last build/test of a chain is the one the result reports.
    let o = outcome(
        "cargo fmt --all && cargo test --lib && git commit -m x",
        None,
    )
    .unwrap();
    assert_eq!(
        (o.kind, o.template.as_str()),
        (OutcomeKind::Test, "cargo test --lib")
    );
    assert_eq!(outcome("cargo run --bin x", None), None);
    assert_eq!(outcome("ls -la && git status", None), None);
}

const RUSTC_FAIL: &str = "   Compiling x v0.1.0 (/repo)
error[E0425]: cannot find value `foo` in this scope
  --> src/lib.rs:10:5
error[E0308]: mismatched types
  --> src/lib.rs:22:9
error[E0425]: cannot find value `bar` in this scope
error: could not compile `x` (lib) due to 3 previous errors
";

#[test]
fn failures_get_codes_and_a_line_number_blind_signature() {
    let failed = CallResult {
        is_error: Some(true),
        excerpt: Some(RUSTC_FAIL.into()),
        ..Default::default()
    };
    let o = outcome("cargo check", Some(failed.clone())).unwrap();
    assert_eq!(o.ok, Some(false));
    assert_eq!(o.codes, ["E0425", "E0308"]);
    let sig = o.signature.clone().unwrap();
    assert_eq!(sig.len(), 16);
    // Same failure, other line numbers: same signature.
    let shifted = CallResult {
        excerpt: Some(RUSTC_FAIL.replace("10:5", "11:7").replace("22:9", "30:1")),
        ..failed
    };
    assert_eq!(
        outcome("cargo check", Some(shifted)).unwrap().signature,
        Some(sig)
    );
    // A failure printed through `| tail` still fails.
    let piped = CallResult {
        exit_code: Some(0),
        excerpt: Some("test result: FAILED. 1 passed; 2 failed".into()),
        ..Default::default()
    };
    assert_eq!(
        outcome("cargo test 2>&1 | tail -3", Some(piped))
            .unwrap()
            .ok,
        Some(false)
    );
    let passed = CallResult {
        exit_code: Some(0),
        ..Default::default()
    };
    let o = outcome("npx tsc --noEmit", Some(passed)).unwrap();
    assert_eq!((o.kind, o.ok), (OutcomeKind::Build, Some(true)));
    assert!(o.codes.is_empty() && o.signature.is_none());
}

#[test]
fn typescript_codes_are_found() {
    let text = "src/a.ts(3,7): error TS2339: Property 'x' does not exist\n\
                src/b.ts(1,1): error TS2345: Argument of type";
    assert_eq!(outcome::error_codes(text), ["TS2339", "TS2345"]);
}

#[test]
fn excerpt_is_kept_only_for_builds_and_tests() {
    assert!(keeps_excerpt(
        "cd x && cargo clippy --all-targets 2>&1 | head"
    ));
    assert!(keeps_excerpt("pnpm test"));
    assert!(!keeps_excerpt("git commit -m x"));
    assert!(!keeps_excerpt("cat Cargo.toml"));
}

#[test]
fn pathish_tokens() {
    assert!(is_pathish("src/a.rs"));
    assert!(is_pathish("Cargo.toml"));
    assert!(is_pathish("a.rs:12"));
    assert!(!is_pathish("-n"));
    assert!(!is_pathish("1,20p"));
    assert!(!is_pathish("https://x.io/a.js"));
    assert!(!is_pathish("hello"));
}
