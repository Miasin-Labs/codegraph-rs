//! Rows the atlas hands out.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::kinds::{LinkKind, ProjectStatus};

/// Files of one language in a project's index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanguageCount {
    pub language: String,
    pub files: u64,
}

/// One registered project: an indexed checkout (or an indexed directory
/// inside one).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: i64,
    /// Canonical index root — the project's identity.
    pub root: PathBuf,
    pub name: String,
    /// Top level of the git checkout holding `root` (`None` outside git).
    pub checkout_root: Option<PathBuf>,
    /// The repository's main checkout (linked worktrees fold into it), or
    /// its git dir for a bare repository or submodule.
    pub repo_root: Option<PathBuf>,
    /// `checkout_root` is a linked worktree (`git worktree add`).
    pub is_worktree: bool,
    /// Normalized remote (`host/owner/repo`), never carrying credentials.
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub head_commit: Option<String>,
    /// Indexed files per language, most files first.
    pub languages: Vec<LanguageCount>,
    pub file_count: Option<u64>,
    pub node_count: Option<u64>,
    pub edge_count: Option<u64>,
    /// `false` when node/edge counts are estimates (a large index whose
    /// exact count would have outlasted the registration budget).
    pub counts_exact: bool,
    /// Bytes on disk of the project's index directory.
    pub index_bytes: Option<u64>,
    pub index_schema: Option<u32>,
    pub extraction_version: Option<u32>,
    pub engine_version: Option<String>,
    /// Newest `files.indexed_at` in the index (epoch ms).
    pub last_indexed_ms: Option<i64>,
    /// When the atlas last read this project (epoch ms).
    pub last_seen_ms: i64,
    pub registered_ms: i64,
    pub status: ProjectStatus,
}

impl Project {
    /// Whether this project is a whole checkout (not a directory inside one).
    pub fn is_checkout(&self) -> bool {
        self.checkout_root.as_deref() == Some(self.root.as_path())
    }

    /// Worktree/clone grouping key: the main checkout, else the remote.
    pub fn repo_key(&self) -> Option<String> {
        self.repo_root
            .as_ref()
            .map(|root| root.to_string_lossy().into_owned())
            .or_else(|| self.remote.clone())
    }
}

/// Where a manifest link was read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    /// The manifest file (absolute).
    pub path: PathBuf,
    /// 1-based line of the entry.
    pub line: Option<u32>,
}

/// One edge of the project graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLink {
    pub id: i64,
    pub from_project: i64,
    /// Nearest registered project at or above `to_path` (`None`: the
    /// target isn't inside any registered project).
    pub to_project: Option<i64>,
    /// The directory the link points at.
    pub to_path: PathBuf,
    pub kind: LinkKind,
    pub evidence: Option<Evidence>,
    /// The dependency / member / module name, or the shared remote.
    pub detail: Option<String>,
}

impl ProjectLink {
    /// Both ends are the same project (a workspace's own members, a path
    /// dep between crates of one repository).
    pub fn is_internal(&self) -> bool {
        self.to_project == Some(self.from_project)
    }

    /// The link crosses into another registered project.
    pub fn is_cross_project(&self) -> bool {
        self.to_project.is_some_and(|to| to != self.from_project)
    }

    /// `manifest:line` for display.
    pub fn evidence_label(&self, relative_to: Option<&Path>) -> Option<String> {
        let evidence = self.evidence.as_ref()?;
        let path = relative_to
            .and_then(|base| evidence.path.strip_prefix(base).ok())
            .unwrap_or(&evidence.path);
        Some(match evidence.line {
            Some(line) => format!("{}:{line}", path.display()),
            None => path.display().to_string(),
        })
    }
}
