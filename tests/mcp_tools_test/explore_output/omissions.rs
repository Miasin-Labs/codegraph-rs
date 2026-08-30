#[tokio::test(flavor = "current_thread")]
async fn explore_reports_omissions_and_continuation() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    for i in 0..5 {
        write(
            &dir.path().join(format!("src/widget{i}.ts")),
            &format!(
                "export function widget{i}(input: string): string {{\n  return input.repeat({});\n}}\n",
                i + 1
            ),
        );
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_explore",
        &json!({ "query": "widget0 widget1 widget2 widget3 widget4", "maxFiles": 1 }),
    );
    assert_ne!(
        result.is_error,
        Some(true),
        "explore errored: {}",
        result.text()
    );
    let structured = result.structured_content.as_ref().expect("structured explore");
    assert_eq!(structured["schemaVersion"], 2);

    let included: Vec<&str> = structured["sourceFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    let omissions = structured["omissions"].as_array().unwrap();

    // Summary counts reconcile with the emitted arrays.
    assert_eq!(
        structured["filesIncluded"].as_u64().unwrap() as usize,
        included.len()
    );
    assert_eq!(
        structured["filesOmitted"].as_u64().unwrap() as usize,
        omissions.len()
    );
    assert_eq!(included.len(), 1, "maxFiles=1 should include exactly one file");
    assert!(!omissions.is_empty(), "remaining ranked files must be omitted");

    // Every omission reason belongs to the closed enum.
    let allowed = ["max_files", "budget", "unavailable", "no_source"];
    for o in omissions {
        let reason = o["reason"].as_str().unwrap();
        assert!(allowed.contains(&reason), "unexpected omission reason {reason}");
        assert!(o["path"].is_string());
        assert!(o["symbols"].is_array());
    }
    // The maxFiles cap is attributed deterministically.
    assert!(
        omissions.iter().any(|o| o["reason"] == "max_files"),
        "expected a max_files omission: {omissions:?}"
    );

    // Included and omitted paths are disjoint (no double counting).
    let omitted_paths: Vec<&str> = omissions
        .iter()
        .map(|o| o["path"].as_str().unwrap())
        .collect();
    for path in &included {
        assert!(
            !omitted_paths.contains(path),
            "{path} was both included and omitted"
        );
    }

    // Continuation offers stateless follow-up queries derived from omitted symbols.
    let queries = structured["continuation"]["suggestedQueries"]
        .as_array()
        .unwrap();
    assert!(!queries.is_empty(), "expected continuation suggestions");
    assert!(queries.iter().all(|q| q.is_string()));
}

#[tokio::test(flavor = "current_thread")]
async fn explore_returns_a_structured_error_when_metadata_alone_exceeds_cap() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "600");
    let dir = TempDir::new().unwrap();
    let mut query = Vec::new();
    for index in 0..20 {
        let symbol = format!("veryLongWidgetSymbolName{index:02}");
        query.push(symbol.clone());
        write(
            &dir
                .path()
                .join(format!("src/very-long-widget-file-name-{index:02}.ts")),
            &format!("export function {symbol}(): number {{ return {index}; }}\n"),
        );
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_explore",
        &json!({ "query": query.join(" "), "maxFiles": 1 }),
    );

    assert_eq!(result.is_error, Some(true), "{}", result.text());
    let structured = result.structured_content.expect("structured cap error");
    assert_eq!(structured["kind"], "error");
    assert!(
        serde_json::to_string(&structured).unwrap().len() <= 600,
        "cap error exceeded configured limit: {structured}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explore_cap_retains_omission_metadata_and_marks_truncation() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "2000");
    let dir = TempDir::new().unwrap();
    let big = "z".repeat(4000);
    for i in 0..4 {
        write(
            &dir.path().join(format!("src/widget{i}.ts")),
            &format!("export function widget{i}(): string {{ return \"{big}\"; }}\n"),
        );
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_explore",
        &json!({ "query": "widget0 widget1 widget2 widget3", "maxFiles": 1 }),
    );
    let structured = result.structured_content.as_ref().expect("structured explore");

    // Omission metadata survives an opt-in output cap.
    let omissions = structured["omissions"].as_array().unwrap();
    assert!(
        !omissions.is_empty(),
        "omission metadata must survive the output cap"
    );
    assert!(omissions.iter().any(|o| o["reason"] == "max_files"));

    // The included file explicitly marks source truncation without corrupting
    // an emitted source chunk with presentation markers.
    let file = &structured["sourceFiles"][0];
    assert_eq!(file["sourceTruncated"], true, "{file}");
    assert!(file["chunks"].as_array().unwrap().iter().all(|chunk| {
        !chunk["source"].as_str().unwrap().contains("[truncated]")
    }));
    assert!(
        serde_json::to_string(structured).unwrap().len() <= 2_000,
        "structured output exceeded configured cap: {structured}"
    );
}
