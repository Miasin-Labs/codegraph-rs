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
