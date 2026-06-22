#[test]
fn analyze_generics_lists_signature_heuristic_definitions_honestly() {
    let (_dir, root) = temp_project();
    init_close_fixture(&root);

    let json = run_analyze_json(&root, &["generics"]);
    // The bridge carries no generics metadata → engine instantiations are
    // empty, and the note says exactly why.
    assert_eq!(json["instantiationCount"].as_u64(), Some(0));
    assert!(json["note"].as_str().unwrap().contains("does not populate"));
    // The signature heuristic still finds identity<T>.
    let definitions = json["likelyGenericDefinitions"].as_array().unwrap();
    let identity = definitions
        .iter()
        .find(|d| d["symbol"]["name"].as_str() == Some("identity"))
        .expect("identity<T> detected");
    assert_eq!(identity["typeParams"][0].as_str(), Some("T"));

    // Filtered to a non-generic symbol → honest empty message, exit 0.
    let out = run_cli(&root, &["analyze", "generics", "totalArea"]);
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("No generic definition matches"));
}
