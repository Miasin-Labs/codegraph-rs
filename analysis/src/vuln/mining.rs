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

use super::{InferenceOrigin, TemplateKind, VulnFinding, cfg_dominance};
use crate::cfg::FunctionCfg;
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
    let min_callers = config.min_callers.max(4);
    let min_support = config.min_support.max(3);
    let threshold = config.threshold.clamp(0.0, 1.0);

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
        if total < min_callers {
            continue;
        }

        // guard candidate -> set of callers that invoke it before the sink.
        let mut guard_callers: HashMap<NodeId, HashSet<NodeId>> = HashMap::new();
        for (caller, &sink_line) in &caller_sink_line {
            // Prefer real control-flow dominance over textual ordering when the
            // caller carries a CFG — a guard inside a conditional branch must
            // not count as protecting a sink that follows the branch.
            let caller_cfg = graph.get_node(caller).and_then(|n| n.cfg.as_ref());
            for (callee, edge) in graph.get_edges_from(caller) {
                if !is_call(&edge.kind) || callee == sink_id || callee == caller {
                    continue;
                }
                if guard_precedes_sink(caller_cfg, edge.source_span.start_line, sink_line) {
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
            if support < min_support as usize || freq < threshold || support >= total {
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

fn build_finding(graph: &CodeGraph, site: NodeId, sink: NodeId, mut dev: Deviation) -> VulnFinding {
    dev.expected.sort_by_key(|id| (node_name(graph, id), id.0));
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

/// Whether the call at `guard_line` is guaranteed to run before the call at
/// `sink_line` within the caller. With a CFG, "before" means **control-flow
/// dominance** — the guard lies on every path from entry to the sink; a guard
/// buried in a conditional branch therefore does not count. Without a CFG (or
/// when a line can't be mapped to a block) it falls back to textual ordering,
/// so the check only ever sharpens the corpus norm.
fn guard_precedes_sink(caller_cfg: Option<&FunctionCfg>, guard_line: u32, sink_line: u32) -> bool {
    match caller_cfg {
        Some(cfg) => cfg_dominance::dominates_by_line(cfg, guard_line, sink_line)
            .unwrap_or(guard_line < sink_line),
        None => guard_line < sink_line,
    }
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

    /// End-to-end control-flow awareness: 4 of 5 callers guard `delete_user`
    /// with `require_admin` on a dominating path; the 5th calls `require_admin`
    /// only *inside an `if`* — textually before the sink but not on every path.
    /// Line ordering would wrongly call h5 "guarded" (no finding); real CFG
    /// dominance exposes it as the deviant.
    #[test]
    fn cfg_dominance_flags_guard_buried_in_branch() {
        use tree_sitter::Parser;

        use crate::cfg::build_cfg;

        let src = "fn h5(cond: bool) {\n    if cond {\n        require_admin();\n    }\n    delete_user();\n}\n";
        let guard_line = (src[..src.find("require_admin").unwrap()]
            .matches('\n')
            .count()
            + 1) as u32;
        let sink_line = (src[..src.find("delete_user").unwrap()]
            .matches('\n')
            .count()
            + 1) as u32;
        assert!(guard_line < sink_line, "guard is textually before the sink");

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();
        let mut cur = root.walk();
        let func_node = root
            .named_children(&mut cur)
            .find(|c| c.kind() == "function_item")
            .unwrap();
        let h5_cfg = build_cfg(func_node, src.as_bytes(), "rust").unwrap();

        // Build the corpus: h1..h4 guard then sink (textual, no CFG); h5 guards
        // inside the branch. `attach_cfg` toggles the control-flow view on h5.
        let build = |attach_cfg: bool| -> CodeGraph {
            let mut g = CodeGraph::new();
            let guard_id = g.add_node(func("require_admin"));
            let sink_id = g.add_node(func("delete_user"));
            for h in ["h1", "h2", "h3", "h4"] {
                let cid = g.add_node(func(h));
                g.add_edge(&cid, &guard_id, call_at(2)).unwrap();
                g.add_edge(&cid, &sink_id, call_at(5)).unwrap();
            }
            let mut h5 = func("h5");
            if attach_cfg {
                h5.cfg = Some(h5_cfg.clone());
            }
            let h5_id = g.add_node(h5);
            g.add_edge(&h5_id, &guard_id, call_at(guard_line)).unwrap();
            g.add_edge(&h5_id, &sink_id, call_at(sink_line)).unwrap();
            g
        };

        // Without the CFG, line ordering treats h5 as guarded → no deviation.
        let line_only = mine_missing_guards(&build(false), &MineConfig::default());
        assert!(
            !line_only
                .iter()
                .any(|f| f.site == id_of("h5") && f.sink == id_of("delete_user")),
            "line ordering should not flag h5: {line_only:#?}"
        );

        // With the CFG, the in-branch guard does not dominate → h5 is flagged.
        let cfg_aware = mine_missing_guards(&build(true), &MineConfig::default());
        let flagged = cfg_aware
            .iter()
            .find(|f| f.site == id_of("h5") && f.sink == id_of("delete_user"))
            .expect("CFG dominance should flag h5 as missing require_admin");
        assert!(flagged.expected.contains(&id_of("require_admin")));
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
