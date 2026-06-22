#[test]
fn analyze_cycles_json_finds_mutual_recursion() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["cycles"]);
    assert_eq!(json["cycleCount"].as_u64(), Some(1));

    let cycle = &json["cycles"][0];
    assert_eq!(cycle["kind"].as_str(), Some("mutualRecursion"));
    assert_eq!(cycle["size"].as_u64(), Some(2));
    let members = names_of(cycle["members"].as_array().unwrap());
    assert_eq!(members, vec!["ping", "pong"]);

    let suggestions = json["breakSuggestions"].as_array().unwrap();
    assert_eq!(suggestions.len(), 1, "one greedy break suggestion");
}
