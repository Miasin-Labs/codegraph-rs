#[test]
fn analyze_slice_json_walks_both_directions() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let fwd = run_analyze_json(&root, &["slice", "main"]);
    assert_eq!(fwd["direction"].as_str(), Some("forward"));
    assert_eq!(fwd["granularity"].as_str(), Some("call-graph"));
    assert_eq!(fwd["size"].as_u64(), Some(2));
    let fwd_names = names_of(fwd["nodes"].as_array().unwrap());
    assert!(fwd_names.contains(&"compute") && fwd_names.contains(&"helper"));
    assert!(
        fwd["note"].as_str().unwrap().contains("call-graph"),
        "honest granularity note present"
    );

    let bwd = run_analyze_json(&root, &["slice", "helper", "--direction", "bwd"]);
    assert_eq!(bwd["direction"].as_str(), Some("backward"));
    let bwd_names = names_of(bwd["nodes"].as_array().unwrap());
    assert!(bwd_names.contains(&"main") && bwd_names.contains(&"compute"));
}

#[test]
fn analyze_slice_rejects_invalid_direction() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let out = run_cli(
        &root,
        &["analyze", "slice", "main", "--direction", "sideways"],
    );
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("--direction"));
}
