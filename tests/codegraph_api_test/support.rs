pub(crate) use std::fs;
pub(crate) use std::path::Path;
use std::process::Command;
pub(crate) use std::time::{Duration, Instant};

pub(crate) use codegraph::{
    CodeGraph,
    IndexOptions,
    NodeKind,
    SearchOptions,
    Severity,
    WatchOptions,
};
pub(crate) use tempfile::TempDir;

pub(crate) fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

pub(crate) async fn setup_indexed(root: &Path) -> CodeGraph {
    write(
        &root.join("src/index.ts"),
        "export function hello() { return 'world'; }",
    );
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    cg
}

pub(crate) fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be runnable");
    assert!(status.success(), "git {args:?} failed in {}", cwd.display());
}

pub(crate) fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should be runnable");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).to_string()
}

pub(crate) async fn setup_git_indexed(root: &Path) -> CodeGraph {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    write(
        &root.join("src/index.ts"),
        "export function hello() { return 'world'; }",
    );
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "initial"]);
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    cg
}

pub(crate) fn search_count(cg: &CodeGraph, query: &str) -> usize {
    cg.search_nodes(query, None).unwrap().len()
}

pub(crate) fn wait_for(mut predicate: impl FnMut() -> bool, timeout: Duration, what: &str) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for: {what}");
}

#[cfg(target_os = "linux")]
pub(crate) fn current_thread_names() -> Vec<String> {
    let Ok(entries) = fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };

    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| fs::read_to_string(entry.path().join("comm")).ok())
        .map(|name| name.trim().to_string())
        .collect()
}
