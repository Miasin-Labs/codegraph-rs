#[test]
fn analyze_coverage_maps_lcov_onto_functions() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    let lcov = write_lcov(&root);

    let json = run_analyze_json(&root, &["coverage", "--lcov", &lcov]);
    assert_eq!(json["functionsTotal"].as_u64(), Some(5));
    assert!(json["functionsTested"].as_u64().unwrap() >= 2);
    assert!(json["functionsUntested"].as_u64().unwrap() >= 1);
    assert_eq!(json["lcovFiles"].as_u64(), Some(2));

    // --untested filters the listing to untested functions only.
    let json = run_analyze_json(&root, &["coverage", "--lcov", &lcov, "--untested"]);
    let functions = json["functions"].as_array().unwrap();
    assert!(!functions.is_empty());
    for function in functions {
        assert_eq!(function["tested"].as_bool(), Some(false));
        assert_eq!(function["coverageCount"].as_u64(), Some(0));
    }
    let symbols: Vec<serde_json::Value> = functions.iter().map(|f| f["symbol"].clone()).collect();
    let untested = names_of(&symbols);
    assert!(
        untested.contains(&"pong"),
        "pong has no covered lines: {untested:?}"
    );

    // Human output names the untested functions.
    let out = run_cli(
        &root,
        &["analyze", "coverage", "--lcov", &lcov, "--untested"],
    );
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("untested"));
}

#[test]
fn analyze_coverage_unreadable_lcov_exits_one() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "coverage", "--lcov", "missing.info"]);
    assert!(!out.status.success(), "missing LCOV file is an error");
    assert!(stderr_str(&out).contains("missing.info"));
}
