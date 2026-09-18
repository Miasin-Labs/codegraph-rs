#[tokio::test(flavor = "current_thread")]
async fn arch_overview_lists_in_scope_modules_and_symbols_only() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/core/util.ts"),
        "export function helper(): number { return 1; }\nexport class Widget { build(): number { return helper(); } }\n",
    );
    write(
        &dir.path().join("src/app.ts"),
        "import { helper } from \"./core/util\";\nexport function run(): number { return helper(); }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute("codegraph_arch", &json!({ "path": "src/core" }));
    assert_ne!(res.is_error, Some(true), "arch errored: {}", res.text());
    let text = res.text();

    assert!(
        text.contains("Architecture overview"),
        "missing header: {text}"
    );
    assert!(
        text.contains("src/core/util.ts"),
        "in-scope file missing: {text}"
    );
    assert!(text.contains("helper"), "function symbol missing: {text}");
    assert!(text.contains("Widget"), "class symbol missing: {text}");
    assert!(
        !text.contains("src/app.ts ["),
        "out-of-scope file leaked into module listing: {text}"
    );
    assert!(
        text.contains("Depends on (external") && text.contains("Depended on by (external"),
        "boundary sections missing: {text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn xref_lists_incoming_references_to_a_symbol() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/lib.ts"),
        "export function target(): number { return 1; }\nexport function caller(): number { return target(); }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute("codegraph_xref", &json!({ "symbol": "target" }));
    assert_ne!(res.is_error, Some(true), "xref errored: {}", res.text());
    let text = res.text();

    assert!(text.contains("target"), "missing symbol header: {text}");
    assert!(text.contains("caller"), "missing incoming caller: {text}");
}

#[tokio::test(flavor = "current_thread")]
async fn node_returns_structured_payload() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/lib.ts"),
        "export function target(): number { return 1; }\nexport function caller(): number { return target(); }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute(
        "codegraph_node",
        &json!({ "symbol": "target", "includeCode": true }),
    );
    assert_ne!(res.is_error, Some(true), "node errored: {}", res.text());
    let structured = res.structured_content.as_ref().expect("structured node");
    assert_eq!(structured["kind"], "node");
    assert_eq!(structured["matches"][0]["name"], "target");
    assert_eq!(structured["matches"][0]["callers"][0]["name"], "caller");
    assert!(structured["matches"][0]["code"].as_str().unwrap().contains("target"));
}

#[tokio::test(flavor = "current_thread")]
async fn node_structured_code_respects_output_cap() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "600");
    let dir = TempDir::new().unwrap();
    let repeated = "x".repeat(5000);
    write(
        &dir.path().join("src/lib.ts"),
        &format!("export function target(): string {{ return \"{repeated}\"; }}\n"),
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute(
        "codegraph_node",
        &json!({ "symbol": "target", "includeCode": true }),
    );
    let structured = res.structured_content.as_ref().expect("structured node");
    assert!(
        serde_json::to_string(structured).unwrap().len() <= 600,
        "{structured}"
    );
    // A one-line body longer than the cap is withheld whole, not cut inside
    // the line, and the cut is flagged.
    let detail = &structured["matches"][0];
    assert!(detail.get("code").is_none(), "{detail}");
    assert_eq!(detail["codeTruncated"], true, "{detail}");
    assert!(!structured.to_string().contains(&repeated), "structured code was not capped");
}

#[tokio::test(flavor = "current_thread")]
async fn paths_finds_call_chain_from_source_to_sink() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/lib.ts"),
        "export function sink(): number { return 1; }\nexport function mid(): number { return sink(); }\nexport function source(): number { return mid(); }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute(
        "codegraph_paths",
        &json!({ "from": "source", "to": "sink" }),
    );
    assert_ne!(res.is_error, Some(true), "paths errored: {}", res.text());
    let text = res.text();

    assert!(
        text.contains("Path from source to sink"),
        "no path header: {text}"
    );
    assert!(text.contains("mid"), "path should traverse mid: {text}");
}
