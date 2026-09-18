fn node_output_schema() -> serde_json::Value {
    tools()
        .into_iter()
        .find(|tool| tool.name == "codegraph_node")
        .expect("codegraph_node tool")
        .output_schema
        .expect("node advertises an output schema")
}

async fn node_handler(files: &[(&str, &str)]) -> (TempDir, ToolHandler) {
    let dir = TempDir::new().unwrap();
    for (path, content) in files {
        write(&dir.path().join(path), content);
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    (dir, handler)
}

fn numbered_consts(count: usize) -> String {
    (0..count)
        .map(|index| format!("export const value{index} = {index};"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_payload_validates_against_advertised_schema() {
    let _env = env_read().await;
    let schema = node_output_schema();
    let (_dir, handler) = node_handler(&[("src/large.ts", &numbered_consts(320))]).await;

    let result = handler.execute("codegraph_node", &json!({ "file": "src/large.ts" }));

    assert_ne!(result.is_error, Some(true), "node errored: {}", result.text());
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured node file view");
    assert!(
        schema_matches(&schema, structured),
        "file-mode payload failed advertised schema: {structured}"
    );
    assert_eq!(structured["schemaVersion"], 2);
    assert_eq!(structured["kind"], "file");
    assert_eq!(structured["path"], "src/large.ts");
    assert_eq!(structured["totalLines"], 320);
    assert_eq!(structured["startLine"], 1);
    assert_eq!(structured["endLine"], 240);
    let source = structured["source"].as_str().expect("source");
    assert_eq!(source.lines().count(), 240);
    assert!(source.ends_with("export const value239 = 239;"));
    // The default limit ended the page, not the budget.
    assert!(structured.get("truncated").is_none(), "{structured}");
    // Source views carry no outline (the source is right there) and no
    // fields that only echo the request.
    for absent in ["symbols", "offset", "limit", "language", "sourceChunks"] {
        assert!(structured.get(absent).is_none(), "{absent}: {structured}");
    }
    assert!(
        result.text().len() < 16_000,
        "default file-mode output should stay compact, got {} chars",
        result.text().len()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_errors_validate_against_advertised_schema() {
    let _env = env_read().await;
    let schema = node_output_schema();
    let (_dir, handler) = node_handler(&[(
        "src/state.ts",
        "export function target(): number { return 1; }\n",
    )])
    .await;

    let projected = handler
        .execute("codegraph_node", &json!({ "file": "src/missing.ts" }))
        .into_mcp_projection()
        .expect("node result projects");
    let structured = projected
        .structured_content
        .expect("structured node error");
    assert!(
        schema_matches(&schema, &structured),
        "file-mode error failed advertised schema: {structured}"
    );
    assert_eq!(structured["kind"], "error");
    assert_eq!(projected.is_error, Some(true));
}

/// A file that shrank since the agent last saw it: an offset past the end
/// returns the file's last lines and says so, instead of failing the call
/// (98 such errors in the replayed sessions).
#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_clamps_an_offset_past_the_end() {
    let _env = env_read().await;
    let schema = node_output_schema();
    let (_dir, handler) = node_handler(&[("src/short.ts", &numbered_consts(10))]).await;

    let result = handler.execute(
        "codegraph_node",
        &json!({ "file": "src/short.ts", "offset": 50, "limit": 4 }),
    );

    assert_ne!(result.is_error, Some(true), "clamp errored: {}", result.text());
    let structured = result.structured_content.as_ref().expect("structured");
    assert!(schema_matches(&schema, structured), "{structured}");
    assert_eq!(structured["requestedOffset"], 50);
    assert_eq!(structured["totalLines"], 10);
    assert_eq!(structured["startLine"], 7);
    assert_eq!(structured["endLine"], 10);
    assert_eq!(
        structured["source"],
        "export const value6 = 6;\nexport const value7 = 7;\nexport const value8 = 8;\nexport const value9 = 9;"
    );
    assert!(result.text().contains("past the end"), "{}", result.text());

    // Without a limit the whole (short) file comes back from line 1.
    let whole = handler.execute(
        "codegraph_node",
        &json!({ "file": "src/short.ts", "offset": 11 }),
    );
    let structured = whole.structured_content.as_ref().expect("structured");
    assert_eq!(structured["requestedOffset"], 11);
    assert_eq!(structured["startLine"], 1);
    assert_eq!(structured["endLine"], 10);
}

/// A window starting mid-definition names the definitions it is inside.
#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_names_the_enclosing_definition() {
    let _env = env_read().await;
    let body = (0..20)
        .map(|index| format!("  const step{index} = {index};"))
        .collect::<Vec<_>>()
        .join("\n");
    let (_dir, handler) = node_handler(&[(
        "src/long.ts",
        &format!("export function outer(): void {{\n{body}\n}}\n"),
    )])
    .await;

    let result = handler.execute(
        "codegraph_node",
        &json!({ "file": "src/long.ts", "offset": 10, "limit": 3 }),
    );

    let structured = result.structured_content.as_ref().expect("structured");
    let enclosing = structured["enclosing"].as_array().expect("enclosing");
    assert!(
        enclosing.iter().any(|row| row["name"] == "outer" && row["line"] == 1),
        "{structured}"
    );
    // Only the first page names the file's dependents.
    assert!(structured.get("dependents").is_none(), "{structured}");
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_rejects_symlink_replacement_outside_project() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let indexed_path = dir.path().join("src/state.ts");
    write(&indexed_path, "export const ready = true;\n");
    write(
        &outside.path().join("secret.ts"),
        "OUTSIDE_NODE_SECRET_SENTINEL\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    fs::remove_file(&indexed_path).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.ts"), &indexed_path).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_node", &json!({ "file": "src/state.ts" }));

    assert_eq!(result.is_error, Some(true), "{}", result.text());
    assert!(!result.text().contains("OUTSIDE_NODE_SECRET_SENTINEL"));
    assert_eq!(
        result.structured_content.as_ref().unwrap()["kind"],
        "error"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn node_symbol_mode_rejects_symlink_replacement_outside_project() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let indexed_path = dir.path().join("src/state.ts");
    write(
        &indexed_path,
        "export function target(): number { return 1; }\n",
    );
    write(
        &outside.path().join("secret.ts"),
        "OUTSIDE_SYMBOL_SECRET_SENTINEL\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    fs::remove_file(&indexed_path).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.ts"), &indexed_path).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_node",
        &json!({ "symbol": "target", "includeCode": true }),
    );

    assert!(!result.text().contains("OUTSIDE_SYMBOL_SECRET_SENTINEL"));
    let structured = result.structured_content.expect("structured node result");
    assert!(structured["matches"][0].get("code").is_none(), "{structured}");
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_preserves_source_below_configured_cap() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "100000");
    let source = format!("export const payload = \"{}\";", "x".repeat(3_000));
    let (_dir, handler) = node_handler(&[("src/state.ts", &source)]).await;

    let structured = handler
        .execute("codegraph_node", &json!({ "file": "src/state.ts" }))
        .structured_content
        .expect("structured node file view");

    assert_eq!(structured["source"], source);
    assert!(structured.get("truncated").is_none(), "{structured}");
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_marks_source_truncation_under_a_tight_cap() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "600");
    let source = format!("export const payload = \"{}\";", "x".repeat(5_000));
    let (_dir, handler) = node_handler(&[("src/state.ts", &source)]).await;

    let result = handler.execute("codegraph_node", &json!({ "file": "src/state.ts" }));
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured node file view");

    // One line longer than the budget: withheld whole, never cut mid-line.
    assert_eq!(structured["truncated"], true, "{structured}");
    assert!(structured.get("source").is_none(), "{structured}");
    assert!(
        serde_json::to_string(&structured).unwrap().len() <= 600,
        "capped payload exceeded configured limit: {structured}"
    );
    assert!(result.text().contains("longer than"), "{}", result.text());
}

/// Pages end where the output budget runs out, on a line boundary, and say
/// so; the next page picks up at `endLine + 1`.
#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_pages_to_the_output_budget() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "2000");
    let (_dir, handler) = node_handler(&[("src/large.ts", &numbered_consts(300))]).await;

    let first = handler
        .execute("codegraph_node", &json!({ "file": "src/large.ts" }))
        .into_mcp_projection()
        .unwrap();
    let page = first.structured_content.as_ref().unwrap();
    assert!(first.text().len() <= 2000, "{} chars", first.text().len());
    assert_eq!(page["truncated"], true, "{page}");
    let end = page["endLine"].as_u64().unwrap();
    assert!(end > 1 && end < 240, "{page}");
    assert_eq!(
        page["source"].as_str().unwrap().lines().count() as u64,
        end,
        "{page}"
    );

    let next = handler.execute(
        "codegraph_node",
        &json!({ "file": "src/large.ts", "offset": end + 1 }),
    );
    let page = next.structured_content.as_ref().unwrap();
    assert_eq!(page["startLine"], end + 1);
    assert!(
        page["source"]
            .as_str()
            .unwrap()
            .starts_with(&format!("export const value{end} = {end};"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_withholds_toml_values() {
    let _env = env_read().await;
    let (_dir, handler) = node_handler(&[(
        "Cargo.toml",
        "[registry]\ntoken = \"cargo-secret-sentinel\"\n",
    )])
    .await;

    let result = handler.execute("codegraph_node", &json!({ "file": "Cargo.toml" }));
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured node file view");

    assert_eq!(structured["valuesWithheld"], true);
    assert!(structured.get("source").is_none(), "{structured}");
    assert!(!result.text().contains("cargo-secret-sentinel"));
    assert!(!structured.to_string().contains("cargo-secret-sentinel"));
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_symbols_only_payload_validates_against_advertised_schema() {
    let _env = env_read().await;
    let schema = node_output_schema();
    let (_dir, handler) = node_handler(&[(
        "src/state.ts",
        "export function target(): number {\n  return 1;\n}\n",
    )])
    .await;

    let result = handler.execute(
        "codegraph_node",
        &json!({ "file": "src/state.ts", "symbolsOnly": true }),
    );

    assert_ne!(result.is_error, Some(true), "node errored: {}", result.text());
    let structured = result
        .structured_content
        .expect("structured node symbols-only view");
    assert!(
        schema_matches(&schema, &structured),
        "symbols-only payload failed advertised schema: {structured}"
    );
    assert_eq!(structured["kind"], "file");
    assert_eq!(structured["path"], "src/state.ts");
    assert!(structured.get("source").is_none(), "{structured}");
    assert!(structured.get("valuesWithheld").is_none(), "{structured}");
    let symbols = structured["symbols"].as_array().expect("outline");
    assert_eq!(symbols[0]["name"], "target");
    assert_eq!(symbols[0]["line"], 1);
    assert_eq!(symbols[0]["endLine"], 3);
    assert!(symbols[0].get("file").is_none(), "file is implied: {structured}");
}

/// A second read of a range the session already holds must come back as a
/// back-reference, not as the same source again.
#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_skips_source_already_sent_this_session() {
    let _env = env_read().await;
    let schema = node_output_schema();
    let body = numbered_consts(60);
    let (dir, handler) = node_handler(&[("src/small.ts", &body)]).await;

    let first = handler.execute("codegraph_node", &json!({ "file": "src/small.ts" }));
    let first_payload = first.structured_content.as_ref().expect("first payload");
    let start = first_payload["startLine"].as_u64().unwrap();
    let end = first_payload["endLine"].as_u64().unwrap();
    assert!(end >= start);

    // The service injects this ledger on every call; build the same shape here.
    let bytes = std::fs::read(dir.path().join("src/small.ts")).unwrap();
    let fingerprint = format!(
        "{}:{}",
        bytes.len(),
        &codegraph::utils::sha256_hex(&bytes)[..16]
    );
    let session = json!({
        "projectRoot": dir.path().to_string_lossy(),
        "callCount": 1,
        "responseBytes": 1024,
        "calls": [{
            "index": 1,
            "responseBytes": 1024,
            "files": [{
                "path": "src/small.ts",
                "ranges": [{ "start": start, "end": end }],
                "bytes": body.len(),
                "fingerprint": fingerprint,
            }],
        }],
    });

    let second = handler.execute(
        "codegraph_node",
        &json!({ "file": "src/small.ts", "_cgExploreSession": session }),
    );
    assert_ne!(second.is_error, Some(true), "node errored: {}", second.text());
    let payload = second.structured_content.as_ref().expect("second payload");
    assert!(
        schema_matches(&schema, payload),
        "back-reference payload failed advertised schema: {payload}"
    );
    assert!(payload.get("source").is_none(), "{payload}");
    assert_eq!(payload["alreadySent"], true);
    assert_eq!(payload["startLine"], start);
    assert_eq!(payload["endLine"], end);
    assert!(
        second.text().contains("already sent"),
        "no back-reference notice: {}",
        second.text()
    );
    assert!(payload["symbolCount"].as_u64().unwrap() > 0);

    // Symbol reads honour the same ledger: `value7` sits inside the range.
    let symbol = handler.execute(
        "codegraph_node",
        &json!({ "symbol": "value7", "includeCode": true, "_cgExploreSession": session }),
    );
    let detail = &symbol.structured_content.as_ref().unwrap()["matches"][0];
    assert!(
        schema_matches(&schema, symbol.structured_content.as_ref().unwrap()),
        "{detail}"
    );
    assert_eq!(detail["alreadySent"], true, "{detail}");
    assert!(detail.get("code").is_none(), "{detail}");
    assert!(symbol.text().contains("already sent"), "{}", symbol.text());
}

/// Symbol rows name what a follow-up call takes and nothing derivable.
#[tokio::test(flavor = "current_thread")]
async fn node_symbol_rows_are_compact() {
    let _env = env_read().await;
    let schema = node_output_schema();
    let (_dir, handler) = node_handler(&[(
        "src/lib.ts",
        "export class Greeter {\n  greet(name: string): string {\n    return helper(name);\n  }\n}\nexport function helper(name: string): string {\n  return name;\n}\n",
    )])
    .await;

    let result = handler.execute(
        "codegraph_node",
        &json!({ "symbol": "greet", "includeCode": true }),
    );

    let payload = result.structured_content.as_ref().expect("structured");
    assert!(schema_matches(&schema, payload), "{payload}");
    assert_eq!(payload["schemaVersion"], 2);
    let detail = &payload["matches"][0];
    assert_eq!(detail["name"], "greet");
    assert_eq!(detail["container"], "Greeter");
    assert_eq!(detail["file"], "src/lib.ts");
    assert_eq!(detail["line"], 2);
    for absent in ["id", "qualifiedName", "language", "node", "filePath"] {
        assert!(detail.get(absent).is_none(), "{absent}: {detail}");
    }
    let callee = &detail["callees"][0];
    assert_eq!(callee["name"], "helper");
    assert_eq!(callee["line"], 6);
    assert_eq!(callee.as_object().unwrap().len(), 4, "{callee}");
}

/// With no `CODEGRAPH_MAX_OUTPUT_CHARS` set, a huge definition is still
/// bounded: its code is cut at a line boundary and flagged.
#[tokio::test(flavor = "current_thread")]
async fn node_symbol_code_is_bounded_by_the_default_budget() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::unset("CODEGRAPH_MAX_OUTPUT_CHARS");
    let body = (0..3_000)
        .map(|index| format!("  const line{index} = \"{}\";", "y".repeat(24)))
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!("export function huge(): void {{\n{body}\n}}\n");
    let (_dir, handler) = node_handler(&[("src/huge.ts", &source)]).await;

    let projected = handler
        .execute(
            "codegraph_node",
            &json!({ "symbol": "huge", "includeCode": true }),
        )
        .into_mcp_projection()
        .unwrap();

    assert!(
        projected.text().len() <= 24_000,
        "{} chars on the wire",
        projected.text().len()
    );
    let payload = projected.structured_content.as_ref().unwrap();
    assert!(schema_matches(&node_output_schema(), payload), "{payload}");
    assert_eq!(payload["truncated"], true);
    let detail = &payload["matches"][0];
    assert_eq!(detail["codeTruncated"], true);
    assert_eq!(detail["codeStartLine"], 1);
    let code = detail["code"].as_str().unwrap();
    let end = detail["codeEndLine"].as_u64().unwrap() as usize;
    assert_eq!(code.lines().count(), end);
    let expected: Vec<&str> = source.lines().take(end).collect();
    assert_eq!(code, expected.join("\n"), "code must be whole, verbatim lines");
}
