use crate::extraction_test::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn project_config_custom_extension_is_scanned_parsed_and_stored_as_mapped_language() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(
        temp.path().join("codegraph.json"),
        r#"{ "extensions": { ".widget": "typescript" } }"#,
    )
    .unwrap();
    fs::write(
        temp.path().join("src/component.widget"),
        "export function renderWidget() { return 1; }",
    )
    .unwrap();

    let (_connection, queries) = open_graph(temp.path());
    let orchestrator = ExtractionOrchestrator::new(temp.path(), &queries);
    let result = orchestrator.index_all(None, None, false).await.unwrap();

    assert!(result.success, "{:#?}", result.errors);
    let file = queries
        .get_file_by_path("src/component.widget")
        .unwrap()
        .expect("custom-extension file should be stored");
    assert_eq!(file.language, Language::Typescript);
    assert!(
        queries
            .get_nodes_by_file("src/component.widget")
            .unwrap()
            .iter()
            .any(|node| node.name == "renderWidget")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn project_config_include_and_exclude_override_git_scope_with_exclude_winning() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::create_dir_all(root.join("private")).unwrap();
    fs::create_dir_all(root.join("src/vendor")).unwrap();
    fs::write(root.join(".gitignore"), "private/\n").unwrap();
    fs::write(root.join("private/keep.ts"), "export const keep = 1;").unwrap();
    fs::write(root.join("private/drop.ts"), "export const drop = 1;").unwrap();
    fs::write(root.join("src/main.ts"), "export const main = 1;").unwrap();
    fs::write(root.join("src/vendor/sdk.ts"), "export const sdk = 1;").unwrap();
    fs::write(
        root.join("codegraph.json"),
        r#"{
            "include": ["private/keep.ts"],
            "exclude": ["src/vendor/**", "private/drop.ts"]
        }"#,
    )
    .unwrap();
    git(root, &["add", ".gitignore", "src", "codegraph.json"]);
    git(root, &["commit", "-q", "-m", "fixture"]);

    let files = scan_directory(root, None);
    assert!(files.contains(&"src/main.ts".to_string()), "{files:?}");
    assert!(files.contains(&"private/keep.ts".to_string()), "{files:?}");
    assert!(!files.contains(&"private/drop.ts".to_string()), "{files:?}");
    assert!(
        !files.contains(&"src/vendor/sdk.ts".to_string()),
        "{files:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn project_config_include_ignored_revives_only_opted_in_embedded_repositories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q"]);
    fs::write(root.join(".gitignore"), "children/\n").unwrap();
    fs::write(
        root.join("codegraph.json"),
        r#"{ "includeIgnored": ["children/"] }"#,
    )
    .unwrap();

    let child = root.join("children/service");
    fs::create_dir_all(&child).unwrap();
    git(&child, &["init", "-q"]);
    git(&child, &["config", "user.email", "test@test.com"]);
    git(&child, &["config", "user.name", "Test"]);
    fs::write(child.join("main.rs"), "pub fn embedded() {}").unwrap();
    git(&child, &["add", "main.rs"]);
    git(&child, &["commit", "-q", "-m", "fixture"]);

    let files = scan_directory(root, None);
    assert!(
        files.contains(&"children/service/main.rs".to_string()),
        "{files:?}"
    );
}
