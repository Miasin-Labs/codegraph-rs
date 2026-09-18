fn tool_output_schema(name: &str) -> serde_json::Value {
    tools()
        .into_iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("{name} tool"))
        .output_schema
        .unwrap_or_else(|| panic!("{name} advertises an output schema"))
}

async fn batch_fixture(root: &Path) -> ToolHandler {
    write(
        &root.join("src/auth.ts"),
        "export function parseToken(raw: string) { return raw.trim(); }\n\
         export class AuthService { signIn(token: string) { return parseToken(token); } }\n\
         export function unrelatedHelper() { return 1; }\n",
    );
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    ToolHandler::new(Some(Rc::new(cg)))
}

/// One search call answers several names, grouped per name — the shape agents
/// otherwise fake with a `a|b|c` grep alternation.
#[tokio::test(flavor = "current_thread")]
async fn search_accepts_several_names_in_one_call() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let handler = batch_fixture(dir.path()).await;

    let result = handler.execute(
        "codegraph_search",
        &json!({ "query": "parseToken", "symbols": ["AuthService", "noSuchSymbolAnywhere"] }),
    );
    assert_ne!(result.is_error, Some(true), "search errored: {}", result.text());
    let payload = result.structured_content.as_ref().expect("structured search");
    assert!(
        schema_matches(&tool_output_schema("codegraph_search"), payload),
        "batch search payload failed advertised schema: {payload}"
    );
    // Names that found nothing are listed; the request is not echoed back.
    assert_eq!(payload["unmatched"], json!(["noSuchSymbolAnywhere"]));
    assert!(payload.get("queries").is_none(), "{payload}");
    let matched: Vec<&str> = payload["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|hit| hit["matchedQuery"].as_str())
        .collect();
    assert!(matched.contains(&"parseToken"), "{payload}");
    assert!(matched.contains(&"AuthService"), "{payload}");
    let text = result.text();
    assert!(text.contains("### `noSuchSymbolAnywhere` - 0 matches"), "{text}");
}

/// A single-name search keeps its original payload shape.
#[tokio::test(flavor = "current_thread")]
async fn search_single_name_shape_is_unchanged() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let handler = batch_fixture(dir.path()).await;

    let result = handler.execute("codegraph_search", &json!({ "query": "parseToken" }));
    let payload = result.structured_content.as_ref().expect("structured search");
    assert!(payload.get("unmatched").is_none(), "{payload}");
    assert!(
        payload["results"][0].get("matchedQuery").is_none(),
        "{payload}"
    );
}

/// `node` reads several symbols in one call and merges their matches.
#[tokio::test(flavor = "current_thread")]
async fn node_reads_several_symbols_in_one_call() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let handler = batch_fixture(dir.path()).await;

    let result = handler.execute(
        "codegraph_node",
        &json!({ "symbol": "parseToken", "symbols": ["AuthService"], "includeCode": true }),
    );
    assert_ne!(result.is_error, Some(true), "node errored: {}", result.text());
    let payload = result.structured_content.as_ref().expect("structured node");
    assert!(
        schema_matches(&tool_output_schema("codegraph_node"), payload),
        "batch node payload failed advertised schema: {payload}"
    );
    assert_eq!(payload["kind"], "node");
    assert_eq!(payload["matchCount"], 2);
    let names: Vec<&str> = payload["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(names.contains(&"parseToken"), "{payload}");
    assert!(names.contains(&"AuthService"), "{payload}");
    let text = result.text();
    assert!(text.contains("## `parseToken`") && text.contains("## `AuthService`"), "{text}");
}

/// One search spans several indexed projects; every hit names its project.
#[tokio::test(flavor = "current_thread")]
async fn search_spans_several_projects_in_one_call() {
    let _env = env_read().await;
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    write(
        &first.path().join("src/a.ts"),
        "export function sharedHelper() { return 1; }\n",
    );
    write(
        &second.path().join("src/b.ts"),
        "export function sharedHelper() { return 2; }\nexport function onlyInSecond() {}\n",
    );
    for dir in [&first, &second] {
        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        cg.index_all(&IndexOptions::default()).await.unwrap();
        cg.close();
    }
    let handler = ToolHandler::new(Some(Rc::new(CodeGraph::open_sync(first.path()).unwrap())));

    let first_path = first.path().to_string_lossy().to_string();
    let second_path = second.path().to_string_lossy().to_string();
    let result = handler.execute(
        "codegraph_search",
        &json!({ "query": "sharedHelper", "projectPaths": [first_path, second_path] }),
    );
    assert_ne!(result.is_error, Some(true), "search errored: {}", result.text());
    let payload = result.structured_content.as_ref().expect("structured search");
    assert!(
        schema_matches(&tool_output_schema("codegraph_search"), payload),
        "multi-project payload failed advertised schema: {payload}"
    );
    let projects: Vec<&str> = payload["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|hit| hit["project"].as_str())
        .collect();
    assert!(projects.contains(&first_path.as_str()), "{payload}");
    assert!(projects.contains(&second_path.as_str()), "{payload}");
    assert!(payload.get("failedProjects").is_none(), "{payload}");
}

/// Search rows are cut to the output budget best-first, and flagged.
#[tokio::test(flavor = "current_thread")]
async fn search_rows_are_cut_to_the_output_budget() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "2500");
    let dir = TempDir::new().unwrap();
    let filler = "x".repeat(40);
    let source = (0..80)
        .map(|index| format!("export function budget_{index}_{filler}() {{ return {index}; }}"))
        .collect::<Vec<_>>()
        .join("\n");
    write(&dir.path().join("src/budget.ts"), &source);
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let projected = handler
        .execute("codegraph_search", &json!({ "query": "budget", "limit": 100 }))
        .into_mcp_projection()
        .unwrap();

    assert!(projected.text().len() <= 2500, "{}", projected.text().len());
    let payload = projected.structured_content.as_ref().unwrap();
    assert!(
        schema_matches(&tool_output_schema("codegraph_search"), payload),
        "{payload}"
    );
    assert_eq!(payload["truncated"], true, "{payload}");
    let rows = payload["results"].as_array().unwrap();
    assert!(!rows.is_empty() && rows.len() < 80, "{} rows", rows.len());
}
