use super::{ANodeId, AnalysisGraph, HashMap, analysis};

/// SCC clusters keyed by their sorted member ids, for set comparison.
pub(super) fn cycle_keys(graph: &AnalysisGraph) -> HashMap<Vec<ANodeId>, Vec<ANodeId>> {
    analysis::find_mutual_recursion(graph)
        .into_iter()
        .map(|cluster| {
            let mut key = cluster.members.clone();
            key.sort();
            (key, cluster.members)
        })
        .collect()
}
