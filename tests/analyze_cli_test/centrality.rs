#[test]
fn analyze_centrality_ranks_symbols_descending() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["centrality", "--top", "3"]);
    assert!(json["analyzed"].as_u64().unwrap() >= 5);
    let nodes = json["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 3, "--top caps the list: {json}");
    let scores: Vec<f64> = nodes.iter().map(|n| n["score"].as_f64().unwrap()).collect();
    let mut sorted = scores.clone();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
    assert_eq!(scores, sorted, "sorted by score descending");

    let out = run_cli(&root, &["analyze", "centrality"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("Most central symbols"));
}

#[test]
fn analyze_critical_finds_articulation_nodes() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["critical"]);
    let nodes = names_of(json["nodes"].as_array().unwrap());
    assert!(
        nodes.contains(&"compute"),
        "compute articulates main → helper: {json}"
    );
    assert!(json["bridgeCount"].as_u64().unwrap() >= 1);
    let bridge = &json["bridges"][0];
    assert!(bridge["from"]["name"].is_string() && bridge["to"]["name"].is_string());
    assert!(json["note"].as_str().unwrap().contains("undirected"));
}
