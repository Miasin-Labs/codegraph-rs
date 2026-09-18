use std::collections::HashSet;

use super::super::context::ToolHandler;
use super::super::format::{
    OrderedNodeMap,
    TYPE_TOKEN_RE,
    extract_symbol_tokens,
    is_explore_seed_kind,
    is_qualified_token,
    is_test_path,
};
use super::types::ExploreSeeds;
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::search::{STOP_WORDS, split_identifier_segments};
use crate::types::Node;

/// Stop words that mark a query as prose ("how does X run …").
const PROSE_STOP_WORDS: usize = 2;
/// Symbols a compound token's segments may stand in for.
const SEGMENT_FALLBACK_LIMIT: usize = 3;

/// Whether `query` reads as a sentence rather than a bag of names. Words are
/// split on everything but `_`, so `is_test_path` is one name, not "is".
fn is_prose(query: &str) -> bool {
    query
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|word| STOP_WORDS.contains(word.to_lowercase().as_str()))
        .count()
        >= PROSE_STOP_WORDS
}

/// An all-lowercase single word (`run`, `check`, `errors`). In a sentence it
/// is English first; in a bag of names it may be a symbol.
fn is_plain_word(token: &str) -> bool {
    token.chars().all(|c| c.is_ascii_lowercase())
}

/// Symbols standing in for a compound identifier nothing is named after —
/// `codegraph_diagnostics`, a tool name, finds `handle_diagnostics` — through
/// the identifier-segment vocabulary. Empty for single-segment tokens and when
/// the vocabulary has no rare, repeated match.
fn segment_stand_ins(cg: &CodeGraph, token: &str) -> Vec<Node> {
    let words = split_identifier_segments(token);
    if words.len() < 2 {
        return Vec::new();
    }
    let Ok(matches) = cg.get_segment_matches(&words, SEGMENT_FALLBACK_LIMIT) else {
        return Vec::new();
    };
    matches
        .into_iter()
        .filter_map(|found| cg.get_nodes_by_name(&found.name).ok())
        .flatten()
        .collect()
}

impl ToolHandler {
    pub(in crate::mcp::tools::explore) fn collect_explore_seeds(
        &self,
        cg: &CodeGraph,
        query: &str,
        roots: &[String],
        nodes: &mut OrderedNodeMap,
        max_named_seed_tokens: usize,
    ) -> Result<ExploreSeeds> {
        let mut glue_node_ids: HashSet<String> = HashSet::new();
        let subgraph_files: HashSet<String> = nodes.values().map(|n| n.file_path.clone()).collect();
        const GLUE_NODE_CAP: usize = 60;
        for root_id in roots {
            crate::graph::cancel::check()?;
            if glue_node_ids.len() >= GLUE_NODE_CAP {
                break;
            }
            let mut neighbors: Vec<Node> = Vec::new();
            let callers = cg.get_callers(root_id, None);
            let callees = cg.get_callees(root_id, None);
            match (callers, callees) {
                (Ok(cr), Ok(ce)) => {
                    neighbors.extend(cr.into_iter().map(|c| c.node));
                    neighbors.extend(ce.into_iter().map(|c| c.node));
                }
                _ => continue,
            }
            for nb in neighbors {
                if glue_node_ids.len() >= GLUE_NODE_CAP {
                    break;
                }
                if nodes.contains(&nb.id) || !subgraph_files.contains(&nb.file_path) {
                    continue;
                }
                glue_node_ids.insert(nb.id.clone());
                nodes.insert(nb);
            }
        }

        let mut named_seed_ids: HashSet<String> = HashSet::new();
        let body_lines = |n: &Node| (n.end_line as i64 - n.start_line as i64).max(0);
        let tokens = extract_symbol_tokens(query);
        let type_tokens: Vec<&String> = tokens
            .iter()
            .filter(|t| TYPE_TOKEN_RE.is_match(t))
            .collect();
        let in_named_context = |n: &Node| {
            type_tokens.iter().any(|ct| {
                let lc = ct.to_lowercase();
                n.file_path.to_lowercase().contains(&lc)
                    || n.qualified_name.to_lowercase().contains(&lc)
            })
        };
        // In a sentence, plain words are prose: "run" and "check" in "how does
        // X run cargo check" must not pin whatever symbol shares the word.
        let prose = is_prose(query);
        for t in tokens
            .iter()
            .filter(|t| !(prose && is_plain_word(t)))
            .take(max_named_seed_tokens)
        {
            crate::graph::cancel::check()?;
            let raw: Vec<Node> = if is_qualified_token(t) {
                self.find_all_symbols(cg, t)?.nodes
            } else {
                cg.get_nodes_by_name(t)?
            };
            let seedable = |n: &Node| is_explore_seed_kind(n.kind) && !is_test_path(&n.file_path);
            let mut cands: Vec<Node> = raw.into_iter().filter(|n| seedable(n)).collect();
            if cands.is_empty() && !is_qualified_token(t) {
                cands = segment_stand_ins(cg, t)
                    .into_iter()
                    .filter(|n| seedable(n))
                    .collect();
            }
            cands.sort_by(|a, b| {
                let a_sub = if body_lines(a) > 1 { 1 } else { 0 };
                let b_sub = if body_lines(b) > 1 { 1 } else { 0 };
                b_sub.cmp(&a_sub).then(body_lines(b).cmp(&body_lines(a)))
            });
            let picks: Vec<Node> = if cands.len() <= 3 {
                cands
            } else {
                let ctx: Vec<Node> = cands
                    .iter()
                    .filter(|n| in_named_context(n))
                    .cloned()
                    .collect();
                if !ctx.is_empty() {
                    ctx.into_iter().take(4).collect()
                } else {
                    cands.into_iter().take(1).collect()
                }
            };
            for n in picks {
                named_seed_ids.insert(n.id.clone());
                if !nodes.contains(&n.id) {
                    nodes.insert(n);
                }
            }
        }

        Ok(ExploreSeeds {
            glue_node_ids,
            named_seed_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{is_plain_word, is_prose};

    #[test]
    fn prose_needs_stop_words_outside_identifiers() {
        assert!(is_prose("how does codegraph_diagnostics run cargo check"));
        assert!(!is_prose("is_test_path for_language set_up_to_date"));
        assert!(!is_prose(
            "dropSessionCaches SESSION_CACHE_LIMIT ToolRegistry"
        ));
    }

    #[test]
    fn plain_words_are_single_lowercase_runs() {
        assert!(is_plain_word("check"));
        assert!(!is_plain_word("check_or_poll"));
        assert!(!is_plain_word("Checker"));
        assert!(!is_plain_word("utf8"));
    }
}
