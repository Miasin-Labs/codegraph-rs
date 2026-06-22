#[test]
fn analyze_impact_json_lists_direct_call_sites() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(
        &root,
        &[
            "impact",
            "helper",
            "--signature",
            "helper(x: number, y: number): number",
        ],
    );
    assert_eq!(json["target"]["name"].as_str(), Some("helper"));
    assert_eq!(
        json["newSignature"].as_str(),
        Some("helper(x: number, y: number): number")
    );
    assert_eq!(
        json["callSiteCount"].as_u64(),
        Some(1),
        "only compute calls helper"
    );
    assert_eq!(json["taskCount"].as_u64(), Some(1));

    let task = &json["tasks"][0];
    assert_eq!(task["file"].as_str(), Some("src/util.ts"));
    assert_eq!(task["callSites"][0]["caller"].as_str(), Some("compute"));
    assert!(task["instruction"].as_str().unwrap().contains("helper"));
}
