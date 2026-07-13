use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use codegraph::{CodeGraph, IndexOptions};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_codegraph")
}

fn run_cli(root: &Path, registry: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(root)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_DAEMON_REGISTRY_DIR", registry)
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph")
}

async fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::Builder::new()
        .prefix("codegraph-cli-tools-")
        .tempdir()
        .unwrap();
    let root = temp.path().join("project");
    let registry = temp.path().join("registry");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn parse_token(input: &str) -> usize {\n    input.len()\n}\n\npub fn consume() -> usize {\n    parse_token(\"ok\")\n}\n",
    )
    .unwrap();
    let graph = CodeGraph::init_sync(&root).unwrap();
    let result = graph.index_all(&IndexOptions::default()).await.unwrap();
    assert!(result.success);
    graph.close();
    (temp, root, registry)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn version_subcommand_prints_the_bare_package_version() {
    let temp = tempfile::tempdir().unwrap();
    let output = run_cli(temp.path(), &temp.path().join("registry"), &["version"]);
    assert!(output.status.success());
    assert_eq!(stdout(&output), format!("{}\n", env!("CARGO_PKG_VERSION")));
}

#[tokio::test(flavor = "current_thread")]
async fn version_aliases_print_the_bare_package_version() {
    let temp = tempfile::tempdir().unwrap();
    for alias in ["--version", "-V", "-v", "-version"] {
        let output = run_cli(temp.path(), &temp.path().join("registry"), &[alias]);
        assert!(output.status.success(), "{alias} failed");
        assert_eq!(stdout(&output), format!("{}\n", env!("CARGO_PKG_VERSION")));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn telemetry_cli_controls_and_reports_local_state() {
    let temp = tempfile::tempdir().unwrap();
    let registry = temp.path().join("registry");
    let run = |args: &[&str]| {
        Command::new(bin())
            .args(args)
            .current_dir(temp.path())
            .env("HOME", temp.path())
            .env_remove("DO_NOT_TRACK")
            .env_remove("CODEGRAPH_TELEMETRY")
            .env("CODEGRAPH_NO_DAEMON", "1")
            .env("CODEGRAPH_DAEMON_REGISTRY_DIR", &registry)
            .stdin(Stdio::null())
            .output()
            .expect("spawn codegraph telemetry")
    };

    let disabled = run(&["telemetry", "off"]);
    assert!(disabled.status.success());
    assert!(stdout(&disabled).contains("Telemetry disabled"));

    let status = run(&["telemetry", "status"]);
    assert!(status.status.success());
    let text = stdout(&status);
    assert!(text.contains("Telemetry: disabled"), "{text}");
    assert!(text.contains("telemetry.json"), "{text}");

    let invalid = run(&["telemetry", "maybe"]);
    assert_eq!(invalid.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("expected status, on, or off"));
}

#[tokio::test(flavor = "current_thread")]
async fn hidden_prompt_hook_is_fail_open_for_invalid_input() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .arg("prompt-hook")
        .current_dir(temp.path())
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.as_mut().unwrap().write_all(b"not-json")?;
            child.wait_with_output()
        })
        .expect("spawn prompt hook");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn explore_and_node_symbol_use_the_mcp_handler_output() {
    let (_temp, root, registry) = fixture().await;

    let explore = run_cli(
        &root,
        &registry,
        &["explore", "parse_token", "consume", "--max-files", "2"],
    );
    assert!(
        explore.status.success(),
        "explore failed: {}",
        String::from_utf8_lossy(&explore.stderr)
    );
    let explore_text = stdout(&explore);
    assert!(explore_text.contains("parse_token"), "{explore_text}");
    assert!(explore_text.contains("src/lib.rs"), "{explore_text}");
    // The CLI serves the human-readable projection: markdown headings and a
    // fenced source block, not the MCP structured JSON.
    assert!(explore_text.contains("### Source Code"), "{explore_text}");
    assert!(explore_text.contains("#### src/lib.rs"), "{explore_text}");
    assert!(explore_text.contains("```"), "{explore_text}");
    assert!(
        !explore_text.contains("\"schemaVersion\""),
        "CLI leaked the MCP JSON projection: {explore_text}"
    );
    assert!(
        !explore_text.contains("\"sourceFiles\""),
        "CLI leaked the MCP JSON projection: {explore_text}"
    );
    assert!(
        !explore_text.trim_start().starts_with('{'),
        "CLI output must not be a JSON document: {explore_text}"
    );

    let node = run_cli(&root, &registry, &["node", "parse_token"]);
    assert!(
        node.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&node.stderr)
    );
    let node_text = stdout(&node);
    assert!(node_text.contains("parse_token"), "{node_text}");
    assert!(node_text.contains("input.len()"), "{node_text}");
}

#[tokio::test(flavor = "current_thread")]
async fn node_file_mode_supports_path_offset_limit_and_symbols_only() {
    let (_temp, root, registry) = fixture().await;

    let read = run_cli(
        &root,
        &registry,
        &["node", "src/lib.rs", "--offset", "2", "--limit", "1"],
    );
    assert!(
        read.status.success(),
        "node file failed: {}",
        String::from_utf8_lossy(&read.stderr)
    );
    let read_text = stdout(&read);
    assert!(read_text.contains("2\t    input.len()"), "{read_text}");
    assert!(!read_text.contains("1\tpub fn parse_token"), "{read_text}");

    let symbols = run_cli(
        &root,
        &registry,
        &["node", "-f", "src/lib.rs", "--symbols-only"],
    );
    assert!(symbols.status.success());
    let symbols_text = stdout(&symbols);
    assert!(symbols_text.contains("**Symbols**"), "{symbols_text}");
    assert!(symbols_text.contains("`parse_token`"), "{symbols_text}");
    assert!(!symbols_text.contains("input.len()"), "{symbols_text}");
}

#[tokio::test(flavor = "current_thread")]
async fn daemon_lists_registry_records_and_cleans_an_invalid_project_lock() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let registry = temp.path().join("registry");
    fs::create_dir_all(root.join(".codegraph")).unwrap();
    fs::create_dir_all(&registry).unwrap();
    fs::write(
        registry.join("live.json"),
        serde_json::to_string(&serde_json::json!({
            "root": root,
            "pid": std::process::id(),
            "version": env!("CARGO_PKG_VERSION"),
            "socketPath": "/tmp/codegraph-cli-tools.sock",
            "startedAt": 1
        }))
        .unwrap(),
    )
    .unwrap();

    let listed = run_cli(&root, &registry, &["daemon", "--json"]);
    assert!(listed.status.success());
    let records: serde_json::Value = serde_json::from_str(stdout(&listed).trim()).unwrap();
    assert_eq!(records[0]["pid"], serde_json::json!(std::process::id()));

    fs::remove_file(registry.join("live.json")).unwrap();
    let lock_path = root.join(".codegraph/daemon.pid");
    fs::write(
        &lock_path,
        serde_json::to_string(&serde_json::json!({
            "pid": -1,
            "version": env!("CARGO_PKG_VERSION"),
            "socketPath": "/tmp/codegraph-cli-tools-stale.sock",
            "startedAt": 1
        }))
        .unwrap(),
    )
    .unwrap();
    let stopped = run_cli(
        &root,
        &registry,
        &[
            "daemon",
            "--stop",
            "--path",
            root.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(stopped.status.success());
    let results: serde_json::Value = serde_json::from_str(stdout(&stopped).trim()).unwrap();
    assert_eq!(results[0]["outcome"], "no-daemon");
    assert!(!lock_path.exists());
}
