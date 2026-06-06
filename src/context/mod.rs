//! Context building — combines FTS search with graph traversal to produce
//! rich, ready-to-inject context for AI agents.
//!
//! Mirrors `src/context/index.ts` re-exports.

pub mod builder;
pub mod formatter;
pub mod markers;

pub use builder::{ContextBuilder, create_context_builder};
pub use formatter::{
    format_bytes,
    format_context_as_json,
    format_context_as_markdown,
    format_subgraph_tree,
};
pub use markers::LOW_CONFIDENCE_MARKER;
