//! Daemon socket + lockfile path helpers — issue #411.
//!
//! One shared `codegraph serve --mcp` daemon per project root means we need a
//! stable, project-keyed rendezvous between cooperating processes. The IPC
//! surface area is just two file paths:
//!
//!   - `daemon.sock` — Unix domain socket / named pipe the daemon listens on.
//!   - `daemon.pid` — atomic-create lockfile holding the daemon's pid + version.
//!
//! Both live under `.codegraph/` so the project-scoped uninstall (`codegraph
//! uninit`) sweeps them up for free.
//!
//! Special-case: Unix domain socket paths have a hard length limit (~104 on
//! macOS, ~108 on Linux); when the in-project path exceeds it we fall back to
//! an absolute-path hash under the OS temp dir. The pidfile always stays in
//! the project (it doesn't have a length limit) — and acts as the
//! authoritative pointer to the socket path the daemon chose.
//!
//! Cross-implementation parity: the sha256-based `project_hash` (first 16 hex
//! chars of the resolved project root) is identical to the TS implementation
//! so TS and Rust daemons rendezvous on the same socket/pipe paths.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::directory::get_codegraph_dir;
use crate::utils::{lexical_resolve, sha256_hex};

/// Soft upper bound for in-project socket paths.
const POSIX_SOCKET_PATH_LIMIT: usize = 100;

/// Env var carrying the *host* PID (the relauncher's own parent) across a
/// re-exec, so the PPID watchdog can poll the real MCP host directly even when
/// an intermediate process sits between them. Same name as the TS constant
/// (`HOST_PPID_ENV` in `src/extraction/wasm-runtime-flags.ts`) — the wasm
/// runtime relaunch itself is Node-only and was dropped in the Rust port, but
/// the env var contract lives on here (#277).
pub const HOST_PPID_ENV: &str = "CODEGRAPH_HOST_PPID";

/// Short stable identifier for a project root — used in tmpdir/pipe names.
///
/// Mirrors TS: `sha256(path.resolve(projectRoot)).slice(0, 16)`. The resolve
/// is lexical (no symlink resolution), exactly like Node's `path.resolve`.
fn project_hash(project_root: &Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let resolved = lexical_resolve(&cwd, &project_root.to_string_lossy());
    let mut hash = sha256_hex(resolved.to_string_lossy().as_bytes());
    hash.truncate(16);
    hash
}

/// Compute the socket / named-pipe path the daemon should listen on (and the
/// proxy should connect to) for `project_root`. Deterministic given a project
/// root, so independent processes converge without coordination.
pub fn get_daemon_socket_path(project_root: &Path) -> PathBuf {
    if cfg!(windows) {
        return PathBuf::from(format!(
            "\\\\.\\pipe\\codegraph-{}",
            project_hash(project_root)
        ));
    }
    let in_project = get_codegraph_dir(project_root).join("daemon.sock");
    if in_project.as_os_str().len() <= POSIX_SOCKET_PATH_LIMIT {
        return in_project;
    }
    // Long project paths (deep monorepos, Bazel out dirs) need tmpdir fallback
    // or `bind` returns EADDRINUSE / ENAMETOOLONG. Hash keeps it project-scoped.
    std::env::temp_dir().join(format!("codegraph-{}.sock", project_hash(project_root)))
}

/// Absolute path to the daemon pid lockfile for `project_root`.
pub fn get_daemon_pid_path(project_root: &Path) -> PathBuf {
    get_codegraph_dir(project_root).join("daemon.pid")
}

/// Structured contents of the pid lockfile.
///
/// Serialized as camelCase JSON — byte-shape-identical to the TS pidfile so
/// TS and Rust daemons can arbitrate against each other's lockfiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonLockInfo {
    pub pid: i64,
    pub version: String,
    pub socket_path: String,
    pub started_at: i64,
}

/// Serialize a [`DaemonLockInfo`] for writing to the pidfile. JSON for human
/// readability — operators occasionally `cat` this when debugging.
///
/// Matches TS `JSON.stringify(info, null, 2) + '\n'` byte for byte.
pub fn encode_lock_info(info: &DaemonLockInfo) -> String {
    let mut body = serde_json::to_string_pretty(info).unwrap_or_else(|_| "{}".to_string());
    body.push('\n');
    body
}

/// Validate a decoded pid before it is trusted by the liveness/clear paths.
///
/// A real OS pid is a positive integer that fits in a `u32` — the liveness
/// checks in `daemon.rs`/`server.rs` narrow `info.pid as u32` before calling
/// [`crate::utils::is_process_alive`], and an out-of-range value silently wraps:
/// `2^32` truncates to `0` (which `libc::kill(0, …)` treats as the *caller's
/// own process group*, reporting a dead daemon as alive and wedging startup),
/// and a negative pid would otherwise leave an unclearable stale lock. Rejecting
/// such values here routes a corrupt/hostile lockfile to `None` ("treat as a
/// dead/unknown holder, clearable") instead of a bogus "live" pid. Mirrors the
/// bound check already enforced by `proxy::parse_pid`.
fn pid_in_valid_range(pid: f64) -> bool {
    pid.is_finite() && pid.fract() == 0.0 && pid > 1.0 && pid <= u32::MAX as f64
}

/// Parse a pidfile body. Tolerant of old-format pidfiles (plain pid that fails
/// JSON parsing, e.g. a leading-zero decimal) so a 0.10.x daemon doesn't trip
/// over a 0.9.x lockfile if that ever happens — we treat such a lockfile as
/// "process is unknown version, refuse to share."
///
/// Faithful-port note: like the TS original, the legacy plain-pid branch is
/// reached only when JSON parsing *fails*. A bare decimal such as `12345` IS
/// valid JSON (a number), so it takes the JSON branch, fails the field checks,
/// and decodes to `None` — same as TS.
pub fn decode_lock_info(raw: &str) -> Option<DaemonLockInfo> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
        // Mirror the TS runtime type checks exactly: pid + startedAt must be
        // JSON numbers, version + socketPath must be strings.
        let pid = parsed.get("pid").and_then(|v| v.as_f64());
        let version = parsed.get("version").and_then(|v| v.as_str());
        let socket_path = parsed.get("socketPath").and_then(|v| v.as_str());
        let started_at = parsed.get("startedAt").and_then(|v| v.as_f64());
        if let (Some(pid), Some(version), Some(socket_path), Some(started_at)) =
            (pid, version, socket_path, started_at)
        {
            // Reject pids that can't be a real process (out of u32 range,
            // non-positive, fractional) so the downstream `as u32` narrowing
            // can never wrap into a live-looking pid.
            if !pid_in_valid_range(pid) {
                return None;
            }
            return Some(DaemonLockInfo {
                pid: pid as i64,
                version: version.to_string(),
                socket_path: socket_path.to_string(),
                started_at: started_at as i64,
            });
        }
        return None;
    }
    // Fall through to legacy plain-pid handling (TS: `Number(trimmed)`).
    if let Ok(pid) = trimmed.parse::<f64>() {
        if pid_in_valid_range(pid) {
            return Some(DaemonLockInfo {
                pid: pid as i64,
                version: "unknown".to_string(),
                socket_path: String::new(),
                started_at: 0,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_hash_is_first_16_of_sha256_of_resolved_root() {
        let root = if cfg!(windows) {
            "C:\\some\\project"
        } else {
            "/some/project"
        };
        let expected: String = sha256_hex(root.as_bytes()).chars().take(16).collect();
        assert_eq!(project_hash(Path::new(root)), expected);
    }

    #[test]
    fn socket_path_is_deterministic_and_in_project_when_short() {
        let root = Path::new(if cfg!(windows) { "C:\\proj" } else { "/proj" });
        assert_eq!(get_daemon_socket_path(root), get_daemon_socket_path(root));
        #[cfg(unix)]
        assert_eq!(
            get_daemon_socket_path(root),
            PathBuf::from("/proj/.codegraph/daemon.sock")
        );
    }

    #[cfg(unix)]
    #[test]
    fn socket_path_falls_back_to_tmpdir_for_long_roots() {
        let long_root = format!("/{}", "a".repeat(200));
        let p = get_daemon_socket_path(Path::new(&long_root));
        assert!(p.starts_with(std::env::temp_dir()));
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("codegraph-") && name.ends_with(".sock"));
        // Project-scoped: the hash of the resolved root is embedded.
        assert!(name.contains(&project_hash(Path::new(&long_root))));
    }

    #[test]
    fn encode_decode_round_trip() {
        let info = DaemonLockInfo {
            pid: 4242,
            version: "1.2.3".to_string(),
            socket_path: "/tmp/x.sock".to_string(),
            started_at: 1_700_000_000_000,
        };
        let encoded = encode_lock_info(&info);
        assert!(encoded.ends_with('\n'));
        assert!(encoded.contains("\"socketPath\""));
        assert!(encoded.contains("\"startedAt\""));
        assert_eq!(decode_lock_info(&encoded), Some(info));
    }

    #[test]
    fn decode_rejects_empty_and_garbage() {
        assert_eq!(decode_lock_info(""), None);
        assert_eq!(decode_lock_info("   \n"), None);
        assert_eq!(decode_lock_info("{\"pid\":\"not-a-number\"}"), None);
        assert_eq!(decode_lock_info("not json at all"), None);
    }

    #[test]
    fn decode_rejects_out_of_range_pids() {
        // 2^32 would truncate to pid 0 via `as u32` — kill(0, 0) targets the
        // caller's own process group and always "succeeds", so a dead daemon
        // would look alive forever and no new daemon could start.
        let json = |pid: &str| {
            format!(
                "{{\"pid\":{pid},\"version\":\"1.0.0\",\"socketPath\":\"/tmp/x.sock\",\"startedAt\":1}}"
            )
        };
        assert_eq!(decode_lock_info(&json("4294967296")), None); // 2^32 → wraps to 0
        assert_eq!(decode_lock_info(&json("4294967297")), None); // 2^32+1 → wraps to 1 (init)
        assert_eq!(decode_lock_info(&json("-1")), None); // negative
        assert_eq!(decode_lock_info(&json("0")), None); // process group
        assert_eq!(decode_lock_info(&json("1")), None); // init — never a daemon
        assert_eq!(decode_lock_info(&json("123.5")), None); // fractional
        assert_eq!(decode_lock_info(&json("1e300")), None); // huge float
        // In-range pid still decodes.
        assert_eq!(decode_lock_info(&json("4242")).map(|i| i.pid), Some(4242));
        assert_eq!(
            decode_lock_info(&json("4294967295")).map(|i| i.pid),
            Some(u32::MAX as i64)
        );
        // Legacy plain-pid branch gets the same range guard.
        assert_eq!(decode_lock_info("-00123"), None);
        assert_eq!(decode_lock_info("04294967296"), None);
    }

    #[test]
    fn decode_matches_ts_legacy_semantics() {
        // Bare decimal is valid JSON → JSON branch → field checks fail → None
        // (same as TS — the legacy branch only fires when JSON.parse throws).
        assert_eq!(decode_lock_info("12345"), None);
        // Leading zero is NOT valid JSON → legacy Number() branch.
        let legacy = decode_lock_info("00123").expect("legacy pid decodes");
        assert_eq!(legacy.pid, 123);
        assert_eq!(legacy.version, "unknown");
        assert_eq!(legacy.socket_path, "");
        assert_eq!(legacy.started_at, 0);
    }
}
