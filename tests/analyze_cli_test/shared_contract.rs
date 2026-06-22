#[test]
fn analyze_human_output_succeeds_without_json() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "complexity"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(stdout_str(&out).contains("Most complex functions"));

    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("mutual recursion"));
}

#[test]
fn analyze_unknown_symbol_exits_zero_with_message() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "dominators", "noSuchSymbolAnywhere"]);
    assert!(out.status.success(), "missing symbol is not an error");
    assert!(stdout_str(&out).contains("not found"));
}

#[test]
fn analyze_requires_initialized_project() {
    let (_dir, root) = temp_project();
    // No init.
    let out = run_cli(&root, &["analyze", "cycles"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("not initialized"));
}

#[test]
fn analyze_json_is_wrapped_in_versioned_envelope() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let envelope = run_analyze_envelope(&root, &["cycles"]);
    assert_eq!(envelope["schemaVersion"].as_u64(), Some(1));
    assert_eq!(envelope["kind"].as_str(), Some("cycles"));
    assert_eq!(envelope["data"]["cycleCount"].as_u64(), Some(1));

    // The kind discriminates per subcommand.
    let envelope = run_analyze_envelope(&root, &["slice", "main"]);
    assert_eq!(envelope["kind"].as_str(), Some("slice"));
    let envelope = run_analyze_envelope(&root, &["query", r#"fn("main") | callees"#]);
    assert_eq!(envelope["kind"].as_str(), Some("query"));
    let envelope = run_analyze_envelope(&root, &["query", r#"fn("main")"#, "--explain"]);
    assert_eq!(envelope["kind"].as_str(), Some("queryPlan"));
}
