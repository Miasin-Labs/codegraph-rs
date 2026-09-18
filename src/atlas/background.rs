//! Detached `codegraph projects register` for MCP sessions, which must not
//! write the atlas on a request path: a session that opens a project the
//! atlas doesn't know (or last saw over a day ago — the watcher's in-process
//! syncs never register) starts one and carries on. Same rules as
//! [`crate::sync::background`]: only the `codegraph` CLI binary is launched,
//! in its own process group, and `CODEGRAPH_NO_BACKGROUND_SYNC=1` disables
//! it.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::sync::background::{background_disabled, cli_binary};

/// A registration older than this is refreshed.
pub const REFRESH_AFTER_MS: i64 = 24 * 60 * 60 * 1000;

/// What [`register_in_background`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundRegister {
    /// The atlas already has a fresh entry for the project.
    Fresh,
    /// A detached `codegraph projects register` was started.
    Started,
    /// The atlas or background work is switched off.
    Disabled,
    /// No `codegraph` CLI binary could be located or started.
    Unavailable,
}

/// Whether an entry last seen at `last_seen_ms` should be refreshed.
pub fn is_stale(last_seen_ms: Option<i64>, now_ms: i64) -> bool {
    last_seen_ms.is_none_or(|t| now_ms - t >= REFRESH_AFTER_MS)
}

/// Make sure the atlas knows `root`, without writing it from this process:
/// one read-only lookup, and when the entry is missing or stale, a detached
/// registration.
pub fn register_in_background(root: &Path) -> BackgroundRegister {
    if !super::atlas_enabled() || background_disabled() {
        return BackgroundRegister::Disabled;
    }
    let last_seen = super::open_read_only()
        .ok()
        .flatten()
        .and_then(|atlas| atlas.project_by_root(root).ok().flatten())
        .map(|project| project.last_seen_ms);
    if !is_stale(last_seen, super::now_ms()) {
        return BackgroundRegister::Fresh;
    }
    let Some(exe) = cli_binary() else {
        return BackgroundRegister::Unavailable;
    };
    let mut command = Command::new(exe);
    command
        .args(["projects", "register", "--quiet"])
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
        BackgroundRegister::Started
    } else {
        BackgroundRegister::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staleness_is_a_day() {
        assert!(is_stale(None, 0));
        assert!(!is_stale(Some(1_000), 1_000 + REFRESH_AFTER_MS - 1));
        assert!(is_stale(Some(1_000), 1_000 + REFRESH_AFTER_MS));
    }
}
