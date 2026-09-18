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
    assert_eq!(
        payload["queries"],
        json!(["parseToken", "AuthService", "noSuchSymbolAnywhere"])
    );
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
    assert!(payload.get("queries").is_none(), "{payload}");
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
    assert_eq!(payload["query"], "parseToken, AuthService");
    let names: Vec<&str> = payload["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["node"]["name"].as_str())
        .collect();
    assert!(names.contains(&"parseToken"), "{payload}");
    assert!(names.contains(&"AuthService"), "{payload}");
    let text = result.text();
    assert!(text.contains("## `parseToken`") && text.contains("## `AuthService`"), "{text}");
}
