//! Flow support for graph MCP tools.

use std::collections::{HashMap, HashSet};

use super::super::context::ToolHandler;
use super::super::format::{
    FlowInfo,
    OrderedNodeMap,
    QUAL_DOT_SPLIT_RE,
    extract_symbol_tokens,
    is_callable_kind,
};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::types::{Edge, EdgeKind, Node, Provenance};

impl ToolHandler {
    pub(in crate::mcp::tools) fn build_flow_from_named_symbols(
        &self,
        cg: &CodeGraph,
        query: &str,
    ) -> FlowInfo {
        self.build_flow_inner(cg, query)
            .unwrap_or_else(|_| FlowInfo::empty())
    }

    fn build_flow_inner(&self, cg: &CodeGraph, query: &str) -> Result<FlowInfo> {
        const MAX_HOPS: usize = 7;
        const MAX_BRIDGE: usize = 1; // ≤1 consecutive UNNAMED hop

        let tokens = extract_symbol_tokens(query);
        if tokens.len() < 2 {
            return Ok(FlowInfo::empty());
        }

        // Pool of name SEGMENTS (Class + method from every token) used to
        // disambiguate an ambiguous SIMPLE name.
        let mut seg_pool: HashSet<String> = HashSet::new();
        for t in &tokens {
            for s in QUAL_DOT_SPLIT_RE.split(&t.to_lowercase()) {
                if !s.is_empty() {
                    seg_pool.insert(s.to_string());
                }
            }
        }

        let mut named = OrderedNodeMap::new();
        // Nodes whose token is SPECIFIC — a (near-)unique callable name
        // (<= 3 defs in the whole graph).
        let mut unique_named_node_ids: HashSet<String> = HashSet::new();
        for t in &tokens {
            let cands: Vec<Node> = self
                .find_all_symbols(cg, t)?
                .nodes
                .into_iter()
                .filter(|n| is_callable_kind(n.kind))
                .collect();
            // A qualified or otherwise-specific name (<=3 hits) keeps all; an
            // ambiguous simple name keeps only candidates whose container is
            // named.
            let specific = cands.len() <= 3;
            let pick: Vec<Node> = if specific {
                cands
            } else {
                cands
                    .into_iter()
                    .filter(|n| {
                        let q = n.qualified_name.to_lowercase();
                        let segs: Vec<&str> = QUAL_DOT_SPLIT_RE
                            .split(&q)
                            .filter(|s| !s.is_empty())
                            .collect();
                        let container = if segs.len() >= 2 {
                            segs[segs.len() - 2]
                        } else {
                            ""
                        };
                        !container.is_empty() && seg_pool.contains(container)
                    })
                    .collect()
            };
            for n in pick.into_iter().take(6) {
                let id = n.id.clone();
                named.insert(n);
                if specific {
                    unique_named_node_ids.insert(id);
                }
            }
            if named.len() > 40 {
                break;
            }
        }
        if named.len() < 2 {
            return Ok(FlowInfo::empty());
        }

        struct ParentEntry {
            prev: Option<String>,
            edge: Option<Edge>,
            node: Node,
        }

        let mut best: Option<Vec<(Node, Option<Edge>)>> = None;
        // BFS the full call graph (incl. synth edges) from each named seed,
        // but only ACCEPT a sink that is also named.
        let seeds: Vec<Node> = named.values().take(8).cloned().collect();
        for seed in &seeds {
            let mut parent: HashMap<String, ParentEntry> = HashMap::new();
            parent.insert(
                seed.id.clone(),
                ParentEntry {
                    prev: None,
                    edge: None,
                    node: seed.clone(),
                },
            );
            let mut q: Vec<(String, usize, usize)> = vec![(seed.id.clone(), 0, 0)];
            let mut deep: Option<String> = None;
            let mut deep_depth = 0usize;
            let mut h = 0usize;
            while h < q.len() && parent.len() < 1500 {
                let (id, depth, streak) = q[h].clone();
                h += 1;
                if id != seed.id && named.contains(&id) && depth > deep_depth {
                    deep = Some(id.clone());
                    deep_depth = depth;
                }
                if depth >= MAX_HOPS - 1 {
                    continue;
                }
                for c in cg.get_callees(&id, None)? {
                    if c.edge.kind != EdgeKind::Calls || parent.contains_key(&c.node.id) {
                        continue;
                    }
                    let new_streak = if named.contains(&c.node.id) {
                        0
                    } else {
                        streak + 1
                    };
                    if new_streak > MAX_BRIDGE {
                        continue;
                    }
                    let cid = c.node.id.clone();
                    parent.insert(
                        cid.clone(),
                        ParentEntry {
                            prev: Some(id.clone()),
                            edge: Some(c.edge),
                            node: c.node,
                        },
                    );
                    q.push((cid, depth + 1, new_streak));
                }
            }
            let Some(deep) = deep else { continue };
            let mut chain: Vec<(Node, Option<Edge>)> = Vec::new();
            let mut cur: Option<String> = Some(deep);
            while let Some(c) = cur {
                let Some(p) = parent.get(&c) else { break };
                chain.push((p.node.clone(), p.edge.clone()));
                cur = p.prev.clone();
            }
            chain.reverse();
            if best.as_ref().map(|b| chain.len() > b.len()).unwrap_or(true) {
                best = Some(chain);
            }
        }
        let has_main = best.as_ref().map(|b| b.len() >= 3).unwrap_or(false);
        let path_ids: HashSet<String> = best
            .as_ref()
            .map(|b| b.iter().map(|(n, _)| n.id.clone()).collect())
            .unwrap_or_default();

        // Supplementary: dynamic-dispatch (synthesized) edges incident to a
        // NAMED symbol.
        let mut synth_lines: Vec<String> = Vec::new();
        let mut synth_seen: HashSet<String> = HashSet::new();
        let named_nodes: Vec<Node> = named.values().cloned().collect();
        'outer: for n in &named_nodes {
            if synth_lines.len() >= 6 {
                break;
            }
            let mut refs = cg.get_callers(&n.id, None)?;
            refs.extend(cg.get_callees(&n.id, None)?);
            for r in refs {
                if synth_lines.len() >= 6 {
                    break 'outer;
                }
                let other = r.node;
                let edge = r.edge;
                if edge.provenance != Some(Provenance::Heuristic) || other.id == n.id {
                    continue;
                }
                if path_ids.contains(&edge.source) && path_ids.contains(&edge.target) {
                    continue; // already in the main chain
                }
                let (src, tgt) = if edge.source == n.id {
                    (n, &other)
                } else {
                    (&other, n)
                };
                let key = format!("{}>{}", src.name, tgt.name);
                if !synth_seen.insert(key) {
                    continue;
                }
                let note = self.synth_edge_note(Some(&edge));
                let tag = note
                    .map(|sn| sn.compact)
                    .unwrap_or_else(|| edge.kind.as_str().to_string());
                synth_lines.push(format!("- {} → {}   [{}]", src.name, tgt.name, tag));
            }
        }

        if !has_main && synth_lines.is_empty() {
            return Ok(FlowInfo::empty());
        }
        let mut out: Vec<String> = Vec::new();
        if let Some(best) = best.as_ref().filter(|_| has_main) {
            out.push("## Flow (call path among the symbols you queried)".to_string());
            out.push(String::new());
            for (i, (node, edge)) in best.iter().enumerate() {
                if let Some(e) = edge {
                    let sy = self.synth_edge_note(Some(e));
                    let tag = sy
                        .map(|s| s.compact)
                        .unwrap_or_else(|| e.kind.as_str().to_string());
                    out.push(format!("   ↓ {tag}"));
                }
                out.push(format!(
                    "{}. {} ({}:{})",
                    i + 1,
                    node.name,
                    node.file_path,
                    node.start_line
                ));
            }
            out.push(String::new());
        }
        if !synth_lines.is_empty() {
            out.push("## Dynamic-dispatch links among your symbols".to_string());
            out.push(
                "(synthesized — the indirect hops grep/Read would reconstruct; the `@file:line` is the wiring site)"
                    .to_string(),
            );
            out.push(String::new());
            out.extend(synth_lines);
            out.push(String::new());
        }
        out.push(
            "> Full source for these symbols is below — the call flow among them, followed by their bodies."
                .to_string(),
        );
        out.push(String::new());

        let named_node_ids: HashSet<String> = named.keys().cloned().collect();
        Ok(FlowInfo {
            text: out.join("\n"),
            path_node_ids: path_ids,
            named_node_ids,
            unique_named_node_ids,
        })
    }
}
