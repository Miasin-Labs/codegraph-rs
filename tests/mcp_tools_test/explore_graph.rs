#[test]
fn arch_overview_lists_in_scope_modules_and_symbols_only() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/core/util.ts"),
        "export function helper(): number { return 1; }\nexport class Widget { build(): number { return helper(); } }\n",
    );
    write(
        &dir.path().join("src/app.ts"),
        "import { helper } from \"./core/util\";\nexport function run(): number { return helper(); }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute("codegraph_arch", &json!({ "path": "src/core" }));
    assert_ne!(res.is_error, Some(true), "arch errored: {}", res.text());
    let text = res.text();

    assert!(
        text.contains("Architecture overview"),
        "missing header: {text}"
    );
    assert!(
        text.contains("src/core/util.ts"),
        "in-scope file missing: {text}"
    );
    assert!(text.contains("helper"), "function symbol missing: {text}");
    assert!(text.contains("Widget"), "class symbol missing: {text}");
    assert!(
        !text.contains("src/app.ts ["),
        "out-of-scope file leaked into module listing: {text}"
    );
    assert!(
        text.contains("Depends on (external") && text.contains("Depended on by (external"),
        "boundary sections missing: {text}"
    );
}

#[test]
fn xref_lists_incoming_references_to_a_symbol() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/lib.ts"),
        "export function target(): number { return 1; }\nexport function caller(): number { return target(); }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute("codegraph_xref", &json!({ "symbol": "target" }));
    assert_ne!(res.is_error, Some(true), "xref errored: {}", res.text());
    let text = res.text();

    assert!(text.contains("target"), "missing symbol header: {text}");
    assert!(text.contains("caller"), "missing incoming caller: {text}");
}

#[test]
fn paths_finds_call_chain_from_source_to_sink() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/lib.ts"),
        "export function sink(): number { return 1; }\nexport function mid(): number { return sink(); }\nexport function source(): number { return mid(); }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute(
        "codegraph_paths",
        &json!({ "from": "source", "to": "sink" }),
    );
    assert_ne!(res.is_error, Some(true), "paths errored: {}", res.text());
    let text = res.text();

    assert!(
        text.contains("Path from source to sink"),
        "no path header: {text}"
    );
    assert!(text.contains("mid"), "path should traverse mid: {text}");
}

#[test]
fn verify_roles_proposes_then_proves_emitting_the_deviant_caller() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/lib.rs"),
        "pub fn check_auth() -> bool { true }\n\
         pub fn delete_order() {}\n\
         pub fn handler_a() {\n    check_auth();\n    delete_order();\n}\n\
         pub fn handler_b() {\n    check_auth();\n    delete_order();\n}\n\
         pub fn handler_c() {\n    check_auth();\n    delete_order();\n}\n\
         pub fn handler_d() {\n    check_auth();\n    delete_order();\n}\n\
         pub fn handler_e() {\n    delete_order();\n}\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute(
        "codegraph_verify_roles",
        &json!({
            "roles": [
                { "symbol": "delete_order", "role": "sink", "confidence": 0.9,
                  "rationale": "destructive write" },
                { "symbol": "check_auth", "role": "guard", "confidence": 0.8,
                  "rationale": "authorization gate" }
            ]
        }),
    );
    assert_ne!(
        res.is_error,
        Some(true),
        "verify_roles errored: {}",
        res.text()
    );
    let text = res.text();

    assert!(
        text.contains("model proposes, graph proves"),
        "missing provenance banner: {text}"
    );
    assert!(
        text.contains("handler_e"),
        "deviant caller not flagged: {text}"
    );
    assert!(text.contains("via llm"), "finding not llm-tagged: {text}");
    assert!(
        text.contains("LLM-proposed sink"),
        "missing finding message: {text}"
    );
    assert!(
        !text.contains("handler_a") && !text.contains("handler_b"),
        "compliant caller wrongly flagged: {text}"
    );
}

#[test]
fn verify_roles_drops_hallucinated_sink_without_enough_callers() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/lib.rs"),
        "pub fn never_called() {}\npub fn auth() -> bool { true }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute(
        "codegraph_verify_roles",
        &json!({
            "roles": [
                { "symbol": "never_called", "role": "sink", "confidence": 0.99 },
                { "symbol": "auth", "role": "guard", "confidence": 0.99 }
            ]
        }),
    );
    assert_ne!(
        res.is_error,
        Some(true),
        "verify_roles errored: {}",
        res.text()
    );
    let text = res.text();
    assert!(
        text.contains("0 verified finding(s)")
            || text.contains("No proposal survived graph verification"),
        "hallucinated sink should yield no findings: {text}"
    );
}
