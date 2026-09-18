/// Regression (explore on this repo, 2026-09): "how does codegraph_diagnostics
/// run cargo check and place errors on symbols" returned `graph/cancel.rs`
/// (a fn named `check`) and a fn named `run` as its top files, and none of the
/// diagnostics code. In a sentence, `run`/`check` are English, not symbol
/// names; and a compound name nothing is called (`tool_diagnostics`, a tool
/// name) should reach the code whose names share its distinctive word.
#[tokio::test(flavor = "current_thread")]
async fn prose_words_do_not_pin_seeds_and_compound_names_reach_their_code() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/cancel.ts"),
        "export function check(): boolean {\n  return true;\n}\n",
    );
    write(
        &dir.path().join("src/runner.ts"),
        "export function run(): number {\n  return 1;\n}\n",
    );
    write(
        &dir.path().join("src/diagnostics/cargo.ts"),
        "export function runCargoCheck(): string[] {\n  return [];\n}\n\nexport function diagnosticsForSymbols(errors: string[]): string[] {\n  return errors;\n}\n",
    );
    write(
        &dir.path().join("src/tools/diagnostics.ts"),
        "import { runCargoCheck, diagnosticsForSymbols } from \"../diagnostics/cargo\";\n\nexport function handleDiagnostics(): string[] {\n  return diagnosticsForSymbols(runCargoCheck());\n}\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_explore",
        &json!({ "query": "how does tool_diagnostics run cargo check and place errors on symbols" }),
    );
    assert_ne!(result.is_error, Some(true), "explore errored: {}", result.text());
    let structured = result.structured_content.as_ref().expect("structured explore");
    let ranked: Vec<&str> = structured["sourceFiles"]
        .as_array()
        .unwrap()
        .iter()
        .chain(structured["omissions"].as_array().unwrap())
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    let first = *ranked.first().unwrap_or_else(|| panic!("nothing ranked: {structured}"));
    assert!(
        first.contains("diagnostics"),
        "the diagnostics code should lead, not a prose-word match: {ranked:?}"
    );
    let position = |path: &str| ranked.iter().position(|ranked| *ranked == path);
    if let Some(cancel) = position("src/cancel.ts") {
        assert!(
            cancel > position(first).unwrap(),
            "`check` in a sentence must not outrank the named code: {ranked:?}"
        );
    }
}

/// A bag of names (no prose) still seeds on plain lowercase words: `check`
/// alone is a symbol lookup.
#[tokio::test(flavor = "current_thread")]
async fn plain_words_still_seed_a_bag_of_names_query() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/cancel.ts"),
        "export function check(): boolean {\n  return true;\n}\n",
    );
    write(
        &dir.path().join("src/other.ts"),
        "export function unrelatedHelper(): number {\n  return 2;\n}\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let structured = handler
        .execute("codegraph_explore", &json!({ "query": "check" }))
        .structured_content
        .expect("structured explore");
    assert_eq!(structured["sourceFiles"][0]["path"], "src/cancel.ts", "{structured}");
}
