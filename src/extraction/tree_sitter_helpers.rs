//! Tree-sitter Shared Helpers
//!
//! Utility functions used by the core `TreeSitterExtractor` and per-language
//! extractors. Extracted to a leaf module to avoid circular imports between
//! `tree_sitter_wrapper.rs` and `languages/`.
//!
//! Ported from `src/extraction/tree-sitter-helpers.ts`. Node-ID derivation is
//! byte-for-byte identical to the TS implementation (same hash input string
//! format), so IDs match across the two implementations.

use std::sync::LazyLock;

use regex::Regex;

use crate::extraction::tree_sitter_types::SyntaxNode;
use crate::types::NodeKind;
use crate::utils::sha256_hex;

/// Generate a unique node ID
///
/// Uses a 32-character (128-bit) hash to avoid collisions when indexing
/// large codebases with many files containing similar symbols.
///
/// Hash input format (identical to TS): `{filePath}:{kind}:{name}:{line}`.
pub fn generate_node_id(file_path: &str, kind: NodeKind, name: &str, line: u32) -> String {
    let input = format!("{}:{}:{}:{}", file_path, kind.as_str(), name, line);
    let hash = sha256_hex(input.as_bytes());
    format!("{}:{}", kind.as_str(), &hash[..32])
}

/// Extract text from a syntax node
pub fn get_node_text<'s>(node: SyntaxNode<'_>, source: &'s str) -> &'s str {
    source.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

/// Find a child node by field name
pub fn get_child_by_field<'t>(node: SyntaxNode<'t>, field_name: &str) -> Option<SyntaxNode<'t>> {
    node.child_by_field_name(field_name)
}

/// Strip leading `/**` / `/*` and trailing `*/` (anchored at the comment's
/// start/end, mirroring the TS `/^\/\*\*?|\*\/$/g` regex without `m`).
static BLOCK_MARKERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/\*\*?|\*/$").expect("valid regex"));
/// Per-line `//` markers (TS `/^\/\/\s?/gm`).
static LINE_MARKERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^//\s?").expect("valid regex"));
/// Per-line leading `*` continuation markers (TS `/^\s*\*\s?/gm`).
static STAR_MARKERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*\*\s?").expect("valid regex"));

/// Get the docstring/comment preceding a node
pub fn get_preceding_docstring(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let mut sibling = node.prev_named_sibling();
    let mut comments: Vec<&str> = Vec::new();

    while let Some(sib) = sibling {
        match sib.kind() {
            "comment" | "line_comment" | "block_comment" | "documentation_comment" => {
                comments.insert(0, get_node_text(sib, source));
                sibling = sib.prev_named_sibling();
            }
            _ => break,
        }
    }

    if comments.is_empty() {
        return None;
    }

    // Clean up comment markers
    let cleaned = comments
        .iter()
        .map(|c| {
            let c = BLOCK_MARKERS.replace_all(c, "");
            let c = LINE_MARKERS.replace_all(&c, "");
            let c = STAR_MARKERS.replace_all(&c, "");
            c.trim().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");

    Some(cleaned.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::grammars::create_parser;
    use crate::types::Language;

    #[test]
    fn generate_node_id_matches_ts_derivation() {
        // sha256("src/a.ts:function:foo:3") =
        // bfb15544fed707794274a5c61006ea7b57284c731398f94a7a8fcded47cdd204
        assert_eq!(
            generate_node_id("src/a.ts", NodeKind::Function, "foo", 3),
            "function:bfb15544fed707794274a5c61006ea7b"
        );
        // sha256("src/utils.ts:class:MathHelper:1") =
        // c35cac13cd06f10160669d6e043645aa702155ef3a735a9e9341bf6bf9eef214
        assert_eq!(
            generate_node_id("src/utils.ts", NodeKind::Class, "MathHelper", 1),
            "class:c35cac13cd06f10160669d6e043645aa"
        );
    }

    #[test]
    fn preceding_docstring_strips_markers() {
        let source = "/**\n * Adds numbers.\n * Second line.\n */\nfunction add() {}\n";
        let mut parser = create_parser(Language::Javascript).expect("js parser");
        let tree = parser.parse(source, None).expect("parse");
        let root = tree.root_node();
        let func = (0..root.named_child_count() as u32)
            .filter_map(|i| root.named_child(i))
            .find(|n| n.kind() == "function_declaration")
            .expect("function node");
        let doc = get_preceding_docstring(func, source).expect("docstring");
        assert_eq!(doc, "Adds numbers.\nSecond line.");
    }

    #[test]
    fn preceding_docstring_joins_line_comments() {
        let source = "// first\n// second\nfunction f() {}\n";
        let mut parser = create_parser(Language::Javascript).expect("js parser");
        let tree = parser.parse(source, None).expect("parse");
        let root = tree.root_node();
        let func = (0..root.named_child_count() as u32)
            .filter_map(|i| root.named_child(i))
            .find(|n| n.kind() == "function_declaration")
            .expect("function node");
        let doc = get_preceding_docstring(func, source).expect("docstring");
        assert_eq!(doc, "first\nsecond");
    }

    #[test]
    fn preceding_docstring_absent_returns_none() {
        let source = "function f() {}\nfunction g() {}\n";
        let mut parser = create_parser(Language::Javascript).expect("js parser");
        let tree = parser.parse(source, None).expect("parse");
        let root = tree.root_node();
        let first = root.named_child(0).expect("first function");
        assert_eq!(get_preceding_docstring(first, source), None);
    }
}
