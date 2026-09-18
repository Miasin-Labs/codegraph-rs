/// A one-file crate with a type error inside `broken`.
async fn broken_crate(root: &Path) -> ToolHandler {
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"cg_diagnostics_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write(
        &root.join("src/lib.rs"),
        "pub fn fine() -> u32 {\n    1\n}\n\npub fn broken() -> u32 {\n    let text: u32 = \"not a number\";\n    text\n}\n",
    );
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    ToolHandler::new(Some(Rc::new(cg)))
}

/// `codegraph_diagnostics` never blocks past `wait`: a zero wait reports the
/// check as running, and the next call collects the same run's result, placed
/// on the symbol the error sits in.
#[tokio::test(flavor = "current_thread")]
async fn diagnostics_places_compiler_errors_on_symbols_across_calls() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let handler = broken_crate(dir.path()).await;

    let first = handler.execute("codegraph_diagnostics", &json!({ "wait": 0 }));
    assert_ne!(first.is_error, Some(true), "{}", first.text());
    assert!(first.text().contains("still running"), "{}", first.text());

    let done = handler.execute("codegraph_diagnostics", &json!({ "wait": 55 }));
    let text = done.text();
    assert_ne!(done.is_error, Some(true), "{text}");
    assert!(text.contains("1 error"), "{text}");
    assert!(text.contains("**src/lib.rs**"), "{text}");
    assert!(text.contains("error[E0308] L6:"), "{text}");
    assert!(text.contains("in `broken`"), "{text}");

    // A result seconds old is reused, and filters apply to it.
    let filtered = handler.execute(
        "codegraph_diagnostics",
        &json!({ "wait": 0, "file": "src/other.rs" }),
    );
    assert!(filtered.text().contains("None match"), "{}", filtered.text());
}

/// A project with neither Cargo.toml nor its own tsc gets a clear error, not
/// a guess at some global toolchain.
#[tokio::test(flavor = "current_thread")]
async fn diagnostics_refuses_projects_without_a_supported_checker() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let handler = insight_fixture(dir.path()).await;
    let result = handler.execute("codegraph_diagnostics", &json!({}));
    assert_eq!(result.is_error, Some(true), "{}", result.text());
    assert!(result.text().contains("no supported checker"), "{}", result.text());

    let clippy = handler.execute("codegraph_diagnostics", &json!({ "checker": "clippy" }));
    assert!(clippy.text().contains("no Cargo.toml"), "{}", clippy.text());
}

/// A checker that fails before reporting any diagnostic (here: a dependency
/// that `--offline` cannot resolve) surfaces its stderr instead of claiming
/// "0 errors".
#[tokio::test(flavor = "current_thread")]
async fn diagnostics_shows_a_checker_failure_instead_of_a_clean_result() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"cg_diagnostics_offline\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\ncg-no-such-crate-anywhere = \"1\"\n\n[workspace]\n",
    );
    write(&root.join("src/lib.rs"), "pub fn fine() {}\n");
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_diagnostics", &json!({ "wait": 55 }));
    let text = result.text();
    assert!(text.contains("failed without reporting a diagnostic"), "{text}");
    assert!(text.contains("cg-no-such-crate-anywhere"), "{text}");
}
