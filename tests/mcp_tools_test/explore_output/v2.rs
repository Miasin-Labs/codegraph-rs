#[tokio::test(flavor = "current_thread")]
async fn explore_returns_version_two_source_chunks() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    const SOURCE: &str = "export function target(): number {\n  return 1;\n}\n// persist project cache gaps after restart";
    write(&dir.path().join("src/state.ts"), SOURCE);
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_explore", &json!({ "query": "target persist" }));
    assert_ne!(result.is_error, Some(true), "explore errored: {}", result.text());
    let structured = result.structured_content.as_ref().expect("structured explore");
    assert_eq!(structured["schemaVersion"], 2);
    assert_eq!(structured["kind"], "explore");
    assert_eq!(structured["query"], "target persist");
    assert!(structured["totalSymbols"].as_u64().unwrap() > 0);
    let source_file = structured["sourceFiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "src/state.ts")
        .expect("state.ts source file");
    assert!(source_file.get("header").is_none(), "{source_file}");
    assert!(source_file.get("body").is_none(), "{source_file}");
    let chunk = &source_file["chunks"][0];
    assert_eq!(chunk["startLine"], 1);
    assert_eq!(chunk["endLine"], 4);
    assert_eq!(chunk["mode"], "whole");
    assert!(chunk["symbols"].as_array().is_some());
    assert_eq!(chunk["source"], SOURCE);
    let raw_source = chunk["source"].as_str().unwrap();
    assert!(!raw_source.contains("```"), "{raw_source}");
    assert!(!raw_source.contains("... (gap) ..."), "{raw_source}");
    assert!(!regex::Regex::new(r"(?m)^\d+\t").unwrap().is_match(raw_source));
}

#[tokio::test(flavor = "current_thread")]
async fn explore_source_chunks_are_invariant_under_line_number_toggle() {
    let _env = env_write().await;
    let dir = TempDir::new().unwrap();
    let mut source = (1..=320)
        .map(|line| format!("// filler line {line}"))
        .collect::<Vec<_>>();
    source.extend([
        "export function target(): number {".to_string(),
        "  return 7;".to_string(),
        "}".to_string(),
    ]);
    write(&dir.path().join("src/large.ts"), &source.join("\n"));
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let numbered = {
        let _guard = EnvVarGuard::unset("CODEGRAPH_EXPLORE_LINENUMS");
        handler
            .execute("codegraph_explore", &json!({ "query": "target" }))
            .structured_content
            .expect("numbered structured explore")["sourceFiles"]
            .clone()
    };
    let unnumbered = {
        let _guard = EnvVarGuard::set("CODEGRAPH_EXPLORE_LINENUMS", "0");
        handler
            .execute("codegraph_explore", &json!({ "query": "target" }))
            .structured_content
            .expect("unnumbered structured explore")["sourceFiles"]
            .clone()
    };

    assert_eq!(numbered, unnumbered);
    let chunk = &numbered[0]["chunks"][0];
    assert_eq!(chunk["startLine"], 318);
    assert_eq!(chunk["endLine"], 323);
    assert_eq!(chunk["mode"], "excerpt");
    assert_eq!(
        chunk["source"],
        "// filler line 318\n// filler line 319\n// filler line 320\nexport function target(): number {\n  return 7;\n}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explore_adaptive_renderer_emits_typed_raw_chunks() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let structured = handler
        .execute(
            "codegraph_explore",
            &json!({ "query": spare_query(), "maxFiles": 15 }),
        )
        .structured_content
        .expect("structured adaptive explore");
    let files = structured["sourceFiles"].as_array().unwrap();
    let bridge = files
        .iter()
        .find(|file| file["path"] == "src/bridge-interceptor.ts")
        .expect("bridge source file");
    let bridge_chunks = bridge["chunks"].as_array().unwrap();
    assert!(!bridge_chunks.is_empty());
    assert!(bridge_chunks.iter().all(|chunk| chunk["mode"] == "signature"));
    assert!(bridge_chunks.iter().all(|chunk| {
        let source = chunk["source"].as_str().unwrap();
        !source.contains("BRIDGE_BODY_MARKER")
            && !source.contains("```")
            && !regex::Regex::new(r"^\d+\t").unwrap().is_match(source)
    }));

    let codec = files
        .iter()
        .find(|file| file["path"] == "src/codec.ts")
        .expect("codec source file");
    let codec_chunks = codec["chunks"].as_array().unwrap();
    assert!(codec_chunks.iter().any(|chunk| chunk["mode"] == "body"));
    assert!(codec_chunks.iter().any(|chunk| chunk["mode"] == "signature"));
}

#[tokio::test(flavor = "current_thread")]
async fn explore_no_result_payload_is_version_two_with_no_source_files() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let structured = handler
        .execute("codegraph_explore", &json!({ "query": "missing_symbol" }))
        .structured_content
        .expect("structured empty explore");
    assert_eq!(structured["schemaVersion"], 2);
    assert_eq!(structured["sourceFiles"], json!([]));
}
