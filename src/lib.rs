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
pub mod context;
pub mod db;
pub mod directory;
pub mod error;
pub mod extraction;
pub mod graph;
pub mod installer;
pub mod mcp;
pub mod resolution;
pub mod search;
pub mod sync;
pub mod types;
pub mod ui;
pub mod utils;

mod codegraph;

pub use codegraph::*;
pub use error::{CodeGraphError, Logger, Result, SilentLogger};
pub use types::*;
