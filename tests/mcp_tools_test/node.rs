fn node_output_schema() -> serde_json::Value {
    tools()
        .into_iter()
        .find(|tool| tool.name == "codegraph_node")
        .expect("codegraph_node tool")
        .output_schema
        .expect("node advertises an output schema")
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_payload_validates_against_advertised_schema() {
    let _env = env_read().await;
    let schema = node_output_schema();
    let dir = TempDir::new().unwrap();
    let mut lines = Vec::new();
    for index in 0..320 {
        lines.push(format!("export const value{index} = {index};"));
    }
    write(&dir.path().join("src/large.ts"), &lines.join("\n"));
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_node", &json!({ "file": "src/large.ts" }));

    assert_ne!(result.is_error, Some(true), "node errored: {}", result.text());
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured node file view");
    assert!(
        schema_matches(&schema, structured),
        "file-mode payload failed advertised schema: {structured}"
    );
    assert_eq!(structured["kind"], "file");
    assert_eq!(structured["path"], "src/large.ts");
    assert_eq!(structured["offset"], 1);
    assert_eq!(structured["limit"], 240);
    assert_eq!(structured["sourceChunks"][0]["startLine"], 1);
    assert_eq!(structured["sourceChunks"][0]["endLine"], 240);
    assert_eq!(structured["sourceTruncated"], true);
    assert!(
        result.text().len() < 16_000,
        "default file-mode output should stay compact, got {} chars",
        result.text().len()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_errors_validate_against_advertised_schema() {
    let _env = env_read().await;
    let schema = node_output_schema();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/state.ts"),
        "export function target(): number { return 1; }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    for arguments in [
        json!({ "file": "src/missing.ts" }),
        json!({ "file": "src/state.ts", "offset": 99 }),
    ] {
        let projected = handler
            .execute("codegraph_node", &arguments)
            .into_mcp_projection()
            .expect("node result projects");
        let structured = projected
            .structured_content
            .expect("structured node error");
        assert!(
            schema_matches(&schema, &structured),
            "file-mode error failed advertised schema: {structured}"
        );
        assert_eq!(structured["kind"], "error");
        assert_eq!(projected.is_error, Some(true));
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_rejects_symlink_replacement_outside_project() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let indexed_path = dir.path().join("src/state.ts");
    write(&indexed_path, "export const ready = true;\n");
    write(
        &outside.path().join("secret.ts"),
        "OUTSIDE_NODE_SECRET_SENTINEL\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    fs::remove_file(&indexed_path).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.ts"), &indexed_path).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_node", &json!({ "file": "src/state.ts" }));

    assert_eq!(result.is_error, Some(true), "{}", result.text());
    assert!(!result.text().contains("OUTSIDE_NODE_SECRET_SENTINEL"));
    assert_eq!(
        result.structured_content.as_ref().unwrap()["kind"],
        "error"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn node_symbol_mode_rejects_symlink_replacement_outside_project() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let indexed_path = dir.path().join("src/state.ts");
    write(
        &indexed_path,
        "export function target(): number { return 1; }\n",
    );
    write(
        &outside.path().join("secret.ts"),
        "OUTSIDE_SYMBOL_SECRET_SENTINEL\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    fs::remove_file(&indexed_path).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.ts"), &indexed_path).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_node",
        &json!({ "symbol": "target", "includeCode": true }),
    );

    assert!(!result.text().contains("OUTSIDE_SYMBOL_SECRET_SENTINEL"));
    let structured = result.structured_content.expect("structured node result");
    assert!(structured["matches"][0].get("code").is_none(), "{structured}");
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_preserves_source_below_configured_cap() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "100000");
    let dir = TempDir::new().unwrap();
    let source = format!("export const payload = \"{}\";", "x".repeat(3_000));
    write(&dir.path().join("src/state.ts"), &source);
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let structured = handler
        .execute("codegraph_node", &json!({ "file": "src/state.ts" }))
        .structured_content
        .expect("structured node file view");

    assert_eq!(structured["sourceChunks"][0]["source"], source);
    assert_eq!(structured["sourceTruncated"], false);
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_marks_source_truncation_under_a_tight_cap() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "600");
    let dir = TempDir::new().unwrap();
    let source = format!("export const payload = \"{}\";", "x".repeat(5_000));
    write(&dir.path().join("src/state.ts"), &source);
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let structured = handler
        .execute("codegraph_node", &json!({ "file": "src/state.ts" }))
        .structured_content
        .expect("structured node file view");

    assert_eq!(structured["sourceTruncated"], true, "{structured}");
    assert!(
        serde_json::to_string(&structured).unwrap().len() <= 600,
        "capped payload exceeded configured limit: {structured}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_withholds_toml_values() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("Cargo.toml"),
        "[registry]\ntoken = \"cargo-secret-sentinel\"\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_node", &json!({ "file": "Cargo.toml" }));
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured node file view");

    assert_eq!(structured["valuesWithheld"], true);
    assert_eq!(structured["sourceChunks"], json!([]));
    assert!(!result.text().contains("cargo-secret-sentinel"));
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_symbols_only_payload_validates_against_advertised_schema() {
    let _env = env_read().await;
    let schema = node_output_schema();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/state.ts"),
        "export function target(): number {\n  return 1;\n}\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_node",
        &json!({ "file": "src/state.ts", "symbolsOnly": true }),
    );

    assert_ne!(result.is_error, Some(true), "node errored: {}", result.text());
    let structured = result
        .structured_content
        .expect("structured node symbols-only view");
    assert!(
        schema_matches(&schema, &structured),
        "symbols-only payload failed advertised schema: {structured}"
    );
    assert_eq!(structured["kind"], "file");
    assert_eq!(structured["path"], "src/state.ts");
    assert_eq!(structured["sourceChunks"], json!([]));
    assert_eq!(structured["valuesWithheld"], false);
    assert_eq!(structured["sourceTruncated"], false);
}
