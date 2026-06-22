#[test]
fn analyze_validate_judges_arity_change_against_callers() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // Arity change: compute (helper's only caller) needs review.
    let json = run_analyze_json(
        &root,
        &[
            "validate",
            "helper",
            "--params-before",
            "1",
            "--params-after",
            "2",
        ],
    );
    assert_eq!(json["target"]["name"].as_str(), Some("helper"));
    assert_eq!(json["isSafe"].as_bool(), Some(false));
    let incompatible = json["incompatible"].as_array().unwrap();
    assert_eq!(incompatible.len(), 1);
    assert_eq!(incompatible[0]["symbol"]["name"].as_str(), Some("compute"));
    assert!(
        incompatible[0]["reason"].as_str().unwrap().contains("2"),
        "reason names the new arity: {json}"
    );
    assert!(!json["callSites"].as_array().unwrap().is_empty());
    assert!(json["note"].as_str().unwrap().contains("call-graph"));

    // Unchanged arity: safe.
    let json = run_analyze_json(
        &root,
        &[
            "validate",
            "helper",
            "--params-before",
            "1",
            "--params-after",
            "1",
        ],
    );
    assert_eq!(json["isSafe"].as_bool(), Some(true));
    assert_eq!(json["incompatible"].as_array().unwrap().len(), 0);

    // Bad arity argument exits 1.
    let out = run_cli(
        &root,
        &[
            "analyze",
            "validate",
            "helper",
            "--params-before",
            "x",
            "--params-after",
            "2",
        ],
    );
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("--params-before"));
}
