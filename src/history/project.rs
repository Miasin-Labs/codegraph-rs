//! Project attribution: which repository a tool call worked in.
//!
//! Every row gets a project (not just shell rows), so `history show -p`
//! sees Read/Edit rows too. The project is the nearest ancestor holding a
//! `.git` entry (a directory, or a worktree's `.git` file) of the most
//! specific location the call names: an absolute file path, else the
//! directory a leading `cd` moved to, else the call's working directory.
//! Without a repository above it, the location itself is the project.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Resolves call locations to project roots, caching filesystem probes.
#[derive(Debug)]
pub struct ProjectResolver {
    /// Probe the filesystem for `.git`; off, the location itself is the project.
    probe: bool,
    home: Option<PathBuf>,
    roots: HashMap<PathBuf, Option<PathBuf>>,
}

impl Default for ProjectResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectResolver {
    /// A resolver that looks for `.git` on disk.
    pub fn new() -> Self {
        Self {
            probe: true,
            home: std::env::var_os("HOME").map(PathBuf::from),
            roots: HashMap::new(),
        }
    }

    /// A resolver that never touches the filesystem (deterministic tests).
    pub fn without_probe() -> Self {
        Self {
            probe: false,
            ..Self::new()
        }
    }

    /// The project for a call that ran in `cwd`, `cd`'d through `cds`
    /// before its real command, and touched `file`.
    pub(crate) fn resolve(
        &mut self,
        cwd: Option<&str>,
        cds: &[String],
        file: Option<&str>,
    ) -> Option<String> {
        let mut base = cwd.map(|c| self.expand(Path::new(c), None));
        for target in cds {
            let joined = self.expand(Path::new(target), base.as_deref());
            if joined.is_absolute() {
                base = Some(joined);
            }
        }
        if let Some(file) = file
            .map(Path::new)
            .filter(|f| f.is_absolute() || f.starts_with("~"))
        {
            let file = self.expand(file, None);
            if let Some(dir) = file.parent() {
                if let Some(root) = self.repo_root(dir) {
                    return Some(root.to_string_lossy().into_owned());
                }
                base = base.or_else(|| Some(dir.to_path_buf()));
            }
        }
        let base = base?;
        let project = self.repo_root(&base).unwrap_or(base);
        Some(project.to_string_lossy().into_owned())
    }

    /// Expand `~`, join a relative path onto `base`, fold `.`/`..` lexically.
    fn expand(&self, path: &Path, base: Option<&Path>) -> PathBuf {
        let path = match (path.strip_prefix("~"), &self.home) {
            (Ok(rest), Some(home)) => home.join(rest),
            _ => path.to_path_buf(),
        };
        let joined = match base {
            Some(base) if path.is_relative() => base.join(path),
            _ => path,
        };
        normalize(&joined)
    }

    /// Nearest ancestor of `dir` (inclusive) holding a `.git` entry.
    fn repo_root(&mut self, dir: &Path) -> Option<PathBuf> {
        if !self.probe {
            return None;
        }
        let mut visited = Vec::new();
        let mut found = None;
        for ancestor in dir.ancestors() {
            if let Some(cached) = self.roots.get(ancestor) {
                found = cached.clone();
                break;
            }
            visited.push(ancestor.to_path_buf());
            if ancestor.join(".git").exists() {
                found = Some(ancestor.to_path_buf());
                break;
            }
        }
        for dir in visited {
            self.roots.insert(dir, found.clone());
        }
        found
    }
}

/// Lexically resolve `.` and `..` (no symlink resolution, no disk access).
fn normalize(path: &Path) -> PathBuf {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cds(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn nearest_git_ancestor_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("work/repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        let repo_s = repo.to_string_lossy().into_owned();
        let mut r = ProjectResolver::new();

        // A file path decides, even when the call ran elsewhere.
        let file = repo.join("src/deep/lib.rs");
        assert_eq!(
            r.resolve(Some("/"), &[], Some(&file.to_string_lossy()))
                .as_deref(),
            Some(repo_s.as_str())
        );
        // A leading cd (relative, with `..`) decides for a shell call.
        let cwd = repo.join("src/deep").to_string_lossy().into_owned();
        assert_eq!(
            r.resolve(Some(&cwd), &cds(&["..", "."]), None).as_deref(),
            Some(repo_s.as_str())
        );
        // No repository above: the location itself.
        let outside = tmp.path().join("work").to_string_lossy().into_owned();
        assert_eq!(
            r.resolve(Some(&outside), &[], None).as_deref(),
            Some(outside.as_str())
        );
    }

    #[test]
    fn worktree_git_file_counts_as_a_root() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /elsewhere\n").unwrap();
        let mut r = ProjectResolver::new();
        let file = wt.join("src/main.rs").to_string_lossy().into_owned();
        assert_eq!(
            r.resolve(None, &[], Some(&file)).as_deref(),
            Some(wt.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn without_probe_uses_the_most_specific_location() {
        let mut r = ProjectResolver::without_probe();
        assert_eq!(
            r.resolve(Some("/a"), &cds(&["b/c", "../d"]), None)
                .as_deref(),
            Some("/a/b/d")
        );
        assert_eq!(
            r.resolve(Some("/a"), &cds(&["/x"]), None).as_deref(),
            Some("/x")
        );
        // A relative file path can't anchor anything; the cwd does.
        assert_eq!(
            r.resolve(Some("/a"), &[], Some("src/lib.rs")).as_deref(),
            Some("/a")
        );
        // An absolute file with no cwd: its directory.
        assert_eq!(
            r.resolve(None, &[], Some("/p/q/r.rs")).as_deref(),
            Some("/p/q")
        );
        assert_eq!(r.resolve(None, &[], None), None);
    }
}
