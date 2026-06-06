//! Git Worktree Awareness
//!
//! A CodeGraph index lives in a `.codegraph/` directory and is resolved by
//! walking up parent directories to the nearest one (see
//! `findNearestCodeGraphRoot`). That walk is unaware of git worktrees: when a
//! worktree is created *inside* the main checkout (e.g. some tools place them
//! under `.gitignore`d paths like `.claude/worktrees/<name>/`), a command run
//! from the worktree walks up and silently resolves the MAIN checkout's index.
//!
//! Every query then returns results from the main tree's code — usually a
//! different branch — rather than the worktree the user is actually editing.
//! Symbols added or changed only in the worktree are invisible. This module
//! detects that "borrowed index" situation so callers can warn about it.
//!
//! Detection is best-effort: when git is unavailable or the path isn't a repo,
//! it reports "no mismatch" and callers carry on unchanged.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::sync::git_hooks::git_output;

/// Absolute, symlink-resolved toplevel of the git working tree that `dir`
/// belongs to, or `None` when `dir` isn't inside a git repo (or git is
/// missing).
///
/// `git rev-parse --show-toplevel` returns the per-worktree root: the main
/// checkout and each linked worktree report their own distinct directory,
/// which is exactly the distinction this module relies on.
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
    /// The git working tree the command was run from.
    pub worktree_root: PathBuf,
    /// The (different) working tree whose `.codegraph` index is being used.
    pub index_root: PathBuf,
}

/// Detect when `start_path` lives in one git working tree but the resolved
/// CodeGraph index (`index_root`) belongs to a *different* working tree.
///
/// Returns `None` — meaning "nothing to warn about" — when:
///   - `start_path` isn't in a git repo (or git is unavailable),
///   - the index already lives in `start_path`'s own working tree, or
///   - `index_root` isn't itself a working-tree root (an unrelated parent dir
///     that merely happens to contain a `.codegraph/`), which keeps non-git
///     and monorepo-subdir layouts from producing false warnings.
pub fn detect_worktree_index_mismatch(
    start_path: &Path,
    index_root: &Path,
) -> Option<WorktreeIndexMismatch> {
    let worktree_root = git_worktree_root(start_path)?;

    let resolved_index_root = realpath(index_root);
    if worktree_root == resolved_index_root {
        return None;
    }

    // Only flag it when the index root is itself a real working-tree root. This
    // distinguishes "borrowed another worktree's index" from "index sits in a
    // plain ancestor directory", and avoids warning outside git entirely.
    if git_worktree_root(&resolved_index_root).as_deref() != Some(resolved_index_root.as_path()) {
        return None;
    }

    Some(WorktreeIndexMismatch {
        worktree_root,
        index_root: resolved_index_root,
    })
}

/// One-line-per-fact warning describing a detected mismatch.
pub fn worktree_mismatch_warning(m: &WorktreeIndexMismatch) -> String {
    format!(
        "This CodeGraph index belongs to a different git working tree.\n\
        \x20 Running in: {}\n\
        \x20 Index from: {}\n\
        Results reflect that tree's code (often a different branch), not this worktree — \
        symbols changed only here are missing. Run \"codegraph init -i\" in this worktree \
        for a worktree-local index.",
        m.worktree_root.display(),
        m.index_root.display()
    )
}

/// Compact, single-line variant for prefixing a tool's result. Read tools
/// return their answer inline, so the heads-up has to ride on the same payload
/// the agent is already reading — a multi-line block would bury the result.
pub fn worktree_mismatch_notice(m: &WorktreeIndexMismatch) -> String {
    format!(
        "⚠ CodeGraph results below come from a different git worktree ({}), \
        not where you're working ({}) — they may reflect another branch, \
        and symbols changed only here are missing. Run \"codegraph init -i\" here for a \
        worktree-local index.",
        m.index_root.display(),
        m.worktree_root.display()
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

    #[test]
    fn warning_contains_both_paths_and_the_fix() {
        let m = WorktreeIndexMismatch {
            worktree_root: PathBuf::from("/repo/wt"),
            index_root: PathBuf::from("/repo"),
        };
        let msg = worktree_mismatch_warning(&m);
        assert!(msg.contains("/repo/wt"));
        assert!(msg.contains("Index from: /repo"));
        assert!(msg.contains("codegraph init"));
        assert!(msg.starts_with("This CodeGraph index belongs to a different git working tree.\n"));
    }

    #[test]
    fn notice_is_single_line_and_compact() {
        let m = WorktreeIndexMismatch {
            worktree_root: PathBuf::from("/repo/wt"),
            index_root: PathBuf::from("/repo"),
        };
        let msg = worktree_mismatch_notice(&m);
        assert!(!msg.contains('\n'));
        assert!(msg.starts_with("⚠ CodeGraph results below come from a different git worktree"));
        assert!(msg.contains("codegraph init -i"));
    }

    #[test]
    fn mismatch_serializes_camel_case() {
        let m = WorktreeIndexMismatch {
            worktree_root: PathBuf::from("/repo/wt"),
            index_root: PathBuf::from("/repo"),
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["worktreeRoot"], "/repo/wt");
        assert_eq!(v["indexRoot"], "/repo");
    }
}
