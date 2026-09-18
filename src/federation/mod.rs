//! Cross-graph queries (federation phase 3): follow a project's
//! `external_edges` into the graphs they point at — a dependency version's
//! shared shard or a linked project's own index — and back: who, across
//! every project that uses a graph, calls into it.
//!
//! * [`GraphSet`] opens those graphs for one request: read-only, creating
//!   nothing (shards `mode=ro&immutable=1`, project indexes through
//!   [`crate::atlas::ro`]), at most [`FederationOptions::max_open`] at a
//!   time (least recently used closed first), a graph that failed to open
//!   not retried.
//! * [`GraphSet::follow`] finds an edge's target by `target_node_id`, and —
//!   when the target graph was rebuilt and ids moved — by qualified name,
//!   kind and file. A missing, unreadable or stale graph degrades to
//!   [`Target::Unavailable`], never an error.
//! * [`GraphSet::callers_across`] / [`GraphSet::impact_across`] answer the
//!   reverse direction from the edges every *user* of a graph recorded —
//!   `deps/registry.db` users of a dependency version, atlas `cargo_path_dep`
//!   links into a project — bounded by a project cap, a per-project cap and
//!   the request's [`Deadline`]; projects whose indexes cannot hold the
//!   answer are listed as skipped, with the reason.
//! * [`GraphSet::resolve_symbol`] finds a symbol a question names inside
//!   another graph (`serde_json::from_str`, or `from_str` with a graph
//!   hint) through the phase-2 path lookup.
//!
//! Nothing here resolves references or writes anything: edges are what
//! phase 2 (`crate::resolution::external`) recorded.

#![forbid(unsafe_code)]

mod deadline;
mod follow;
mod graph;
mod impact;
pub mod render;
mod resolve;
mod set;
mod source;
mod users;

use std::time::Duration;

pub use deadline::Deadline;
pub use follow::{Followed, Target, Unavailable};
pub use graph::{GraphId, OpenGraph, dependency_label};
pub use impact::{CrossImpact, ProjectImpact};
pub use resolve::ForeignSymbol;
pub use set::{GraphSet, OpenCounts};
pub use source::{SourceWindow, read_window};
pub use users::{Caller, CrossCallers, ProjectCallers, ProjectRef, SkipReason, SkippedProject};

use crate::resolution::external::FederationHome;

/// Default wall-clock budget of one cross-graph request.
pub const DEFAULT_DEADLINE: Duration = Duration::from_millis(2_500);
/// Default number of graphs one request keeps open.
pub const DEFAULT_MAX_OPEN: usize = 16;
/// Default number of other projects a cross-project question reads.
pub const DEFAULT_MAX_PROJECTS: usize = 16;

/// Bounds and stores of the cross-graph layer.
#[derive(Debug, Clone)]
pub struct FederationOptions {
    pub home: FederationHome,
    /// Wall-clock budget of one request (`CODEGRAPH_FEDERATION_DEADLINE_MS`).
    pub deadline: Duration,
    /// Graphs held open at once.
    pub max_open: usize,
    /// Projects a "who uses this" question reads
    /// (`CODEGRAPH_FEDERATION_MAX_PROJECTS`).
    pub max_projects: usize,
}

impl FederationOptions {
    /// The stores under `codegraph_home()` and the bounds from the
    /// environment.
    pub fn from_env() -> Self {
        Self {
            home: FederationHome::from_env(),
            deadline: env_number("CODEGRAPH_FEDERATION_DEADLINE_MS")
                .map_or(DEFAULT_DEADLINE, Duration::from_millis),
            max_open: DEFAULT_MAX_OPEN,
            max_projects: env_number("CODEGRAPH_FEDERATION_MAX_PROJECTS")
                .map_or(DEFAULT_MAX_PROJECTS, |n| (n as usize).max(1)),
        }
    }

    /// Default bounds over the stores of an explicit home (tests, tools).
    pub fn at(home: FederationHome) -> Self {
        Self {
            home,
            deadline: DEFAULT_DEADLINE,
            max_open: DEFAULT_MAX_OPEN,
            max_projects: DEFAULT_MAX_PROJECTS,
        }
    }
}

/// Queries follow external edges unless `CODEGRAPH_FEDERATION=0` (or
/// `false`/`off`).
pub fn federation_enabled() -> bool {
    !std::env::var("CODEGRAPH_FEDERATION").is_ok_and(|value| {
        let value = value.trim();
        value == "0" || value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("off")
    })
}

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}
