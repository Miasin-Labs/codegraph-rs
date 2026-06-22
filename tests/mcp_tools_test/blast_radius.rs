fn blast_fixture(root: &Path) -> CodeGraph {
    let src = root.join("src");
    // `target` is depended on by a sibling (caller) and a test file.
    write(
        &src.join("feature.ts"),
        "export function target() { return 1; }\nexport function caller() { return target(); }\n",
    );
    write(
        &src.join("feature.test.ts"),
        "import { target } from './feature';\nexport function checkTarget() { return target(); }\n",
    );
    // A leaf with no dependents — must NOT show up in the blast radius.
    write(
        &src.join("leaf.ts"),
        "export function lonelyLeaf() { return 42; }\n",
    );

    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    cg
}

#[test]
fn lists_dependents_and_covering_tests_for_an_entry_symbol() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = blast_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "target");

    assert!(
        text.contains("### Blast radius"),
        "missing blast radius:\n{text}"
    );
    assert!(text.contains("`src/feature.ts::target`"));
    assert!(text.contains("caller")); // a caller count is reported
    // It names WHERE (the caller file) — not the caller's source body.
    assert!(text.contains("feature.ts"));
    // Test coverage is surfaced (covering test file, or the warning).
    let tests_re = regex::Regex::new(r"tests:.*feature\.test\.ts").unwrap();
    assert!(
        tests_re.is_match(&text) || text.contains("no covering tests"),
        "test coverage not surfaced:\n{text}"
    );
}

#[test]
fn omits_symbols_that_have_no_dependents_from_the_blast_radius() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = blast_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "lonelyLeaf");
    // lonelyLeaf has zero callers — it must never appear under a blast-radius
    // bullet. (TS: /Blast radius[\s\S]*`lonelyLeaf`/ must not match.)
    if let Some(pos) = text.find("Blast radius") {
        assert!(
            !text[pos..].contains("`lonelyLeaf`"),
            "lonelyLeaf appeared in the blast radius:\n{text}"
        );
    }
}

#[test]
fn callees_do_not_fall_back_to_a_wrong_symbol_for_qualified_misses() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src").join("lib.ts"),
        "export function helper() { return 1; }\nexport function run_index_all() { return helper(); }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute(
        "codegraph_callees",
        &json!({ "symbol": "nope.run_index_all", "limit": 10 }),
    );
    assert_ne!(res.is_error, Some(true), "callees errored: {}", res.text());
    let text = res.text();
    assert!(
        text.contains("Symbol \"nope.run_index_all\" not found in the codebase"),
        "unexpected qualified-miss response:\n{text}"
    );
    assert!(
        !text.contains("helper"),
        "qualified miss must not fall back to run_index_all:\n{text}"
    );
}

// =============================================================================
// CODEGRAPH_MCP_TOOLS allowlist (__tests__/mcp-tool-allowlist.test.ts)
