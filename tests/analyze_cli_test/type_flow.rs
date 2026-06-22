#[test]
fn analyze_types_propagates_concrete_types_through_traits() {
    let (_dir, root) = temp_project();
    init_close_fixture(&root);

    let json = run_analyze_json(&root, &["types", "totalArea"]);
    assert_eq!(json["symbol"]["name"].as_str(), Some("totalArea"));
    let inputs: Vec<&str> = json["inputTypes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for expected in ["Shape", "Circle", "Square"] {
        assert!(inputs.contains(&expected), "{expected} flows in: {json}");
    }
    assert!(json["functionsAnnotated"].as_u64().unwrap() >= 1);

    let json = run_analyze_json(&root, &["types", "readUserInput"]);
    assert_eq!(json["inputTypes"].as_array().unwrap().len(), 0);
    assert!(json["note"].as_str().unwrap().contains("No concrete types"));
}
