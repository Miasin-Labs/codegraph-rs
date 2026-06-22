//! Relevance support for graph MCP tools.

use std::collections::{HashMap, HashSet};

use super::super::context::ToolHandler;
use crate::types::{Edge, EdgeKind};

impl ToolHandler {
    pub(in crate::mcp::tools) fn compute_graph_relevance(
        &self,
        node_ids: &[String],
        edges: &[Edge],
        seed_ids: &HashSet<String>,
    ) -> HashMap<String, f64> {
        let mut out: HashMap<String, f64> = HashMap::new();
        let n = node_ids.len();
        if n == 0 {
            return out;
        }
        let mut idx: HashMap<&str, usize> = HashMap::new();
        for (i, id) in node_ids.iter().enumerate() {
            idx.insert(id.as_str(), i);
        }

        fn rank_edge(kind: EdgeKind) -> bool {
            matches!(
                kind,
                EdgeKind::Calls
                    | EdgeKind::References
                    | EdgeKind::Extends
                    | EdgeKind::Implements
                    | EdgeKind::Overrides
                    | EdgeKind::Instantiates
                    | EdgeKind::Returns
                    | EdgeKind::TypeOf
                    | EdgeKind::Imports
            )
        }

        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for e in edges {
            if !rank_edge(e.kind) {
                continue;
            }
            let (Some(&i), Some(&j)) = (idx.get(e.source.as_str()), idx.get(e.target.as_str()))
            else {
                continue;
            };
            if i == j {
                continue;
            }
            adj[i].push(j);
            adj[j].push(i); // undirected — reachable either direction
        }

        // Restart vector: uniform over seeds present in the candidate set.
        let mut r = vec![0.0f64; n];
        let mut rsum = 0.0f64;
        for id in seed_ids {
            if let Some(&i) = idx.get(id.as_str()) {
                r[i] = 1.0;
                rsum += 1.0;
            }
        }
        if rsum == 0.0 {
            for v in r.iter_mut() {
                *v = 1.0;
            }
            rsum = n as f64;
        }
        for v in r.iter_mut() {
            *v /= rsum;
        }

        let alpha = 0.25f64;
        let mut s = r.clone();
        for _ in 0..25 {
            let mut next = vec![0.0f64; n];
            for i in 0..n {
                let si = s[i];
                if si == 0.0 {
                    continue;
                }
                let d = adj[i].len();
                if d == 0 {
                    next[i] += si; // dangling: keep its mass
                    continue;
                }
                let share = si / d as f64;
                for &j in &adj[i] {
                    next[j] += share;
                }
            }
            for i in 0..n {
                s[i] = (1.0 - alpha) * next[i] + alpha * r[i];
            }
        }
        for (i, id) in node_ids.iter().enumerate() {
            out.insert(id.clone(), s[i]);
        }
        out
    }

    // =========================================================================
    // codegraph_explore — deep exploration in a single call
}
