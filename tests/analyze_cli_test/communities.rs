#[test]
fn analyze_communities_json_separates_call_clusters() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["communities"]);
    assert!(json["modularity"].is_number());
    assert!(
        json["multiMemberCount"].as_u64().unwrap() >= 2,
        "main/compute/helper and ping/pong form separate call clusters: {json}"
    );

    let communities = json["communities"].as_array().expect("communities array");
    // Sorted by size descending.
    let sizes: Vec<u64> = communities
        .iter()
        .map(|c| c["size"].as_u64().unwrap())
        .collect();
    let mut sorted = sizes.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(sizes, sorted);

    // The ping/pong pair lands in one community together.
    let has_recursion_pair = communities.iter().any(|c| {
        let names = names_of(c["members"].as_array().unwrap());
        names.contains(&"ping") && names.contains(&"pong")
    });
    assert!(has_recursion_pair, "ping/pong share a community: {json}");
}
