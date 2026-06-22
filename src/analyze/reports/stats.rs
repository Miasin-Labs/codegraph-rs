use super::*;

// =============================================================================
// analyze stats
// =============================================================================

/// Above this node count, reachability estimates use the engine's
/// HyperLogLog sketches (ANF-style, ~2% standard error); at or below it,
/// exact per-node BFS counts are computed instead — small graphs get exact
/// numbers, huge graphs get the estimator they need.
const HLL_EXACT_THRESHOLD: usize = 5_000;

/// Per-node reachability counts (exact or estimated — see `method`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReachabilityEntry {
    pub symbol: SymbolRef,
    /// Nodes reachable from this one (any edge kind).
    pub descendants: f64,
    /// Nodes that can reach this one.
    pub ancestors: f64,
}

/// Reachability section of [`StatsReport`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReachabilitySection {
    /// `exact` (BFS per node) or `hyperloglog` (ANF sketches).
    pub method: String,
    /// Node count above which the HLL estimator is used.
    pub exact_threshold: usize,
    /// Top nodes by descendant count.
    pub top: Vec<ReachabilityEntry>,
    pub note: String,
}

/// Result of [`stats_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsReport {
    pub node_count: usize,
    pub edge_count: usize,
    /// Distinct files contributing nodes (placeholders excluded).
    pub file_count: usize,
    /// Placeholder nodes anchoring unresolved calls.
    pub placeholder_count: usize,
    pub nodes_by_kind: BTreeMap<String, usize>,
    pub edges_by_kind: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reachability: Option<ReachabilitySection>,
}

/// Exact per-node reachability via BFS in both directions. O(V·(V+E)) —
/// only used at or below [`HLL_EXACT_THRESHOLD`].
fn exact_reachability(graph: &AnalysisGraph) -> HashMap<ANodeId, (f64, f64)> {
    let ids: Vec<&ANodeId> = graph.all_node_ids();
    let index_of: HashMap<&ANodeId, usize> =
        ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();

    let mut forward: Vec<Vec<usize>> = vec![Vec::new(); ids.len()];
    let mut backward: Vec<Vec<usize>> = vec![Vec::new(); ids.len()];
    for (i, id) in ids.iter().enumerate() {
        for (target, _) in graph.get_edges_from(id) {
            if let Some(&j) = index_of.get(target) {
                forward[i].push(j);
                backward[j].push(i);
            }
        }
    }

    let bfs_count = |adjacency: &[Vec<usize>], start: usize| -> usize {
        let mut seen = vec![false; adjacency.len()];
        seen[start] = true;
        let mut queue = std::collections::VecDeque::from([start]);
        let mut count = 0usize;
        while let Some(current) = queue.pop_front() {
            for &next in &adjacency[current] {
                if !seen[next] {
                    seen[next] = true;
                    count += 1;
                    queue.push_back(next);
                }
            }
        }
        count
    };

    ids.iter()
        .enumerate()
        .map(|(i, id)| {
            (
                (*id).clone(),
                (
                    bfs_count(&forward, i) as f64,
                    bfs_count(&backward, i) as f64,
                ),
            )
        })
        .collect()
}

/// Bridged-graph statistics, with optional reachability profiling — exact at
/// small scale, the engine's HyperLogLog estimator
/// (`hll::approximate_reachability`) above [`HLL_EXACT_THRESHOLD`].
pub fn stats_report(graph: &AnalysisGraph, estimate_reachability: bool, top: usize) -> StatsReport {
    let mut nodes_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut edges_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut files: HashSet<&Path> = HashSet::new();
    let mut placeholder_count = 0usize;

    for id in graph.all_node_ids() {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        if is_placeholder(node) {
            placeholder_count += 1;
        } else {
            files.insert(node.file_path.as_path());
        }
        *nodes_by_kind.entry(kind_label(node.kind)).or_default() += 1;
        for (_, edge) in graph.get_edges_from(id) {
            *edges_by_kind
                .entry(edge_kind_label(&edge.kind).to_string())
                .or_default() += 1;
        }
    }

    let reachability = estimate_reachability.then(|| {
        let node_count = graph.node_count();
        let (method, counts, note) = if node_count <= HLL_EXACT_THRESHOLD {
            (
                "exact",
                exact_reachability(graph),
                format!(
                    "Exact BFS counts — the graph ({node_count} nodes) is at or below the \
                     {HLL_EXACT_THRESHOLD}-node threshold where the HyperLogLog estimator \
                     takes over."
                ),
            )
        } else {
            let estimates = approximate_reachability(graph);
            let mut counts: HashMap<ANodeId, (f64, f64)> = HashMap::new();
            for (id, descendants) in estimates.descendant_count {
                counts.entry(id).or_insert((0.0, 0.0)).0 = descendants.max(0.0);
            }
            for (id, ancestors) in estimates.ancestor_count {
                counts.entry(id).or_insert((0.0, 0.0)).1 = ancestors.max(0.0);
            }
            (
                "hyperloglog",
                counts,
                "HyperLogLog (ANF) estimates with ~2% standard error — exact per-node counts \
                 would cost O(V·(V+E)) at this scale."
                    .to_string(),
            )
        };

        let mut entries: Vec<ReachabilityEntry> = counts
            .into_iter()
            .filter_map(|(id, (descendants, ancestors))| {
                let node = graph.get_node(&id)?;
                if is_placeholder(node) {
                    return None;
                }
                Some(ReachabilityEntry {
                    symbol: symbol_ref(node),
                    descendants,
                    ancestors,
                })
            })
            .collect();
        entries.sort_by(|a, b| {
            b.descendants
                .total_cmp(&a.descendants)
                .then_with(|| b.ancestors.total_cmp(&a.ancestors))
                .then_with(|| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)))
        });
        entries.truncate(top);

        ReachabilitySection {
            method: method.to_string(),
            exact_threshold: HLL_EXACT_THRESHOLD,
            top: entries,
            note,
        }
    });

    StatsReport {
        node_count: graph.node_count(),
        edge_count: graph.edge_count(),
        file_count: files.len(),
        placeholder_count,
        nodes_by_kind,
        edges_by_kind,
        reachability,
    }
}
