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
use crate::types::Node;

impl ToolHandler {
    pub(in crate::mcp::tools::explore) fn collect_explore_seeds(
        &self,
        cg: &CodeGraph,
        query: &str,
        roots: &[String],
        nodes: &mut OrderedNodeMap,
    ) -> Result<ExploreSeeds> {
        let mut glue_node_ids: HashSet<String> = HashSet::new();
        let subgraph_files: HashSet<String> = nodes.values().map(|n| n.file_path.clone()).collect();
        const GLUE_NODE_CAP: usize = 60;
        for root_id in roots {
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
        for t in &tokens {
            let raw: Vec<Node> = if is_qualified_token(t) {
                self.find_all_symbols(cg, t)?.nodes
            } else {
                cg.get_nodes_by_name(t)?
            };
            let mut cands: Vec<Node> = raw
                .into_iter()
                .filter(|n| is_explore_seed_kind(n.kind) && !is_test_path(&n.file_path))
                .collect();
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
