//! The prompt hook's session-start digest: on the first prompt of a
//! session, what earlier sessions in the same repository did.
//!
//! Time-boxed like every hook path: a few `stat`s to find the repository,
//! one marker-file create to tell the first prompt from later ones, and one
//! indexed read of the precomputed digest on a read-only connection. It
//! never writes the history store and never ingests inline — a missing,
//! outdated or stale store is refreshed by a detached ingest (see
//! [`super::background`]) and this prompt gets nothing.
//!
//! When the atlas links the project to others by path dependencies, one
//! more line names those with recent failures or edits to the shared code
//! ([`super::atlas_join::linked_line`]); the block stays within
//! [`DIGEST_BUDGET`].

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, io};

use super::atlas_join::{LINKED_LINE_MAX, linked_line};
use super::background::{history_enabled, is_stale, spawn_background_ingest};
use super::memory::{DIGEST_BUDGET, DigestStatus, read_digest};
use super::sources::key_hash;
use super::{default_history_path, now_ms, repo_root_of};

/// Session markers older than this are pruned.
const MARKER_TTL: Duration = Duration::from_secs(3 * 86_400);
/// Most markers looked at by one prune.
const PRUNE_SCAN: usize = 1_000;

/// The `<codegraph_history>` block for this prompt, if it is the first of
/// `session_id` and the repository containing `cwd` has a digest.
pub fn session_start_digest(cwd: &Path, session_id: Option<&str>) -> Option<String> {
    if !history_enabled() {
        return None;
    }
    let atlas = crate::atlas::atlas_enabled().then(crate::atlas::atlas_path);
    session_start_digest_at(
        &default_history_path(),
        atlas.as_deref(),
        cwd,
        session_id,
        &mut |db| {
            let _ = spawn_background_ingest(db);
        },
    )
}

/// [`session_start_digest`] against the store at `db` and the atlas at
/// `atlas` (`None`: no linked-projects line); `refresh` is asked to start a
/// detached ingest when the store is missing, outdated or stale.
pub fn session_start_digest_at(
    db: &Path,
    atlas: Option<&Path>,
    cwd: &Path,
    session_id: Option<&str>,
    refresh: &mut dyn FnMut(&Path),
) -> Option<String> {
    let repo = repo_root_of(cwd)?;
    let read = read_digest(db, &repo);
    let stale = match read.status {
        DigestStatus::NoStore | DigestStatus::Outdated => true,
        DigestStatus::Absent | DigestStatus::Ready { .. } => is_stale(read.last_run_ms, now_ms()),
    };
    if stale {
        refresh(db);
    }
    let DigestStatus::Ready { body, .. } = read.status else {
        return None;
    };
    if !first_prompt(db, session_id?) {
        return None;
    }
    let body = match atlas {
        Some(atlas) => with_linked_line(body.trim_end(), |room| {
            linked_line(db, atlas, cwd, &repo, room)
        }),
        None => body,
    };
    Some(format!(
        "<codegraph_history note=\"What earlier agent sessions did in this repository, from local session logs (paths repo-relative). Use it to skip re-discovery; verify before relying on it.\">\n{}\n</codegraph_history>\n",
        body.trim_end()
    ))
}

/// `body` plus the linked-projects line `line(room)` renders into the room
/// left under [`DIGEST_BUDGET`] — making room, when there is too little,
/// by dropping the digest's hot-files line (recall still has it).
fn with_linked_line(body: &str, line: impl FnOnce(usize) -> Option<String>) -> String {
    let room = |text: &str| DIGEST_BUDGET.saturating_sub(text.len() + 1);
    let join = |text: &str, extra: String| format!("{text}\n{extra}");
    if room(body) >= LINKED_LINE_MAX / 2 {
        return match line(room(body)) {
            Some(extra) => join(body, extra),
            None => body.to_owned(),
        };
    }
    let trimmed: String = body
        .lines()
        .filter(|l| !l.starts_with("Hot files"))
        .collect::<Vec<_>>()
        .join("\n");
    match line(room(&trimmed)) {
        Some(extra) => join(&trimmed, extra),
        None => body.to_owned(),
    }
}

/// Whether this is the first prompt of `session_id`: claims a marker file
/// next to the store (`hook-sessions/<hash>`), created exactly once.
fn first_prompt(db: &Path, session_id: &str) -> bool {
    let Some(dir) = db.parent().map(|d| d.join("hook-sessions")) else {
        return false;
    };
    if create_private_dir(&dir).is_err() {
        return false;
    }
    let name = key_hash(&format!("session\0{session_id}"));
    let marker = dir.join(&name[..16]);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&marker) {
        Ok(_) => {
            if name.starts_with('0') {
                prune(&dir);
            }
            true
        }
        Err(_) => false,
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Drop stale markers (bounded).
fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let old: Vec<PathBuf> = entries
        .take(PRUNE_SCAN)
        .filter_map(Result::ok)
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > MARKER_TTL)
        })
        .map(|e| e.path())
        .collect();
    for path in old {
        let _ = fs::remove_file(path);
    }
}
