use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub(crate) fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_codegraph")
}

/// Run the built binary with `cwd`, stdin closed (no interactive prompts),
/// `CODEGRAPH_NO_DAEMON=1` pinned like the rest of the CLI suite.
pub(crate) fn run_cli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph binary")
}

pub(crate) fn stdout_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

pub(crate) fn stderr_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Run an analyze subcommand with `--json` and parse the full envelope —
/// `{"schemaVersion": N, "kind": "<kind>", "data": …}`.
pub(crate) fn run_analyze_envelope(cwd: &Path, args: &[&str]) -> serde_json::Value {
    let mut full: Vec<&str> = vec!["analyze"];
    full.extend_from_slice(args);
    full.push("--json");
    let out = run_cli(cwd, &full);
    assert!(
        out.status.success(),
        "analyze {} failed: {}",
        args.join(" "),
        stderr_str(&out)
    );
    serde_json::from_str(stdout_str(&out).trim()).unwrap_or_else(|e| {
        panic!(
            "analyze {} did not print valid JSON ({e}): {}",
            args.join(" "),
            stdout_str(&out)
        )
    })
}

/// Run an analyze subcommand with `--json`, assert the envelope contract,
/// and return its `data` payload.
pub(crate) fn run_analyze_json(cwd: &Path, args: &[&str]) -> serde_json::Value {
    let envelope = run_analyze_envelope(cwd, args);
    assert!(
        envelope["schemaVersion"].as_u64().is_some(),
        "envelope carries schemaVersion: {envelope}"
    );
    assert!(
        envelope["kind"].as_str().is_some(),
        "envelope carries kind: {envelope}"
    );
    envelope["data"].clone()
}

/// Canonicalized tempdir (macOS /var → /private/var symlink parity).
pub(crate) fn temp_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("codegraph-analyze-cli-test-")
        .tempdir()
        .expect("create tempdir");
    let root = dir.path().canonicalize().expect("canonicalize tempdir");
    (dir, root)
}

pub(crate) fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

pub(crate) fn names_of(values: &[serde_json::Value]) -> Vec<&str> {
    values
        .iter()
        .map(|v| v["name"].as_str().unwrap_or_default())
        .collect()
}
