//! CodeGraph — local-first code intelligence library.
//!
//! Rust port of the TypeScript implementation in `../src/`. Parses any
//! supported codebase with tree-sitter, stores symbols/edges/files in SQLite
//! (FTS5), and exposes a knowledge graph to AI agents over MCP.
//!
//! The public API surface is the [`CodeGraph`] struct, which wires all the
//! layers together — extraction → resolution → graph → context.

pub mod analysis_bridge;
pub mod analyze;
pub mod analyze_ir;
pub mod context;
pub mod context_analysis;
pub mod db;
pub mod directory;
pub mod error;
pub mod extraction;
pub mod graph;
pub mod history;
pub mod installer;
pub mod mcp;
pub mod project_config;
pub mod prompt_hook;
pub mod resolution;
pub mod search;
pub mod sync;
pub mod telemetry;
pub mod types;
pub mod ui;
pub mod upgrade;
pub mod utils;

mod codegraph;

pub use codegraph::*;
/// Grow the stack before another level of recursive descent.
///
/// Recursive walkers over unbounded input — AST nesting (extraction), graph
/// chains (impact/type hierarchy), parsed-query trees (DSL), directory depth
/// (file-tree rendering) — recurse to a depth set by their input, not by a
/// fixed bound. On a worker thread with a fixed stack (scheduler workers, the MCP
/// dispatch thread, the file watcher) a pathologically deep input would
/// otherwise overflow and abort the whole process. Calling this at each
/// recursive function's head bounds depth by input size, never by thread
/// stack. Mirrors rustc's `ensure_sufficient_stack`.
///
/// Re-exported from the analysis crate so there is exactly ONE
/// implementation (and one red-zone/segment-size configuration) across the
/// workspace.
pub use codegraph_analysis::ensure_sufficient_stack;
pub use error::{CodeGraphError, Logger, Result, SilentLogger};
pub use project_config::{
    PROJECT_CONFIG_FILENAME,
    ProjectConfig,
    clear_project_config_cache,
    load_project_config,
};
pub use types::*;
