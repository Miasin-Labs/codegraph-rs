use super::{ANodeData, ANodeId, AnalysisGraph, HashMap, HashSet, is_placeholder};

/// Structural comparison of one node present in both states. Pure position
/// shifts (a function pushed down by an edit above it) are deliberately NOT
/// changes — only span *length*, byte *length*, and carried structure count.
pub(super) fn node_change_reasons(base: &ANodeData, current: &ANodeData) -> Vec<String> {
    let mut reasons: Vec<String> = Vec::new();
    let line_len = |n: &ANodeData| n.span.end_line.saturating_sub(n.span.start_line);
    if line_len(base) != line_len(current) {
        reasons.push("spanLines".to_string());
    }
    // Byte lengths only when both states carry real ranges — a pre-v5 base
    // (degraded 0..0) against a v5 current is a schema artifact, not an edit.
    if !base.span.byte_range.is_empty()
        && !current.span.byte_range.is_empty()
        && base.span.byte_range.len() != current.span.byte_range.len()
    {
        reasons.push("byteLength".to_string());
    }
    for (key, label) in [
        ("signature", "signature"),
        ("fields", "fields"),
        ("variants", "variants"),
        ("accessed_fields", "accessedFields"),
        ("async", "async"),
        ("exported", "exported"),
    ] {
        if base.metadata.get(key) != current.metadata.get(key) {
            reasons.push(label.to_string());
        }
    }
    if base.visibility != current.visibility {
        reasons.push("visibility".to_string());
    }
    reasons
}

/// All non-placeholder node ids of a graph, with their data.
pub(super) fn diffable_nodes(graph: &AnalysisGraph) -> HashMap<&ANodeId, &ANodeData> {
    graph
        .all_node_ids()
        .into_iter()
        .filter_map(|id| {
            let node = graph.get_node(id)?;
            if is_placeholder(node) {
                return None;
            }
            Some((id, node))
        })
        .collect()
}

/// Distinct edge triples of a graph, keyed for set comparison. The kind key
/// uses the engine's `Debug` form so `UnresolvedCall("name")` edges to
/// different names stay distinct; placeholder-anchored unresolved calls are
/// included (an added call to an unknown function is a real delta).
pub(super) fn edge_set(graph: &AnalysisGraph) -> HashSet<(ANodeId, ANodeId, String)> {
    let mut set = HashSet::new();
    for id in graph.all_node_ids() {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        if is_placeholder(node) {
            continue;
        }
        for (target, edge) in graph.get_edges_from(id) {
            set.insert((id.clone(), target.clone(), format!("{:?}", edge.kind)));
        }
    }
    set
}

/// Human/JSON label for an edge-set kind key (`Debug` form → camelCase).
pub(super) fn edge_key_label(kind_key: &str) -> String {
    match kind_key.split('(').next().unwrap_or(kind_key) {
        "Calls" => "calls".to_string(),
        "UnresolvedCall" => {
            // Surface the callee name: UnresolvedCall("foo") → unresolvedCall(foo)
            let name = kind_key
                .trim_start_matches("UnresolvedCall(\"")
                .trim_end_matches("\")");
            format!("unresolvedCall({name})")
        }
        "UsesType" => "usesType".to_string(),
        "References" => "references".to_string(),
        "Contains" => "contains".to_string(),
        "Implements" => "implements".to_string(),
        "ExternalCall" => "externalCall".to_string(),
        "Extends" => "extends".to_string(),
        "Returns" => "returns".to_string(),
        "TypeOf" => "typeOf".to_string(),
        other => other.to_string(),
    }
}
