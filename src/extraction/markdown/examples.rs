//! Fenced code blocks as doc examples: a block tagged with a language this
//! binary parses runs through that language's extractor, and the symbols it
//! declares become nodes of the document, contained by the enclosing
//! section.
//!
//! They are *examples*, not project code, so every node is stamped
//! `language: markdown` (the document's language, whatever the block's):
//! resolution and the analysis bridge read nodes through
//! `QueryBuilder::get_all_nodes`, which leaves Markdown out, and the
//! block's own references are dropped — an RFC's `v.push(x)` must never
//! become a caller of a project `push`.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use super::scan::Fence;
use crate::extraction::grammars::{create_parser, has_grammar};
use crate::extraction::languages::extractor_for;
use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
use crate::types::{Edge, Language, Node, NodeKind};

/// The language a fence's info string names, or `Untagged` for a bare fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FenceLanguage {
    Tagged(Language),
    Untagged,
    Other,
}

pub(super) fn fence_language(info: &str) -> FenceLanguage {
    let info = info.trim().trim_start_matches('{').trim_start_matches('.');
    let Some(first) = info
        .split(|c: char| c == ',' || c == '}' || c.is_whitespace())
        .find(|token| !token.is_empty())
    else {
        return FenceLanguage::Untagged;
    };
    let token = first.to_ascii_lowercase();
    // rustdoc's attributes alone (```ignore, ```compile_fail, ```edition2021)
    // still mean Rust.
    if matches!(
        token.as_str(),
        "ignore" | "no_run" | "compile_fail" | "should_panic" | "test_harness" | "standalone_crate"
    ) || token.starts_with("edition20")
    {
        return FenceLanguage::Tagged(Language::Rust);
    }
    let language = match token.as_str() {
        "rust" | "rs" => Language::Rust,
        "c" | "h" => Language::C,
        "cpp" | "c++" | "cxx" | "cc" | "hpp" => Language::Cpp,
        "python" | "py" | "python3" => Language::Python,
        "javascript" | "js" | "mjs" | "cjs" => Language::Javascript,
        "jsx" => Language::Jsx,
        "typescript" | "ts" => Language::Typescript,
        "tsx" => Language::Tsx,
        "go" | "golang" => Language::Go,
        "java" => Language::Java,
        "kotlin" | "kt" => Language::Kotlin,
        "swift" => Language::Swift,
        "ruby" | "rb" => Language::Ruby,
        "php" => Language::Php,
        "csharp" | "cs" | "c#" => Language::Csharp,
        "scala" => Language::Scala,
        "lua" => Language::Lua,
        "bash" | "sh" | "shell" => Language::Bash,
        "dart" => Language::Dart,
        "solidity" | "sol" => Language::Solidity,
        "objc" | "objective-c" => Language::Objc,
        other => match other.parse::<Language>() {
            Ok(language) => language,
            Err(_) => return FenceLanguage::Other,
        },
    };
    if has_grammar(language) {
        FenceLanguage::Tagged(language)
    } else {
        FenceLanguage::Other
    }
}

/// Words that make an untagged block worth trying as Rust.
const RUST_MARKERS: &[&str] = &[
    "fn ",
    "struct ",
    "enum ",
    "trait ",
    "impl",
    "let ",
    "mod ",
    "use ",
    "macro_rules!",
    "::",
];

/// An untagged fence is parsed as Rust when it reads like Rust and parses
/// with no syntax error. rustdoc and mdBook treat a bare fence as Rust, so
/// in Rust documentation that is what it is; the clean-parse check keeps
/// grammar sketches, shell sessions and prose (which never parse cleanly)
/// out, in any repository.
pub(super) fn untagged_is_rust(code: &str) -> bool {
    if !RUST_MARKERS.iter().any(|marker| code.contains(marker)) {
        return false;
    }
    let Some(mut parser) = create_parser(Language::Rust) else {
        return false;
    };
    parser
        .parse(code, None)
        .is_some_and(|tree| !tree.root_node().has_error())
}

/// The block's text with `indent` columns of leading spaces removed from
/// each line (a fence nested in a list item).
pub(super) fn dedented(content: &str, indent: usize) -> Cow<'_, str> {
    if indent == 0 {
        return Cow::Borrowed(content);
    }
    let mut out = String::with_capacity(content.len());
    for (at, line) in content.split('\n').enumerate() {
        if at > 0 {
            out.push('\n');
        }
        let strip = line.bytes().take(indent).take_while(|&b| b == b' ').count();
        out.push_str(&line[strip..]);
    }
    Cow::Owned(out)
}

/// Where a block's nodes go in the document.
pub(super) struct Placement<'a> {
    pub file_path: &'a str,
    /// The node that contains the block's top-level symbols.
    pub parent_id: &'a str,
    /// The qualified name the block's symbols are nested under.
    pub parent_qualified: &'a str,
}

/// Parse one block and return its symbols as document nodes plus their
/// containment edges, at most `max_nodes`. `taken` holds the ids already
/// used in this document (two blocks may declare `fn main` on one line).
pub(super) fn extract_block(
    fence: &Fence,
    code: &str,
    language: Language,
    placement: &Placement<'_>,
    max_nodes: usize,
    taken: &mut HashSet<String>,
) -> (Vec<Node>, Vec<Edge>) {
    let result = TreeSitterExtractor::new(
        placement.file_path,
        code,
        Some(language),
        extractor_for(language),
    )
    .extract();

    let line_shift = fence.first_line.saturating_sub(1);
    let byte_shift = (fence.indent == 0).then_some(fence.content.start as u32);
    let own_prefix = format!("{}::", placement.file_path);
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut nodes = Vec::new();
    for mut node in result.nodes {
        if node.kind == NodeKind::File {
            ids.insert(node.id.clone(), placement.parent_id.to_string());
            continue;
        }
        if nodes.len() >= max_nodes {
            break;
        }
        node.start_line += line_shift;
        node.end_line += line_shift;
        if fence.indent > 0 {
            node.start_column += fence.indent as u32;
            node.end_column += fence.indent as u32;
        }
        match (byte_shift, node.start_byte, node.end_byte) {
            (Some(shift), Some(start), Some(end)) => {
                node.start_byte = Some(start + shift);
                node.end_byte = Some(end + shift);
            }
            _ => {
                node.start_byte = None;
                node.end_byte = None;
            }
        }
        node.language = Language::Markdown;
        node.qualified_name = match node.qualified_name.strip_prefix(&own_prefix) {
            Some(rest) => format!("{}::{rest}", placement.parent_qualified),
            None => format!("{}::{}", placement.parent_qualified, node.qualified_name),
        };
        let mut id = generate_node_id(placement.file_path, node.kind, &node.name, node.start_line);
        if taken.contains(&id) {
            let disambiguated = format!("{}@{}", node.name, node.start_column);
            id = generate_node_id(
                placement.file_path,
                node.kind,
                &disambiguated,
                node.start_line,
            );
        }
        if !taken.insert(id.clone()) {
            continue;
        }
        ids.insert(std::mem::replace(&mut node.id, id.clone()), id);
        nodes.push(node);
    }

    let edges = result
        .edges
        .into_iter()
        .filter_map(|mut edge| {
            edge.source = ids.get(&edge.source)?.clone();
            edge.target = ids.get(&edge.target)?.clone();
            (edge.source != edge.target).then_some(edge)
        })
        .collect();
    (nodes, edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_strings_name_languages() {
        assert_eq!(
            fence_language("rust"),
            FenceLanguage::Tagged(Language::Rust)
        );
        assert_eq!(
            fence_language("rust,ignore"),
            FenceLanguage::Tagged(Language::Rust)
        );
        assert_eq!(
            fence_language("compile_fail,E0499"),
            FenceLanguage::Tagged(Language::Rust)
        );
        assert_eq!(fence_language("C"), FenceLanguage::Tagged(Language::C));
        assert_eq!(
            fence_language("{.python}"),
            FenceLanguage::Tagged(Language::Python)
        );
        assert_eq!(fence_language(""), FenceLanguage::Untagged);
        assert_eq!(fence_language("text"), FenceLanguage::Other);
        assert_eq!(fence_language("console"), FenceLanguage::Other);
        assert_eq!(fence_language("toml"), FenceLanguage::Other);
    }

    #[test]
    fn untagged_blocks_must_parse_as_rust() {
        assert!(untagged_is_rust("fn main() {\n    let x = 1;\n}\n"));
        assert!(!untagged_is_rust("expr := expr '+' term | term\n"));
        assert!(!untagged_is_rust(
            "$ cargo build\n   Compiling foo v0.1.0\n"
        ));
        // Pseudo-Rust with elided bodies does not parse cleanly.
        assert!(!untagged_is_rust("impl<T> Foo for T { ... }\n"));
    }

    #[test]
    fn dedent_removes_list_indentation_only() {
        assert_eq!(dedented("    fn a() {}\n  b\n", 4), "fn a() {}\nb\n");
        assert_eq!(dedented("x\n", 0), "x\n");
    }
}
