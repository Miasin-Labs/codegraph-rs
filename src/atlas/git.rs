//! Git facts about a project, read from `.git` files — never a `git`
//! process (registration runs after every `codegraph sync`, including the
//! ones git hooks start).
//!
//! The checkout holding a project is the nearest ancestor-or-self with a
//! `.git` entry: a directory (a repository's own working tree) or a
//! `gitdir:` file (a linked worktree, a submodule, a separate-git-dir
//! clone). A linked worktree's private git dir names the shared one in
//! `commondir`; refs and config live there.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::remote::normalize_remote;
use crate::directory::real_path_lenient;
use crate::utils::lexical_resolve;

/// Largest `HEAD`/ref/`commondir` file read.
const SMALL_FILE: u64 = 4 * 1024;
/// Largest `config` read.
const CONFIG_FILE: u64 = 1024 * 1024;
/// Largest `packed-refs` read.
const PACKED_REFS_FILE: u64 = 16 * 1024 * 1024;

/// What a project's git checkout says about it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFacts {
    /// Top level of the checkout holding the project.
    pub checkout_root: Option<PathBuf>,
    /// The repository's main checkout (worktrees fold into it), or the
    /// shared git dir of a bare repository or submodule.
    pub repo_root: Option<PathBuf>,
    /// `checkout_root` is a linked worktree.
    pub is_worktree: bool,
    /// Normalized `origin` (else first) remote, credentials stripped.
    pub remote: Option<String>,
    /// Checked-out branch (`None` when detached).
    pub branch: Option<String>,
    /// Commit `HEAD` resolves to, when a loose or packed ref names it.
    pub head_commit: Option<String>,
}

/// Read the git facts of the checkout holding `root` (all `None` outside git).
pub fn read_git_facts(root: &Path) -> GitFacts {
    let Some((checkout, dot_git)) = root.ancestors().find_map(|dir| {
        let dot_git = dir.join(".git");
        dot_git.exists().then(|| (dir.to_path_buf(), dot_git))
    }) else {
        return GitFacts::default();
    };
    let Some(dirs) = GitDirs::locate(&checkout, &dot_git) else {
        return GitFacts {
            checkout_root: Some(real_path_lenient(&checkout)),
            ..GitFacts::default()
        };
    };
    let (branch, head_commit) = dirs.head();
    GitFacts {
        checkout_root: Some(real_path_lenient(&checkout)),
        repo_root: Some(real_path_lenient(&dirs.repo_root())),
        is_worktree: dirs.is_worktree,
        remote: dirs.remote(),
        branch,
        head_commit,
    }
}

/// The per-checkout and shared git directories of one checkout.
struct GitDirs {
    /// `HEAD` lives here (per worktree).
    git_dir: PathBuf,
    /// Refs, `packed-refs` and `config` live here.
    common_dir: PathBuf,
    is_worktree: bool,
}

impl GitDirs {
    fn locate(checkout: &Path, dot_git: &Path) -> Option<Self> {
        if dot_git.is_dir() {
            return Some(Self {
                git_dir: dot_git.to_path_buf(),
                common_dir: dot_git.to_path_buf(),
                is_worktree: false,
            });
        }
        let text = read_small(dot_git, SMALL_FILE)?;
        let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
        if target.is_empty() {
            return None;
        }
        let git_dir = lexical_resolve(checkout, target);
        let commondir = read_small(&git_dir.join("commondir"), SMALL_FILE);
        // `<common>/worktrees/<name>`: a linked worktree even when its admin
        // dir (and so `commondir`) is gone.
        let admin_parent = git_dir
            .parent()
            .filter(|p| p.file_name() == Some(std::ffi::OsStr::new("worktrees")))
            .and_then(Path::parent);
        let common_dir = match commondir.as_deref().map(str::trim) {
            Some(c) if !c.is_empty() => lexical_resolve(&git_dir, c),
            _ => admin_parent.map_or_else(|| git_dir.clone(), Path::to_path_buf),
        };
        Some(Self {
            is_worktree: commondir.is_some() || admin_parent.is_some(),
            git_dir,
            common_dir,
        })
    }

    /// Main checkout of a non-bare repository (the parent of its `.git`),
    /// else the shared git dir itself.
    fn repo_root(&self) -> PathBuf {
        if self.common_dir.file_name() == Some(std::ffi::OsStr::new(".git")) {
            if let Some(parent) = self.common_dir.parent() {
                return parent.to_path_buf();
            }
        }
        self.common_dir.clone()
    }

    /// `(branch, commit)` of `HEAD`.
    fn head(&self) -> (Option<String>, Option<String>) {
        let Some(head) = read_small(&self.git_dir.join("HEAD"), SMALL_FILE) else {
            return (None, None);
        };
        let head = head.trim();
        match head.strip_prefix("ref:") {
            Some(reference) => {
                let reference = reference.trim();
                let branch = reference
                    .strip_prefix("refs/heads/")
                    .unwrap_or(reference)
                    .to_owned();
                (Some(branch), self.resolve_ref(reference))
            }
            None => (None, is_object_id(head).then(|| head.to_owned())),
        }
    }

    /// Commit a ref names: its loose file, else its `packed-refs` line.
    fn resolve_ref(&self, reference: &str) -> Option<String> {
        if reference.contains("..") {
            return None;
        }
        for dir in [&self.git_dir, &self.common_dir] {
            if let Some(text) = read_small(&dir.join(reference), SMALL_FILE) {
                let id = text.trim();
                return is_object_id(id).then(|| id.to_owned());
            }
        }
        let packed = read_small(&self.common_dir.join("packed-refs"), PACKED_REFS_FILE)?;
        packed.lines().find_map(|line| {
            let (id, name) = line.split_once(' ')?;
            (name.trim() == reference && is_object_id(id)).then(|| id.to_owned())
        })
    }

    /// The `origin` remote's URL (else the first remote's), normalized.
    fn remote(&self) -> Option<String> {
        let config = read_small(&self.common_dir.join("config"), CONFIG_FILE)?;
        let remotes = config_remote_urls(&config);
        let url = remotes
            .iter()
            .find(|(name, _)| name == "origin")
            .or_else(|| remotes.first())
            .map(|(_, url)| url.as_str())?;
        normalize_remote(url)
    }
}

/// `(remote name, url)` for each `[remote "name"]` section's `url`.
fn config_remote_urls(config: &str) -> Vec<(String, String)> {
    let mut remotes = Vec::new();
    let mut section: Option<String> = None;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line
                .trim_start_matches('[')
                .split_once(']')
                .and_then(|(header, _)| {
                    let (kind, name) = header.split_once(char::is_whitespace)?;
                    kind.eq_ignore_ascii_case("remote")
                        .then(|| name.trim().trim_matches('"').to_owned())
                });
            continue;
        }
        let Some(name) = &section else { continue };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("url") && !remotes.iter().any(|(n, _)| n == name) {
            let value = value.trim().trim_matches('"').to_owned();
            remotes.push((name.clone(), value));
        }
    }
    remotes
}

fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Read at most `limit` bytes of `path` as UTF-8 (lossy).
fn read_small(path: &Path, limit: u64) -> Option<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(limit)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const SHA_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const SHA_B: &str = "89abcdef0123456789abcdef0123456789abcdef";

    /// A repository at `root` on branch `main` (loose ref `SHA_A`) with the
    /// given `origin` URL.
    pub(crate) fn fake_repo(root: &Path, origin: &str) {
        let git = root.join(".git");
        fs::create_dir_all(git.join("refs/heads")).unwrap();
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(git.join("refs/heads/main"), format!("{SHA_A}\n")).unwrap();
        fs::write(
            git.join("config"),
            format!(
                "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.com/up/x.git\n\
                 [remote \"origin\"]\n\turl = {origin}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"
            ),
        )
        .unwrap();
    }

    /// A linked worktree of `main` at `worktree`, on `branch` (packed ref `SHA_B`).
    pub(crate) fn fake_worktree(main: &Path, worktree: &Path, name: &str, branch: &str) {
        let admin = main.join(".git/worktrees").join(name);
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        fs::write(admin.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
        fs::write(
            main.join(".git/packed-refs"),
            format!("# pack-refs with: peeled fully-peeled sorted\n{SHA_B} refs/heads/{branch}\n"),
        )
        .unwrap();
        fs::create_dir_all(worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
    }

    #[test]
    fn main_checkout_reads_branch_commit_and_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fake_repo(
            &repo,
            "https://bob:hunter2-secret@github.com/Acme/Widget.git",
        );
        let facts = read_git_facts(&repo.join("crates/inner"));
        let real = fs::canonicalize(&repo).unwrap();
        assert_eq!(facts.checkout_root.as_deref(), Some(real.as_path()));
        assert_eq!(facts.repo_root.as_deref(), Some(real.as_path()));
        assert!(!facts.is_worktree);
        assert_eq!(facts.remote.as_deref(), Some("github.com/acme/widget"));
        assert_eq!(facts.branch.as_deref(), Some("main"));
        assert_eq!(facts.head_commit.as_deref(), Some(SHA_A));
    }

    #[test]
    fn worktree_folds_into_its_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        fake_repo(&main, "git@github.com:acme/widget.git");
        let worktree = main.join(".claude/worktrees/wt");
        fake_worktree(&main, &worktree, "wt", "feat/x");
        let facts = read_git_facts(&worktree);
        assert!(facts.is_worktree);
        assert_eq!(
            facts.checkout_root,
            Some(fs::canonicalize(&worktree).unwrap())
        );
        assert_eq!(facts.repo_root, Some(fs::canonicalize(&main).unwrap()));
        assert_eq!(facts.remote.as_deref(), Some("github.com/acme/widget"));
        assert_eq!(facts.branch.as_deref(), Some("feat/x"));
        assert_eq!(facts.head_commit.as_deref(), Some(SHA_B));
    }

    #[test]
    fn detached_head_and_no_git_at_all() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fake_repo(&repo, "/srv/git/widget.git");
        fs::write(repo.join(".git/HEAD"), format!("{SHA_B}\n")).unwrap();
        let facts = read_git_facts(&repo);
        assert_eq!(facts.branch, None);
        assert_eq!(facts.head_commit.as_deref(), Some(SHA_B));
        assert_eq!(facts.remote.as_deref(), Some("/srv/git/widget"));

        let plain = tmp.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        // The temp dir itself may sit inside a checkout on some machines;
        // only assert when it does not.
        if !tmp.path().ancestors().any(|d| d.join(".git").exists()) {
            assert_eq!(read_git_facts(&plain), GitFacts::default());
        }
    }
}
