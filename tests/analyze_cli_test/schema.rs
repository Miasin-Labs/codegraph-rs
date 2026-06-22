#[test]
fn analyze_schema_prints_engine_json_schemas() {
    // Pure schema read — no init required.
    let (_dir, root) = temp_project();

    for kind in [
        "query_result",
        "entrypoint_summary",
        "context_result",
        "formatted_output",
    ] {
        let out = run_cli(&root, &["analyze", "schema", kind]);
        assert!(out.status.success(), "schema {kind} exits 0");
        let schema: serde_json::Value = serde_json::from_str(stdout_str(&out).trim())
            .unwrap_or_else(|e| panic!("schema {kind} is valid JSON ({e})"));
        assert!(schema["title"].is_string());
        assert_eq!(
            schema["properties"]["schema_version"]["type"].as_str(),
            Some("integer")
        );
    }

    let out = run_cli(&root, &["analyze", "schema", "bogus"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("known kinds"));
}
