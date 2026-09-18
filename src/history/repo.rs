//! Repository attribution for the cross-session memory: which git repository
//! a file belongs to, and its path relative to that repository.
//!
//! Unlike [`super::ProjectResolver`] (which names the nearest `.git` of a
//! call), this folds git *worktrees* into their main repository — an agent
//! working in `repo/.claude/worktrees/agent-x/src/a.rs` touched `src/a.rs` of
//! `repo` — so every session in the same repository shares one memory.
//! Paths outside any repository (or in the home directory itself) have no
//! repository and are not remembered.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Worktree directories inside a checkout whose contents mirror the
/// repository root (`.claude/worktrees/<name>/…`, `.worktrees/<name>/…`).
const WORKTREE_DIRS: &[&str] = &[".claude/worktrees/", ".worktrees/"];

/// Resolves absolute paths to `(canonical repo root, repo-relative path)`,
/// caching `.git` probes.
#[derive(Debug, Default)]
pub(crate) struct RepoLocator {
    home: Option<PathBuf>,
    /// Directory → (checkout root, canonical repo root), or `None` outside a repo.
    roots: HashMap<PathBuf, Option<(PathBuf, PathBuf)>>,
}

impl RepoLocator {
    pub(crate) fn new() -> Self {
        Self {
            home: std::env::var_os("HOME").map(PathBuf::from),
            roots: HashMap::new(),
        }
    }

    /// Canonical repository root containing `dir` (a directory).
    pub(crate) fn repo_of_dir(&mut self, dir: &Path) -> Option<PathBuf> {
        self.checkout(dir).map(|(_, canonical)| canonical)
    }

    /// `(canonical root, repo-relative path)` of an absolute file path.
    pub(crate) fn locate(&mut self, file: &Path) -> Option<(PathBuf, String)> {
        let file = normalize(file);
        if !file.is_absolute() {
            return None;
        }
        let dir = file.parent()?;
        let (checkout, canonical) = self.checkout(dir)?;
        let rel = file.strip_prefix(&checkout).ok()?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        let rel = strip_worktree_prefix(&rel);
        if rel.is_empty() || rel == ".git" || rel.starts_with(".git/") {
            return None;
        }
        Some((canonical, rel.to_owned()))
    }

    /// The checkout root (nearest ancestor with `.git`) of `dir` and the
    /// canonical repository it belongs to.
    fn checkout(&mut self, dir: &Path) -> Option<(PathBuf, PathBuf)> {
        let mut visited = Vec::new();
        let mut found = None;
        for ancestor in dir.ancestors() {
            if let Some(cached) = self.roots.get(ancestor) {
                found = cached.clone();
                break;
            }
            visited.push(ancestor.to_path_buf());
            let git = ancestor.join(".git");
            if git.is_dir() {
                found = Some((ancestor.to_path_buf(), ancestor.to_path_buf()));
                break;
            }
            if git.is_file() {
                let canonical = worktree_main(&git).unwrap_or_else(|| ancestor.to_path_buf());
                found = Some((ancestor.to_path_buf(), canonical));
                break;
            }
        }
        if let Some((_, canonical)) = &found {
            if Some(canonical) == self.home.as_ref() || canonical.parent().is_none() {
                found = None;
            }
        }
        for dir in visited {
            self.roots.insert(dir, found.clone());
        }
        found
    }
}

/// The main checkout of a linked worktree, from its `.git` file
/// (`gitdir: /repo/.git/worktrees/<name>`).
fn worktree_main(git_file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(git_file).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let at = gitdir.find("/.git/worktrees/")?;
    Some(PathBuf::from(&gitdir[..at]))
}

/// `".claude/worktrees/agent-x/src/a.rs"` → `"src/a.rs"`: a worktree whose
/// directory is gone resolves to its parent checkout lexically.
fn strip_worktree_prefix(rel: &str) -> &str {
    for dir in WORKTREE_DIRS {
        if let Some(rest) = rel.strip_prefix(dir) {
            if let Some((_, inner)) = rest.split_once('/') {
                return inner;
            }
        }
    }
    rel
}

/// Lexically resolve `.` and `..` (no symlinks, no disk access).
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Absolute form of `path` (expanding `~`, joining a relative path onto `cwd`).
pub(crate) fn absolutize(path: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME")?).join(rest),
        None => PathBuf::from(path),
    };
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        cwd?.join(expanded)
    };
    Some(normalize(&joined))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktrees_fold_into_their_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("repo");
        std::fs::create_dir_all(main.join(".git/worktrees/agent-x")).unwrap();
        let wt = main.join(".claude/worktrees/agent-x");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}/.git/worktrees/agent-x\n", main.display()),
        )
        .unwrap();
        let mut loc = RepoLocator::new();
        assert_eq!(
            loc.locate(&wt.join("src/lib.rs")),
            Some((main.clone(), "src/lib.rs".to_owned()))
        );
        // A worktree that no longer exists resolves lexically.
        assert_eq!(
            loc.locate(&main.join(".claude/worktrees/agent-gone/src/x.rs")),
            Some((main.clone(), "src/x.rs".to_owned()))
        );
        assert_eq!(
            loc.locate(&main.join("src/./deep/../a.rs")),
            Some((main.clone(), "src/a.rs".to_owned()))
        );
        assert_eq!(loc.locate(&main.join(".git/config")), None);
        assert_eq!(loc.repo_of_dir(&wt), Some(main));
    }

    #[test]
    fn paths_outside_a_repo_have_none() {
        let tmp = tempfile::tempdir().unwrap();
        let mut loc = RepoLocator::new();
        assert_eq!(loc.locate(&tmp.path().join("loose.txt")), None);
        assert_eq!(loc.locate(Path::new("relative/a.rs")), None);
    }

    #[test]
    fn absolutize_joins_and_normalizes() {
        assert_eq!(
            absolutize("src/../lib.rs", Some(Path::new("/r"))),
            Some(PathBuf::from("/r/lib.rs"))
        );
        assert_eq!(absolutize("x.rs", None), None);
        assert_eq!(absolutize("/a/./b", None), Some(PathBuf::from("/a/b")));
    }
}
