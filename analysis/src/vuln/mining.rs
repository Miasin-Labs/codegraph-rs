//! Deviant-frequency mining: discover guard→sink relationships from corpus
//! consistency, then flag the call sites that deviate.
//!
//! The rule-free core of the engine. For a candidate **sink** function (one
//! called from many places), look at every caller and the guards it invokes
//! *before* the sink call. If some guard precedes the sink in almost every
//! caller but not all, the callers that skip it are anomalies — and the guard
//! was *discovered*, never named. This is the Engler "bugs as deviant behavior"
//! idea over the call graph: the de-facto authorization/validation gate is
//! whatever dominates the sink across the corpus.
//!
//! Uses only the resolved call graph (`EdgeKind::Calls`) plus each edge's
//! call-site line for before/after ordering — no source re-parsing, scales to
//! the whole graph. Class labels (BAC / IDOR) are a name-lexicon *annotation*
//! on top of the structural finding, not part of detection.

use std::collections::{HashMap, HashSet};

use super::{InferenceOrigin, TemplateKind, VulnFinding};
use crate::edges::EdgeKind;
use crate::graph::CodeGraph;
use crate::nodes::{NodeId, NodeKind};

/// Tunables for the miner. Defaults are deliberately conservative — a small
/// corpus has no reliable "norm", so we require real support before flagging.
#[derive(Debug, Clone)]
pub struct MineConfig {
    /// A sink must be called from at least this many distinct callers before
    /// its guard norm is considered meaningful.
    pub min_callers: usize,
    /// A guard must precede the sink at at least this many call sites to count
    /// as a norm (suppresses 2-of-3 coincidences).
    pub min_support: u32,
    /// Fraction of callers that must share the guard for it to be the norm.
    /// Must be `< 1.0` in effect — a guard present everywhere has no deviants.
    pub threshold: f64,
}

impl Default for MineConfig {
    fn default() -> Self {
        Self {
            min_callers: 4,
            min_support: 3,
            threshold: 0.75,
        }
    }
}

/// Mine the graph for sinks whose guard norm is violated at some call site.
///
/// One [`VulnFinding`] per `(site, sink)` deviation, merging all guards the
/// site is missing. Sorted by confidence descending (strongest norm first).
pub fn mine_missing_guards(graph: &CodeGraph, config: &MineConfig) -> Vec<VulnFinding> {
    // Merge key: a single caller→sink site may be missing several guards; we
    // emit one finding listing them all.
    let mut merged: HashMap<(NodeId, NodeId), Deviation> = HashMap::new();

    for sink in graph.nodes_by_kind(NodeKind::Function) {
        let sink_id = &sink.id;

        // Distinct callers of this sink, with the earliest call-site line each
        // (a guard must precede the *first* reach to count).
        let mut caller_sink_line: HashMap<NodeId, u32> = HashMap::new();
        for (src, edge) in graph.get_edges_to(sink_id) {
            if !is_call(&edge.kind) || src == sink_id {
                continue;
            }
            let line = edge.source_span.start_line;
            caller_sink_line
                .entry(src.clone())
                .and_modify(|l| *l = (*l).min(line))
                .or_insert(line);
        }
        let total = caller_sink_line.len();
        if total < config.min_callers {
            continue;
        }

        // guard candidate -> set of callers that invoke it before the sink.
        let mut guard_callers: HashMap<NodeId, HashSet<NodeId>> = HashMap::new();
        for (caller, &sink_line) in &caller_sink_line {
            for (callee, edge) in graph.get_edges_from(caller) {
                if !is_call(&edge.kind) || callee == sink_id || callee == caller {
                    continue;
                }
                if edge.source_span.start_line < sink_line {
                    guard_callers
                        .entry(callee.clone())
                        .or_default()
                        .insert(caller.clone());
                }
            }
        }

        for (guard, callers_with) in &guard_callers {
            let support = callers_with.len();
            let freq = support as f64 / total as f64;
            // A norm: shared by most callers, but not all (else no deviant).
            if support < config.min_support as usize || freq < config.threshold || support >= total
            {
                continue;
            }
            // Every caller missing this guard is a deviation.
            for caller in caller_sink_line.keys() {
                if callers_with.contains(caller) {
                    continue;
                }
                let dev = merged
                    .entry((caller.clone(), sink_id.clone()))
                    .or_insert_with(|| Deviation {
                        total: total as u32,
                        ..Deviation::default()
                    });
                dev.expected.push(guard.clone());
                dev.support = dev.support.max(support as u32);
                dev.confidence = dev.confidence.max(freq);
            }
        }
    }

    let mut findings: Vec<VulnFinding> = merged
        .into_iter()
        .map(|((site, sink), dev)| build_finding(graph, site, sink, dev))
        .collect();
    findings.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.site.0.cmp(&b.site.0))
            .then_with(|| a.sink.0.cmp(&b.sink.0))
    });
    findings
}

/// Accumulator for one `(site, sink)` deviation while merging guards.
#[derive(Default)]
struct Deviation {
    expected: Vec<NodeId>,
    support: u32,
    total: u32,
    confidence: f64,
}

fn build_finding(graph: &CodeGraph, site: NodeId, sink: NodeId, dev: Deviation) -> VulnFinding {
    let site_name = node_name(graph, &site);
    let sink_name = node_name(graph, &sink);
    let guard_names: Vec<String> = dev.expected.iter().map(|g| node_name(graph, g)).collect();

    // Class label: name-lexicon annotation over the structural deviation.
    let class = infer_class(&guard_names, &sink_name);
    let guards_joined = guard_names.join("`, `");
    let pct = (dev.confidence * 100.0).round() as u32;
    let class_suffix = match &class {
        Some(c) => format!("; possible {c}"),
        None => String::new(),
    };
    let message = format!(
        "`{site_name}` reaches `{sink_name}` without `{guards_joined}`, which guards \
         {support}/{total} of the other call sites ({pct}% of callers){class_suffix}",
        support = dev.support,
        total = dev.total,
    );

    VulnFinding {
        template: TemplateKind::MissingDominatorCheck,
        class,
        site,
        sink,
        expected: dev.expected,
        support: dev.support,
        total: dev.total,
        confidence: dev.confidence,
        origin: InferenceOrigin::Frequency,
        message,
    }
}

fn is_call(kind: &EdgeKind) -> bool {
    matches!(kind, EdgeKind::Calls)
}

fn node_name(graph: &CodeGraph, id: &NodeId) -> String {
    graph
        .get_node(id)
        .map(|n| n.name.clone())
        .unwrap_or_else(|| format!("node#{}", id.0))
}

/// Lowercased substrings that read as an authorization / validation guard.
const AUTH_LEXICON: &[&str] = &[
    "auth",
    "authz",
    "permission",
    "permit",
    "require",
    "ensure",
    "verify",
    "validate",
    "guard",
    "admin",
    "owner",
    "access",
    "allow",
    "role",
    "can_",
    "is_allowed",
    "check",
    "csrf",
    "forbid",
    "login",
    "session",
    "privilege",
];

/// Lowercased substrings that read as a read-style resource fetch (→ IDOR when
/// the missing guard is an authorization check).
const RESOURCE_READ_LEXICON: &[&str] = &[
    "get", "find", "load", "fetch", "read", "select", "query", "by_id", "lookup", "retrieve",
    "show", "view",
];

/// Annotate a structural deviation with a human class label. Pure naming
/// heuristic; absence of a label never suppresses the finding.
fn infer_class(guard_names: &[String], sink_name: &str) -> Option<String> {
    let any_auth = guard_names.iter().any(|g| lexicon_hit(g, AUTH_LEXICON));
    if !any_auth {
        return None;
    }
    if lexicon_hit(sink_name, RESOURCE_READ_LEXICON) {
        Some("IDOR".to_owned())
    } else {
        Some("BAC".to_owned())
    }
}

fn lexicon_hit(name: &str, lexicon: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    lexicon.iter().any(|term| lower.contains(term))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::edges::{EdgeData, EdgeKind};
    use crate::nodes::{NodeData, NodeKind, Span, Visibility};

    fn span_at(line: u32) -> Span {
        Span {
            file: PathBuf::from("src/h.rs"),
            start_line: line,
            start_col: 0,
            end_line: line,
            end_col: 1,
            byte_range: 0..1,
        }
    }

    fn func(name: &str) -> NodeData {
        NodeData {
            id: NodeId::new("src/h.rs", &format!("crate::{name}"), NodeKind::Function),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: format!("crate::{name}"),
            file_path: PathBuf::from("src/h.rs"),
            span: span_at(1),
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            birth_revision: 0,
            last_modified_revision: 0,
            complexity: None,
            cfg: None,
            dataflow: None,
        }
    }

    fn call_at(line: u32) -> EdgeData {
        EdgeData {
            kind: EdgeKind::Calls,
            source_span: span_at(line),
            weight: 1.0,
        }
    }

    /// Build a graph: `callers` each call `sink`; those in `guarded` also call
    /// `guard` *before* the sink. Returns the populated graph.
    fn scenario(guard: &str, sink: &str, callers: &[&str], guarded: &[&str]) -> CodeGraph {
        let mut g = CodeGraph::new();
        let guard_id = g.add_node(func(guard));
        let sink_id = g.add_node(func(sink));
        for caller in callers {
            let caller_id = g.add_node(func(caller));
            // Everyone logs first (a guard present at 100% — must NOT be flagged).
            let log_id = id_of("log");
            if g.get_node(&log_id).is_none() {
                g.add_node(func("log"));
            }
            g.add_edge(&caller_id, &log_id, call_at(1)).unwrap();
            if guarded.contains(caller) {
                g.add_edge(&caller_id, &guard_id, call_at(2)).unwrap();
            }
            g.add_edge(&caller_id, &sink_id, call_at(5)).unwrap();
        }
        g
    }

    fn id_of(name: &str) -> NodeId {
        NodeId::new("src/h.rs", &format!("crate::{name}"), NodeKind::Function)
    }

    #[test]
    fn flags_the_caller_missing_the_inferred_guard() {
        // 4 of 5 handlers guard delete_user with require_admin; handler5 doesn't.
        let g = scenario(
            "require_admin",
            "delete_user",
            &["h1", "h2", "h3", "h4", "h5"],
            &["h1", "h2", "h3", "h4"],
        );
        let findings = mine_missing_guards(&g, &MineConfig::default());
        assert_eq!(findings.len(), 1, "got {findings:#?}");
        let f = &findings[0];
        assert_eq!(f.template, TemplateKind::MissingDominatorCheck);
        assert_eq!(f.site, id_of("h5"));
        assert_eq!(f.sink, id_of("delete_user"));
        assert_eq!(f.expected, vec![id_of("require_admin")]);
        assert_eq!(f.support, 4);
        assert_eq!(f.total, 5);
        // require_admin is auth; delete_user is not a read -> BAC.
        assert_eq!(f.class.as_deref(), Some("BAC"));
    }

    #[test]
    fn labels_read_sink_as_idor() {
        let g = scenario(
            "check_owner",
            "find_document_by_id",
            &["a", "b", "c", "d", "e"],
            &["a", "b", "c", "d"],
        );
        let findings = mine_missing_guards(&g, &MineConfig::default());
        assert_eq!(findings.len(), 1, "got {findings:#?}");
        assert_eq!(findings[0].class.as_deref(), Some("IDOR"));
    }

    #[test]
    fn does_not_flag_when_all_callers_guarded() {
        let g = scenario(
            "require_admin",
            "delete_user",
            &["h1", "h2", "h3", "h4", "h5"],
            &["h1", "h2", "h3", "h4", "h5"],
        );
        assert!(mine_missing_guards(&g, &MineConfig::default()).is_empty());
    }

    #[test]
    fn does_not_flag_below_min_callers() {
        // Only 3 callers — below the default min_callers of 4: no reliable norm.
        let g = scenario(
            "require_admin",
            "delete_user",
            &["h1", "h2", "h3"],
            &["h1", "h2"],
        );
        assert!(mine_missing_guards(&g, &MineConfig::default()).is_empty());
    }

    #[test]
    fn ubiquitous_guard_is_not_a_deviation() {
        // `log` is called by 100% of callers; it must never produce a finding
        // even though it precedes the sink everywhere.
        let g = scenario(
            "require_admin",
            "delete_user",
            &["h1", "h2", "h3", "h4", "h5"],
            &["h1", "h2", "h3", "h4"],
        );
        let findings = mine_missing_guards(&g, &MineConfig::default());
        assert!(findings.iter().all(|f| !f.expected.contains(&id_of("log"))));
    }
}
