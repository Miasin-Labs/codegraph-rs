//! Detached `codegraph sync` for callers that must not block.
//!
//! Time-boxed callers — the Claude prompt hook (killed at 30s) and MCP tool
//! calls (clients time out) — cannot run schema migrations or a vocabulary
//! rebuild inline on a large index. They hand the work to a detached
//! `codegraph sync` and answer immediately.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::directory::get_codegraph_dir;
use crate::utils::FileLock;

/// Outcome of asking for a background sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundSync {
    /// A detached `codegraph sync` was started for the project.
    Started,
    /// Another live process holds the project lock and is doing the work.
    AlreadyRunning,
    /// `CODEGRAPH_NO_BACKGROUND_SYNC` opts this process out of background work.
    Disabled,
    /// No `codegraph` CLI binary could be located or started.
    Unavailable,
}

impl BackgroundSync {
    /// True when the work is happening elsewhere and the caller should not
    /// also do it inline.
    pub fn in_progress(self) -> bool {
        matches!(self, Self::Started | Self::AlreadyRunning)
    }
}

/// `CODEGRAPH_NO_BACKGROUND_SYNC=1` keeps all work inline (deterministic tests,
/// or hosts that forbid detached processes).
fn background_disabled() -> bool {
    std::env::var("CODEGRAPH_NO_BACKGROUND_SYNC").is_ok_and(|value| {
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

/// The `codegraph` CLI: this executable when it is the CLI, otherwise a
/// `codegraph` next to it (e.g. beside `codegraph-mcp-server`). Never any
/// other binary — a test harness or the MCP server would misread `sync`.
fn cli_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = |path: &Path| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    };
    if name(&exe).as_deref() == Some("codegraph") {
        return Some(exe);
    }
    let sibling = exe.with_file_name(format!("codegraph{}", std::env::consts::EXE_SUFFIX));
    sibling.is_file().then_some(sibling)
}

/// Start `codegraph sync --quiet <root>` detached from this process.
pub fn spawn_background_sync(root: &Path) -> BackgroundSync {
    if background_disabled() {
        return BackgroundSync::Disabled;
    }
    if FileLock::new(get_codegraph_dir(root).join("codegraph.lock"))
        .live_holder()
        .is_some()
    {
        return BackgroundSync::AlreadyRunning;
    }
    let Some(exe) = cli_binary() else {
        return BackgroundSync::Unavailable;
    };
    let mut command = Command::new(exe);
    command
        .arg("sync")
        .arg("--quiet")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // A separate process group survives the caller's group being killed.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    if command.spawn().is_ok() {
        BackgroundSync::Started
    } else {
        BackgroundSync::Unavailable
    }
}
