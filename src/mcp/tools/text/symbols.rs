//! The indexed symbol a hit falls in, and the live [`PageSource`] the
//! handler renders pages from.

use std::collections::HashMap;
use std::io::Read;

use super::super::format::is_callable_kind;
use super::open::Opener;
use super::render::{Enclosing, PageSource};
use crate::codegraph::CodeGraph;
use crate::types::{Node, NodeKind};

/// Longest symbol name sent; deeper qualified names keep their tail.
const MAX_SYMBOL_CHARS: usize = 80;

/// Innermost definition around a line, per file, loaded once per file.
pub(in crate::mcp::tools) struct EnclosingSymbols<'a> {
    cg: &'a CodeGraph,
    files: HashMap<String, Vec<Span>>,
}

/// A definition's line span and its display name.
#[derive(Debug, Clone)]
struct Span {
    start: u32,
    end: u32,
    callable: bool,
    name: String,
}

impl<'a> EnclosingSymbols<'a> {
    pub fn new(cg: &'a CodeGraph) -> Self {
        Self {
            cg,
            files: HashMap::new(),
        }
    }

    /// The innermost definition containing `line` of `file`, named as the
    /// file qualifies it (`Type::method`), or `None` at file scope.
    pub fn at(&mut self, file: &str, line: u32) -> Option<Enclosing> {
        let cg = self.cg;
        let spans = self.files.entry(file.to_string()).or_insert_with(|| {
            cg.get_nodes_in_file(file)
                .unwrap_or_default()
                .iter()
                .filter(|node| is_definition(node.kind))
                .map(|node| Span {
                    start: node.start_line,
                    end: node.end_line,
                    callable: is_callable_kind(node.kind),
                    name: display_name(file, node),
                })
                .collect()
        });
        innermost(spans, line).map(|span| Enclosing {
            name: span.name.clone(),
            start: span.start,
            end: span.end,
        })
    }
}

/// Symbols from the index, file contents from disk.
pub(in crate::mcp::tools) struct LiveSource<'a> {
    symbols: EnclosingSymbols<'a>,
    opener: Opener<'a>,
    max_file_bytes: u64,
}

impl<'a> LiveSource<'a> {
    pub fn new(cg: &'a CodeGraph, root: &'a std::path::Path, max_file_bytes: u64) -> Self {
        Self {
            symbols: EnclosingSymbols::new(cg),
            opener: Opener::new(root),
            max_file_bytes,
        }
    }
}

impl PageSource for LiveSource<'_> {
    fn enclosing(&mut self, path: &str, line: u32) -> Option<Enclosing> {
        self.symbols.at(path, line)
    }

    fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        let file = self.opener.open(path)?;
        let mut buf = Vec::new();
        file.take(self.max_file_bytes).read_to_end(&mut buf).ok()?;
        Some(buf)
    }
}

/// Kinds that name a place in the code; imports, parameters, and the file
/// itself do not.
fn is_definition(kind: NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::File | NodeKind::Import | NodeKind::Export | NodeKind::Parameter
    )
}

fn innermost(spans: &[Span], line: u32) -> Option<&Span> {
    spans
        .iter()
        .filter(|span| span.start <= line && line <= span.end)
        // Innermost first; at equal span prefer a callable over its container.
        .min_by_key(|span| (span.end - span.start, !span.callable))
}

fn display_name(file: &str, node: &Node) -> String {
    let qualified = node.qualified_name.as_str();
    let name = qualified
        .strip_prefix(file)
        .and_then(|rest| rest.strip_prefix("::"))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(if qualified.is_empty() {
            &node.name
        } else {
            qualified
        });
    let chars = name.chars().count();
    if chars <= MAX_SYMBOL_CHARS {
        return name.to_string();
    }
    let tail: String = name.chars().skip(chars - (MAX_SYMBOL_CHARS - 1)).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: u32, end: u32, callable: bool, name: &str) -> Span {
        Span {
            start,
            end,
            callable,
            name: name.into(),
        }
    }

    #[test]
    fn picks_the_innermost_definition_and_prefers_callables_on_ties() {
        let spans = vec![
            span(1, 100, false, "Parser"),
            span(10, 40, true, "Parser::parse"),
            span(12, 12, false, "Parser::parse::LIMIT"),
            span(50, 50, false, "Field"),
            span(50, 50, true, "method"),
        ];
        assert_eq!(innermost(&spans, 20).unwrap().name, "Parser::parse");
        assert_eq!(innermost(&spans, 12).unwrap().name, "Parser::parse::LIMIT");
        assert_eq!(innermost(&spans, 60).unwrap().name, "Parser");
        assert_eq!(innermost(&spans, 50).unwrap().name, "method");
        assert!(innermost(&spans, 200).is_none());
    }
}
