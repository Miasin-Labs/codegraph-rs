//! Cross-graph resolution (federation phase 2): a project's references
//! whose target lives in a dependency (its shared shard) or in a linked
//! project (that project's own index) resolve into that graph, as
//! `external_edges` rows naming the target graph and the node in it.
//!
//! In-project resolution runs first and is untouched: only what it leaves
//! unresolved is examined here, and only a *resolved* target — one item of
//! the crate a path or a typed receiver names — becomes an edge. Every
//! other graph is opened read-only ([`open`]); nothing outside the
//! project's own index is written. Rust is resolved today ([`rust`]); npm
//! and Go shards join through [`graphs::discover`] and a resolver per
//! language.
//!
//! The pass runs where indexing does — `codegraph init|index|sync`, the
//! watcher's syncs, and the background sync a finished `deps build`
//! queues — never on an MCP request or the prompt hook, and within a time
//! budget ([`pass`]).

mod context;
mod declarations;
pub mod graphs;
mod manifest;
mod names;
pub mod open;
pub mod pass;
pub(crate) mod rust;
mod worker;

pub use graphs::{FederationHome, GraphLocation, Reach, ReachableGraph, Skipped, discover};
pub use open::OpenStats;
pub use pass::{
    EXTERNAL_RESOLUTION_VERSION,
    ExternalMode,
    ExternalOptions,
    ExternalRefs,
    ExternalReport,
    STATE_KEY,
    external_resolution_enabled,
    run,
};
pub use rust::ExternalResolvedBy;
