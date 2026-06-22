//! Tree-sitter Parser Wrapper
//!
//! Handles parsing source code and extracting structural information.
//!
//! Ported from `src/extraction/tree-sitter.ts` (the `TreeSitterExtractor`
//! class plus its private helpers). The bottom-of-file `extractFromSource`
//! dispatcher is NOT here — it routes to the standalone extractors
//! (svelte/vue/liquid/mybatis/dfm/ida) and framework resolvers owned by
//! other modules; see `notes/extraction-core.md` for the wiring contract.
//!
//! Native deviations from the TS original (web-tree-sitter):
//! - No WASM memory management: the TS `tree.delete()` / "memory access out
//!   of bounds" re-throw / `this.source = ''` GC-pressure dance disappears —
//!   native trees free on drop.
//! - Positions: tree-sitter rows are 0-based; stored `startLine` is 1-based
//!   (the `+1` is preserved everywhere the TS applies it — including the one
//!   place it deliberately does NOT, the anonymous-class `extends` ref).
//! - Text indices are byte offsets into UTF-8 (TS used UTF-16 code-unit
//!   indices); identical for ASCII sources.

use std::collections::HashMap;

use crate::extraction::grammars::detect_language;
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{Edge, ExtractionError, Language, Node, NodeKind, UnresolvedReference};

/// TreeSitterExtractor - Main extraction class
pub struct TreeSitterExtractor<'a> {
    pub(super) file_path: String,
    pub(super) language: Language,
    pub(super) source: &'a str,
    pub(super) nodes: Vec<Node>,
    pub(super) edges: Vec<Edge>,
    pub(super) unresolved_references: Vec<UnresolvedReference>,
    pub(super) errors: Vec<ExtractionError>,
    pub(super) extractor: Option<&'a dyn LanguageExtractor>,
    /// Stack of parent node IDs
    pub(super) node_stack: Vec<String>,
    /// lookup key → node ID for Pascal defProc lookup
    pub(super) method_index: Option<HashMap<String, String>>,
}

impl<'a> TreeSitterExtractor<'a> {
    /// Create an extractor. `language: None` detects from the file path +
    /// source (TS constructor's `language || detectLanguage(...)`).
    ///
    /// `extractor` is the per-language config — the TS constructor looked it
    /// up in `EXTRACTORS[language]`; here the caller passes it in (typically
    /// `languages::extractor_for(language)`), keeping this module a leaf.
    pub fn new(
        file_path: impl Into<String>,
        source: &'a str,
        language: Option<Language>,
        extractor: Option<&'a dyn LanguageExtractor>,
    ) -> Self {
        let file_path = file_path.into();
        let language = language.unwrap_or_else(|| detect_language(&file_path, Some(source)));
        TreeSitterExtractor {
            file_path,
            language,
            source,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
            extractor,
            node_stack: Vec::new(),
            method_index: None,
        }
    }

    /// The language this extractor will parse as.
    pub fn language(&self) -> Language {
        self.language
    }
}

impl ExtractorContext for TreeSitterExtractor<'_> {
    fn create_node(
        &mut self,
        kind: NodeKind,
        name: &str,
        node: SyntaxNode<'_>,
        extra: NodeExtra,
    ) -> Option<Node> {
        TreeSitterExtractor::create_node(self, kind, name, node, extra)
    }

    fn visit_node(&mut self, node: SyntaxNode<'_>) {
        TreeSitterExtractor::visit_node(self, node);
    }

    fn visit_function_body(&mut self, body: SyntaxNode<'_>, function_id: &str) {
        TreeSitterExtractor::visit_function_body(self, body, function_id);
    }

    fn add_unresolved_reference(&mut self, reference: UnresolvedReference) {
        self.unresolved_references.push(reference);
    }

    fn push_scope(&mut self, node_id: String) {
        self.node_stack.push(node_id);
    }

    fn pop_scope(&mut self) {
        self.node_stack.pop();
    }

    fn file_path(&self) -> &str {
        &self.file_path
    }

    fn source(&self) -> &str {
        self.source
    }

    fn node_stack(&self) -> &[String] {
        &self.node_stack
    }

    fn nodes(&self) -> &[Node] {
        &self.nodes
    }
}
