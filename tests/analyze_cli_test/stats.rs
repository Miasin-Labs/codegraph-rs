#[test]
fn analyze_stats_counts_graph_and_estimates_reachability() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["stats"]);
    assert_eq!(json["nodesByKind"]["function"].as_u64(), Some(5));
    assert!(json["nodeCount"].as_u64().unwrap() >= 5);
    assert!(json["edgesByKind"]["calls"].as_u64().unwrap() >= 4);
    assert_eq!(json["fileCount"].as_u64(), Some(2));
    assert!(
        json.get("reachability").is_none() || json["reachability"].is_null(),
        "reachability is opt-in: {json}"
    );

    let json = run_analyze_json(&root, &["stats", "--estimate-reachability", "--top", "5"]);
    let reachability = &json["reachability"];
    assert_eq!(
        reachability["method"].as_str(),
        Some("exact"),
        "small graphs get exact numbers: {json}"
    );
    let top = reachability["top"].as_array().unwrap();
    assert!(!top.is_empty() && top.len() <= 5);
    let main_entry = top
        .iter()
        .find(|e| e["symbol"]["name"].as_str() == Some("main"));
    if let Some(main_entry) = main_entry {
        assert!(
            main_entry["descendants"].as_f64().unwrap() >= 2.0,
            "main reaches compute and helper: {json}"
        );
    }

    let out = run_cli(&root, &["analyze", "stats", "--estimate-reachability"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("Bridged analysis graph"));
}
