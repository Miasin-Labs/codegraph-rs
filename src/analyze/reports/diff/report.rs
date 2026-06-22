use super::{
    ANodeData,
    ANodeId,
    ANodeKind,
    AnalysisGraph,
    BTreeMap,
    BaseSnapshot,
    ChangedFunctionDelta,
    ChangedNode,
    CycleSummary,
    DIFF_IMPACT_WALK_CAP,
    DeltaImpact,
    DiffBaseDescriptor,
    DiffEdge,
    DiffReport,
    HashMap,
    HashSet,
    StoredComplexity,
    SymbolRef,
    TraversalConfig,
    TraversalDirection,
    classify_cycle,
    cycle_keys,
    diffable_nodes,
    edge_key_label,
    edge_set,
    is_placeholder,
    node_change_reasons,
    symbol_ref,
    symbol_sort_key,
    traverse,
};

/// Compare the current bridged graph against a base snapshot: nodes/edges
/// added/removed/changed, complexity deltas for changed functions,
/// newly-introduced cycles, and the impact set of the delta (incoming-edge
/// BFS from every delta node — added/changed walked in the current graph,
/// removed walked in the base).
///
/// `current_complexity` is the working tree's measurement
/// ([`measure_complexity_map`]); base complexity comes from the snapshot's
/// sidecar ([`BaseSnapshot::complexity`]) when a prior `analyze diff` wrote
/// one, and is honestly reported as unavailable otherwise.
pub fn diff_report(
    base: &BaseSnapshot,
    current: &AnalysisGraph,
    current_complexity: &HashMap<ANodeId, StoredComplexity>,
    impact_depth: usize,
    top: usize,
) -> DiffReport {
    let base_nodes = diffable_nodes(&base.graph);
    let current_nodes = diffable_nodes(current);

    let mut nodes_added: Vec<SymbolRef> = Vec::new();
    let mut nodes_removed: Vec<SymbolRef> = Vec::new();
    let mut nodes_changed: Vec<ChangedNode> = Vec::new();
    let mut delta_current_ids: Vec<ANodeId> = Vec::new(); // added + changed
    let mut delta_base_ids: Vec<ANodeId> = Vec::new(); // removed
    let mut changed_fn_ids: Vec<ANodeId> = Vec::new();

    for (id, node) in &current_nodes {
        match base_nodes.get(*id) {
            None => {
                nodes_added.push(symbol_ref(node));
                delta_current_ids.push((*id).clone());
                if node.kind == ANodeKind::Function {
                    changed_fn_ids.push((*id).clone());
                }
            }
            Some(base_node) => {
                let reasons = node_change_reasons(base_node, node);
                if !reasons.is_empty() {
                    nodes_changed.push(ChangedNode {
                        symbol: symbol_ref(node),
                        reasons,
                    });
                    delta_current_ids.push((*id).clone());
                    if node.kind == ANodeKind::Function {
                        changed_fn_ids.push((*id).clone());
                    }
                }
            }
        }
    }
    for (id, node) in &base_nodes {
        if !current_nodes.contains_key(*id) {
            nodes_removed.push(symbol_ref(node));
            delta_base_ids.push((*id).clone());
        }
    }
    nodes_added.sort();
    nodes_removed.sort();
    nodes_changed.sort_by(|a, b| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)));

    // --- Edges ---------------------------------------------------------------
    let base_edges = edge_set(&base.graph);
    let current_edges = edge_set(current);
    let render_edge = |graph_a: &AnalysisGraph,
                       graph_b: &AnalysisGraph,
                       (from, to, kind): &(ANodeId, ANodeId, String)|
     -> Option<DiffEdge> {
        let lookup = |id: &ANodeId| {
            graph_a
                .get_node(id)
                .or_else(|| graph_b.get_node(id))
                .map(|n| n.qualified_name.clone())
        };
        Some(DiffEdge {
            from: lookup(from)?,
            to: lookup(to)?,
            kind: edge_key_label(kind),
        })
    };
    let mut edges_added: Vec<DiffEdge> = current_edges
        .difference(&base_edges)
        .filter_map(|e| render_edge(current, &base.graph, e))
        .collect();
    let mut edges_removed: Vec<DiffEdge> = base_edges
        .difference(&current_edges)
        .filter_map(|e| render_edge(&base.graph, current, e))
        .collect();
    let edge_sort =
        |a: &DiffEdge, b: &DiffEdge| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind));
    edges_added.sort_by(edge_sort);
    edges_removed.sort_by(edge_sort);

    // --- Complexity deltas for changed/added functions ------------------------
    changed_fn_ids.sort();
    let mut changed_functions: Vec<ChangedFunctionDelta> = changed_fn_ids
        .iter()
        .filter_map(|id| {
            let node = current.get_node(id)?;
            let line_len = |n: &ANodeData| n.span.end_line.saturating_sub(n.span.start_line) + 1;
            let before = base.complexity.get(id);
            let after = current_complexity.get(id);
            let delta = |b: Option<u32>, a: Option<u32>| match (b, a) {
                (Some(b), Some(a)) => Some(i64::from(a) - i64::from(b)),
                _ => None,
            };
            Some(ChangedFunctionDelta {
                symbol: symbol_ref(node),
                lines_before: base.graph.get_node(id).map(line_len).unwrap_or(0),
                lines_after: line_len(node),
                cyclomatic_before: before.map(|c| c.cyclomatic),
                cyclomatic_after: after.map(|c| c.cyclomatic),
                cyclomatic_delta: delta(before.map(|c| c.cyclomatic), after.map(|c| c.cyclomatic)),
                cognitive_before: before.map(|c| c.cognitive),
                cognitive_after: after.map(|c| c.cognitive),
                cognitive_delta: delta(before.map(|c| c.cognitive), after.map(|c| c.cognitive)),
            })
        })
        .collect();
    changed_functions.sort_by(|a, b| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)));

    // --- Newly-introduced cycles ----------------------------------------------
    let base_cycles = cycle_keys(&base.graph);
    let current_cycles = cycle_keys(current);
    let mut new_cycles: Vec<CycleSummary> = current_cycles
        .iter()
        .filter(|(key, _)| !base_cycles.contains_key(*key))
        .map(|(_, members)| {
            let mut refs: Vec<SymbolRef> = members
                .iter()
                .filter_map(|id| current.get_node(id))
                .map(symbol_ref)
                .collect();
            refs.sort();
            CycleSummary {
                size: refs.len(),
                kind: classify_cycle(&refs),
                members: refs,
            }
        })
        .collect();
    new_cycles.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.members.cmp(&b.members)));
    let resolved_cycle_count = base_cycles
        .keys()
        .filter(|key| !current_cycles.contains_key(*key))
        .count();

    // --- Impact set of the delta ----------------------------------------------
    let delta_ids: HashSet<&ANodeId> = delta_current_ids
        .iter()
        .chain(delta_base_ids.iter())
        .collect();
    let config = TraversalConfig {
        max_depth: impact_depth,
        max_nodes: DIFF_IMPACT_WALK_CAP,
        direction: TraversalDirection::Incoming,
        parallel: false,
    };
    let mut impacted: BTreeMap<ANodeId, SymbolRef> = BTreeMap::new();
    let mut walk_into = |graph: &AnalysisGraph, seed: &ANodeId| {
        for id in traverse(graph, seed, &config).nodes {
            if delta_ids.contains(&id) || impacted.contains_key(&id) {
                continue;
            }
            let Some(node) = graph.get_node(&id) else {
                continue;
            };
            if is_placeholder(node) {
                continue;
            }
            impacted.insert(id, symbol_ref(node));
        }
    };
    for id in &delta_current_ids {
        walk_into(current, id);
    }
    for id in &delta_base_ids {
        walk_into(&base.graph, id);
    }
    let mut impact_nodes: Vec<SymbolRef> = impacted.into_values().collect();
    impact_nodes.sort_by(|a, b| symbol_sort_key(a).cmp(&symbol_sort_key(b)));
    let impacted_count = impact_nodes.len();
    let impact_truncated = impact_nodes.len() > top;
    impact_nodes.truncate(top);

    // --- Counts, truncation, note ----------------------------------------------
    let nodes_added_count = nodes_added.len();
    let nodes_removed_count = nodes_removed.len();
    let nodes_changed_count = nodes_changed.len();
    let edges_added_count = edges_added.len();
    let edges_removed_count = edges_removed.len();
    let truncated = nodes_added_count > top
        || nodes_removed_count > top
        || nodes_changed_count > top
        || edges_added_count > top
        || edges_removed_count > top;
    nodes_added.truncate(top);
    nodes_removed.truncate(top);
    nodes_changed.truncate(top);
    edges_added.truncate(top);
    edges_removed.truncate(top);

    let mut note = "Change detection is structural: span/byte length, signature, carried \
                    fields/variants, visibility. Pure position shifts (code moved by edits \
                    elsewhere in the file) are not counted; a same-length in-place edit that \
                    alters none of those is invisible to the diff."
        .to_string();
    if base.complexity.is_empty() && !changed_functions.is_empty() {
        note.push_str(
            " Base complexity is unavailable (the base snapshot predates a diff run); this \
             run annotated the current snapshot, so the next `analyze diff` against it will \
             report full before/after deltas.",
        );
    }

    DiffReport {
        base: DiffBaseDescriptor {
            source: base.generation.as_str().to_string(),
            index_fingerprint: base.index_fingerprint.map(|fp| format!("{fp:016x}")),
        },
        nodes_added_count,
        nodes_removed_count,
        nodes_changed_count,
        nodes_added,
        nodes_removed,
        nodes_changed,
        edges_added_count,
        edges_removed_count,
        edges_added,
        edges_removed,
        truncated,
        changed_functions,
        new_cycle_count: new_cycles.len(),
        new_cycles,
        resolved_cycle_count,
        impact: DeltaImpact {
            depth: impact_depth,
            impacted_count,
            truncated: impact_truncated,
            nodes: impact_nodes,
        },
        note,
    }
}
