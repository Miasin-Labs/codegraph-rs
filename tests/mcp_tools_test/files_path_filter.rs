// =============================================================================
// codegraph_files path-filter normalization (#426)
// (__tests__/mcp-files-path-normalization.test.ts)
// =============================================================================

fn files_fixture(root: &Path) -> CodeGraph {
    write(&root.join("src/index.ts"), "export const x = 1;\n");
    write(
        &root.join("src/components/Button.ts"),
        "export const Button = () => 1;\n",
    );
    write(&root.join("tests/a.test.ts"), "export const t = 1;\n");
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
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

#[test]
fn treats_rootish_path_filters_as_project_root() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
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

#[test]
fn matches_a_real_subdirectory_prefix() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("src"));
    assert!(output.contains("src/index.ts"));
    assert!(output.contains("src/components/Button.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[test]
fn tolerates_a_leading_slash_on_a_real_subdirectory() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("/src"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[test]
fn tolerates_a_leading_dot_slash_on_a_real_subdirectory() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("./src"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[test]
fn tolerates_a_trailing_slash_on_a_real_subdirectory() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("src/"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[test]
fn normalizes_windows_backslashes() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("src\\components"));
    assert!(output.contains("src/components/Button.ts"));
    assert!(!output.contains("src/index.ts"));
}

#[test]
fn does_not_match_sibling_directories_that_share_a_prefix() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = Rc::new(files_fixture(dir.path()));
    let handler = ToolHandler::new(Some(Rc::clone(&cg)));

    // Old code matched on raw `startsWith`, so a filter "src" would also
    // return a sibling like "src-utils/...".
    write(
        &dir.path().join("src-utils/helper.ts"),
        "export const h = 1;\n",
    );
    cg.index_all(&IndexOptions::default()).unwrap();

    let output = listed(&handler, Some("src"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("src-utils/helper.ts"));
}
