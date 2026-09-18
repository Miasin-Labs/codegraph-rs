/// A seed file whose *other* symbols have callers the query never names:
/// `runEngine` is asked about; `printer.ts` calls `formatReport` in the same
/// file, and `lonely.ts` touches nothing.
async fn related_fixture(root: &Path) -> CodeGraph {
    write(
        &root.join("src/core/engine.ts"),
        "export function runEngine(input: number): number {\n  return input + 1;\n}\n\nexport function formatReport(total: number): string {\n  return `total=${total}`;\n}\n",
    );
    write(
        &root.join("src/report/printer.ts"),
        "import { formatReport } from \"../core/engine\";\n\nexport function printReport(total: number): string {\n  const line = formatReport(total);\n  return line.trim();\n}\n",
    );
    write(
        &root.join("src/lonely.ts"),
        "export function lonely(): number {\n  return 3;\n}\n",
    );
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    cg
}

fn related_paths(structured: &serde_json::Value) -> Vec<String> {
    structured["relatedFiles"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| row["path"].as_str().unwrap().to_string())
        .collect()
}

fn sha256_fingerprint(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}:{}", bytes.len(), &hex[..16])
}

#[tokio::test(flavor = "current_thread")]
async fn explore_folds_in_one_hop_neighbours_of_its_files_as_related_rows() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = related_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_explore", &json!({ "query": "runEngine" }));
    assert_ne!(result.is_error, Some(true), "explore errored: {}", result.text());
    let structured = result.structured_content.as_ref().expect("structured explore");
    assert!(
        schema_matches(&explore_output_schema(), structured),
        "payload with relatedFiles failed the advertised schema: {structured}"
    );

    let rows = structured["relatedFiles"].as_array().expect("relatedFiles rows");
    let printer = rows
        .iter()
        .find(|row| row["path"] == "src/report/printer.ts")
        .unwrap_or_else(|| panic!("printer.ts is one hop from engine.ts: {structured}"));
    assert_eq!(printer["reason"], "calls formatReport");
    assert_eq!(printer["symbol"], "printReport");
    assert_eq!(printer["line"], 3);
    assert!(
        !related_paths(structured).contains(&"src/lonely.ts".to_string()),
        "an unlinked file is not related: {structured}"
    );
    let ranked: Vec<&str> = structured["sourceFiles"]
        .as_array()
        .unwrap()
        .iter()
        .chain(structured["omissions"].as_array().unwrap())
        .filter(|file| file["path"] != "src/report/printer.ts")
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    for path in related_paths(structured) {
        assert!(!ranked.contains(&path.as_str()), "{path} is both ranked and related");
    }

    // The best related file carries a short window of its key symbol, as an
    // ordinary source file the session ledger records.
    let window = structured["sourceFiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "src/report/printer.ts")
        .unwrap_or_else(|| panic!("printer.ts window missing: {structured}"));
    let chunk = &window["chunks"][0];
    assert_eq!(chunk["mode"], "excerpt");
    assert_eq!(chunk["startLine"], 3);
    assert!(chunk["source"].as_str().unwrap().starts_with("export function printReport"));

    let text = result.text();
    assert!(text.contains("### Related files"), "markdown lacks the related section:\n{text}");
    assert!(text.contains("src/report/printer.ts — calls formatReport"), "{text}");
}

#[tokio::test(flavor = "current_thread")]
async fn related_windows_honour_the_session_ledger() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = related_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let printer = fs::read(dir.path().join("src/report/printer.ts")).unwrap();
    let prior = json!({
        "projectRoot": dir.path().to_string_lossy(),
        "callCount": 1,
        "responseBytes": 0,
        "calls": [{
            "index": 1,
            "responseBytes": 0,
            "files": [{
                "path": "src/report/printer.ts",
                "ranges": [{ "start": 1, "end": 6 }],
                "bytes": printer.len(),
                "fingerprint": sha256_fingerprint(&printer),
            }],
        }],
    });

    let result = handler.execute(
        "codegraph_explore",
        &json!({ "query": "runEngine", "_cgExploreSession": prior }),
    );
    let structured = result.structured_content.as_ref().expect("structured explore");
    assert!(
        related_paths(structured).contains(&"src/report/printer.ts".to_string()),
        "the row stays — it is metadata, not source: {structured}"
    );
    assert!(
        !structured["sourceFiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "src/report/printer.ts"),
        "a window already sent this session is not repeated: {structured}"
    );
}
