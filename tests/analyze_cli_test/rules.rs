/// A rule whose `resolves-to` predicate only the index can answer: which
/// `send` a call reaches.
const CLIENT_SEND_RULE: &str = r#"
id: client-send-unvalidated
description: Client::send called in a function that never calls validate.
severity: high
language: rust
check-patterns:
  - name: send
    query: |
      ((call_expression function: (field_expression field: (field_identifier) @m)) @call
       (#eq? @m "send"))
    where:
      - capture: m
        resolves-to: "^Client::send$"
      - enclosing-function:
          calls-not: "validate$"
message: "{m} reaches Client::send without validate() in {function}"
examples:
  bad:
    - code: "fn f(c: Client) { c.send(); }"
      resolves: {send: "Client::send"}
  good:
    - "fn f(tx: Sender) { tx.send(); }"
"#;

fn run_cli_stdin(cwd: &std::path::Path, args: &[&str], stdin: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = std::process::Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env(
            "CODEGRAPH_HOME",
            concat!(env!("CARGO_TARGET_TMPDIR"), "/codegraph-home"),
        )
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn codegraph binary");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn analyze_rules_check_runs_examples_without_an_index() {
    let (_dir, root) = temp_project();
    support::write(&root.join("rules/client.yaml"), CLIENT_SEND_RULE);
    // A rule whose good example matches: the check fails and says where.
    support::write(
        &root.join("rules/broken.yaml"),
        "id: any-unwrap\nlanguage: rust\ncheck-patterns:\n  - query: \"((call_expression function: (field_expression field: (field_identifier) @m)) @call (#eq? @m \\\"unwrap\\\"))\"\nexamples:\n  bad: [\"fn f(x: Option<u8>) { x.unwrap(); }\"]\n  good: [\"fn g(x: Option<u8>) { x.unwrap(); }\"]\n",
    );

    let out = run_cli(&root, &["analyze", "rules", "--check", "rules/client.yaml"]);
    assert!(out.status.success(), "{}{}", stdout_str(&out), stderr_str(&out));
    assert!(stdout_str(&out).contains("PASS"), "{}", stdout_str(&out));
    assert!(!root.join(".codegraph").exists(), "--check needs no index");

    let out = run_cli(&root, &["analyze", "rules", "--check", "rules"]);
    assert!(!out.status.success());
    let text = stdout_str(&out);
    assert!(text.contains("FAIL"), "{text}");
    assert!(
        text.contains("good[0] matched check-patterns[0] at example line 1"),
        "{text}"
    );
    assert!(text.contains("1 passed, 1 failed"), "{text}");

    let report = run_analyze_envelope(&root, &["rules", "--check", "rules/client.yaml"]);
    assert_eq!(report["kind"], "rules-check");
    assert_eq!(report["data"]["passed"], 1);

    // The built-in rules pass their own examples.
    let out = run_cli(&root, &["analyze", "rules", "--check", "--builtin"]);
    assert!(out.status.success(), "{}", stdout_str(&out));

    // Rule YAML from stdin, with a load error located on its line.
    let out = run_cli_stdin(
        &root,
        &["analyze", "rules", "--check", "--rule-text", "-"],
        "id: x\nlanguage: rust\ncheck-patterns:\n  - query: \"(identifier) @i\"\n    wher: []\n",
    );
    assert!(!out.status.success());
    let text = stdout_str(&out);
    assert!(text.contains("<stdin>:5"), "{text}");
    assert!(text.contains("did you mean `where`?"), "{text}");
    let out = run_cli_stdin(
        &root,
        &["analyze", "rules", "--check", "--rule-text", "-"],
        CLIENT_SEND_RULE,
    );
    assert!(out.status.success(), "{}", stdout_str(&out));
}

#[test]
fn analyze_rules_reports_findings_through_the_index() {
    let (_dir, root) = temp_project();
    support::write(
        &root.join("src/lib.rs"),
        "pub struct Client;\npub struct Sender;\n\nimpl Client {\n    pub fn send(&self) -> u32 {\n        1\n    }\n}\n\nimpl Sender {\n    pub fn send(&self) -> u32 {\n        2\n    }\n}\n\npub fn validate() {}\n\npub fn post(c: &Client, tx: &Sender) -> u32 {\n    c.send() + tx.send()\n}\n\npub fn checked(c: &Client) -> u32 {\n    validate();\n    c.send()\n}\n",
    );
    init_fixture_files_only(&root);

    // From stdin, run over the index: only the Client::send call in the
    // function without validate() is reported.
    let out = run_cli_stdin(
        &root,
        &["analyze", "rules", "--rule-text", "-", "--json"],
        CLIENT_SEND_RULE,
    );
    assert!(out.status.success(), "{}", stderr_str(&out));
    let envelope: serde_json::Value = serde_json::from_str(stdout_str(&out).trim()).unwrap();
    assert_eq!(envelope["kind"], "rules");
    let report = &envelope["data"];
    let findings = report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{report}");
    let finding = &findings[0];
    assert_eq!(finding["rule"], "client-send-unvalidated");
    assert_eq!(finding["detector"], "rule");
    assert_eq!(finding["file"], "src/lib.rs");
    assert_eq!(finding["line"], 19);
    assert_eq!(finding["col"], 4);
    assert_eq!(finding["function"], "post");
    assert_eq!(finding["confidence"], 0.75);
    assert_eq!(
        finding["message"],
        "send reaches Client::send without validate() in post"
    );
    assert_eq!(report["byRule"]["client-send-unvalidated"], 1);

    // Review packets for rule findings carry the rule's own question.
    support::write(&root.join("rules.yaml"), CLIENT_SEND_RULE);
    let packets = run_analyze_json(
        &root,
        &["review", "--rules", "rules.yaml", "--detector", "rule"],
    );
    let packets = packets.as_array().unwrap();
    assert_eq!(packets.len(), 1, "{packets:?}");
    assert_eq!(packets[0]["finding"]["rule"], "client-send-unvalidated");
    assert!(
        packets[0]["checklist"][0]
            .as_str()
            .unwrap()
            .starts_with("Rule `client-send-unvalidated` (high)"),
        "{}",
        packets[0]
    );
    assert!(
        packets[0]["function"]["source"]
            .as_str()
            .unwrap()
            .contains("19\t    c.send() + tx.send()"),
        "{}",
        packets[0]
    );

    // A rule that does not load stops the run and says why.
    support::write(&root.join("bad.yaml"), "id: x\nlanguage: rust\n");
    let out = run_cli(&root, &["analyze", "rules", "--rules", "bad.yaml"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("missing field `check-patterns`"), "{}", stderr_str(&out));

    // Human output.
    let out = run_cli(&root, &["analyze", "rules", "rules.yaml"]);
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert!(stdout_str(&out).contains("src/lib.rs:19"), "{}", stdout_str(&out));
}

#[test]
fn analyze_rules_json_is_unbounded_unless_top_is_given() {
    let (_dir, root) = temp_project();
    let source: String = (0..60)
        .map(|i| format!("pub fn f{i}(x: Option<u8>) -> u8 {{\n    x.unwrap()\n}}\n\n"))
        .collect();
    support::write(&root.join("src/lib.rs"), &source);
    support::write(
        &root.join("unwrap.yaml"),
        "id: any-unwrap\nlanguage: rust\ncheck-patterns:\n  - query: \"((call_expression function: (field_expression field: (field_identifier) @m)) @call (#eq? @m \\\"unwrap\\\"))\"\nexamples:\n  bad: [\"fn f(x: Option<u8>) { x.unwrap(); }\"]\n  good: [\"fn g(x: Option<u8>) { x.expect(\\\"set\\\"); }\"]\n",
    );
    init_fixture_files_only(&root);

    // JSON is for a consumer that filters itself: every finding.
    let report = run_analyze_json(&root, &["rules", "unwrap.yaml"]);
    assert_eq!(report["findings"].as_array().unwrap().len(), 60, "{report}");
    assert_eq!(report["findingsOmitted"], 0, "{report}");

    // An explicit --top still applies to JSON.
    let report = run_analyze_json(&root, &["rules", "unwrap.yaml", "--top", "7"]);
    assert_eq!(report["findings"].as_array().unwrap().len(), 7, "{report}");
    assert_eq!(report["findingsOmitted"], 53, "{report}");

    // A person reading the list gets the 50 most confident.
    let out = run_cli(&root, &["analyze", "rules", "unwrap.yaml"]);
    assert!(out.status.success(), "{}", stderr_str(&out));
    let text = stdout_str(&out);
    assert!(text.contains("Rule findings: 50 of 60"), "{text}");
    assert!(text.contains("10 more (raise --top)"), "{text}");
}

/// A tiny labeled corpus: six functions that `eval` their input (bad), six
/// that parse it (good), and one more `eval` outside every row.
fn write_score_corpus(root: &std::path::Path) {
    let mut source = String::new();
    let mut rows = String::new();
    for (label, body) in [("bad", "eval(data)"), ("good", "int(data)")] {
        for i in 0..6 {
            let line = source.lines().count() + 1;
            source.push_str(&format!("def {label}_{i}(data):\n    return {body}\n\n"));
            rows.push_str(&format!(
                "{{\"file\": \"app.py\", \"label\": \"{label}\", \"granularity\": \"function\", \
                 \"line_start\": {line}, \"line_end\": {}, \"cwe\": \"CWE-95\"}}\n",
                line + 1
            ));
        }
    }
    source.push_str("print(eval('1'))\n");
    support::write(&root.join("app.py"), &source);
    support::write(&root.join("ground_truth.jsonl"), &rows);
}

const SCORE_RULES: &str = r#"id: py-eval
language: python
check-patterns:
  - query: |
      ((call function: (identifier) @f) @call (#eq? @f "eval"))
examples:
  bad: ["eval(x)"]
  good: ["int(x)"]
---
id: py-int
language: python
check-patterns:
  - query: |
      ((call function: (identifier) @f) @call (#eq? @f "int"))
examples:
  bad: ["int(x)"]
  good: ["eval(x)"]
"#;

#[test]
fn analyze_rules_score_judges_rules_against_ground_truth() {
    let (_dir, root) = temp_project();
    write_score_corpus(&root.join("corpus"));
    support::write(&root.join("rules.yaml"), SCORE_RULES);
    let score = |extra: &[&str]| {
        let mut args = vec![
            "rules",
            "rules.yaml",
            "--no-saved",
            "--score",
            "corpus",
            "--work",
            "work",
        ];
        args.extend_from_slice(extra);
        run_analyze_envelope(&root, &args)
    };

    // The unit is staged and indexed under --work, then scored.
    let report = score(&[]);
    assert_eq!(report["kind"], "rules-score");
    let data = &report["data"];
    assert_eq!(data["complete"], true, "{data}");
    assert_eq!(data["units"]["indexed"], 1);
    assert_eq!(data["baseRate"], 0.5);
    assert_eq!(data["positives"], 6);
    let rules = data["rules"].as_array().unwrap();
    let eval = rules.iter().find(|r| r["id"] == "py-eval").unwrap();
    assert_eq!(eval["findings"], 7);
    assert_eq!(
        (eval["tp"].as_u64(), eval["fp"].as_u64()),
        (Some(6), Some(0))
    );
    assert_eq!(eval["unlabeled"], 1);
    assert_eq!(eval["verdict"], "keep", "{eval}");
    let int = rules.iter().find(|r| r["id"] == "py-int").unwrap();
    assert_eq!(int["verdict"], "discard", "{int}");
    // bugbench's metrics shape rides along for comparison with score.py.
    let metrics = &data["metrics"];
    assert_eq!(metrics["rows"]["bad"], 6);
    assert_eq!(metrics["per_rule"]["py-eval"]["precision"], 1.0);
    assert_eq!(metrics["per_rule"]["py-int"]["good_flag_rate"], 1.0);
    assert_eq!(metrics["slack"], 3);
    assert!(root.join("work/home").is_dir(), "indexed under a scratch home");
    assert!(
        !root.join("corpus/.codegraph").exists(),
        "the corpus is never indexed in place"
    );

    // Rerun: the fresh index is reused; human output gives the verdicts.
    let again = score(&[]);
    assert_eq!(again["data"]["units"]["indexed"], 0);
    let out = run_cli(
        &root,
        &[
            "analyze",
            "rules",
            "rules.yaml",
            "--no-saved",
            "--score",
            "corpus",
            "--work",
            "work",
        ],
    );
    assert!(out.status.success(), "{}", stderr_str(&out));
    let text = stdout_str(&out);
    assert!(text.contains("KEEP"), "{text}");
    assert!(text.contains("DISCARD"), "{text}");

}

/// A tiny RustSec-style corpus: two advisories, each a `vuln/` version that
/// `eval`s in `run` and a `fixed/` one that parses, with the fix region on
/// `run` in `vuln/`.
fn write_pairs_corpus(root: &std::path::Path) {
    let mut advisories = String::new();
    let mut rows = String::new();
    for id in ["RUSTSEC-0001", "RUSTSEC-0002"] {
        advisories.push_str(&format!(
            "{{\"advisory\": \"{id}\", \"vuln_dir\": \"{id}/vuln\", \"fixed_dir\": \"{id}/fixed\", \
             \"diff_tightness\": \"tight\", \"localization\": \"fix_commit\", \"categories\": [\"code-execution\"]}}\n"
        ));
        support::write(
            &root.join(format!("{id}/vuln/app.py")),
            "def run(data):\n    return eval(data)\n\n\ndef other(data):\n    return eval('1')\n",
        );
        support::write(
            &root.join(format!("{id}/fixed/app.py")),
            "def run(data):\n    return int(data)\n\n\ndef other(data):\n    return eval('1')\n",
        );
        rows.push_str(&format!(
            "{{\"file\": \"{id}/vuln/app.py\", \"label\": \"bad\", \"granularity\": \"function\", \
             \"line_start\": 1, \"line_end\": 2, \"function\": \"run\"}}\n"
        ));
    }
    support::write(&root.join("advisories.jsonl"), &advisories);
    support::write(&root.join("ground_truth.jsonl"), &rows);
}

#[test]
fn analyze_rules_score_is_differential_on_pairs_and_resumes() {
    let (_dir, root) = temp_project();
    write_pairs_corpus(&root.join("pairs"));
    support::write(&root.join("rules.yaml"), SCORE_RULES);
    let score = |extra: &[&str]| {
        let mut args = vec![
            "rules",
            "rules.yaml",
            "--no-saved",
            "--score",
            "pairs",
            "--work",
            "work",
            "--jobs",
            "1",
        ];
        args.extend_from_slice(extra);
        run_analyze_json(&root, &args)
    };

    // A deadline of 0 still scores one unit (every call makes progress),
    // then hands back a cursor at the first unit left.
    let cut = score(&["--deadline", "0"]);
    assert_eq!(cut["scoring"], "differential");
    assert_eq!(cut["complete"], false, "{cut}");
    assert_eq!(cut["units"]["done"], 1, "{cut}");
    assert_eq!(cut["units"]["pending"], 3);
    let cursor = cut["nextCursor"].as_str().unwrap().to_string();
    assert!(cursor.starts_with("1:"), "{cursor}");

    // Resumed to the end: the `eval` in `run` is gone after each fix (a
    // catch in the fix region), the one in `other` stays (background).
    let mut data = score(&["--cursor", &cursor]);
    while data["complete"] == false {
        let cursor = data["nextCursor"].as_str().unwrap().to_string();
        data = score(&["--deadline", "0", "--cursor", &cursor]);
    }
    assert_eq!(data["units"]["done"], 4, "{data}");
    assert_eq!(data["positives"], 2, "two scoreable pairs");
    let eval = data["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "py-eval")
        .unwrap()
        .clone();
    assert_eq!(eval["tp"], 2, "{eval}");
    assert_eq!(eval["background"], 2, "{eval}");
    assert_eq!(eval["offFix"], 0, "{eval}");
    assert_eq!(eval["precision"], 1.0, "{eval}");
    assert_eq!(eval["recall"], 1.0, "{eval}");
    assert_eq!(data["metrics"]["overall"]["pairs_detected"], 2);
    assert_eq!(data["metrics"]["slack"], 5);
    // Too few differential findings to keep, however precise.
    assert_eq!(eval["verdict"], "discard", "{eval}");

    // A cursor from other rules is refused.
    let out = run_cli(
        &root,
        &[
            "analyze",
            "rules",
            "--builtin",
            "--no-saved",
            "--score",
            "pairs",
            "--work",
            "work",
            "--cursor",
            &cursor,
        ],
    );
    assert!(!out.status.success());
    assert!(
        stderr_str(&out).contains("other rules"),
        "{}",
        stderr_str(&out)
    );
}

#[test]
fn analyze_rules_runs_the_projects_saved_rules() {
    let (_dir, root) = temp_project();
    support::write(
        &root.join("src/lib.rs"),
        "pub struct Client;\nimpl Client {\n    pub fn send(&self) -> u32 {\n        1\n    }\n}\npub fn post(c: &Client) -> u32 {\n    c.send()\n}\n",
    );
    init_fixture_files_only(&root);
    let out = run_cli(&root, &["analyze", "rules"]);
    assert!(!out.status.success(), "no rules at all is an error");
    support::write(&root.join(".codegraph/rules/client.yaml"), CLIENT_SEND_RULE);

    let report = run_analyze_json(&root, &["rules"]);
    assert_eq!(
        report["byRule"]["client-send-unvalidated"], 1,
        "{report}"
    );
    let report = run_analyze_json(&root, &["rules", "--builtin", "--no-saved"]);
    assert!(
        report["byRule"].get("client-send-unvalidated").is_none(),
        "{report}"
    );
    // A rule passed by name shadows the saved rule of its id.
    let shadow = CLIENT_SEND_RULE.replace("^Client::send$", "^Nothing$");
    support::write(&root.join("edited.yaml"), &shadow);
    let report = run_analyze_json(&root, &["rules", "edited.yaml"]);
    assert!(
        report["byRule"].get("client-send-unvalidated").is_none(),
        "{report}"
    );
    let out = run_cli(&root, &["analyze", "rules", "--check"]);
    assert!(out.status.success(), "{}", stdout_str(&out));
    let text = stdout_str(&out);
    assert!(
        text.contains(".codegraph/rules/client.yaml") && text.contains("1 passed, 0 failed"),
        "{text}"
    );
}
