//! Detached `codegraph history ingest --incremental` for callers that must
//! not block: the prompt hook and MCP `codegraph_recall`. Same rules as
//! [`crate::sync::background`]: only the `codegraph` CLI binary is ever
//! launched, in its own process group, and `CODEGRAPH_NO_BACKGROUND_SYNC=1`
//! disables it (those callers then simply answer from what's stored — they
//! never ingest inline).

use std::path::Path;
use std::process::{Command, Stdio};

use super::ingest::writer_lock;
use crate::sync::background::{BackgroundSync, background_disabled, cli_binary};

/// An ingest finished less than this long ago is fresh enough.
pub const REFRESH_AFTER_MS: i64 = 120_000;

/// Whether a store last ingested at `last_run_ms` should be refreshed.
pub fn is_stale(last_run_ms: Option<i64>, now_ms: i64) -> bool {
    last_run_ms.is_none_or(|t| now_ms - t >= REFRESH_AFTER_MS)
}

/// Start a detached incremental ingest into the store at `db_path`.
pub fn spawn_background_ingest(db_path: &Path) -> BackgroundSync {
    if background_disabled() {
        return BackgroundSync::Disabled;
    }
    if writer_lock(db_path).live_holder().is_some() {
        return BackgroundSync::AlreadyRunning;
    }
    let Some(exe) = cli_binary() else {
        return BackgroundSync::Unavailable;
    };
    let mut command = Command::new(exe);
    command
        .args(["history", "ingest", "--incremental", "--quiet", "--db"])
        .arg(db_path)
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
        BackgroundSync::Started
    } else {
        BackgroundSync::Unavailable
    }
}

/// History is on unless `CODEGRAPH_HISTORY=0` (or `false`/`off`).
pub fn history_enabled() -> bool {
    !std::env::var("CODEGRAPH_HISTORY").is_ok_and(|v| {
        let v = v.trim();
        v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")
    })
}
