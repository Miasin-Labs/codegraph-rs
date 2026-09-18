//! The atlas: a small, machine-wide graph of indexed projects, the git
//! checkouts they are, and the local links between them.
//!
//! Every project keeps its own index (`<root>/.codegraph/codegraph.db`, the
//! write-local shard). The atlas (`atlas.db` in
//! [`crate::directory::codegraph_home`]) only records *about* them, so no
//! watcher, sync or schema migration ever waits on one global writer:
//!
//! * `projects` — one row per indexed checkout, keyed by its canonical root
//!   path (the join key shared with `deps/registry.db`): display name, git
//!   facts read straight from `.git` files (normalized remote with
//!   credentials stripped, branch, head commit, the repository's main
//!   checkout), index stats read from the project's own index (languages,
//!   file/node/edge counts, size, schema and extraction versions, last
//!   indexed), and a status (`ok`, `missing`, `stale-schema`,
//!   `unreadable`).
//! * `project_links` — `from_project → to_path` with its [`LinkKind`] and
//!   evidence (manifest + line). `to_project` is the nearest registered
//!   project at or above `to_path`, re-resolved whenever the set of
//!   projects changes, so a link to a directory registered later appears
//!   without re-reading the linking project. Manifest links come from
//!   Cargo path deps and workspace members, npm/pnpm workspaces and
//!   `file:`/`link:`/`workspace:` deps, and go.mod `replace … => ./dir`
//!   ([`manifest`]); `same_remote` (separate clones of one remote) and
//!   `nested_workspace` (an index inside another project's root) are
//!   derived from the project rows.
//!
//! There is no separate checkouts table: a project row *is* a checkout.
//! Linked worktrees of one repository share `repo_root` (their main
//! checkout's root); separate clones share `remote`.
//!
//! Writers are the CLI's `init`/`index`/`sync` ([`register`]), `codegraph
//! projects scan|register|prune`, and a detached `codegraph projects
//! register` an MCP session starts for a project the atlas doesn't know yet
//! ([`background`]). Each registration gathers its facts first, then writes
//! in one short `BEGIN IMMEDIATE` transaction with a bounded busy timeout —
//! on contention it is skipped and reported, never waited out. Readers
//! ([`Atlas::open_read_only`]) never create or change a file.

#![forbid(unsafe_code)]

pub mod background;
mod facts;
mod git;
mod index_stats;
mod kinds;
pub mod manifest;
pub mod mermaid;
mod model;
mod query;
mod register;
pub mod remote;
mod ro;
pub mod scan;
mod schema;
mod store;
mod write;

use std::path::{Path, PathBuf};

pub use facts::{GatherOptions, ProjectFacts};
pub use git::{GitFacts, read_git_facts};
pub use index_stats::IndexFacts;
pub use kinds::{LinkKind, ProjectStatus};
pub use model::{Evidence, LanguageCount, Project, ProjectLink};
pub use register::{RegisterOutcome, register_project, register_project_at};
pub use scan::{ScanOptions, ScanReport};
pub use store::{Atlas, AtlasError, PruneReport};
pub use write::Registered;

/// Where the atlas lives: `atlas.db` in [`crate::directory::codegraph_home`].
pub fn atlas_path() -> PathBuf {
    crate::directory::codegraph_home().join("atlas.db")
}

/// The atlas is on unless `CODEGRAPH_ATLAS=0` (or `false`/`off`).
pub fn atlas_enabled() -> bool {
    !std::env::var("CODEGRAPH_ATLAS").is_ok_and(|v| {
        let v = v.trim();
        v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")
    })
}

/// Open the default atlas read-only. `Ok(None)` when there is none yet;
/// never creates the file, its directory, or SQLite's side files.
pub fn open_read_only() -> Result<Option<Atlas>, AtlasError> {
    Atlas::open_read_only(&atlas_path())
}

/// The atlas's identity for a project directory: absolute, symlinks
/// resolved as far as the path exists (the join key with `deps/`).
pub fn canonical_root(path: &Path) -> PathBuf {
    crate::directory::real_path_lenient(path)
}

/// Current wall-clock time in epoch milliseconds.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests;
