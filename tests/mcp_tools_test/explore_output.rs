#[tokio::test(flavor = "current_thread")]
async fn keeps_total_output_under_the_small_project_cap() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let small_budget = get_explore_output_budget(100);
    assert!(
        text.len() < small_budget.max_output_chars + 500,
        "explore output too large: {} chars",
        text.len()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explore_returns_complete_output_without_destructive_cuts() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let src_dir = dir.path().join("src");
    for f in 0..12 {
        let mut lines: Vec<String> = vec![format!("export class Service{f} {{")];
        for i in 0..40 {
            lines.push(format!("  process{f}x{i}(arg: string): string {{"));
            lines.push(format!(
                "    return this.transform{f}x{i}(arg) + \"suffix-{f}-{i}\";"
            ));
            lines.push("  }".to_string());
            lines.push(format!("  transform{f}x{i}(arg: string): string {{"));
            lines.push(format!("    return arg.repeat({});", i + 1));
            lines.push("  }".to_string());
        }
        lines.push("}".to_string());
        write(&src_dir.join(format!("service{f}.ts")), &lines.join("\n"));
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(
        &handler,
        "Service0 Service1 Service2 Service3 Service4 Service5 process transform",
    );
    assert!(
        !text.contains("output truncated to budget"),
        "explore destructively truncated without an opt-in cap"
    );
    assert_eq!(text.matches("```").count() % 2, 0, "unbalanced code fences");
}

#[tokio::test(flavor = "current_thread")]
async fn explore_uses_qualified_symbol_labels() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method0 helper0");
    assert!(
        text.contains("Session::method0") || text.contains("Session::helper0"),
        "explore output should include fully qualified symbol labels:\n{text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn omits_the_meta_text_gated_off_for_small_projects() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    assert!(!text.contains("### Additional relevant files"));
    assert!(!text.contains("Complete source code is included above"));
    assert!(!text.contains("Explore budget:"));
}

#[tokio::test(flavor = "current_thread")]
async fn still_includes_the_relationships_section_or_source() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let has_relationships = text.contains("### Relationships");
    let source_follows_header = text.find("### Source Code").map(|i| i > 0).unwrap_or(false);
    assert!(has_relationships || source_follows_header);
}

#[tokio::test(flavor = "current_thread")]
async fn prefixes_source_lines_with_line_numbers_by_default() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::unset("CODEGRAPH_EXPLORE_LINENUMS");
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let re = regex::Regex::new(r"\n\d+\t").unwrap();
    assert!(text.contains("### Source Code"), "{text}");
    assert!(text.contains("```typescript\n"), "{text}");
    assert!(re.is_match(&text));
}

#[tokio::test(flavor = "current_thread")]
async fn omits_line_numbers_when_linenums_env_is_zero() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_EXPLORE_LINENUMS", "0");
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let re = regex::Regex::new(r"\n\d+\t(?:export|  )").unwrap();
    assert!(!re.is_match(&text));
}

#[tokio::test(flavor = "current_thread")]
async fn uses_language_neutral_omission_markers() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    assert!(!text.contains("// ... (gap)"));
    assert!(!text.contains("// ... trimmed"));
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_collapse_a_whole_file_class_into_just_its_header() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path()).await;
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let re = regex::Regex::new(r"method\d+\(arg: string\)").unwrap();
    assert!(re.is_match(&text));
}

#[tokio::test(flavor = "current_thread")]
async fn explore_surfaces_literal_content_matches_without_symbol_hits() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/state.ts"),
        "export const ready = true;\n// TODO: not implemented: persist project cache gaps\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_explore",
        &json!({ "query": "TODO not implemented" }),
    );
    let text = result.text();
    assert!(text.contains("### Literal content matches"), "{text}");
    assert!(text.contains("src/state.ts:2"), "{text}");
    assert!(text.contains("persist project cache gaps"), "{text}");
    let structured = result.structured_content.expect("structured literal explore");
    assert_eq!(structured["schemaVersion"], 2);
    assert_eq!(structured["sourceFiles"], json!([]));
    assert_eq!(structured["literalMatches"][0]["filePath"], "src/state.ts");
    assert_eq!(structured["literalMatches"][0]["lines"][0]["lineNumber"], 2);
    let terms = structured["literalMatches"][0]["lines"][0]["terms"]
        .as_array()
        .unwrap();
    assert!(terms.contains(&json!("todo")));
    assert!(terms.contains(&json!("not implemented")));
    assert!(structured.get("literalTotalFiles").is_none(), "{structured}");
    assert!(structured["literalMatches"][0].get("chunks").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn explore_surfaces_short_raw_literal_queries() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/state.ts"),
        "export const ready = true;\n// persist project cache gaps after restart\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let text = explore(&handler, "persist project cache gaps");
    assert!(text.contains("### Literal content matches"), "{text}");
    assert!(text.contains("src/state.ts:2"), "{text}");
}

#[tokio::test(flavor = "current_thread")]
async fn explore_literal_scan_does_not_expose_sensitive_config_values() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("application.yml"),
        "database:\n  password: production-secret-sentinel\n",
    );
    write(
        &dir.path().join("Cargo.toml"),
        "[registry]\ntoken = \"cargo-secret-sentinel\"\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    for (query, forbidden_line) in [
        (
            "production-secret-sentinel",
            "password: production-secret-sentinel",
        ),
        ("cargo-secret-sentinel", "token = \"cargo-secret-sentinel\""),
    ] {
        let result = handler.execute("codegraph_explore", &json!({ "query": query }));
        assert!(!result.text().contains(forbidden_line), "{}", result.text());
        let structured = result.structured_content.expect("structured explore");
        assert_eq!(structured["literalMatches"], json!([]));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explore_reports_literal_match_omissions() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    for index in 0..10 {
        write(
            &dir.path().join(format!("src/state{index}.ts")),
            "export const ready = true;\n// TODO: persist project cache gaps\n",
        );
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_explore",
        &json!({ "query": "TODO persist project cache gaps" }),
    );

    let text = result.text();
    assert!(text.contains("matching file(s) omitted"), "{text}");
    let structured = result.structured_content.expect("structured literal explore");
    assert_eq!(structured["literalMatches"].as_array().unwrap().len(), 8);
    assert!(structured.get("literalTotalFiles").is_none(), "{structured}");
}

#[tokio::test(flavor = "current_thread")]
async fn explore_preserves_literal_matches_when_graph_symbols_also_match() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/state.ts"),
        "export function target(): number {\n  return 1;\n}\n// TODO: persist project cache gaps\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute(
        "codegraph_explore",
        &json!({ "query": "target TODO persist project cache gaps" }),
    );

    assert_ne!(result.is_error, Some(true), "explore errored: {}", result.text());
    assert!(
        result.text().contains("### Literal content matches"),
        "mixed graph/literal query lost literal data: {}",
        result.text()
    );
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured literal explore");
    assert_eq!(structured["literalMatches"][0]["filePath"], "src/state.ts");

    let definition = get_static_tools()
        .into_iter()
        .find(|tool| tool.name == "codegraph_explore")
        .expect("explore definition");
    assert!(
        !definition.input_schema.properties.contains_key("literal"),
        "the approved v2 interface must not grow a literal request parameter"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explore_max_files_only_limits_rendering_not_discovery() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    for i in 0..8 {
        write(
            &dir.path().join(format!("src/widget{i}.ts")),
            &format!("export function widget{i}(): number {{\n  return {i};\n}}\n"),
        );
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let query = "widget0 widget1 widget2 widget3 widget4 widget5 widget6 widget7";

    let narrow = handler
        .execute("codegraph_explore", &json!({ "query": query, "maxFiles": 1 }))
        .structured_content
        .expect("narrow structured explore");
    let wide = handler
        .execute("codegraph_explore", &json!({ "query": query, "maxFiles": 8 }))
        .structured_content
        .expect("wide structured explore");

    assert_eq!(narrow["filesIncluded"], 1);
    assert_eq!(narrow["totalSymbols"], wide["totalSymbols"]);
    assert_eq!(narrow["totalFiles"], wide["totalFiles"]);
    assert_eq!(narrow["relationships"], wide["relationships"]);
}

#[tokio::test(flavor = "current_thread")]
async fn explore_max_files_does_not_limit_literal_discovery() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    for index in 0..250 {
        write(
            &dir.path().join(format!("src/a{index:03}.ts")),
            &format!("export const value{index} = {index};\n"),
        );
    }
    write(
        &dir.path().join("src/z999.ts"),
        "export const tail = true;\n// UNIQUE_LITERAL_SENTINEL\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    for max_files in [1, 2] {
        let result = handler.execute(
            "codegraph_explore",
            &json!({ "query": "UNIQUE_LITERAL_SENTINEL", "maxFiles": max_files }),
        );
        assert!(
            result.text().contains("### Literal content matches"),
            "maxFiles={max_files} changed literal discovery: {}",
            result.text()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explore_literal_payload_respects_output_cap_and_marks_trimming() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "600");
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/state.ts"),
        &format!("// TODO {}\n", "x".repeat(500_000)),
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let structured = handler
        .execute("codegraph_explore", &json!({ "query": "TODO" }))
        .structured_content
        .expect("structured literal explore");

    assert_eq!(structured["trimmed"], true, "{structured}");
    assert!(
        serde_json::to_string(&structured).unwrap().len() <= 600,
        "literal payload exceeded configured cap: {structured}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explore_structured_source_respects_output_cap() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "600");
    let dir = TempDir::new().unwrap();
    let repeated = "x".repeat(5000);
    write(
        &dir.path().join("src/state.ts"),
        &format!("export function target(): string {{ return \"{repeated}\"; }}\n"),
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_explore", &json!({ "query": "target" }));
    let structured = result.structured_content.as_ref().expect("structured explore");
    let file = &structured["sourceFiles"][0];
    assert_eq!(file["sourceTruncated"], true, "{file}");
    assert_eq!(structured["trimmed"], true, "{structured}");
    assert!(file["chunks"].as_array().unwrap().iter().all(|chunk| {
        !chunk["source"].as_str().unwrap().contains("[truncated]")
    }));
    assert!(
        serde_json::to_string(structured).unwrap().len() <= 600,
        "structured source exceeded configured cap: {structured}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_whole_file_skip_emits_linkscope_event() {
    let _env = env_read().await;
    linkscope::trace_enable();
    let dir = TempDir::new().unwrap();
    let mut lines = Vec::new();
    for i in 0..400 {
        lines.push(format!("export const value{i} = {i};"));
    }
    write(&dir.path().join("src/huge.ts"), &lines.join("\n"));
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_explore", &json!({ "query": "value399 huge" }));

    assert_ne!(result.is_error, Some(true), "explore errored: {}", result.text());
    assert!(linkscope::profile().records.iter().any(|record| matches!(
        record,
        linkscope::Record::Event { label, .. } if label == "codegraph.explore.whole_file_skipped"
    )));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn explore_graph_source_rejects_symlink_replacement_escape() {
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
        "OUTSIDE_EXPLORE_SECRET_SENTINEL\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    fs::remove_file(&indexed_path).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.ts"), &indexed_path).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_explore", &json!({ "query": "target" }));

    assert!(!result.text().contains("OUTSIDE_EXPLORE_SECRET_SENTINEL"));
    let structured = result.structured_content.expect("structured explore");
    assert!(structured["sourceFiles"].as_array().unwrap().is_empty());
    assert!(structured["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|omission| omission["path"] == "src/state.ts"
            && omission["reason"] == "unavailable"));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn explore_literal_scan_rejects_symlink_replacement_escape() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let indexed_path = dir.path().join("src/state.ts");
    write(&indexed_path, "export const ready = true;\n");
    write(
        &outside.path().join("secret.ts"),
        "export const secret = 'outside-only-token leak marker';\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    std::fs::remove_file(&indexed_path).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.ts"), &indexed_path).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let text = explore(&handler, "secret leak marker");
    assert!(!text.contains("outside-only-token"), "{text}");
    assert!(!text.contains("### Literal content matches"), "{text}");
}

include!("explore_output/v2.rs");
include!("explore_output/omissions.rs");
include!("explore_output/unicode.rs");
include!("explore_output/schema.rs");
