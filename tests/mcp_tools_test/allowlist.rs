/// The default surface in catalog order (`get_static_tools` keeps it).
const DEFAULT_TOOLS: [&str; 8] = [
    "codegraph_search",
    "codegraph_callers",
    "codegraph_callees",
    "codegraph_impact",
    "codegraph_node",
    "codegraph_explore",
    "codegraph_status",
    "codegraph_files",
];

fn sorted_default_tools() -> Vec<&'static str> {
    let mut names = DEFAULT_TOOLS.to_vec();
    names.sort_unstable();
    names
}

fn listed_names() -> Vec<String> {
    let mut names: Vec<String> = ToolHandler::new(None)
        .get_tools()
        .into_iter()
        .map(|t| t.name)
        .collect();
    names.sort();
    names
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_the_core_tools_when_unset() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::unset("CODEGRAPH_MCP_TOOLS");
    assert_eq!(listed_names(), sorted_default_tools());
}

#[tokio::test(flavor = "current_thread")]
async fn filters_list_tools_to_the_allowlisted_short_names() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "explore,search,node");
    assert_eq!(
        listed_names(),
        vec!["codegraph_explore", "codegraph_node", "codegraph_search"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn accepts_fully_qualified_names_and_ignores_whitespace() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", " codegraph_explore , search ");
    assert_eq!(
        listed_names(),
        vec!["codegraph_explore", "codegraph_search"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn treats_an_empty_whitespace_value_as_unset() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "   ");
    assert_eq!(listed_names(), sorted_default_tools());
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_a_disabled_tool_on_execute() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "node");
    let res = ToolHandler::new(None).execute("codegraph_explore", &json!({}));
    assert_eq!(res.is_error, Some(true));
    assert!(res.text().contains("disabled via CODEGRAPH_MCP_TOOLS"));
}

#[tokio::test(flavor = "current_thread")]
async fn lets_an_allowlisted_tool_past_the_guard() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "search");
    // No CodeGraph attached, so it fails *after* the allowlist guard — the
    // "disabled" message must NOT appear, proving the guard passed it through.
    let res = ToolHandler::new(None).execute("codegraph_search", &json!({ "query": "x" }));
    assert!(!res.text().contains("disabled via CODEGRAPH_MCP_TOOLS"));
}

#[tokio::test(flavor = "current_thread")]
async fn static_tools_honor_the_allowlist_too() {
    let _env = env_write().await;
    {
        let _guard = EnvVarGuard::unset("CODEGRAPH_MCP_TOOLS");
        assert_eq!(
            get_static_tools().into_iter().map(|tool| tool.name).collect::<Vec<_>>(),
            DEFAULT_TOOLS
        );
    }
    {
        let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "explore,files");
        let mut names: Vec<String> = get_static_tools().into_iter().map(|t| t.name).collect();
        names.sort();
        assert_eq!(names, vec!["codegraph_explore", "codegraph_files"]);
    }
}
