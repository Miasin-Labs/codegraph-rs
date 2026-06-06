//! Git Sync Hooks
//!
//! When the live file watcher is disabled (e.g. on WSL2 `/mnt/*` drives,
//! see watch_policy.rs), the CodeGraph index would otherwise go stale until
//! the user runs `codegraph sync` by hand. As an opt-in alternative, we can
//! install git hooks that refresh the index after the operations that change
//! files on disk: commit, merge (covers `git pull`), and checkout.
//!
//! The hooks run `codegraph sync` in the background so they never block git,
//! and are guarded by `command -v codegraph` so they no-op cleanly when the
//! CLI isn't on PATH. Our snippet is delimited by marker comments so install
//! is idempotent and removal preserves any user-authored hook content.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{fmt, fs};

use serde::{Deserialize, Serialize};

pub(crate) const MARKER_BEGIN: &str = "# >>> codegraph sync hook >>>";
pub(crate) const MARKER_END: &str = "# <<< codegraph sync hook <<<";

/// A git hook CodeGraph can install a sync snippet into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GitHookName {
    #[serde(rename = "post-commit")]
    PostCommit,
    #[serde(rename = "post-merge")]
    PostMerge,
    #[serde(rename = "post-checkout")]
    PostCheckout,
}

impl GitHookName {
    pub fn as_str(&self) -> &'static str {
        match self {
            GitHookName::PostCommit => "post-commit",
            GitHookName::PostMerge => "post-merge",
            GitHookName::PostCheckout => "post-checkout",
        }
    }
}

impl fmt::Display for GitHookName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Hooks installed by default: commit, merge (git pull), and checkout.
pub const DEFAULT_SYNC_HOOKS: [GitHookName; 3] = [
    GitHookName::PostCommit,
    GitHookName::PostMerge,
    GitHookName::PostCheckout,
];

/// Result of a hook install/remove operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHookResult {
    /// Hook names that were created or updated.
    pub installed: Vec<GitHookName>,
    /// Resolved hooks directory, or `None` when not a git repo.
    pub hooks_dir: Option<PathBuf>,
    /// Reason nothing happened (e.g. not a git repository).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// Run `git <args>` in `cwd` and return trimmed stdout, or `None` on any
/// failure (git missing, non-zero exit, unreadable cwd). Mirrors the TS
/// `execFileSync(..., { stdio: ['ignore','pipe','ignore'], windowsHide: true })`.
pub(crate) fn git_output(args: &[&str], cwd: &Path) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether `project_root` is inside a git working tree. Returns false if git
/// isn't installed or the path isn't a repo.
pub fn is_git_repo(project_root: &Path) -> bool {
    git_output(&["rev-parse", "--is-inside-work-tree"], project_root).as_deref() == Some("true")
}

/// Resolve the git hooks directory for a project, honoring `core.hooksPath`
/// and git worktrees. Returns an absolute path, or `None` when not a repo.
fn git_hooks_dir(project_root: &Path) -> Option<PathBuf> {
    let out = git_output(&["rev-parse", "--git-path", "hooks"], project_root)?;
    if out.is_empty() {
        return None;
    }
    let p = Path::new(&out);
    if p.is_absolute() {
        Some(p.to_path_buf())
    } else {
        Some(project_root.join(p))
    }
}

/// The shell snippet (between markers) injected into each hook.
/// Byte-identical to the TS implementation so existing installed hooks match.
fn marker_block() -> String {
    [
        MARKER_BEGIN,
        "# Keeps the CodeGraph index fresh while the live file watcher is off",
        "# (e.g. WSL2 /mnt drives). Runs in the background so it never blocks git.",
        "# Managed by codegraph; remove with `codegraph uninit` or delete this block.",
        "if command -v codegraph >/dev/null 2>&1; then",
        "  ( codegraph sync >/dev/null 2>&1 & ) >/dev/null 2>&1",
        "fi",
        MARKER_END,
    ]
    .join("\n")
}

/// Remove our marker block (and the marker lines) from hook content.
fn strip_marker_block(content: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in content.split('\n') {
        let trimmed = line.trim();
        if trimmed == MARKER_BEGIN {
            in_block = true;
            continue;
        }
        if trimmed == MARKER_END {
            in_block = false;
            continue;
        }
        if !in_block {
            kept.push(line);
        }
    }
    kept.join("\n")
}

/// Whether a hook body is just a shebang / blank lines (i.e. only ever ours).
fn is_effectively_empty(content: &str) -> bool {
    content
        .split('\n')
        .map(|l| l.trim())
        .all(|l| l.is_empty() || l.starts_with('#'))
}

fn chmod_executable(file: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(file, fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    {
        // chmod is a no-op / unsupported on some platforms (e.g. Windows)
        let _ = file;
    }
}

/// Install (or update) the CodeGraph sync hooks in a git repository.
/// Idempotent: re-running replaces our marker block rather than duplicating
/// it, and any user-authored hook content is preserved.
pub fn install_git_sync_hook(project_root: &Path, hooks: &[GitHookName]) -> GitHookResult {
    let Some(hooks_dir) = git_hooks_dir(project_root) else {
        return GitHookResult {
            installed: vec![],
            hooks_dir: None,
            skipped: Some("not a git repository".to_string()),
        };
    };

    if fs::create_dir_all(&hooks_dir).is_err() {
        return GitHookResult {
            installed: vec![],
            hooks_dir: Some(hooks_dir),
            skipped: Some("could not access the git hooks directory".to_string()),
        };
    }

    let block = marker_block();
    let mut installed: Vec<GitHookName> = Vec::new();

    for &hook in hooks {
        let file = hooks_dir.join(hook.as_str());
        let content = if file.exists() {
            // Strip any prior block, then re-append the current one.
            let original = fs::read_to_string(&file).unwrap_or_default();
            let stripped = strip_marker_block(&original);
            let base = stripped.trim_end();
            if !base.is_empty() {
                format!("{base}\n\n{block}\n")
            } else {
                format!("#!/bin/sh\n{block}\n")
            }
        } else {
            format!("#!/bin/sh\n{block}\n")
        };

        if fs::write(&file, content).is_err() {
            continue;
        }
        chmod_executable(&file);
        installed.push(hook);
    }

    GitHookResult {
        installed,
        hooks_dir: Some(hooks_dir),
        skipped: None,
    }
}

/// Remove the CodeGraph sync hooks. Strips only our marker block; deletes the
/// hook file entirely when nothing but a shebang remains, otherwise rewrites
/// the user's content untouched.
pub fn remove_git_sync_hook(project_root: &Path, hooks: &[GitHookName]) -> GitHookResult {
    let Some(hooks_dir) = git_hooks_dir(project_root) else {
        return GitHookResult {
            installed: vec![],
            hooks_dir: None,
            skipped: Some("not a git repository".to_string()),
        };
    };

    let mut removed: Vec<GitHookName> = Vec::new();

    for &hook in hooks {
        let file = hooks_dir.join(hook.as_str());
        if !file.exists() {
            continue;
        }

        let original = fs::read_to_string(&file).unwrap_or_default();
        if !original.contains(MARKER_BEGIN) {
            continue;
        }

        let stripped = strip_marker_block(&original);
        if is_effectively_empty(&stripped) {
            let _ = fs::remove_file(&file);
        } else {
            let _ = fs::write(&file, format!("{}\n", stripped.trim_end()));
            chmod_executable(&file);
        }
        removed.push(hook);
    }

    GitHookResult {
        installed: removed,
        hooks_dir: Some(hooks_dir),
        skipped: None,
    }
}

/// Whether any CodeGraph sync hook is currently installed.
pub fn is_sync_hook_installed(project_root: &Path, hooks: &[GitHookName]) -> bool {
    let Some(hooks_dir) = git_hooks_dir(project_root) else {
        return false;
    };
    hooks.iter().any(|hook| {
        let file = hooks_dir.join(hook.as_str());
        file.exists()
            && fs::read_to_string(&file)
                .map(|c| c.contains(MARKER_BEGIN))
                .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_block_bytes_match_the_ts_snippet() {
        // Keep hook script bytes identical to the TS implementation so
        // previously-installed hooks round-trip through install/uninstall.
        let expected = "# >>> codegraph sync hook >>>\n\
# Keeps the CodeGraph index fresh while the live file watcher is off\n\
# (e.g. WSL2 /mnt drives). Runs in the background so it never blocks git.\n\
# Managed by codegraph; remove with `codegraph uninit` or delete this block.\n\
if command -v codegraph >/dev/null 2>&1; then\n\
\x20\x20( codegraph sync >/dev/null 2>&1 & ) >/dev/null 2>&1\n\
fi\n\
# <<< codegraph sync hook <<<";
        assert_eq!(marker_block(), expected);
    }

    #[test]
    fn strip_marker_block_preserves_user_content() {
        let content = format!("#!/bin/sh\necho \"user\"\n\n{}\n", marker_block());
        let stripped = strip_marker_block(&content);
        assert!(stripped.contains("echo \"user\""));
        assert!(!stripped.contains("codegraph sync"));
        assert!(!stripped.contains(MARKER_BEGIN));
        assert!(!stripped.contains(MARKER_END));
    }

    #[test]
    fn is_effectively_empty_treats_shebang_and_comments_as_empty() {
        assert!(is_effectively_empty("#!/bin/sh\n\n# comment\n"));
        assert!(is_effectively_empty(""));
        assert!(!is_effectively_empty("#!/bin/sh\necho hi\n"));
    }
}
