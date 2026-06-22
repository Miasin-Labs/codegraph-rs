use std::collections::{HashMap, HashSet};

use super::super::context::ToolHandler;
use super::super::format::{OrderedNodeMap, QUERY_MENTIONS_TESTS_RE, is_low_value};
use super::types::{FileGroup, RankedExploreFiles};
use crate::extraction::is_generated_file;
use crate::types::{Edge, NodeKind};

impl ToolHandler {
    pub(in crate::mcp::tools::explore) fn rank_explore_files(
        &self,
        query: &str,
        roots: &[String],
        edges: &[Edge],
        nodes: &OrderedNodeMap,
        named_seed_ids: &HashSet<String>,
    ) -> RankedExploreFiles {
        let mut file_order: Vec<String> = Vec::new();
        let mut file_groups: HashMap<String, FileGroup> = HashMap::new();
        let entry_node_ids: HashSet<String> = roots
            .iter()
            .cloned()
            .chain(named_seed_ids.iter().cloned())
            .collect();
        let mut connected_to_entry: HashSet<String> = HashSet::new();
        for edge in edges {
            if entry_node_ids.contains(&edge.source) {
                connected_to_entry.insert(edge.target.clone());
            }
            if entry_node_ids.contains(&edge.target) {
                connected_to_entry.insert(edge.source.clone());
            }
        }

        for node in nodes.values() {
            if node.kind == NodeKind::Import || node.kind == NodeKind::Export {
                continue;
            }
            if !file_groups.contains_key(&node.file_path) {
                file_order.push(node.file_path.clone());
            }
            let group = file_groups
                .entry(node.file_path.clone())
                .or_insert_with(|| FileGroup {
                    nodes: Vec::new(),
                    score: 0,
                });
            group.nodes.push(node.clone());
            if named_seed_ids.contains(&node.id) {
                group.score += 50;
            } else if entry_node_ids.contains(&node.id) {
                group.score += 10;
            } else if connected_to_entry.contains(&node.id) {
                group.score += 3;
            } else {
                group.score += 1;
            }
        }

        let mut relevant_files: Vec<String> = file_order
            .iter()
            .filter(|fp| file_groups[*fp].score >= 3)
            .cloned()
            .collect();
        if !QUERY_MENTIONS_TESTS_RE.is_match(query) {
            let non_low: Vec<String> = relevant_files
                .iter()
                .filter(|p| !is_low_value(p))
                .cloned()
                .collect();
            if non_low.len() >= 2 {
                relevant_files = non_low;
            }
        }

        let query_terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|t| t.chars().count() >= 3)
            .map(String::from)
            .collect();
        let mut seen_terms = HashSet::new();
        let unique_query_terms: Vec<String> = query_terms
            .iter()
            .filter(|t| t.chars().count() >= 3 && seen_terms.insert(t.as_str()))
            .cloned()
            .collect();
        let file_term_hits =
            count_file_term_hits(&relevant_files, &file_groups, &unique_query_terms);
        let node_ids: Vec<String> = nodes.keys().cloned().collect();
        let node_rwr = self.compute_graph_relevance(&node_ids, edges, &entry_node_ids);
        let (graph_score_order, file_graph_score, max_graph) =
            score_files_by_graph(nodes, &node_rwr);
        let central_files =
            choose_central_files(&graph_score_order, &file_graph_score, &file_term_hits);

        let mut entry_files: HashSet<String> = HashSet::new();
        for id in &entry_node_ids {
            if let Some(n) = nodes.get(id) {
                entry_files.insert(n.file_path.clone());
            }
        }
        if max_graph > 0.0 {
            let gated: Vec<String> = relevant_files
                .iter()
                .filter(|fp| {
                    file_graph_score.get(*fp).copied().unwrap_or(0.0) >= max_graph * 0.06
                        || central_files.contains(*fp)
                        || entry_files.contains(*fp)
                        || file_term_hits.get(*fp).copied().unwrap_or(0) >= 2
                })
                .cloned()
                .collect();
            if gated.len() >= 2 {
                relevant_files = gated;
            }
        }

        let named_seed_files: HashSet<String> = named_seed_ids
            .iter()
            .filter_map(|id| nodes.get(id).map(|n| n.file_path.clone()))
            .collect();
        let mut sorted_files = relevant_files;
        sorted_files.sort_by(|a, b| {
            use std::cmp::Ordering;
            let a_named = named_seed_files.contains(a);
            let b_named = named_seed_files.contains(b);
            if a_named != b_named {
                return b_named.cmp(&a_named);
            }
            let a_g = file_graph_score.get(a).copied().unwrap_or(0.0);
            let b_g = file_graph_score.get(b).copied().unwrap_or(0.0);
            if (a_g - b_g).abs() > max_graph * 0.01 {
                return b_g.partial_cmp(&a_g).unwrap_or(Ordering::Equal);
            }
            let a_hits = file_term_hits.get(a).copied().unwrap_or(0);
            let b_hits = file_term_hits.get(b).copied().unwrap_or(0);
            if a_hits != b_hits {
                return b_hits.cmp(&a_hits);
            }
            let a_low = is_low_value(&a.to_lowercase());
            let b_low = is_low_value(&b.to_lowercase());
            if a_low != b_low {
                return if a_low {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            let a_gen = is_generated_file(a);
            let b_gen = is_generated_file(b);
            if a_gen != b_gen {
                return if a_gen {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            let a_score = file_groups[a].score;
            let b_score = file_groups[b].score;
            if a_score != b_score {
                return b_score.cmp(&a_score);
            }
            file_groups[b].nodes.len().cmp(&file_groups[a].nodes.len())
        });

        RankedExploreFiles {
            file_order,
            file_groups,
            entry_node_ids,
            connected_to_entry,
            central_files,
            sorted_files,
        }
    }
}

fn count_file_term_hits(
    relevant_files: &[String],
    file_groups: &HashMap<String, FileGroup>,
    unique_query_terms: &[String],
) -> HashMap<String, usize> {
    let mut hits_by_file = HashMap::new();
    for fp in relevant_files {
        let group = &file_groups[fp];
        let hay = format!(
            "{} {}",
            fp.to_lowercase(),
            group
                .nodes
                .iter()
                .map(|n| n.name.to_lowercase())
                .collect::<Vec<_>>()
                .join(" ")
        );
        let hits = unique_query_terms
            .iter()
            .filter(|t| hay.contains(t.as_str()))
            .count();
        hits_by_file.insert(fp.clone(), hits);
    }
    hits_by_file
}

fn score_files_by_graph(
    nodes: &OrderedNodeMap,
    node_rwr: &HashMap<String, f64>,
) -> (Vec<String>, HashMap<String, f64>, f64) {
    let mut graph_score_order: Vec<String> = Vec::new();
    let mut file_graph_score: HashMap<String, f64> = HashMap::new();
    for node in nodes.values() {
        if !file_graph_score.contains_key(&node.file_path) {
            graph_score_order.push(node.file_path.clone());
        }
        *file_graph_score
            .entry(node.file_path.clone())
            .or_insert(0.0) += node_rwr.get(&node.id).copied().unwrap_or(0.0);
    }
    let max_graph = file_graph_score.values().fold(0.0f64, |a, &b| a.max(b));
    (graph_score_order, file_graph_score, max_graph)
}

fn choose_central_files(
    graph_score_order: &[String],
    file_graph_score: &HashMap<String, f64>,
    file_term_hits: &HashMap<String, usize>,
) -> HashSet<String> {
    let mut entries: Vec<(&String, f64)> = graph_score_order
        .iter()
        .map(|fp| (fp, file_graph_score[fp]))
        .filter(|(fp, g)| *g > 0.0 && file_term_hits.get(*fp).copied().unwrap_or(0) >= 1)
        .collect();
    entries.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                file_term_hits
                    .get(b.0)
                    .copied()
                    .unwrap_or(0)
                    .cmp(&file_term_hits.get(a.0).copied().unwrap_or(0))
            })
    });
    entries
        .into_iter()
        .take(2)
        .map(|(f, _)| f.clone())
        .collect()
}
