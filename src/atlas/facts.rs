//! Everything the atlas records about one project, gathered before any
//! atlas write so the write transaction stays short. Reads only: the
//! project's `.git` files, its manifests, and its index (opened so that
//! nothing is created in its `.codegraph/`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::git::{GitFacts, read_git_facts};
use super::index_stats::{IndexFacts, read_index_facts};
use super::kinds::ProjectStatus;
use super::manifest::{ManifestLink, scan_manifests};
use crate::db::CURRENT_SCHEMA_VERSION;
use crate::directory::is_initialized;

/// Knobs for [`ProjectFacts::gather`].
#[derive(Debug, Clone, Copy)]
pub struct GatherOptions {
    /// Budget for the exact node/edge counts (estimates past it).
    pub count_budget: Duration,
}

impl Default for GatherOptions {
    fn default() -> Self {
        Self {
            count_budget: Duration::from_millis(500),
        }
    }
}

/// One project's registration input.
#[derive(Debug, Clone)]
pub struct ProjectFacts {
    /// Canonical index root.
    pub root: PathBuf,
    pub name: String,
    pub git: GitFacts,
    pub index: IndexFacts,
    pub links: Vec<ManifestLink>,
    pub status: ProjectStatus,
}

impl ProjectFacts {
    /// Read the facts of the project indexed at `root`.
    pub fn gather(root: &Path, opts: GatherOptions) -> Self {
        let root = super::canonical_root(root);
        let git = read_git_facts(&root);
        let (status, index) = if !is_initialized(&root) {
            (ProjectStatus::Missing, IndexFacts::default())
        } else {
            match read_index_facts(&root, opts.count_budget) {
                Ok(index) => {
                    let current = index.schema.is_some_and(|v| v >= CURRENT_SCHEMA_VERSION);
                    let status = if current {
                        ProjectStatus::Ok
                    } else {
                        ProjectStatus::StaleSchema
                    };
                    (status, index)
                }
                Err(_) => (ProjectStatus::Unreadable, IndexFacts::default()),
            }
        };
        let manifests = if root.is_dir() {
            scan_manifests(&root, &index.manifests)
        } else {
            Default::default()
        };
        let name = display_name(&root, &git);
        Self {
            root,
            name,
            git,
            index,
            links: manifests.links,
            status,
        }
    }
}

/// The directory name; a linked worktree reads `repo@worktree`.
fn display_name(root: &Path, git: &GitFacts) -> String {
    let base = |p: &Path| {
        p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    };
    let own = base(root);
    match (&git.repo_root, git.checkout_root.as_deref()) {
        (Some(repo), Some(checkout)) if git.is_worktree && checkout == root => {
            format!("{}@{own}", base(repo))
        }
        _ => own,
    }
}
