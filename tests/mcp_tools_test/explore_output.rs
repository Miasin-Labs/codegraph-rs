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
