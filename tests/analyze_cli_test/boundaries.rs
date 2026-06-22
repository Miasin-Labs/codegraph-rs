#[test]
fn analyze_boundaries_is_honestly_empty_over_bridged_index() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["boundaries"]);
    assert_eq!(json["boundaryCount"].as_u64(), Some(0));
    assert!(
        json["note"]
            .as_str()
            .unwrap()
            .contains("does not populate these keys"),
        "honest capability note: {json}"
    );
    assert_eq!(json["crossLanguageCalls"]["edgesEmitted"].as_u64(), Some(0));

    // Human output prints the capability note instead of silence.
    let out = run_cli(&root, &["analyze", "boundaries"]);
    assert!(out.status.success());
    assert!(
        stderr_str(&out).contains("No cross-language boundaries")
            || stdout_str(&out).contains("No cross-language boundaries"),
        "stdout: {} stderr: {}",
        stdout_str(&out),
        stderr_str(&out)
    );
}
