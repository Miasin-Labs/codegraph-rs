// =============================================================================
// codegraph_files path-filter normalization (#426)
// (__tests__/mcp-files-path-normalization.test.ts)
// =============================================================================

async fn files_fixture(root: &Path) -> CodeGraph {
    write(&root.join("src/index.ts"), "export const x = 1;\n");
    write(
        &root.join("src/components/Button.ts"),
        "export const Button = () => 1;\n",
    );
    write(&root.join("tests/a.test.ts"), "export const t = 1;\n");
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    cg
}

fn listed(handler: &ToolHandler, path_filter: Option<&str>) -> String {
    let mut args = serde_json::Map::new();
    if let Some(pf) = path_filter {
        args.insert("path".into(), json!(pf));
    }
    args.insert("format".into(), json!("flat"));
    args.insert("includeMetadata".into(), json!(false));
    let result = handler.execute("codegraph_files", &serde_json::Value::Object(args));
    assert_ne!(
        result.is_error,
        Some(true),
        "codegraph_files errored: {}",
        result.text()
    );
    result.text().to_string()
}

fn listed_with_pattern(handler: &ToolHandler, pattern: &str) -> String {
    let result = handler.execute(
        "codegraph_files",
        &json!({
            "format": "flat",
            "includeMetadata": false,
            "pattern": pattern,
        }),
    );
    assert_ne!(
        result.is_error,
        Some(true),
        "codegraph_files errored: {}",
        result.text()
    );
    result.text().to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn treats_rootish_path_filters_as_project_root() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    // Root-ish filters: every shape an agent might guess for "whole project"
    // must list the same files as no filter at all.
    for rootish in ["/", ".", "./", "", "\\", "//", ".//"] {
        let output = listed(&handler, Some(rootish));
        assert!(
            output.contains("src/index.ts"),
            "path={rootish:?}:\n{output}"
        );
        assert!(
            output.contains("src/components/Button.ts"),
            "path={rootish:?}"
        );
        assert!(output.contains("tests/a.test.ts"), "path={rootish:?}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn matches_a_real_subdirectory_prefix() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("src"));
    assert!(output.contains("src/index.ts"));
    assert!(output.contains("src/components/Button.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn tolerates_a_leading_slash_on_a_real_subdirectory() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("/src"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn tolerates_a_leading_dot_slash_on_a_real_subdirectory() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("./src"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn tolerates_a_trailing_slash_on_a_real_subdirectory() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("src/"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn normalizes_windows_backslashes() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("src\\components"));
    assert!(output.contains("src/components/Button.ts"));
    assert!(!output.contains("src/index.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_match_sibling_directories_that_share_a_prefix() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = Rc::new(files_fixture(dir.path()).await);
    let handler = ToolHandler::new(Some(Rc::clone(&cg)));

    // Old code matched on raw `startsWith`, so a filter "src" would also
    // return a sibling like "src-utils/...".
    write(
        &dir.path().join("src-utils/helper.ts"),
        "export const h = 1;\n",
    );
    cg.index_all(&IndexOptions::default()).await.unwrap();

    let output = listed(&handler, Some("src"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("src-utils/helper.ts"));
}

#[tokio::test(flavor = "current_thread")]
async fn supports_common_brace_extension_globs() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(&dir.path().join("src/index.ts"), "export const x = 1;\n");
    write(&dir.path().join("src/view.tsx"), "export const View = () => 1;\n");
    write(&dir.path().join("src/lib.rs"), "pub fn run() {}\n");
    write(&dir.path().join("README.md"), "# docs\n");
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let output = listed_with_pattern(&handler, "**/*.{ts,tsx,rs}");
    assert!(output.contains("src/index.ts"), "{output}");
    assert!(output.contains("src/view.tsx"), "{output}");
    assert!(output.contains("src/lib.rs"), "{output}");
    assert!(!output.contains("README.md"), "{output}");
}

#[tokio::test(flavor = "current_thread")]
async fn files_returns_structured_payload() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_files",
        &json!({ "format": "grouped", "includeMetadata": false }),
    );
    let structured = result.structured_content.as_ref().expect("structured files");
    assert_eq!(structured["kind"], "files");
    assert_eq!(structured["total"], 3);
    assert!(
        schema_matches(&tool_output_schema("codegraph_files"), structured),
        "{structured}"
    );
    let src = structured["dirs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|dir| dir["path"] == "src")
        .expect("src directory");
    assert!(src["files"].get("index.ts").is_some(), "{structured}");
}

async fn paging_fixture(root: &Path) -> ToolHandler {
    for index in 0..60 {
        write(
            &root.join(format!("pkg/mod_{}/file_number_{index}.ts", index % 3)),
            "export const x = 1;\n",
        );
    }
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    ToolHandler::new(Some(Rc::new(cg)))
}

/// Under a tight budget a listing pages: every page fits on the wire, and
/// following `nextCursor` lists every file exactly once.
#[tokio::test(flavor = "current_thread")]
async fn files_pages_a_listing_to_the_output_budget() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "1500");
    let dir = TempDir::new().unwrap();
    let handler = paging_fixture(dir.path()).await;

    let mut listed = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let mut args = json!({ "maxDepth": 3 });
        if let Some(cursor) = &cursor {
            args["cursor"] = json!(cursor);
        }
        let projected = handler
            .execute("codegraph_files", &args)
            .into_mcp_projection()
            .unwrap();
        assert!(projected.text().len() <= 1500, "{}", projected.text());
        let page = projected.structured_content.as_ref().unwrap();
        assert!(
            schema_matches(&tool_output_schema("codegraph_files"), page),
            "{page}"
        );
        assert_eq!(page["total"], 60);
        for dir in page["dirs"].as_array().unwrap() {
            for name in dir["files"]
                .as_object()
                .into_iter()
                .flat_map(|files| files.keys())
            {
                listed.push(format!("{}/{name}", dir["path"].as_str().unwrap()));
            }
        }
        cursor = page["nextCursor"].as_str().map(str::to_string);
        assert_eq!(page.get("truncated").is_some(), cursor.is_some(), "{page}");
        if cursor.is_none() {
            break;
        }
    }
    assert!(cursor.is_none(), "listing never finished");
    listed.sort();
    listed.dedup();
    assert_eq!(listed.len(), 60, "{listed:?}");
}

/// With no `maxDepth`, a listing too big for one reply is cut at the deepest
/// level that fits, and the collapsed directories still count every file.
#[tokio::test(flavor = "current_thread")]
async fn files_picks_a_depth_that_fits_when_none_is_given() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "1500");
    let dir = TempDir::new().unwrap();
    let handler = paging_fixture(dir.path()).await;

    let result = handler.execute("codegraph_files", &json!({}));
    let payload = result.structured_content.as_ref().unwrap();
    assert_eq!(payload["autoDepth"], true, "{payload}");
    assert_eq!(payload["maxDepth"], 2, "{payload}");
    assert!(payload.get("nextCursor").is_none(), "{payload}");
    assert_eq!(
        payload["dirs"],
        json!([{ "path": "pkg", "dirs": { "mod_0": 20, "mod_1": 20, "mod_2": 20 } }])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn files_refuses_a_cursor_from_another_listing() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "1500");
    let dir = TempDir::new().unwrap();
    let handler = paging_fixture(dir.path()).await;

    let first = handler.execute("codegraph_files", &json!({ "maxDepth": 3 }));
    let cursor = first.structured_content.as_ref().unwrap()["nextCursor"]
        .as_str()
        .expect("a second page")
        .to_string();
    let other = handler.execute(
        "codegraph_files",
        &json!({ "path": "pkg/mod_1", "cursor": cursor }),
    );
    assert_eq!(other.is_error, Some(true), "{}", other.text());
    assert!(other.text().contains("does not belong"), "{}", other.text());
}
