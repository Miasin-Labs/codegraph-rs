#[test]
fn analyze_query_valid_dsl_returns_rows() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["query", r#"fn("main") | callees | depth 3"#]);
    assert_eq!(
        json["query"].as_str(),
        Some(r#"fn("main") | callees | depth 3"#)
    );
    let nodes = names_of(json["nodes"].as_array().expect("nodes array"));
    assert!(
        nodes.contains(&"compute") && nodes.contains(&"helper"),
        "main's transitive callees resolved: {json}"
    );
    assert_eq!(json["nodeCount"].as_u64(), Some(nodes.len() as u64));
    assert_eq!(json["truncated"].as_bool(), Some(false));
}

/// Every worked example in `analyze query --help` must actually run over a
/// bridged codegraph index (the engine's native adapters see more kinds
/// than the bridge carries — these are pinned to calls/contains data).
#[test]
fn analyze_query_help_examples_run_on_bridged_index() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Example 1: transitive callees.
    let json = run_analyze_json(&root, &["query", r#"fn("main") | callees | depth 3"#]);
    let nodes = names_of(json["nodes"].as_array().unwrap());
    assert!(nodes.contains(&"compute") && nodes.contains(&"helper"));

    // Example 2: shortest call path (hops in path order in `edges`).
    let json = run_analyze_json(&root, &["query", r#"path fn("main") -> fn("helper")"#]);
    let nodes = names_of(json["nodes"].as_array().unwrap());
    for expected in ["main", "compute", "helper"] {
        assert!(nodes.contains(&expected), "path node {expected}: {json}");
    }
    assert!(
        !json["edges"].as_array().unwrap().is_empty(),
        "path hops surface as edges: {json}"
    );

    // Example 3: strongly-connected components (the ping/pong pair).
    let json = run_analyze_json(&root, &["query", "scc"]);
    let nodes = names_of(json["nodes"].as_array().unwrap());
    assert!(
        nodes.contains(&"ping") && nodes.contains(&"pong"),
        "mutual recursion pair found: {json}"
    );
}

#[test]
fn analyze_query_human_output_renders_table() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "query", r#"fn("main") | callees"#]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("Query results"), "header: {stdout}");
    assert!(
        stdout.contains("KIND") && stdout.contains("NAME") && stdout.contains("LOCATION"),
        "table columns: {stdout}"
    );
    assert!(stdout.contains("compute"), "result row: {stdout}");
}

#[test]
fn analyze_query_why_includes_provenance() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["query", r#"fn("main") | callees"#, "--why"]);
    let why = json["why"].as_array().expect("why array present");
    let compute = why
        .iter()
        .find(|w| w["symbol"]["name"].as_str() == Some("compute"))
        .expect("result row compute is explained");
    let has_main_predecessor = compute["steps"].as_array().unwrap().iter().any(|step| {
        step["predecessors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap_or_default().contains("main"))
    });
    assert!(
        has_main_predecessor,
        "compute's provenance references seed main: {json}"
    );
}

#[test]
fn analyze_query_explain_prints_plan_without_executing() {
    // No init — --explain must not touch the index at all.
    let (_dir, root) = temp_project();

    let json = run_analyze_json(
        &root,
        &[
            "query",
            r#"fn("main") | callees | callees | callees"#,
            "--explain",
        ],
    );
    assert_eq!(json["kind"].as_str(), Some("pipe"));
    let steps: Vec<&str> = json["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(
        steps.iter().any(|s| s.contains("Depth(3)")),
        "depth fusion applied by the optimiser: {steps:?}"
    );
    assert!(json["strategy"].is_string());

    // Human flavor.
    let out = run_cli(
        &root,
        &["analyze", "query", r#"fn("main") | callees"#, "--explain"],
    );
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(stdout_str(&out).contains("not executed"));
}

#[test]
fn analyze_query_syntax_error_exits_one_quoting_token() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(&root, &["analyze", "query", r#"fn("main") | bogus_op"#]);
    assert!(!out.status.success(), "syntax errors exit non-zero");
    let stderr = stderr_str(&out);
    assert!(
        stderr.contains("bogus_op"),
        "offending token quoted: {stderr}"
    );

    // Same contract under --explain (parse error, no panic).
    let out = run_cli(
        &root,
        &["analyze", "query", r#"fn("main") | bogus_op"#, "--explain"],
    );
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("bogus_op"));
}

#[test]
fn analyze_query_aggregation_surfaces_in_metadata() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["query", r#"count fn("ping")"#]);
    let metadata: Vec<&str> = json["metadata"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    assert!(
        metadata.iter().any(|m| m.starts_with("scalar = 1")),
        "count projected into metadata: {json}"
    );
}

#[test]
fn analyze_query_lcov_unblinds_untested_operator() {
    let (_dir, root) = temp_project();
    init_fixture(&root);
    let lcov = write_lcov(&root);

    // ping has no DA lines → untested keeps it.
    let json = run_analyze_json(
        &root,
        &["query", r#"fn("ping") | untested"#, "--lcov", &lcov],
    );
    let nodes = names_of(json["nodes"].as_array().unwrap());
    assert_eq!(nodes, vec!["ping"], "uncovered ping survives: {json}");

    // compute is covered → untested filters it out.
    let json = run_analyze_json(
        &root,
        &["query", r#"fn("compute") | untested"#, "--lcov", &lcov],
    );
    assert_eq!(
        json["nodes"].as_array().unwrap().len(),
        0,
        "covered compute is filtered: {json}"
    );
}
