#[test]
fn analyze_complexity_json_reports_per_function_metrics() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["complexity", "--top", "3"]);
    assert_eq!(json["functionsAnalyzed"].as_u64(), Some(5));
    assert_eq!(json["functionsTotal"].as_u64(), Some(5));

    let functions = json["functions"].as_array().expect("functions array");
    assert_eq!(functions.len(), 3, "--top caps the list");

    // compute (loop + branch) is the most complex function in the fixture.
    let first = &functions[0];
    assert_eq!(first["symbol"]["name"].as_str(), Some("compute"));
    assert_eq!(first["symbol"]["file"].as_str(), Some("src/util.ts"));
    assert_eq!(first["language"].as_str(), Some("typescript"));
    assert!(first["cyclomatic"].as_u64().unwrap() >= 3);
    assert!(first["cognitive"].as_u64().unwrap() >= 2);
    assert!(first["maxNesting"].as_u64().unwrap() >= 2);
    assert!(first["maintainabilityIndex"].as_f64().is_some());

    // Sorted cyclomatic-descending.
    let cyclo: Vec<u64> = functions
        .iter()
        .map(|f| f["cyclomatic"].as_u64().unwrap())
        .collect();
    let mut sorted = cyclo.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(cyclo, sorted);
}
