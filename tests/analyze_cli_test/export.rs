#[test]
fn analyze_export_emits_dot_for_graph_and_symbol_neighborhood() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Human output is the raw DOT document — pipeable, no decoration.
    let out = run_cli(&root, &["analyze", "export"]);
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let dot = stdout_str(&out);
    assert!(dot.starts_with("digraph"), "raw DOT on stdout: {dot}");
    assert!(dot.contains("->"), "edges rendered: {dot}");
    assert!(dot.contains("compute"), "node labels rendered: {dot}");

    // JSON wraps the document plus scope metadata.
    let json = run_analyze_json(&root, &["export"]);
    assert_eq!(json["format"].as_str(), Some("dot"));
    assert_eq!(json["scope"].as_str(), Some("graph"));
    assert!(json["nodeCount"].as_u64().unwrap() >= 5);
    assert!(json["dot"].as_str().unwrap().starts_with("digraph"));

    // --symbol narrows to the neighborhood.
    let json = run_analyze_json(&root, &["export", "--symbol", "main", "--depth", "1"]);
    assert_eq!(json["scope"].as_str(), Some("subgraph"));
    assert_eq!(json["seed"]["name"].as_str(), Some("main"));
    assert!(json["nodeCount"].as_u64().unwrap() >= 2);

    // Unsupported formats are rejected up front.
    let out = run_cli(&root, &["analyze", "export", "--format", "svg"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("--format"));
}
