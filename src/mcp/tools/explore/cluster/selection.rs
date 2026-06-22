use std::collections::{HashMap, HashSet};

use super::super::super::format::{ExploreOutputBudget, number_source_lines};
use super::ranges::LineRange;

const GAP_MARKER: &str = "\n\n... (gap) ...\n\n";

#[derive(Clone)]
struct Cluster {
    start: i64,
    end: i64,
    symbols: Vec<String>,
    score: i64,
    max_importance: i64,
}

struct RankedCluster {
    idx: usize,
    span: i64,
}

pub(super) fn render_selected_clusters(
    ranges: &[LineRange],
    file_lines: &[&str],
    budget: ExploreOutputBudget,
    total_chars: usize,
    with_line_numbers: bool,
) -> Option<(String, Vec<String>)> {
    let clusters = merge_clusters(ranges, budget.gap_threshold);
    let chosen_indices = choose_clusters(
        &clusters,
        file_lines,
        budget,
        total_chars,
        with_line_numbers,
    );
    if chosen_indices.is_empty() {
        return None;
    }

    let mut file_section = String::new();
    let mut all_symbols = Vec::new();
    for (i, cluster) in clusters.iter().enumerate() {
        if !chosen_indices.contains(&i) {
            continue;
        }
        let section = build_section(cluster, file_lines, with_line_numbers);
        if !file_section.is_empty() {
            file_section.push_str(GAP_MARKER);
        }
        file_section.push_str(&section);
        all_symbols.extend(cluster.symbols.iter().cloned());
    }
    Some((file_section, all_symbols))
}

fn merge_clusters(ranges: &[LineRange], gap_threshold: i64) -> Vec<Cluster> {
    let mut clusters = Vec::new();
    let mut current = Cluster {
        start: ranges[0].start,
        end: ranges[0].end,
        symbols: vec![format!("{}({})", ranges[0].name, ranges[0].kind)],
        score: ranges[0].importance,
        max_importance: ranges[0].importance,
    };
    for r in ranges.iter().skip(1) {
        if r.start <= current.end + gap_threshold {
            current.end = current.end.max(r.end);
            current.symbols.push(format!("{}({})", r.name, r.kind));
            current.score += r.importance;
            current.max_importance = current.max_importance.max(r.importance);
        } else {
            clusters.push(current);
            current = Cluster {
                start: r.start,
                end: r.end,
                symbols: vec![format!("{}({})", r.name, r.kind)],
                score: r.importance,
                max_importance: r.importance,
            };
        }
    }
    clusters.push(current);
    clusters
}

fn choose_clusters(
    clusters: &[Cluster],
    file_lines: &[&str],
    budget: ExploreOutputBudget,
    total_chars: usize,
    with_line_numbers: bool,
) -> HashSet<usize> {
    let mut ranked: Vec<RankedCluster> = clusters
        .iter()
        .enumerate()
        .map(|(i, c)| RankedCluster {
            idx: i,
            span: c.end - c.start + 1,
        })
        .collect();
    ranked.sort_by(|a, b| rank_clusters(a, b, clusters));

    let file_budget = budget
        .max_chars_per_file
        .min((budget.max_output_chars as i64 - total_chars as i64 - 200).max(0) as usize);
    let mut chosen = HashSet::new();
    let mut projected_chars = 0usize;
    for rc in &ranked {
        let gap_len = if chosen.is_empty() {
            0
        } else {
            GAP_MARKER.len()
        };
        let section_len =
            build_section(&clusters[rc.idx], file_lines, with_line_numbers).len() + gap_len;
        if chosen.is_empty() {
            chosen.insert(rc.idx);
            projected_chars += section_len;
            continue;
        }
        if projected_chars + section_len > file_budget {
            continue;
        }
        chosen.insert(rc.idx);
        projected_chars += section_len;
    }
    chosen
}

fn rank_clusters(a: &RankedCluster, b: &RankedCluster, clusters: &[Cluster]) -> std::cmp::Ordering {
    let ca = &clusters[a.idx];
    let cb = &clusters[b.idx];
    if cb.max_importance != ca.max_importance {
        return cb.max_importance.cmp(&ca.max_importance);
    }
    let density_a = ca.score as f64 / a.span as f64;
    let density_b = cb.score as f64 / b.span as f64;
    if density_b != density_a {
        return density_b
            .partial_cmp(&density_a)
            .unwrap_or(std::cmp::Ordering::Equal);
    }
    if cb.score != ca.score {
        return cb.score.cmp(&ca.score);
    }
    a.span.cmp(&b.span)
}

fn build_section(cluster: &Cluster, file_lines: &[&str], with_line_numbers: bool) -> String {
    let context_padding = 3i64;
    let start_idx = (cluster.start - 1 - context_padding).max(0) as usize;
    let end_idx = ((cluster.end + context_padding).max(0) as usize).min(file_lines.len());
    let slice = if start_idx >= end_idx {
        String::new()
    } else {
        file_lines[start_idx..end_idx].join("\n")
    };
    if with_line_numbers {
        number_source_lines(&slice, start_idx + 1)
    } else {
        slice
    }
}

pub(super) fn cluster_header(all_symbols: &[String], budget: ExploreOutputBudget) -> String {
    let mut count_order = Vec::new();
    let mut symbol_counts = HashMap::new();
    for s in all_symbols {
        if !symbol_counts.contains_key(s) {
            count_order.push(s.clone());
        }
        *symbol_counts.entry(s.clone()).or_insert(0usize) += 1;
    }
    let mut sorted_symbols = count_order;
    sorted_symbols.sort_by(|a, b| symbol_counts[b].cmp(&symbol_counts[a]));
    let header_cap = budget.max_symbols_in_file_header;
    let header_symbols: Vec<String> = sorted_symbols.iter().take(header_cap).cloned().collect();
    let omitted_count = sorted_symbols.len() as i64 - header_symbols.len() as i64;
    if omitted_count > 0 {
        format!("{}, +{} more", header_symbols.join(", "), omitted_count)
    } else {
        header_symbols.join(", ")
    }
}
