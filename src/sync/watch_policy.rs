//! Watch Policy
//!
//! Decides whether the live file watcher should run for a given project.
//!
//! Native recursive `fs.watch` is pathologically slow on WSL2 `/mnt/*`
//! drives (NTFS exposed over the 9p/drvfs bridge): setting up the recursive
//! watch walks the directory tree, and every readdir/stat crosses the
//! Windows boundary. Inside an MCP server this stalls the event loop during
//! startup long enough to blow past host handshake timeouts (opencode's 30s),
//! so the tools never appear. See issue #199.
//!
//! This module centralizes the on/off decision so the watcher, the MCP
//! server (for diagnostics), and the installer all agree.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use regex::Regex;

use crate::utils::normalize_path;

static WSL_CACHE: Mutex<Option<bool>> = Mutex::new(None);

/// Detect whether the current process is running under WSL (Windows
/// Subsystem for Linux). Result is cached after the first call.
///
/// Checks the WSL-specific env vars first (no I/O), then falls back to
/// `/proc/version`, which contains "microsoft" on WSL kernels.
pub fn detect_wsl() -> bool {
    let mut cache = WSL_CACHE.lock().unwrap();
    if let Some(value) = *cache {
        return value;
    }
    let value = compute_wsl();
    *cache = Some(value);
    value
}

fn compute_wsl() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    // Mirror JS truthiness: a present-but-empty env var does NOT count.
    let truthy_env = |key: &str| std::env::var(key).map(|v| !v.is_empty()).unwrap_or(false);
    if truthy_env("WSL_DISTRO_NAME") || truthy_env("WSL_INTEROP") {
        return true;
    }
    match std::fs::read_to_string("/proc/version") {
        Ok(version) => {
            let version = version.to_lowercase();
            version.contains("microsoft") || version.contains("wsl")
        }
        Err(_) => false,
    }
}

/// True for WSL Windows-drive mounts like `/mnt/c` or `/mnt/d/project`.
/// Deliberately matches only single-letter drive mounts, so genuinely fast
/// Linux mounts such as `/mnt/wsl/...` are not flagged.
fn is_windows_drive_mount(project_root: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)^/mnt/[a-z](/|$)").unwrap());
    re.is_match(&normalize_path(project_root))
}

/// Inputs that can be overridden in tests so the decision is deterministic
/// without touching real env vars or `/proc/version`.
#[derive(Debug, Default, Clone)]
pub struct WatchProbe {
    /// Defaults to the process environment. When `Some`, the map fully
    /// replaces the process env (a missing key is treated as unset).
    pub env: Option<HashMap<String, String>>,
    /// Defaults to `detect_wsl()`.
    pub is_wsl: Option<bool>,
}

/// Decide whether the file watcher should be disabled for a project, and why.
///
/// Returns a short human-readable reason when watching should be skipped, or
/// `None` when it should run normally.
///
/// Precedence (first match wins):
///  1. `CODEGRAPH_NO_WATCH=1`    → off  (explicit opt-out always wins)
///  2. `CODEGRAPH_FORCE_WATCH=1` → on   (overrides auto-detection)
///  3. WSL2 + `/mnt/*` drive     → off  (recursive fs.watch is too slow; #199)
pub fn watch_disabled_reason(project_root: &str, probe: &WatchProbe) -> Option<String> {
    let get = |key: &str| -> Option<String> {
        match &probe.env {
            Some(env) => env.get(key).cloned(),
            None => std::env::var(key).ok(),
        }
    };

    if get("CODEGRAPH_NO_WATCH").as_deref() == Some("1") {
        return Some("CODEGRAPH_NO_WATCH=1 is set".to_string());
    }
    if get("CODEGRAPH_FORCE_WATCH").as_deref() == Some("1") {
        return None;
    }

    let is_wsl = probe.is_wsl.unwrap_or_else(detect_wsl);
    if is_wsl && is_windows_drive_mount(project_root) {
        return Some(
            "project is on a WSL2 /mnt/ drive, where recursive fs.watch is too slow to be reliable"
                .to_string(),
        );
    }

    None
}

/// Test-only: reset the cached WSL detection.
pub fn reset_wsl_cache_for_tests() {
    *WSL_CACHE.lock().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Option<HashMap<String, String>> {
        Some(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn returns_a_reason_when_codegraph_no_watch_is_set() {
        let reason = watch_disabled_reason(
            "/home/me/project",
            &WatchProbe {
                env: env(&[("CODEGRAPH_NO_WATCH", "1")]),
                is_wsl: Some(false),
            },
        );
        let reason = reason.expect("expected a reason");
        assert!(reason.contains("CODEGRAPH_NO_WATCH"));
    }

    #[test]
    fn auto_disables_on_a_wsl2_mnt_drive() {
        let reason = watch_disabled_reason(
            "/mnt/d/code/project",
            &WatchProbe {
                env: env(&[]),
                is_wsl: Some(true),
            },
        );
        let reason = reason.expect("expected a reason");
        assert!(reason.contains("mnt"));
    }

    #[test]
    fn does_not_disable_on_a_native_wsl_home_path() {
        assert_eq!(
            watch_disabled_reason(
                "/home/me/project",
                &WatchProbe {
                    env: env(&[]),
                    is_wsl: Some(true)
                },
            ),
            None
        );
    }

    #[test]
    fn does_not_disable_on_mnt_when_not_running_under_wsl() {
        // A real Linux box may legitimately have a fast /mnt mount.
        assert_eq!(
            watch_disabled_reason(
                "/mnt/d/code/project",
                &WatchProbe {
                    env: env(&[]),
                    is_wsl: Some(false)
                },
            ),
            None
        );
    }

    #[test]
    fn does_not_treat_mnt_wsl_fast_linux_mount_as_a_windows_drive() {
        assert_eq!(
            watch_disabled_reason(
                "/mnt/wsl/project",
                &WatchProbe {
                    env: env(&[]),
                    is_wsl: Some(true)
                },
            ),
            None
        );
    }

    #[test]
    fn codegraph_force_watch_overrides_wsl_auto_detect() {
        let reason = watch_disabled_reason(
            "/mnt/d/code/project",
            &WatchProbe {
                env: env(&[("CODEGRAPH_FORCE_WATCH", "1")]),
                is_wsl: Some(true),
            },
        );
        assert_eq!(reason, None);
    }

    #[test]
    fn codegraph_no_watch_wins_over_codegraph_force_watch() {
        let reason = watch_disabled_reason(
            "/home/me/project",
            &WatchProbe {
                env: env(&[("CODEGRAPH_NO_WATCH", "1"), ("CODEGRAPH_FORCE_WATCH", "1")]),
                is_wsl: Some(false),
            },
        );
        assert!(reason.is_some());
    }
}
