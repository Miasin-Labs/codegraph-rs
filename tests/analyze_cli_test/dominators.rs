#[test]
fn analyze_dominators_json_chains_back_to_entry() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["dominators", "main"]);
    assert_eq!(json["entry"]["name"].as_str(), Some("main"));
    assert_eq!(json["analyzed"].as_u64(), Some(2));

    let nodes = json["nodes"].as_array().expect("nodes array");
    let helper = nodes
        .iter()
        .find(|n| n["symbol"]["name"].as_str() == Some("helper"))
        .expect("helper reachable from main");
    assert_eq!(
        helper["immediateDominator"]["name"].as_str(),
        Some("compute"),
        "every path from main to helper passes through compute"
    );
    assert_eq!(helper["dominatorDepth"].as_u64(), Some(2));
}
