//! Compatibility facade for grammars whose bindings still request
//! `tree-sitter ~0.20` but only exchange the stable `Language` wrapper.

pub use tree_sitter_next::*;
