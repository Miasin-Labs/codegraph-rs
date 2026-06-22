use super::*;

// =============================================================================
// analyze communities
// =============================================================================

/// One detected community (size ≥ 2).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunitySummary {
    /// Louvain community label.
    pub id: u32,
    pub size: usize,
    /// Up to 3 files with the most members.
    pub top_files: Vec<String>,
    /// Up to `sample` members, name-sorted.
    pub members: Vec<SymbolRef>,
    /// True when `members` was capped below `size`.
    pub truncated: bool,
}

/// Result of [`communities_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunitiesReport {
    /// Total Louvain communities (singletons included).
    pub community_count: u32,
    /// Final modularity score in [-0.5, 1.0].
    pub modularity: f64,
    /// Communities with ≥ 2 members (the interesting ones — Louvain runs on
    /// call edges, so symbols without call relationships stay singletons).
    pub multi_member_count: usize,
    pub singleton_count: usize,
    pub communities: Vec<CommunitySummary>,
}

/// Louvain community detection over the call graph (deterministic seed).
pub fn communities_report(graph: &AnalysisGraph, sample: usize) -> CommunitiesReport {
    let result = louvain(graph, LOUVAIN_RESOLUTION, LOUVAIN_SEED);

    let mut groups: BTreeMap<u32, Vec<&ANodeData>> = BTreeMap::new();
    for (id, label) in &result.assignments {
        if let Some(node) = graph.get_node(id) {
            groups.entry(*label).or_default().push(node);
        }
    }

    let singleton_count = groups.values().filter(|m| m.len() < 2).count();
    let mut communities: Vec<CommunitySummary> = groups
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .map(|(label, members)| {
            let mut file_counts: BTreeMap<String, usize> = BTreeMap::new();
            for m in &members {
                *file_counts
                    .entry(m.file_path.display().to_string())
                    .or_default() += 1;
            }
            let mut files: Vec<(String, usize)> = file_counts.into_iter().collect();
            files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let top_files: Vec<String> = files.into_iter().take(3).map(|(f, _)| f).collect();

            let size = members.len();
            let mut refs: Vec<SymbolRef> = members.iter().map(|m| symbol_ref(m)).collect();
            refs.sort();
            let truncated = refs.len() > sample;
            refs.truncate(sample);

            CommunitySummary {
                id: label,
                size,
                top_files,
                members: refs,
                truncated,
            }
        })
        .collect();
    communities.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.id.cmp(&b.id)));

    CommunitiesReport {
        community_count: result.community_count,
        modularity: result.modularity,
        multi_member_count: communities.len(),
        singleton_count,
        communities,
    }
}
