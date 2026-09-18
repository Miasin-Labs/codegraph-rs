//! Git Worktree Awareness
//!
//! A CodeGraph index lives in a `.codegraph/` directory and is resolved by
//! walking up parent directories to the nearest one (see
//! [`find_nearest_codegraph_root`]). That walk is unaware of git checkouts:
//! when a worktree is created *inside* the main checkout (e.g. some tools place
//! them under `.gitignore`d paths like `.claude/worktrees/<name>/`), a command
//! run from the worktree walks up and silently resolves the MAIN checkout's
//! index.
//!
//! Every query then returns results from the main tree's code — usually a
//! different branch — rather than the worktree the user is actually editing,
//! and every write (`index`, `sync`, the watcher, a background sync) would
//! change the main checkout's index instead of this one's. This module detects
//! that "borrowed index" situation: reads warn about it, writes refuse it
//! ([`find_writable_codegraph_root`]).
//!
//! Detection is filesystem-only — a `.git` directory or file marks a
//! checkout's top level (see [`crate::directory::is_git_checkout_root`]) — so
//! it is cheap enough for time-boxed callers and needs no `git` binary.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::directory::{checkout_below_index_root, find_nearest_codegraph_root, real_path_lenient};
use crate::sync::git_hooks::git_output;

/// Absolute, symlink-resolved toplevel of the git working tree that `dir`
/// belongs to, or `None` when `dir` isn't inside a git repo (or git is
/// missing).
///
/// `git rev-parse --show-toplevel` returns the per-worktree root: the main
/// checkout and each linked worktree report their own distinct directory.
/// This spawns `git`; index-ownership checks use the filesystem-only
/// [`detect_worktree_index_mismatch`] instead.
pub fn git_worktree_root(dir: &Path) -> Option<PathBuf> {
    let out = git_output(&["rev-parse", "--show-toplevel"], dir)?;
    if out.is_empty() {
        None
    } else {
        Some(realpath(Path::new(&out)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeIndexMismatch {
    /// The git checkout the command was run from.
    pub worktree_root: PathBuf,
    /// The directory holding the (other checkout's) `.codegraph` index.
    pub index_root: PathBuf,
}

/// Detect when `start_path` lives in one git checkout but the resolved
/// CodeGraph index (`index_root`) belongs to a *different* checkout.
///
/// That is the case when a checkout's top level — a `.git` directory, or a
/// linked worktree's `.git` file — sits between `start_path` and `index_root`:
/// the walk up to the index left `start_path`'s own checkout.
///
/// Returns `None` — meaning "the index is this checkout's own" — when:
///   - `start_path` is `index_root` or a plain subdirectory of it (no `.git`
///     of its own on the way up), which keeps monorepo subpaths working,
///   - the only `.git` on the way is a submodule's, which the superproject's
///     index covers, or
///   - `index_root` isn't an ancestor of `start_path` at all.
pub fn detect_worktree_index_mismatch(
    start_path: &Path,
    index_root: &Path,
) -> Option<WorktreeIndexMismatch> {
    let checkout = checkout_below_index_root(start_path, index_root)?;
    Some(WorktreeIndexMismatch {
        worktree_root: checkout,
        index_root: real_path_lenient(index_root),
    })
}

/// Resolve the index a command that WRITES it may use, starting at
/// `start_path`.
///
/// `Ok(Some(root))` is the nearest index when it belongs to `start_path`'s own
/// checkout — including a worktree's own index, which the walk reaches before
/// it leaves the worktree. `Ok(None)` means there is no index at or above
/// `start_path`. `Err` means the nearest index belongs to a different checkout
/// (from a worktree nested in the main checkout, the main checkout's index):
/// the caller must leave it alone and say how to proceed
/// ([`worktree_write_refusal`]).
pub fn find_writable_codegraph_root(
    start_path: &Path,
) -> Result<Option<PathBuf>, WorktreeIndexMismatch> {
    let Some(root) = find_nearest_codegraph_root(start_path) else {
        return Ok(None);
    };
    match detect_worktree_index_mismatch(start_path, &root) {
        Some(mismatch) => Err(mismatch),
        None => Ok(Some(root)),
    }
}

/// One-line-per-fact warning describing a detected mismatch.
pub fn worktree_mismatch_warning(m: &WorktreeIndexMismatch) -> String {
    format!(
        "This CodeGraph index belongs to a different git checkout.\n\
        \x20 Running in: {}\n\
        \x20 Index from: {}\n\
        Results reflect that checkout's code (often a different branch), not this one — \
        symbols changed only here are missing. Run \"codegraph init\" in this checkout \
        for a checkout-local index.",
        m.worktree_root.display(),
        m.index_root.display()
    )
}

/// Compact, single-line variant for prefixing a tool's result. Read tools
/// return their answer inline, so the heads-up has to ride on the same payload
/// the agent is already reading — a multi-line block would bury the result.
pub fn worktree_mismatch_notice(m: &WorktreeIndexMismatch) -> String {
    format!(
        "CodeGraph results below come from a different git checkout's index ({}), \
        not where you're working ({}) — they may reflect another branch, \
        and symbols changed only here are missing. Run \"codegraph init\" here for a \
        checkout-local index.",
        m.index_root.display(),
        m.worktree_root.display()
    )
}

/// Why a command that writes the index left this one alone, and what to run
/// instead. Multi-line, for the CLI.
pub fn worktree_write_refusal(m: &WorktreeIndexMismatch) -> String {
    format!(
        "The nearest CodeGraph index belongs to a different git checkout, so it was left \
        untouched.\n\
        \x20 Running in: {here}\n\
        \x20 Index from: {index}\n\
        Run \"codegraph init {here}\" to give this checkout its own index (index and sync \
        then work here), or run the command from {index} to update that checkout's index.",
        here = m.worktree_root.display(),
        index = m.index_root.display()
    )
}

/// Why an MCP session does not watch or sync a borrowed index — reported as
/// the session's auto-sync-disabled reason.
pub fn worktree_auto_sync_reason(m: &WorktreeIndexMismatch) -> String {
    format!(
        "the index at {index} belongs to a different git checkout than this session \
        ({here}), so this session reads it but never syncs it; run \"codegraph init {here}\" \
        for a live, checkout-local index",
        here = m.worktree_root.display(),
        index = m.index_root.display()
    )
}

/// Resolve symlinks where possible so tmp/realpath quirks don't break equality.
/// (TS: `fs.realpathSync(path.resolve(p))`, falling back to `path.resolve(p)`.)
fn realpath(p: &Path) -> PathBuf {
    let absolute = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> WorktreeIndexMismatch {
        WorktreeIndexMismatch {
            worktree_root: PathBuf::from("/repo/wt"),
            index_root: PathBuf::from("/repo"),
        }
    }

    #[test]
    fn warning_contains_both_paths_and_the_fix() {
        let msg = worktree_mismatch_warning(&sample());
        assert!(msg.contains("/repo/wt"));
        assert!(msg.contains("Index from: /repo"));
        assert!(msg.contains("codegraph init"));
        assert!(msg.starts_with("This CodeGraph index belongs to a different git checkout.\n"));
    }

    #[test]
    fn notice_is_single_line_and_compact() {
        let msg = worktree_mismatch_notice(&sample());
        assert!(!msg.contains('\n'));
        assert!(msg.starts_with("CodeGraph results below come from a different git checkout"));
        assert!(msg.contains("codegraph init"));
    }

    #[test]
    fn write_refusal_names_both_checkouts_and_both_ways_forward() {
        let msg = worktree_write_refusal(&sample());
        assert!(msg.contains("left untouched"));
        assert!(msg.contains("Running in: /repo/wt"));
        assert!(msg.contains("Index from: /repo"));
        assert!(msg.contains("codegraph init /repo/wt"));
        assert!(msg.contains("run the command from /repo"));
        let reason = worktree_auto_sync_reason(&sample());
        assert!(!reason.contains('\n'));
        assert!(reason.contains("never syncs it"));
    }

    #[test]
    fn mismatch_serializes_camel_case() {
        let v = serde_json::to_value(sample()).unwrap();
        assert_eq!(v["worktreeRoot"], "/repo/wt");
        assert_eq!(v["indexRoot"], "/repo");
    }
}
