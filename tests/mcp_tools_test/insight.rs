fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(status.status.success(), "git {args:?}: {status:?}");
}

async fn insight_fixture(root: &Path) -> ToolHandler {
    write(
        &root.join("src/auth.ts"),
        "export function parseToken(raw: string) { return raw.trim(); }\n\
         export function neverTested() { return 2; }\n",
    );
    write(
        &root.join("src/session.ts"),
        "import { parseToken } from './auth';\n\
         export function login(raw: string) { return parseToken(raw); }\n",
    );
    write(
        &root.join("tests/session.test.ts"),
        "import { login } from '../src/session';\n\
         export function testLoginTrimsToken() { return login(' x '); }\n",
    );
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    ToolHandler::new(Some(Rc::new(cg)))
}

/// `codegraph_tests` walks callers back to test code: parseToken <- login <- test.
#[tokio::test(flavor = "current_thread")]
async fn tests_tool_finds_tests_reaching_a_symbol_through_callers() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let handler = insight_fixture(dir.path()).await;

    let result = handler.execute("codegraph_tests", &json!({ "symbol": "parseToken" }));
    assert_ne!(result.is_error, Some(true), "tests errored: {}", result.text());
    let text = result.text();
    assert!(text.contains("testLoginTrimsToken"), "{text}");
    assert!(text.contains("tests/session.test.ts"), "{text}");

    let untested = handler.execute("codegraph_tests", &json!({ "symbol": "neverTested" }));
    assert!(untested.text().contains("No test reaches"), "{}", untested.text());
}

/// `codegraph_history` reports files that historically change together.
#[tokio::test(flavor = "current_thread")]
async fn history_tool_reports_code_that_changes_together() {
    let _env = env_read().await;
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    for round in 0..3 {
        write(
            &root.join("src/auth.ts"),
            &format!("export function parseToken(raw: string) {{ return raw.trim() + '{round}'; }}\n"),
        );
        write(
            &root.join("src/session.ts"),
            &format!(
                "import {{ parseToken }} from './auth';\n\
                 export function login(raw: string) {{ return parseToken(raw) + {round}; }}\n"
            ),
        );
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", &format!("round {round}")]);
    }
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let result = handler.execute("codegraph_history", &json!({ "symbol": "parseToken" }));
    assert_ne!(result.is_error, Some(true), "history errored: {}", result.text());
    let text = result.text();
    assert!(text.contains("login"), "{text}");
    assert!(text.contains("src/session.ts"), "{text}");
    let payload = result.structured_content.as_ref().expect("structured history");
    assert!(payload["commitsAnalyzed"].as_u64().unwrap() >= 3, "{payload}");
}

/// Both tools ship in the default surface.
#[tokio::test(flavor = "current_thread")]
async fn history_and_tests_are_default_tools() {
    let _env = env_write().await;
    let _guard = EnvVarGuard::unset("CODEGRAPH_MCP_TOOLS");
    let names: Vec<String> = get_static_tools().into_iter().map(|t| t.name).collect();
    assert!(names.contains(&"codegraph_history".to_string()), "{names:?}");
    assert!(names.contains(&"codegraph_tests".to_string()), "{names:?}");
}
