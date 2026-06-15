//! Pluggable predicate classification, with **graph verification** as the trust
//! boundary.
//!
//! The highest-ceiling inference mechanism is to let a model name which
//! functions are sinks / guards / sanitizers — but a model can hallucinate.
//! So a proposal is never trusted directly: [`GraphVerifier`] checks each
//! proposed role against actual graph facts (a "sink" must really be called
//! from many places; a "guard" must really precede a verified sink at some call
//! site) before it can produce a finding. The model supplies *semantic
//! judgment*; the graph supplies *ground truth*.
//!
//! The classifier itself is a trait so the LLM-backed implementation can live
//! outside this crate (in the MCP/agent layer, which has model access) while
//! the verification — the part that must be sound — stays here and is tested.

use std::collections::HashSet;

use super::{InferenceOrigin, TemplateKind, VulnFinding};
use crate::edges::EdgeKind;
use crate::graph::CodeGraph;
use crate::nodes::NodeId;

/// A role a candidate function might play in a vulnerability template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PredicateRole {
    Source,
    Sink,
    Guard,
    Sanitizer,
}

/// A proposed role for a node, from any classifier (LLM, lexicon, heuristic).
#[derive(Debug, Clone)]
pub struct RoleProposal {
    pub node: NodeId,
    pub role: PredicateRole,
    pub confidence: f64,
    pub rationale: String,
}

/// Anything that proposes roles for candidate functions. The LLM-backed
/// implementation lives in the agent/MCP layer; tests use a canned one.
pub trait PredicateClassifier {
    fn classify(&self, graph: &CodeGraph, candidates: &[NodeId]) -> Vec<RoleProposal>;
}

/// A proposal after graph verification. `verified == false` means the graph
/// contradicted the proposed role; such roles must not produce findings.
#[derive(Debug, Clone)]
pub struct VerifiedRole {
    pub node: NodeId,
    pub role: PredicateRole,
    pub confidence: f64,
    pub origin: InferenceOrigin,
    pub rationale: String,
    pub verified: bool,
}

/// Verifies role proposals against graph facts before they are trusted.
pub struct GraphVerifier<'a> {
    pub graph: &'a CodeGraph,
    /// A proposed sink must have at least this many distinct callers to be a
    /// believable "called from many places" sink.
    pub min_callers: usize,
}

impl<'a> GraphVerifier<'a> {
    pub fn new(graph: &'a CodeGraph) -> Self {
        Self {
            graph,
            min_callers: 4,
        }
    }

    /// Verify every proposal. Sinks are checked first (guards are verified
    /// relative to verified sinks). Returns one [`VerifiedRole`] per proposal,
    /// with `verified` set; callers keep only the verified ones.
    pub fn verify(&self, proposals: &[RoleProposal]) -> Vec<VerifiedRole> {
        // Pass 1: which proposed sinks survive the in-degree check.
        let verified_sinks: Vec<NodeId> = proposals
            .iter()
            .filter(|p| p.role == PredicateRole::Sink)
            .filter(|p| self.distinct_callers(&p.node) >= self.min_callers)
            .map(|p| p.node.clone())
            .collect();

        proposals
            .iter()
            .map(|p| {
                let verified = match p.role {
                    PredicateRole::Sink => verified_sinks.contains(&p.node),
                    PredicateRole::Guard => verified_sinks
                        .iter()
                        .any(|s| self.precedes_in_some_caller(&p.node, s)),
                    // A sanitizer must actually be invoked somewhere.
                    PredicateRole::Sanitizer => self.distinct_callers(&p.node) >= 1,
                    // A source must actually flow somewhere (have callees).
                    PredicateRole::Source => !self.callees(&p.node).is_empty(),
                };
                VerifiedRole {
                    node: p.node.clone(),
                    role: p.role,
                    confidence: p.confidence,
                    origin: InferenceOrigin::Llm,
                    rationale: p.rationale.clone(),
                    verified,
                }
            })
            .collect()
    }

    /// Distinct caller functions of `node` via resolved `Calls` edges.
    fn distinct_callers(&self, node: &NodeId) -> usize {
        let mut callers: HashSet<&NodeId> = HashSet::new();
        for (src, edge) in self.graph.get_edges_to(node) {
            if matches!(edge.kind, EdgeKind::Calls) && src != node {
                callers.insert(src);
            }
        }
        callers.len()
    }

    fn callees(&self, node: &NodeId) -> Vec<NodeId> {
        self.graph
            .get_edges_from(node)
            .into_iter()
            .filter(|(_, e)| matches!(e.kind, EdgeKind::Calls))
            .map(|(t, _)| t.clone())
            .collect()
    }

    /// True if some caller calls `guard` strictly before its call to `sink`.
    fn precedes_in_some_caller(&self, guard: &NodeId, sink: &NodeId) -> bool {
        // Callers of the sink, with the earliest sink call-site line.
        for (caller, sink_edge) in self.graph.get_edges_to(sink) {
            if !matches!(sink_edge.kind, EdgeKind::Calls) || caller == sink {
                continue;
            }
            let sink_line = sink_edge.source_span.start_line;
            for (callee, edge) in self.graph.get_edges_from(caller) {
                if matches!(edge.kind, EdgeKind::Calls)
                    && callee == guard
                    && edge.source_span.start_line < sink_line
                {
                    return true;
                }
            }
        }
        false
    }

    /// Callers of `node` reachable via resolved `Calls` edges (distinct).
    fn caller_set(&self, node: &NodeId) -> HashSet<NodeId> {
        let mut out: HashSet<NodeId> = HashSet::new();
        for (src, edge) in self.graph.get_edges_to(node) {
            if matches!(edge.kind, EdgeKind::Calls) && src != node {
                out.insert(src.clone());
            }
        }
        out
    }
}

/// Turn a model's role proposals into verified vulnerability findings.
///
/// This is the production entry point for the **Llm** inference origin and the
/// "model proposes, graph proves" boundary described at the top of this module:
/// the agent layer (which has model access) supplies [`RoleProposal`]s naming
/// suspected sinks / guards; [`GraphVerifier`] keeps only the ones the call
/// graph actually corroborates; then for every verified guard that protects a
/// verified sink, each caller of that sink which does **not** pass the guard is
/// emitted as a [`TemplateKind::MissingDominatorCheck`] finding tagged
/// [`InferenceOrigin::Llm`].
///
/// Soundness rests entirely on [`GraphVerifier`] — an unverified proposal can
/// never reach this function's output. A hallucinated "sink" with too few
/// callers, or a "guard" that never precedes the sink, is dropped before any
/// finding is built.
pub fn findings_from_verified_roles(
    graph: &CodeGraph,
    proposals: &[RoleProposal],
    min_callers: usize,
) -> Vec<VulnFinding> {
    let verifier = GraphVerifier {
        graph,
        min_callers: min_callers.max(1),
    };
    let verified = verifier.verify(proposals);

    let sinks: Vec<&VerifiedRole> = verified
        .iter()
        .filter(|r| r.verified && r.role == PredicateRole::Sink)
        .collect();
    let guards: Vec<&VerifiedRole> = verified
        .iter()
        .filter(|r| r.verified && r.role == PredicateRole::Guard)
        .collect();

    let mut findings: Vec<VulnFinding> = Vec::new();
    for sink in &sinks {
        let sink_callers = verifier.caller_set(&sink.node);
        if sink_callers.is_empty() {
            continue;
        }
        // Guards (verified) that actually precede this specific sink.
        let protecting: Vec<&&VerifiedRole> = guards
            .iter()
            .filter(|g| verifier.precedes_in_some_caller(&g.node, &sink.node))
            .collect();
        if protecting.is_empty() {
            continue;
        }
        for caller in &sink_callers {
            // Which protecting guards does this caller invoke before the sink?
            let missing: Vec<NodeId> = protecting
                .iter()
                .filter(|g| !calls_before(graph, caller, &g.node, &sink.node))
                .map(|g| g.node.clone())
                .collect();
            if missing.is_empty() {
                continue;
            }
            let total = sink_callers.len() as u32;
            let support = total.saturating_sub(
                sink_callers
                    .iter()
                    .filter(|c| {
                        missing
                            .iter()
                            .any(|g| !calls_before(graph, c, g, &sink.node))
                    })
                    .count() as u32,
            );
            // Confidence: the model's own min confidence over the roles
            // involved, so a shaky proposal yields a low-confidence finding even
            // after structural verification.
            let conf = role_confidence(&sinks, &sink.node)
                .min(min_guard_confidence(&guards, &missing))
                .clamp(0.0, 1.0);
            let sink_name = node_name(graph, &sink.node);
            let site_name = node_name(graph, caller);
            let guard_names: Vec<String> = missing.iter().map(|g| node_name(graph, g)).collect();
            let message = format!(
                "`{site_name}` reaches LLM-proposed sink `{sink_name}` without \
                 graph-verified guard(s) `{}` (origin: llm, verified against the call graph)",
                guard_names.join("`, `"),
            );
            findings.push(VulnFinding {
                template: TemplateKind::MissingDominatorCheck,
                class: Some("BAC".to_owned()),
                site: caller.clone(),
                sink: sink.node.clone(),
                expected: missing,
                support,
                total,
                confidence: conf,
                origin: InferenceOrigin::Llm,
                message,
            });
        }
    }

    findings.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.site.0.cmp(&b.site.0))
            .then_with(|| a.sink.0.cmp(&b.sink.0))
    });
    findings
}

/// True if `caller` invokes `guard` strictly before its call to `sink`.
fn calls_before(graph: &CodeGraph, caller: &NodeId, guard: &NodeId, sink: &NodeId) -> bool {
    let mut earliest_sink: Option<u32> = None;
    for (callee, edge) in graph.get_edges_from(caller) {
        if matches!(edge.kind, EdgeKind::Calls) && callee == sink {
            let l = edge.source_span.start_line;
            earliest_sink = Some(earliest_sink.map_or(l, |e| e.min(l)));
        }
    }
    let Some(sink_line) = earliest_sink else {
        return false;
    };
    for (callee, edge) in graph.get_edges_from(caller) {
        if matches!(edge.kind, EdgeKind::Calls)
            && callee == guard
            && edge.source_span.start_line < sink_line
        {
            return true;
        }
    }
    false
}

fn role_confidence(roles: &[&VerifiedRole], node: &NodeId) -> f64 {
    roles
        .iter()
        .find(|r| &r.node == node)
        .map(|r| r.confidence)
        .unwrap_or(0.5)
}

fn min_guard_confidence(guards: &[&VerifiedRole], nodes: &[NodeId]) -> f64 {
    nodes
        .iter()
        .filter_map(|n| guards.iter().find(|g| &g.node == n).map(|g| g.confidence))
        .fold(1.0_f64, f64::min)
}

fn node_name(graph: &CodeGraph, id: &NodeId) -> String {
    graph
        .get_node(id)
        .map(|n| n.name.clone())
        .unwrap_or_else(|| format!("node#{}", id.0))
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

    fn func(g: &mut CodeGraph, name: &str) -> NodeId {
        g.add_node(NodeData {
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
        })
    }

    fn call(g: &mut CodeGraph, from: &NodeId, to: &NodeId, line: u32) {
        g.add_edge(
            from,
            to,
            EdgeData {
                kind: EdgeKind::Calls,
                source_span: span_at(line),
                weight: 1.0,
            },
        )
        .unwrap();
    }

    #[test]
    fn graph_rejects_hallucinated_roles_and_keeps_grounded_ones() {
        let mut g = CodeGraph::new();
        let sink = func(&mut g, "do_delete");
        let auth = func(&mut g, "auth");
        let bogus_sink = func(&mut g, "never_called");
        let unrelated = func(&mut g, "unrelated_guard");
        // 5 callers reach the sink; 4 call auth before it.
        for i in 0..5 {
            let h = func(&mut g, &format!("handler{i}"));
            if i < 4 {
                call(&mut g, &h, &auth, 2);
            }
            call(&mut g, &h, &sink, 5);
        }

        let proposals = vec![
            RoleProposal {
                node: sink.clone(),
                role: PredicateRole::Sink,
                confidence: 0.9,
                rationale: "looks like a delete".into(),
            },
            RoleProposal {
                node: bogus_sink.clone(),
                role: PredicateRole::Sink,
                confidence: 0.9,
                rationale: "model guessed".into(),
            },
            RoleProposal {
                node: auth.clone(),
                role: PredicateRole::Guard,
                confidence: 0.8,
                rationale: "auth gate".into(),
            },
            RoleProposal {
                node: unrelated.clone(),
                role: PredicateRole::Guard,
                confidence: 0.8,
                rationale: "model guessed".into(),
            },
        ];

        let verifier = GraphVerifier::new(&g);
        let verified: Vec<_> = verifier
            .verify(&proposals)
            .into_iter()
            .filter(|v| v.verified)
            .map(|v| (v.node, v.role))
            .collect();

        assert!(verified.contains(&(sink, PredicateRole::Sink)));
        assert!(verified.contains(&(auth, PredicateRole::Guard)));
        assert!(!verified.iter().any(|(n, _)| *n == bogus_sink));
        assert!(!verified.iter().any(|(n, _)| *n == unrelated));
        assert_eq!(verified.len(), 2);
    }

    #[test]
    fn verified_roles_yield_llm_finding_for_the_deviant_caller_only() {
        let mut g = CodeGraph::new();
        let sink = func(&mut g, "do_delete");
        let auth = func(&mut g, "auth");
        // 5 callers reach the sink; handler0..3 call auth first, handler4 does not.
        let mut handlers = Vec::new();
        for i in 0..5 {
            let h = func(&mut g, &format!("handler{i}"));
            if i < 4 {
                call(&mut g, &h, &auth, 2);
            }
            call(&mut g, &h, &sink, 5);
            handlers.push(h);
        }

        let proposals = vec![
            RoleProposal {
                node: sink.clone(),
                role: PredicateRole::Sink,
                confidence: 0.9,
                rationale: "delete".into(),
            },
            RoleProposal {
                node: auth.clone(),
                role: PredicateRole::Guard,
                confidence: 0.8,
                rationale: "auth gate".into(),
            },
        ];

        let findings = findings_from_verified_roles(&g, &proposals, 4);
        // Exactly one deviant caller (handler4) reaches the sink without auth.
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.site, handlers[4]);
        assert_eq!(f.sink, sink);
        assert_eq!(f.template, TemplateKind::MissingDominatorCheck);
        assert_eq!(f.origin, InferenceOrigin::Llm);
        assert_eq!(f.expected, vec![auth]);
        // Confidence is bounded by the model's own role confidences.
        assert!(f.confidence <= 0.8 + f64::EPSILON);
        assert!(f.message.contains("LLM-proposed sink"));
    }

    #[test]
    fn hallucinated_sink_produces_no_findings() {
        let mut g = CodeGraph::new();
        let bogus = func(&mut g, "never_called");
        let guard = func(&mut g, "auth");
        let proposals = vec![
            RoleProposal {
                node: bogus,
                role: PredicateRole::Sink,
                confidence: 0.99,
                rationale: "hallucinated".into(),
            },
            RoleProposal {
                node: guard,
                role: PredicateRole::Guard,
                confidence: 0.99,
                rationale: "hallucinated".into(),
            },
        ];
        // The sink has zero callers → fails verification → no findings escape.
        assert!(findings_from_verified_roles(&g, &proposals, 4).is_empty());
    }
}
