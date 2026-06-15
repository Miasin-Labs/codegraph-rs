//! Inferred-seed taint: the `reaches_without_sanitizer` template.
//!
//! Generalizes [`crate::taint_v2`] from "caller hand-lists sources/sinks" to
//! "the engine *infers* them." Sources and sinks come from the
//! [`crate::taint_naming`] lexicon (a name like `read_request_body` is a source
//! prior; `exec_sql` is a sink prior); sanitizers come from a sanitizer lexicon
//! plus any guards a frequency pass already discovered (stacking
//! [`InferenceOrigin`]s). A flow that reaches a sink **without** passing a
//! sanitizer is the vulnerability — IDOR / SSRF / injection fall out of the
//! single template by which sink the tainted value lands in.

use std::collections::HashSet;

use super::{InferenceOrigin, TemplateKind, VulnFinding};
use crate::graph::CodeGraph;
use crate::nodes::{NodeId, NodeKind};
use crate::slicing::DataflowOracle;
use crate::taint_naming::{classify_name, flow_priority};
use crate::taint_v2::{self, TaintConfig, TaintFlow};

/// Lowercased substrings that read as a sanitizer / validator. Deliberately
/// broad: a missed sanitizer turns into a false *positive*, so erring toward
/// recognizing more sanitizers keeps the lint trustworthy.
const SANITIZER_LEXICON: &[&str] = &[
    "sanitize",
    "escape",
    "validate",
    "verify",
    "clean",
    "encode",
    "quote",
    "authorize",
    "authz",
    "ensure",
    "permit",
    "guard",
    "filter",
    "scrub",
    "normalize",
    "canonical",
    "whitelist",
    "allowlist",
];

/// Substrings that mark a *genuinely dangerous* sink. The base
/// [`crate::taint_naming`] sink lexicon includes benign-in-most-code tokens
/// (`read`, `write`, `open`, `load`, `remove`, `send`) that flood a systems
/// codebase with false positives; a sink must additionally hit one of these to
/// be seeded. Keeps injection/exec/deserialize/SSRF sinks, drops collection/IO
/// noise.
const STRONG_SINK_LEXICON: &[&str] = &[
    "exec",
    "execute",
    "system",
    "eval",
    "command",
    "cmd",
    "shell",
    "spawn",
    "popen",
    "deserialize",
    "unmarshal",
    "unpickle",
    "sql",
    "query",
    "html",
    "render",
    "template",
    "fetch",
    "http",
    "url",
    "curl",
    "request",
    "webhook",
    "redirect",
];

/// Inferred source / sink / sanitizer sets for a taint scan.
#[derive(Debug, Clone, Default)]
pub struct InferredSeeds {
    pub sources: Vec<NodeId>,
    pub sinks: Vec<NodeId>,
    pub sanitizers: Vec<NodeId>,
}

/// Infer taint seeds from identifier naming, folding in `extra_sanitizers`
/// (e.g. guards discovered by frequency mining) so independently-inferred
/// signals reinforce each other.
pub fn infer_seeds(graph: &CodeGraph, extra_sanitizers: &[NodeId]) -> InferredSeeds {
    let mut seeds = InferredSeeds::default();
    let mut sanitizers: HashSet<NodeId> = extra_sanitizers.iter().cloned().collect();
    for f in graph.nodes_by_kind(NodeKind::Function) {
        let class = classify_name(&f.name);
        if class.looks_like_source() {
            seeds.sources.push(f.id.clone());
        }
        // A sink must look like one *and* be genuinely dangerous — otherwise
        // ubiquitous `read`/`open`/`remove` calls drown the real findings.
        if class.looks_like_sink() && lexicon_hit(&f.name, STRONG_SINK_LEXICON) {
            seeds.sinks.push(f.id.clone());
        }
        if lexicon_hit(&f.name, SANITIZER_LEXICON) {
            sanitizers.insert(f.id.clone());
        }
    }
    // Stable order for deterministic output.
    seeds.sources.sort_by_key(|n| n.0);
    seeds.sinks.sort_by_key(|n| n.0);
    let mut sani: Vec<NodeId> = sanitizers.into_iter().collect();
    sani.sort_by_key(|n| n.0);
    seeds.sanitizers = sani;
    seeds
}

/// Convert taint flows into findings: only flows that reach a sink **without**
/// a sanitizer on the path are real.
pub fn flows_to_findings(graph: &CodeGraph, flows: &[TaintFlow]) -> Vec<VulnFinding> {
    let mut out: Vec<VulnFinding> = Vec::new();
    for flow in flows {
        if flow.passed_through_sanitizer.is_some() {
            continue;
        }
        let source_name = node_name(graph, &flow.source);
        let sink_name = node_name(graph, &flow.sink);
        let class = infer_class(&sink_name);
        let hops = flow.path.len().saturating_sub(1);
        let confidence = flow_priority(&source_name, &sink_name).clamp(0.0, 1.0);
        let class_suffix = match &class {
            Some(c) => format!("; possible {c}"),
            None => String::new(),
        };
        let message = format!(
            "tainted value from `{source_name}` reaches `{sink_name}` in {hops} hop(s) \
             with no sanitizer on the path{class_suffix}"
        );
        out.push(VulnFinding {
            template: TemplateKind::ReachesWithoutSanitizer,
            class,
            site: flow.source.clone(),
            sink: flow.sink.clone(),
            expected: Vec::new(),
            support: 0,
            total: 0,
            confidence,
            origin: InferenceOrigin::Name,
            message,
        });
    }
    out.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.site.0.cmp(&b.site.0))
            .then_with(|| a.sink.0.cmp(&b.sink.0))
    });
    out
}

/// Full scan over a `DataflowOracle`: infer seeds, run taint, keep unsanitized
/// flows. The oracle is injected so callers can use the real
/// [`crate::slicing::PointsToOracle`] in production and a stub in tests.
pub fn scan_with_oracle(
    graph: &CodeGraph,
    oracle: &dyn DataflowOracle,
    extra_sanitizers: &[NodeId],
) -> Vec<VulnFinding> {
    let seeds = infer_seeds(graph, extra_sanitizers);
    let config = TaintConfig {
        sources: &seeds.sources,
        sinks: &seeds.sinks,
        sanitizers: &seeds.sanitizers,
    };
    let flows = taint_v2::analyze(graph, oracle, &config);
    flows_to_findings(graph, &flows)
}

/// Production entry point: build the interprocedural points-to oracle from the
/// graph's IR map, then [`scan_with_oracle`].
pub fn scan_unsanitized_flows(graph: &CodeGraph, extra_sanitizers: &[NodeId]) -> Vec<VulnFinding> {
    let ir_map = crate::ir_map::build_ir_map(graph);
    let oracle = crate::slicing::PointsToOracle::build(graph, &ir_map);
    scan_with_oracle(graph, &oracle, extra_sanitizers)
}

/// Annotate a flow with a likely class from the sink name. Naming heuristic
/// only; absence of a label never suppresses the finding.
fn infer_class(sink_name: &str) -> Option<String> {
    let s = sink_name.to_ascii_lowercase();
    let any = |terms: &[&str]| terms.iter().any(|t| s.contains(t));
    if any(&["sql", "query"]) {
        Some("SQL injection".to_owned())
    } else if any(&["url", "http", "fetch", "curl", "request", "ssrf", "webhook"]) {
        Some("SSRF".to_owned())
    } else if any(&[
        "exec", "system", "command", "cmd", "shell", "spawn", "popen", "run",
    ]) {
        Some("command injection".to_owned())
    } else if any(&["html", "render", "template"]) {
        Some("XSS".to_owned())
    } else if any(&["deserialize", "unmarshal", "unpickle"]) {
        Some("unsafe deserialization".to_owned())
    } else if any(&["open", "path", "file", "read", "write"]) {
        Some("path traversal".to_owned())
    } else {
        None
    }
}

fn lexicon_hit(name: &str, lexicon: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    lexicon.iter().any(|term| lower.contains(term))
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
    use crate::nodes::{NodeData, NodeKind, Span, Visibility};

    /// Minimal `DataflowOracle` backed by an explicit forward adjacency.
    struct MockOracle {
        forward: HashMap<NodeId, Vec<NodeId>>,
    }
    impl DataflowOracle for MockOracle {
        fn def_uses(&self, _node: &NodeId) -> Vec<NodeId> {
            Vec::new()
        }
        fn use_defs(&self, node: &NodeId) -> Vec<NodeId> {
            self.forward.get(node).cloned().unwrap_or_default()
        }
    }

    fn func(g: &mut CodeGraph, name: &str) -> NodeId {
        g.add_node(NodeData {
            id: NodeId::new("src/h.rs", &format!("crate::{name}"), NodeKind::Function),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: format!("crate::{name}"),
            file_path: PathBuf::from("src/h.rs"),
            span: Span {
                file: PathBuf::from("src/h.rs"),
                start_line: 1,
                start_col: 0,
                end_line: 1,
                end_col: 1,
                byte_range: 0..1,
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            birth_revision: 0,
            last_modified_revision: 0,
            complexity: None,
            cfg: None,
            dataflow: None,
        })
    }

    #[test]
    fn seeds_are_inferred_from_names() {
        let mut g = CodeGraph::new();
        func(&mut g, "read_user_input");
        func(&mut g, "exec_sql");
        func(&mut g, "sanitize");
        func(&mut g, "transform");
        let seeds = infer_seeds(&g, &[]);
        assert!(seeds.sources.contains(&NodeId::new(
            "src/h.rs",
            "crate::read_user_input",
            NodeKind::Function
        )));
        assert!(seeds.sinks.contains(&NodeId::new(
            "src/h.rs",
            "crate::exec_sql",
            NodeKind::Function
        )));
        assert!(seeds.sanitizers.contains(&NodeId::new(
            "src/h.rs",
            "crate::sanitize",
            NodeKind::Function
        )));
    }

    #[test]
    fn flags_unsanitized_flow_only() {
        let mut g = CodeGraph::new();
        let src_a = func(&mut g, "read_user_input");
        let src_b = func(&mut g, "read_request_body");
        let sink = func(&mut g, "exec_sql");
        let san = func(&mut g, "sanitize");
        let mid = func(&mut g, "transform");

        // src_a → transform → exec_sql  (UNSANITIZED → finding)
        // src_b → sanitize  → exec_sql  (sanitized   → no finding)
        let mut forward: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        forward.insert(src_a.clone(), vec![mid.clone()]);
        forward.insert(mid, vec![sink.clone()]);
        forward.insert(src_b, vec![san.clone()]);
        forward.insert(san, vec![sink.clone()]);
        let oracle = MockOracle { forward };

        let findings = scan_with_oracle(&g, &oracle, &[]);
        assert_eq!(findings.len(), 1, "got {findings:#?}");
        let f = &findings[0];
        assert_eq!(f.template, TemplateKind::ReachesWithoutSanitizer);
        assert_eq!(f.site, src_a);
        assert_eq!(f.sink, sink);
        assert_eq!(f.class.as_deref(), Some("SQL injection"));
    }

    #[test]
    fn no_finding_when_every_flow_sanitized() {
        let mut g = CodeGraph::new();
        let src = func(&mut g, "read_user_input");
        let sink = func(&mut g, "exec_sql");
        let san = func(&mut g, "sanitize");
        let mut forward: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        forward.insert(src, vec![san.clone()]);
        forward.insert(san, vec![sink]);
        let oracle = MockOracle { forward };
        assert!(scan_with_oracle(&g, &oracle, &[]).is_empty());
    }
}
