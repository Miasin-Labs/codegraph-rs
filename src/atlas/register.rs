//! Registration entry points for the CLI's write paths (`init`, `index`,
//! `sync`, `projects register`). Never called from the prompt hook or an MCP
//! request — those hand it to a detached `codegraph projects register`
//! ([`super::background`]).
//!
//! A registration never fails its command: contention, a newer atlas, or
//! an I/O error come back as an outcome the caller may report and move on.

use std::path::Path;

use super::facts::{GatherOptions, ProjectFacts};
use super::store::{Atlas, AtlasError};
use crate::directory::is_initialized;

/// What registering a project did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterOutcome {
    Registered {
        project_id: i64,
        /// Manifest links recorded.
        links: usize,
        /// First time the atlas saw this root.
        new: bool,
    },
    /// `CODEGRAPH_ATLAS=0`.
    Disabled,
    /// No CodeGraph index at that root.
    NotIndexed,
    /// Another writer held the atlas past the busy timeout; skipped.
    Busy,
    Failed(String),
}

impl RegisterOutcome {
    /// One line for the CLI when registration did not happen (and should
    /// be mentioned), `None` otherwise.
    pub fn warning(&self, root: &Path) -> Option<String> {
        match self {
            Self::Busy => Some(format!(
                "Atlas busy — skipped registering {} (the next index/sync retries)",
                root.display()
            )),
            Self::Failed(reason) => Some(format!(
                "Atlas: could not register {}: {reason}",
                root.display()
            )),
            Self::Registered { .. } | Self::Disabled | Self::NotIndexed => None,
        }
    }
}

/// Register the project indexed at `root` in the default atlas.
pub fn register_project(root: &Path) -> RegisterOutcome {
    if !super::atlas_enabled() {
        return RegisterOutcome::Disabled;
    }
    if let Err(e) = crate::directory::ensure_codegraph_home() {
        return RegisterOutcome::Failed(e.to_string());
    }
    register_project_at(&super::atlas_path(), root, GatherOptions::default())
}

/// Register the project indexed at `root` in the atlas at `atlas`.
pub fn register_project_at(atlas: &Path, root: &Path, opts: GatherOptions) -> RegisterOutcome {
    if !is_initialized(root) {
        return RegisterOutcome::NotIndexed;
    }
    // Facts first, so the write transaction is only the write.
    let facts = ProjectFacts::gather(root, opts);
    let result = Atlas::open(atlas).and_then(|mut atlas| atlas.register(&facts));
    match result {
        Ok(done) => RegisterOutcome::Registered {
            project_id: done.project_id,
            links: done.links,
            new: done.new,
        },
        Err(e) if e.is_busy() => RegisterOutcome::Busy,
        Err(AtlasError::TooNew { found, supported }) => RegisterOutcome::Failed(format!(
            "the atlas is at schema v{found}; this build writes v{supported} (upgrade codegraph)"
        )),
        Err(e) => RegisterOutcome::Failed(e.to_string()),
    }
}
