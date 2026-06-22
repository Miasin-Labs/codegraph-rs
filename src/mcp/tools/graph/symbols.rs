//! Symbol lookup and ambiguity handling for graph MCP tools.

use super::super::context::ToolHandler;
use super::super::format::{
    EXT_STRIP_RE,
    QUALIFIER_SPLIT_RE,
    RUST_PATH_PREFIXES,
    last_qualifier_part,
};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::extraction::is_generated_file;
use crate::types::{Node, NodeKind, SearchOptions, SearchResult};

impl ToolHandler {
    fn matches_symbol(&self, node: &Node, symbol: &str) -> bool {
        // Simple name match
        if node.name == symbol {
            return true;
        }
        // File basename match (e.g., "product-card" matches "product-card.liquid")
        if node.kind == NodeKind::File && EXT_STRIP_RE.replace(&node.name, "") == symbol {
            return true;
        }

        // Qualified-name lookups
        if !(symbol.contains('.') || symbol.contains('/') || symbol.contains("::")) {
            return false;
        }
        let parts: Vec<&str> = QUALIFIER_SPLIT_RE
            .split(symbol)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() < 2 {
            return false;
        }

        let last_part = parts[parts.len() - 1];
        if node.name != last_part {
            return false;
        }

        // Stage 1: qualified-name suffix match.
        let colon_suffix = parts.join("::");
        if node.qualified_name.contains(&colon_suffix) {
            return true;
        }

        // Stage 2: file-path containment.
        let container_hints: Vec<&str> = parts[..parts.len() - 1]
            .iter()
            .filter(|p| !RUST_PATH_PREFIXES.contains(p))
            .copied()
            .collect();
        if container_hints.is_empty() {
            return false;
        }

        let segments: Vec<&str> = node
            .file_path
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        container_hints.iter().all(|hint| {
            segments
                .iter()
                .any(|seg| seg == hint || EXT_STRIP_RE.replace(seg, "") == *hint)
        })
    }

    /// Find ALL definitions matching a name, ranked, so codegraph_node can
    /// return every overload instead of guessing one.
    pub(in crate::mcp::tools) fn find_symbol_matches(
        &self,
        cg: &CodeGraph,
        symbol: &str,
    ) -> Result<Vec<Node>> {
        let is_qualified = symbol.contains('.') || symbol.contains('/') || symbol.contains("::");

        // For a bare name, enumerate EVERY exact-name definition via the
        // direct index (not FTS, which caps + ranks).
        if !is_qualified {
            let exact = cg.get_nodes_by_name(symbol)?;
            if !exact.is_empty() {
                let mut sorted = exact;
                sorted.sort_by_key(|n| {
                    if is_generated_file(&n.file_path) {
                        1
                    } else {
                        0
                    }
                });
                return Ok(sorted);
            }
            // No exact match — use the single top fuzzy result.
            let fuzzy = cg.search_nodes(
                symbol,
                Some(&SearchOptions {
                    limit: Some(10),
                    ..Default::default()
                }),
            )?;
            return Ok(fuzzy.into_iter().take(1).map(|r| r.node).collect());
        }

        // Qualified lookup: FTS + matches_symbol.
        let limit = 50;
        let mut results = cg.search_nodes(
            symbol,
            Some(&SearchOptions {
                limit: Some(limit),
                ..Default::default()
            }),
        )?;

        // FTS strips colons — re-search by the bare last part.
        if results.is_empty() {
            let tail = last_qualifier_part(symbol);
            if !tail.is_empty() && tail != symbol {
                results = cg.search_nodes(
                    &tail,
                    Some(&SearchOptions {
                        limit: Some(limit),
                        ..Default::default()
                    }),
                )?;
            }
        }

        if results.is_empty() {
            return Ok(Vec::new());
        }

        let exact_matches: Vec<&SearchResult> = results
            .iter()
            .filter(|r| self.matches_symbol(&r.node, symbol))
            .collect();
        if exact_matches.is_empty() {
            // A qualified lookup must not fall back to a fuzzy file hit (#173).
            return Ok(Vec::new());
        }

        // Down-rank generated files.
        let mut ranked: Vec<Node> = exact_matches.into_iter().map(|r| r.node.clone()).collect();
        ranked.sort_by_key(|n| {
            if is_generated_file(&n.file_path) {
                1
            } else {
                0
            }
        });
        Ok(ranked)
    }

    /// Find ALL symbols matching a name. Used by callers/callees/impact to
    /// aggregate results across all matching symbols.
    pub(in crate::mcp::tools) fn find_all_symbols(
        &self,
        cg: &CodeGraph,
        symbol: &str,
    ) -> Result<SymbolMatches> {
        let is_qualified = symbol.contains('.') || symbol.contains('/') || symbol.contains("::");
        let mut results = cg.search_nodes(
            symbol,
            Some(&SearchOptions {
                limit: Some(50),
                ..Default::default()
            }),
        )?;

        // Mirror the fallback for qualified queries.
        if results.is_empty() && is_qualified {
            let tail = last_qualifier_part(symbol);
            if !tail.is_empty() && tail != symbol {
                results = cg.search_nodes(
                    &tail,
                    Some(&SearchOptions {
                        limit: Some(50),
                        ..Default::default()
                    }),
                )?;
            }
        }

        if results.is_empty() {
            return Ok(SymbolMatches {
                nodes: Vec::new(),
                note: String::new(),
            });
        }

        let exact_matches: Vec<&SearchResult> = results
            .iter()
            .filter(|r| self.matches_symbol(&r.node, symbol))
            .collect();

        if exact_matches.is_empty() && is_qualified {
            return Ok(SymbolMatches {
                nodes: Vec::new(),
                note: String::new(),
            });
        }

        if exact_matches.len() <= 1 {
            let node = exact_matches
                .first()
                .map(|r| r.node.clone())
                .unwrap_or_else(|| results[0].node.clone());
            return Ok(SymbolMatches {
                nodes: vec![node],
                note: String::new(),
            });
        }

        // Same generated-file down-rank as find_symbol_matches.
        let mut ranked: Vec<Node> = exact_matches.into_iter().map(|r| r.node.clone()).collect();
        ranked.sort_by_key(|n| {
            if is_generated_file(&n.file_path) {
                1
            } else {
                0
            }
        });

        let locations: Vec<String> = ranked
            .iter()
            .map(|n| format!("{} at {}:{}", n.kind.as_str(), n.file_path, n.start_line))
            .collect();
        let note = format!(
            "\n\n> **Note:** Aggregated results across {} symbols named \"{}\": {}",
            ranked.len(),
            symbol,
            locations.join(", ")
        );
        Ok(SymbolMatches {
            nodes: ranked,
            note,
        })
    }
}

pub(in crate::mcp::tools) struct SymbolMatches {
    pub(in crate::mcp::tools) nodes: Vec<Node>,
    pub(in crate::mcp::tools) note: String,
}
