//! Which history repositories hold an atlas project's sessions.
//!
//! History keys a repository by its main checkout — a linked worktree's
//! sessions fold into it ([`crate::history::repo`]) — while the atlas has
//! one row per checkout (`checkout_root`) and groups the checkouts of one
//! repository by `repo_root`. A project's scope is every history
//! repository recorded at its repository's main checkout, at its own
//! checkout, or at any other registered checkout of the same repository
//! (a worktree whose sessions history could not fold, e.g. one with a
//! relative `gitdir`). So one history repository maps to every atlas
//! checkout of that repository, and each checkout sees all of them.
//!
//! A project nested inside its checkout (an index below the git root)
//! narrows to its repo-relative path prefix: history paths are relative to
//! the checkout.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::repo_id;
use crate::atlas::{Atlas, Project};
use crate::history::store::HistoryError;

/// Where an atlas project's agent activity lives in the history store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryScope {
    /// `(history repository id, its root)`, the main checkout's first.
    pub(crate) repos: Vec<(i64, PathBuf)>,
    /// Repo-relative prefix (`sub/dir/`) of a project nested in its
    /// checkout; empty for a whole checkout.
    pub(crate) prefix: String,
}

impl HistoryScope {
    /// The history repositories `project` maps to; `None` outside git (no
    /// history is kept there) or when no session was recorded in any.
    pub fn resolve(
        conn: &Connection,
        atlas: &Atlas,
        project: &Project,
    ) -> Result<Option<Self>, HistoryError> {
        let Some(checkout) = &project.checkout_root else {
            return Ok(None);
        };
        let mut roots: Vec<PathBuf> = Vec::new();
        // A bare repository's or submodule's `repo_root` is a git dir, never
        // a history key; looking it up simply finds nothing.
        roots.extend(project.repo_root.clone());
        roots.push(checkout.clone());
        if project.repo_root.is_some() {
            for other in atlas.worktrees_of(project)? {
                roots.extend(other.checkout_root);
            }
        }
        let mut repos: Vec<(i64, PathBuf)> = Vec::new();
        for root in roots {
            if repos.iter().any(|(_, r)| *r == root) {
                continue;
            }
            if let Some(id) = repo_id(conn, &root)? {
                if !repos.iter().any(|(known, _)| *known == id) {
                    repos.push((id, root));
                }
            }
        }
        if repos.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            repos,
            prefix: nested_prefix(&project.root, checkout),
        }))
    }

    /// History repository ids.
    pub(crate) fn ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.repos.iter().map(|(id, _)| *id)
    }

    /// Roots of the history repositories, the main checkout's first.
    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        self.repos.iter().map(|(_, root)| root.as_path())
    }

    /// The repo-relative prefix of a nested project (`""`: whole checkout).
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Both scopes read (some of) the same history repository.
    pub(crate) fn overlaps(&self, other: &Self) -> bool {
        self.ids().any(|id| other.ids().any(|o| o == id))
    }
}

/// `root` relative to `checkout` as a path prefix (`a/b/`), or `""`.
fn nested_prefix(root: &Path, checkout: &Path) -> String {
    match root.strip_prefix(checkout) {
        Ok(rel) if !rel.as_os_str().is_empty() => {
            format!("{}/", rel.to_string_lossy().replace('\\', "/"))
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_projects_narrow_to_their_prefix() {
        let checkout = Path::new("/r/app");
        assert_eq!(nested_prefix(Path::new("/r/app"), checkout), "");
        assert_eq!(
            nested_prefix(Path::new("/r/app/tools/gen"), checkout),
            "tools/gen/"
        );
        assert_eq!(nested_prefix(Path::new("/elsewhere"), checkout), "");
    }
}
