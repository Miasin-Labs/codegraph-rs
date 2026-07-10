//! Best-effort discovery and control for per-project MCP daemons.
//!
//! The per-project `daemon.pid` remains authoritative. Registry records under
//! `~/.codegraph/daemons` only make otherwise unrelated projects discoverable
//! to `codegraph daemon`; dead and malformed records are pruned on reads.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{fs, thread};

use serde::{Deserialize, Serialize};

use crate::mcp::daemon_paths::{
    DaemonLockInfo,
    decode_lock_info,
    get_daemon_pid_path,
    get_daemon_socket_path,
};
use crate::utils::{is_process_alive, lexical_resolve, sha256_hex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonRecord {
    pub root: String,
    pub pid: i64,
    pub version: String,
    pub socket_path: String,
    pub started_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopOutcome {
    Term,
    Kill,
    NotRunning,
    NoDaemon,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopResult {
    pub root: String,
    pub pid: Option<i64>,
    pub outcome: StopOutcome,
}

pub fn get_registry_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("CODEGRAPH_DAEMON_REGISTRY_DIR") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codegraph")
        .join("daemons")
}

fn normalized_root(root: &Path) -> PathBuf {
    fs::canonicalize(root).unwrap_or_else(|_| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        lexical_resolve(&cwd, &root.to_string_lossy())
    })
}

fn record_path(root: &Path) -> PathBuf {
    let root = normalized_root(root);
    let mut hash = sha256_hex(root.to_string_lossy().as_bytes());
    hash.truncate(16);
    get_registry_dir().join(format!("{hash}.json"))
}

fn record_from_lock(root: &Path, lock: &DaemonLockInfo) -> DaemonRecord {
    DaemonRecord {
        root: normalized_root(root).to_string_lossy().to_string(),
        pid: lock.pid,
        version: lock.version.clone(),
        socket_path: lock.socket_path.clone(),
        started_at: lock.started_at,
    }
}

/// Best-effort registration. A registry failure must never stop the daemon.
pub fn register_daemon(root: &Path, lock: &DaemonLockInfo) {
    let record = record_from_lock(root, lock);
    let path = record_path(Path::new(&record.root));
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(mut body) = serde_json::to_string_pretty(&record) else {
        return;
    };
    body.push('\n');
    let _ = write_private_file(&path, body.as_bytes());
}

/// Best-effort removal of one project's discovery record.
pub fn deregister_daemon(root: &Path) {
    let _ = fs::remove_file(record_path(root));
}

fn valid_record(record: &DaemonRecord) -> bool {
    let Ok(pid) = u32::try_from(record.pid) else {
        return false;
    };
    pid > 1 && !record.root.trim().is_empty() && is_process_alive(pid)
}

/// Return live registered daemons, newest first. Dead/garbage records are
/// removed when `prune` is true.
pub fn list_daemons(prune: bool) -> Vec<DaemonRecord> {
    let Ok(entries) = fs::read_dir(get_registry_dir()) else {
        return Vec::new();
    };
    let mut live = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let record = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<DaemonRecord>(&raw).ok());
        if let Some(record) = record.filter(valid_record) {
            live.push(record);
        } else if prune {
            let _ = fs::remove_file(path);
        }
    }
    live.sort_by_key(|record| std::cmp::Reverse(record.started_at));
    live
}

fn read_project_lock(root: &Path) -> Option<DaemonLockInfo> {
    fs::read_to_string(get_daemon_pid_path(root))
        .ok()
        .and_then(|raw| decode_lock_info(&raw))
}

/// Read a live daemon directly from one project's authoritative lockfile.
/// This also makes daemons started by an older Rust binary visible before
/// global registry support was added.
pub fn daemon_at(root: &Path) -> Option<DaemonRecord> {
    let record = record_from_lock(root, &read_project_lock(root)?);
    valid_record(&record).then_some(record)
}

fn same_root(left: &Path, right: &Path) -> bool {
    normalized_root(left) == normalized_root(right)
}

fn cleanup_artifacts(root: &Path, socket_path: Option<&str>) {
    let _ = fs::remove_file(get_daemon_pid_path(root));
    #[cfg(unix)]
    {
        if let Some(socket_path) = socket_path.filter(|value| !value.is_empty()) {
            let _ = fs::remove_file(socket_path);
        }
        let _ = fs::remove_file(get_daemon_socket_path(root));
    }
    deregister_daemon(root);
}

fn wait_for_death(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_process_alive(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    !is_process_alive(pid)
}

#[cfg(unix)]
fn terminate(pid: u32, force: bool) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    if pid <= 1 {
        return;
    }
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    unsafe {
        libc::kill(pid, signal);
    }
}

#[cfg(windows)]
fn terminate(pid: u32, force: bool) {
    let mut command = std::process::Command::new("taskkill");
    command.arg("/PID").arg(pid.to_string());
    if force {
        command.arg("/F");
    }
    let _ = command.status();
}

/// Stop the daemon serving `root`, preferring its authoritative lockfile and
/// falling back to the global discovery record.
pub fn stop_daemon_at(root: &Path) -> StopResult {
    let root = normalized_root(root);
    let lock = read_project_lock(&root);
    let registry = list_daemons(false)
        .into_iter()
        .find(|record| same_root(Path::new(&record.root), &root));
    let pid = lock
        .as_ref()
        .map(|info| info.pid)
        .or_else(|| registry.as_ref().map(|record| record.pid));
    let socket_path = lock
        .as_ref()
        .map(|info| info.socket_path.as_str())
        .or_else(|| registry.as_ref().map(|record| record.socket_path.as_str()));
    let root_string = root.to_string_lossy().to_string();

    let Some(pid) = pid else {
        cleanup_artifacts(&root, socket_path);
        return StopResult {
            root: root_string,
            pid: None,
            outcome: StopOutcome::NoDaemon,
        };
    };
    let Ok(pid_u32) = u32::try_from(pid) else {
        cleanup_artifacts(&root, socket_path);
        return StopResult {
            root: root_string,
            pid: Some(pid),
            outcome: StopOutcome::NotRunning,
        };
    };
    #[cfg(unix)]
    let signalable = libc::pid_t::try_from(pid_u32).is_ok_and(|pid| pid > 1);
    #[cfg(windows)]
    let signalable = pid_u32 > 1;
    if !signalable || !is_process_alive(pid_u32) {
        cleanup_artifacts(&root, socket_path);
        return StopResult {
            root: root_string,
            pid: Some(pid),
            outcome: StopOutcome::NotRunning,
        };
    }

    terminate(pid_u32, false);
    let outcome = if wait_for_death(pid_u32, Duration::from_secs(3)) {
        StopOutcome::Term
    } else {
        terminate(pid_u32, true);
        let _ = wait_for_death(pid_u32, Duration::from_secs(2));
        StopOutcome::Kill
    };
    cleanup_artifacts(&root, socket_path);
    StopResult {
        root: root_string,
        pid: Some(pid),
        outcome,
    }
}

/// Stop all currently registered live daemons.
pub fn stop_all_daemons() -> Vec<StopResult> {
    list_daemons(true)
        .into_iter()
        .map(|record| stop_daemon_at(Path::new(&record.root)))
        .collect()
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_shape_matches_the_daemon_lock_contract() {
        let dir = tempfile::tempdir().unwrap();
        let lock = DaemonLockInfo {
            pid: std::process::id() as i64,
            version: "1.2.3".to_string(),
            socket_path: "/tmp/codegraph-test.sock".to_string(),
            started_at: 123,
        };
        let record = record_from_lock(dir.path(), &lock);
        let json = serde_json::to_value(record).unwrap();
        assert_eq!(json["pid"], serde_json::json!(std::process::id()));
        assert_eq!(json["socketPath"], "/tmp/codegraph-test.sock");
        assert_eq!(json["startedAt"], 123);
    }
}
