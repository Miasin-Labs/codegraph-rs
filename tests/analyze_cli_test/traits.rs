#[test]
fn analyze_traits_reports_hierarchies_and_clusters() {
    let (_dir, root) = temp_project();
    init_close_fixture(&root);

    let json = run_analyze_json(&root, &["traits"]);
    assert_eq!(json["traitCount"].as_u64(), Some(1));
    let hierarchy = &json["hierarchies"][0];
    assert_eq!(hierarchy["trait"]["name"].as_str(), Some("Shape"));
    assert_eq!(hierarchy["implementorCount"].as_u64(), Some(2));
    let implementors = names_of(hierarchy["implementors"].as_array().unwrap());
    assert_eq!(implementors, vec!["Circle", "Square"]);

    // totalArea manipulates Shape → clustered under it.
    let clusters = json["clusters"].as_array().unwrap();
    let shape_cluster = clusters
        .iter()
        .find(|c| c["primaryType"]["name"].as_str() == Some("Shape"))
        .expect("Shape cluster");
    let members = names_of(shape_cluster["functions"].as_array().unwrap());
    assert!(members.contains(&"totalArea"), "members: {members:?}");

    // Type filter narrows to the requested type.
    let json = run_analyze_json(&root, &["traits", "Shape"]);
    assert_eq!(json["traitCount"].as_u64(), Some(1));
    let json = run_analyze_json(&root, &["traits", "NoSuchType"]);
    assert_eq!(json["traitCount"].as_u64(), Some(0));

    // Human output renders the hierarchy.
    let out = run_cli(&root, &["analyze", "traits"]);
    assert!(out.status.success());
    let stdout = stdout_str(&out);
    assert!(stdout.contains("Shape") && stdout.contains("Circle"));
}
