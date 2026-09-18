//! Runtime selection for the rmcp CodeGraph server.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::Result;
use crate::mcp::engine::EngineHandle;
use crate::sync::worktree::find_writable_codegraph_root;

#[cfg(unix)]
mod daemon;
mod direct;
#[cfg(unix)]
mod proxy;

const DAEMON_INTERNAL_ENV: &str = "CODEGRAPH_DAEMON_INTERNAL";

fn env_enabled(name: &str) -> bool {
    std::env::var(name)
        .is_ok_and(|value| !value.is_empty() && value != "0" && value.to_lowercase() != "false")
}

fn daemon_opt_out_set() -> bool {
    env_enabled("CODEGRAPH_NO_DAEMON")
}

fn daemon_internal_set() -> bool {
    env_enabled(DAEMON_INTERNAL_ENV)
}

/// The index root whose shared daemon this session may join (or spawn).
///
/// A daemon watches and syncs its root, so only a session inside the checkout
/// that owns the index gets one. A session in a different checkout — a
/// worktree nested in the main checkout finds the main's index — runs direct:
/// it reads the borrowed index with the worktree notice, and neither starts
/// nor shares the other checkout's daemon (which could not tell its sessions
/// apart to warn this one).
fn resolve_daemon_root(explicit_path: Option<&str>) -> Option<PathBuf> {
    let candidate = explicit_path
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    let root = find_writable_codegraph_root(&candidate).ok().flatten()?;
    Some(std::fs::canonicalize(&root).unwrap_or(root))
}

pub struct MCPServer {
    project_path: Option<String>,
    engine: Mutex<Option<EngineHandle>>,
    stopped: AtomicBool,
}

impl MCPServer {
    pub fn new<P: Into<String>>(project_path: Option<P>) -> MCPServer {
        MCPServer {
            project_path: project_path.map(Into::into),
            engine: Mutex::new(None),
            stopped: AtomicBool::new(false),
        }
    }

    pub async fn start(&self) -> Result<()> {
        if daemon_internal_set() {
            #[cfg(unix)]
            return daemon::start(self);
            #[cfg(not(unix))]
            return direct::start(
                self,
                "CODEGRAPH_DAEMON_INTERNAL set on a platform without daemon support",
            )
            .await;
        }
        if daemon_opt_out_set() {
            return direct::start(self, "CODEGRAPH_NO_DAEMON set").await;
        }

        let Some(root) = resolve_daemon_root(self.project_path.as_deref()) else {
            return direct::start(self, "no .codegraph/ root owned by this checkout").await;
        };

        #[cfg(unix)]
        {
            if let Some(socket) = proxy::connect_or_spawn(&root) {
                crate::mcp::proxy::run_connected_proxy(socket);
            }
            direct::start(self, "shared daemon unavailable").await
        }
        #[cfg(not(unix))]
        {
            let _ = root;
            direct::start(self, "daemon mode unavailable on this platform").await
        }
    }

    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(engine) = self
            .engine
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            engine.stop();
        }
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_env_parsers_match_ts_truthiness() {
        assert!(env_enabled_for_test("1"));
        assert!(env_enabled_for_test("true"));
        assert!(env_enabled_for_test("yes"));
        assert!(!env_enabled_for_test("0"));
        assert!(!env_enabled_for_test("false"));
        assert!(!env_enabled_for_test("FALSE"));
        assert!(!env_enabled_for_test(""));
    }

    #[test]
    fn resolve_daemon_root_finds_and_canonicalizes_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(root.join(".codegraph")).unwrap();
        std::fs::write(root.join(".codegraph").join("codegraph.db"), b"").unwrap();
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        let resolved = resolve_daemon_root(Some(&nested.to_string_lossy())).unwrap();
        assert_eq!(resolved, std::fs::canonicalize(&root).unwrap());

        let missing = tmp.path().join("no-project");
        std::fs::create_dir_all(&missing).unwrap();
        assert!(resolve_daemon_root(Some(&missing.to_string_lossy())).is_none());
    }

    #[test]
    fn a_nested_worktree_never_joins_the_main_checkouts_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(main.join(".git/worktrees/wt")).unwrap();
        std::fs::write(main.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
        std::fs::create_dir_all(main.join(".codegraph")).unwrap();
        std::fs::write(main.join(".codegraph").join("codegraph.db"), b"").unwrap();
        let worktree = main.join(".claude/worktrees/wt");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()),
        )
        .unwrap();

        assert!(resolve_daemon_root(Some(&worktree.join("src").to_string_lossy())).is_none());
        assert_eq!(
            resolve_daemon_root(Some(&main.to_string_lossy())),
            Some(std::fs::canonicalize(&main).unwrap())
        );
    }

    fn env_enabled_for_test(value: &str) -> bool {
        !value.is_empty() && value != "0" && value.to_lowercase() != "false"
    }
}
