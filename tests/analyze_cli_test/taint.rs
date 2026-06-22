#[test]
fn analyze_taint_json_connects_source_to_sink_with_edge_kinds() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    let json = run_analyze_json(&root, &["taint", "main", "helper"]);
    assert_eq!(json["source"]["name"].as_str(), Some("main"));
    assert_eq!(json["sink"]["name"].as_str(), Some("helper"));
    assert_eq!(json["granularity"].as_str(), Some("call-graph"));
    assert_eq!(json["pathCount"].as_u64(), Some(1));

    let path = &json["paths"][0];
    let nodes = names_of(path["nodes"].as_array().unwrap());
    assert_eq!(nodes, vec!["main", "compute", "helper"]);
    let edge_kinds: Vec<&str> = path["edgeKinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(edge_kinds, vec!["calls", "calls"]);
    assert!(
        json["note"].as_str().unwrap().contains("dataflow IR"),
        "honest capability note present"
    );
}

#[test]
fn analyze_taint_json_reports_no_paths_against_call_direction() {
    let (_dir, root) = temp_project();
    init_fixture(&root);

    // helper never reaches main following edge direction.
    let json = run_analyze_json(&root, &["taint", "helper", "main"]);
    assert_eq!(json["pathCount"].as_u64(), Some(0));
    assert_eq!(json["paths"].as_array().unwrap().len(), 0);
}

#[test]
fn analyze_taint_suggest_ranks_sources_and_sinks_by_name() {
    let (_dir, root) = temp_project();
    init_close_fixture(&root);

    let json = run_analyze_json(&root, &["taint", "--suggest"]);
    let sources = json["sources"].as_array().unwrap();
    let sinks = json["sinks"].as_array().unwrap();
    assert!(
        sources
            .iter()
            .any(|c| c["symbol"]["name"].as_str() == Some("readUserInput")),
        "readUserInput is source-named: {json}"
    );
    assert!(
        sinks
            .iter()
            .any(|c| c["symbol"]["name"].as_str() == Some("execQuery")),
        "execQuery is sink-named: {json}"
    );
    let pairs = json["pairs"].as_array().unwrap();
    assert!(!pairs.is_empty());
    assert!(pairs[0]["priority"].as_f64().unwrap() > 0.0);
    assert!(json["note"].as_str().unwrap().contains("naming"));

    // Bare `analyze taint` defaults to suggestion mode.
    let envelope = run_analyze_envelope(&root, &["taint"]);
    assert_eq!(envelope["kind"].as_str(), Some("taintSuggest"));

    // One symbol without --suggest is a usage error.
    let out = run_cli(&root, &["analyze", "taint", "readUserInput"]);
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("--suggest"));
}
