#[test]
fn keeps_total_output_under_the_small_project_cap() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let small_budget = get_explore_output_budget(100);
    assert!(
        text.len() < small_budget.max_output_chars + 500,
        "explore output too large: {} chars",
        text.len()
    );
}

#[test]
fn explore_returns_complete_output_without_destructive_cuts() {
    let _env = env_read();
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
    cg.index_all(&IndexOptions::default()).unwrap();
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

#[test]
fn explore_uses_qualified_symbol_labels() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method0 helper0");
    assert!(
        text.contains("Session::method0") || text.contains("Session::helper0"),
        "explore output should include fully qualified symbol labels:\n{text}"
    );
}

#[test]
fn omits_the_meta_text_gated_off_for_small_projects() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    assert!(!text.contains("### Additional relevant files"));
    assert!(!text.contains("Complete source code is included above"));
    assert!(!text.contains("Explore budget:"));
}

#[test]
fn still_includes_the_relationships_section_or_source() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let has_relationships = text.contains("### Relationships");
    let source_follows_header = text.find("### Source Code").map(|i| i > 0).unwrap_or(false);
    assert!(has_relationships || source_follows_header);
}

#[test]
fn prefixes_source_lines_with_line_numbers_by_default() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_EXPLORE_LINENUMS");
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let re = regex::Regex::new(r"\n\d+\t").unwrap();
    assert!(re.is_match(&text));
}

#[test]
fn omits_line_numbers_when_linenums_env_is_zero() {
    let _env = env_write();
    let _guard = EnvVarGuard::set("CODEGRAPH_EXPLORE_LINENUMS", "0");
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let re = regex::Regex::new(r"\n\d+\t(?:export|  )").unwrap();
    assert!(!re.is_match(&text));
}

#[test]
fn uses_language_neutral_omission_markers() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    assert!(!text.contains("// ... (gap)"));
    assert!(!text.contains("// ... trimmed"));
}

#[test]
fn does_not_collapse_a_whole_file_class_into_just_its_header() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let re = regex::Regex::new(r"method\d+\(arg: string\)").unwrap();
    assert!(re.is_match(&text));
}

#[test]
fn explore_surfaces_literal_content_matches_without_symbol_hits() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/state.ts"),
        "export const ready = true;\n// TODO: not implemented: persist project cache gaps\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let text = explore(&handler, "TODO not implemented");
    assert!(text.contains("### Literal content matches"), "{text}");
    assert!(text.contains("src/state.ts:2"), "{text}");
    assert!(text.contains("persist project cache gaps"), "{text}");
}

#[test]
fn explore_surfaces_short_raw_literal_queries() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/state.ts"),
        "export const ready = true;\n// persist project cache gaps after restart\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let text = explore(&handler, "persist project cache gaps");
    assert!(text.contains("### Literal content matches"), "{text}");
    assert!(text.contains("src/state.ts:2"), "{text}");
}

#[test]
fn explore_returns_structured_payload() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/state.ts"),
        "export function target(): number { return 1; }\n// persist project cache gaps after restart\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_explore", &json!({ "query": "target persist" }));
    assert_ne!(result.is_error, Some(true), "explore errored: {}", result.text());
    let structured = result.structured_content.as_ref().expect("structured explore");
    assert_eq!(structured["kind"], "explore");
    assert_eq!(structured["query"], "target persist");
    assert!(structured["totalSymbols"].as_u64().unwrap() > 0);
    assert!(structured["sourceFiles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["path"] == "src/state.ts"));
    assert!(structured["sourceFiles"][0]["body"].as_str().unwrap().contains("target"));
}

#[test]
fn explore_structured_source_respects_output_cap() {
    let _env = env_write();
    let _guard = EnvVarGuard::set("CODEGRAPH_MAX_OUTPUT_CHARS", "600");
    let dir = TempDir::new().unwrap();
    let repeated = "x".repeat(5000);
    write(
        &dir.path().join("src/state.ts"),
        &format!("export function target(): string {{ return \"{repeated}\"; }}\n"),
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_explore", &json!({ "query": "target" }));
    let structured = result.structured_content.as_ref().expect("structured explore");
    let body = structured["sourceFiles"][0]["body"].as_str().unwrap();
    assert!(body.contains("[truncated]"), "{body}");
    assert!(body.len() < repeated.len(), "structured body was not capped");
}

#[test]
fn oversized_whole_file_skip_emits_linkscope_event() {
    let _env = env_read();
    linkscope::trace_enable();
    let dir = TempDir::new().unwrap();
    let mut lines = Vec::new();
    for i in 0..400 {
        lines.push(format!("export const value{i} = {i};"));
    }
    write(&dir.path().join("src/huge.ts"), &lines.join("\n"));
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_explore", &json!({ "query": "value399 huge" }));

    assert_ne!(result.is_error, Some(true), "explore errored: {}", result.text());
    assert!(linkscope::profile().records.iter().any(|record| matches!(
        record,
        linkscope::Record::Event { label, .. } if label == "codegraph.explore.whole_file_skipped"
    )));
}

#[cfg(unix)]
#[test]
fn explore_literal_scan_rejects_symlink_replacement_escape() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let indexed_path = dir.path().join("src/state.ts");
    write(&indexed_path, "export const ready = true;\n");
    write(
        &outside.path().join("secret.ts"),
        "export const secret = 'outside-only-token leak marker';\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    std::fs::remove_file(&indexed_path).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.ts"), &indexed_path).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let text = explore(&handler, "secret leak marker");
    assert!(!text.contains("outside-only-token"), "{text}");
    assert!(!text.contains("### Literal content matches"), "{text}");
}
