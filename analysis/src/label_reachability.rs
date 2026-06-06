//! Label-constrained reachability (RLC) over the typed edge graph.
//!
//! From *Reachability queries with label-constraints* (the RLC line of work):
//! instead of plain "is `t` reachable from `s`", answer "is `t` reachable via a
//! path whose **edge-label sequence** matches a constraint". The graph already
//! carries typed [`EdgeKind`] labels ([`crate::edges`]); this module lets a
//! query say things like *"a function reachable via `(Calls)+`"* or *"a method
//! reachable via `Contains` then `(Calls)+`"* and get an exact yes/no plus the
//! full set of matching endpoints.
//!
//! The constraint is a small regular pattern — a sequence of [`PatternAtom`]s,
//! each an edge matcher with a repetition (`One` / `Star` / `Plus`). It is
//! compiled to a Thompson-style ε-NFA over edge labels ([`compile`]), and the
//! query runs a **product BFS** over `(NodeId, nfa_state)` pairs using the same
//! `get_edges_from` / `get_edges_to` accessors as [`crate::closure`]. The
//! visited set is bounded by `nodes × nfa_states`, so cycles terminate exactly
//! as in `closure`.
//!
//! This is the precision lever the gap analysis flagged: the existing
//! reachability is label-*agnostic*, so it conflates a `Calls` path with a
//! `UsesType` path. Constraining the label sequence sharpens what `taint_v2`
//! and the DSL can ask for. (Full calling-context sensitivity — matched
//! call/return brackets — needs a per-call-site bracketed edge model the graph
//! does not yet carry; see the module note in `taint_naming` / the gap doc.)

use std::collections::HashSet;

use crate::closure::ClosureDirection;
use crate::edges::EdgeKind;
use crate::graph::CodeGraph;
use crate::nodes::NodeId;

/// Matches a single edge by its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeMatcher {
    /// Matches edges of the given kind, comparing by *discriminant* — so
    /// `Exactly(EdgeKind::UnresolvedCall(String::new()))` matches any
    /// `UnresolvedCall(_)` regardless of payload.
    Exactly(EdgeKind),
    /// Matches any edge label.
    Any,
}

impl EdgeMatcher {
    pub fn matches(&self, kind: &EdgeKind) -> bool {
        match self {
            EdgeMatcher::Any => true,
            EdgeMatcher::Exactly(k) => std::mem::discriminant(k) == std::mem::discriminant(kind),
        }
    }
}

/// Repetition applied to one [`PatternAtom`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rep {
    /// Exactly one matching edge.
    One,
    /// Zero or more matching edges.
    Star,
    /// One or more matching edges.
    Plus,
}

/// One atom of a label-constraint pattern: an edge matcher with a repetition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternAtom {
    pub matcher: EdgeMatcher,
    pub rep: Rep,
}

impl PatternAtom {
    /// Exactly one edge of `kind`.
    pub fn one(kind: EdgeKind) -> Self {
        Self {
            matcher: EdgeMatcher::Exactly(kind),
            rep: Rep::One,
        }
    }
    /// Zero or more edges of `kind`.
    pub fn star(kind: EdgeKind) -> Self {
        Self {
            matcher: EdgeMatcher::Exactly(kind),
            rep: Rep::Star,
        }
    }
    /// One or more edges of `kind`.
    pub fn plus(kind: EdgeKind) -> Self {
        Self {
            matcher: EdgeMatcher::Exactly(kind),
            rep: Rep::Plus,
        }
    }
    /// Any-label atom with the given repetition.
    pub fn any(rep: Rep) -> Self {
        Self {
            matcher: EdgeMatcher::Any,
            rep,
        }
    }
}

/// Error from [`parse_pattern`]: the offending atom text plus a reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid pattern atom '{atom}': {reason}")]
pub struct PatternParseError {
    /// The whitespace-delimited token that failed to parse.
    pub atom: String,
    /// Human-readable failure reason (unknown label, dangling repetition…).
    pub reason: String,
}

/// Parse a textual label-constraint pattern into [`PatternAtom`]s.
///
/// Syntax: whitespace-separated atoms, each an edge-kind label with an
/// optional trailing repetition suffix:
///
/// - `Calls` — exactly one `Calls` edge ([`Rep::One`])
/// - `Calls+` — one or more ([`Rep::Plus`])
/// - `Calls*` — zero or more ([`Rep::Star`])
/// - `any` / `any+` / `any*` — wildcard label ([`EdgeMatcher::Any`])
///
/// Valid labels mirror [`EdgeKind`]'s unit-payload spelling: `Calls`,
/// `UnresolvedCall`, `UsesType`, `References`, `Contains`, `Implements`,
/// `ExternalCall`, `Extends`, `Returns`, `TypeOf` (payload-carrying kinds
/// match by discriminant — see [`EdgeMatcher::Exactly`]). An empty / all-
/// whitespace input parses to the empty pattern, which matches only the
/// zero-length path (the source itself).
///
/// This is the string form consumed by the DSL's `reachable via "<pattern>"`
/// operator and available to hosts building CLI surfaces (e.g.
/// `analyze reachable --edge-pattern`).
pub fn parse_pattern(input: &str) -> Result<Vec<PatternAtom>, PatternParseError> {
    let mut atoms = Vec::new();
    for raw in input.split_whitespace() {
        let (label, rep) = match raw.as_bytes().last() {
            Some(b'*') => (&raw[..raw.len() - 1], Rep::Star),
            Some(b'+') => (&raw[..raw.len() - 1], Rep::Plus),
            _ => (raw, Rep::One),
        };
        if label.is_empty() {
            return Err(PatternParseError {
                atom: raw.to_string(),
                reason: "repetition suffix without an edge label".to_string(),
            });
        }
        let matcher = if label.eq_ignore_ascii_case("any") {
            EdgeMatcher::Any
        } else {
            EdgeMatcher::Exactly(
                edge_kind_from_label(label).ok_or_else(|| PatternParseError {
                    atom: raw.to_string(),
                    reason: format!(
                        "unknown edge label '{label}'. Valid: Calls, UnresolvedCall, UsesType, \
                     References, Contains, Implements, ExternalCall, Extends, Returns, TypeOf, any"
                    ),
                })?,
            )
        };
        atoms.push(PatternAtom { matcher, rep });
    }
    Ok(atoms)
}

/// Render a pattern back to the textual form [`parse_pattern`] accepts.
/// Round-trips: `parse_pattern(&format_pattern(&p)) == Ok(p)`.
pub fn format_pattern(pattern: &[PatternAtom]) -> String {
    pattern
        .iter()
        .map(|atom| {
            let label = match &atom.matcher {
                EdgeMatcher::Any => "any",
                EdgeMatcher::Exactly(kind) => edge_kind_label(kind),
            };
            let suffix = match atom.rep {
                Rep::One => "",
                Rep::Star => "*",
                Rep::Plus => "+",
            };
            format!("{label}{suffix}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Edge-kind label → [`EdgeKind`] (payload-carrying kinds get empty
/// payloads; matching is by discriminant). Inverse of [`edge_kind_label`].
fn edge_kind_from_label(label: &str) -> Option<EdgeKind> {
    match label {
        "Calls" => Some(EdgeKind::Calls),
        "UnresolvedCall" => Some(EdgeKind::UnresolvedCall(String::new())),
        "UsesType" => Some(EdgeKind::UsesType),
        "References" => Some(EdgeKind::References),
        "Contains" => Some(EdgeKind::Contains),
        "Implements" => Some(EdgeKind::Implements),
        "ExternalCall" => Some(EdgeKind::ExternalCall(String::new(), String::new())),
        "Extends" => Some(EdgeKind::Extends),
        "Returns" => Some(EdgeKind::Returns),
        "TypeOf" => Some(EdgeKind::TypeOf),
        _ => None,
    }
}

/// [`EdgeKind`] → its pattern label. Inverse of [`edge_kind_from_label`].
fn edge_kind_label(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "Calls",
        EdgeKind::UnresolvedCall(_) => "UnresolvedCall",
        EdgeKind::UsesType => "UsesType",
        EdgeKind::References => "References",
        EdgeKind::Contains => "Contains",
        EdgeKind::Implements => "Implements",
        EdgeKind::ExternalCall(_, _) => "ExternalCall",
        EdgeKind::Extends => "Extends",
        EdgeKind::Returns => "Returns",
        EdgeKind::TypeOf => "TypeOf",
    }
}

/// A compiled ε-NFA over edge labels. `trans[q]` lists the outgoing transitions
/// of state `q`; the machine starts in `start` and accepts in `accept`.
#[derive(Debug)]
struct Nfa {
    trans: Vec<Vec<Trans>>,
    start: usize,
    accept: usize,
}

#[derive(Debug)]
enum Trans {
    /// Free move (no edge consumed).
    Eps(usize),
    /// Consume one graph edge matching `EdgeMatcher`, then move to the state.
    On(EdgeMatcher, usize),
}

/// Compile a label-constraint pattern into an ε-NFA.
///
/// Each atom contributes an entry→exit pair chained onto the previous exit:
/// - `One`:  entry —On(m)→ exit
/// - `Plus`: entry —On(m)→ exit, exit —On(m)→ exit  (one, then loop)
/// - `Star`: entry —ε→ exit,   exit —On(m)→ exit    (skip, or loop)
///
/// An empty pattern compiles to a single state that is both start and accept,
/// matching only the zero-length path (the source itself).
fn compile(pattern: &[PatternAtom]) -> Nfa {
    let mut trans: Vec<Vec<Trans>> = vec![Vec::new()]; // state 0 = start
    let start = 0usize;
    let mut current = start;
    for atom in pattern {
        trans.push(Vec::new());
        let exit = trans.len() - 1;
        match atom.rep {
            Rep::One => {
                trans[current].push(Trans::On(atom.matcher.clone(), exit));
            }
            Rep::Plus => {
                trans[current].push(Trans::On(atom.matcher.clone(), exit));
                trans[exit].push(Trans::On(atom.matcher.clone(), exit));
            }
            Rep::Star => {
                trans[current].push(Trans::Eps(exit));
                trans[exit].push(Trans::On(atom.matcher.clone(), exit));
            }
        }
        current = exit;
    }
    Nfa {
        trans,
        start,
        accept: current,
    }
}

/// Product BFS over `(NodeId, nfa_state)`. Returns the set of graph nodes
/// reached in the NFA's accept state. If `stop_at` is `Some(t)`, returns as soon
/// as `t` is accepted (the returned set then contains `t`).
fn product_bfs(
    graph: &CodeGraph,
    source: &NodeId,
    nfa: &Nfa,
    direction: ClosureDirection,
    stop_at: Option<&NodeId>,
) -> HashSet<NodeId> {
    let mut visited: HashSet<(NodeId, usize)> = HashSet::new();
    let mut frontier: Vec<(NodeId, usize)> = Vec::new();
    let mut accepting: HashSet<NodeId> = HashSet::new();

    visited.insert((source.clone(), nfa.start));
    frontier.push((source.clone(), nfa.start));

    while let Some((node, q)) = frontier.pop() {
        if q == nfa.accept {
            accepting.insert(node.clone());
            if stop_at == Some(&node) {
                return accepting;
            }
        }
        for tr in &nfa.trans[q] {
            step_transition(graph, direction, &node, tr, &mut visited, &mut frontier);
        }
    }
    accepting
}

/// Apply one NFA transition from `(node, q)`: an ε-move stays on `node` and
/// advances the NFA state; an `On(matcher)` consumes each matching graph edge
/// and advances both. Newly-discovered `(node, state)` pairs are pushed onto
/// the frontier. Extracted from [`product_bfs`] to keep nesting shallow.
fn step_transition(
    graph: &CodeGraph,
    direction: ClosureDirection,
    node: &NodeId,
    tr: &Trans,
    visited: &mut HashSet<(NodeId, usize)>,
    frontier: &mut Vec<(NodeId, usize)>,
) {
    match tr {
        Trans::Eps(q2) => {
            if visited.insert((node.clone(), *q2)) {
                frontier.push((node.clone(), *q2));
            }
        }
        Trans::On(matcher, q2) => {
            let nbrs = match direction {
                ClosureDirection::Outgoing => graph.get_edges_from(node),
                ClosureDirection::Incoming => graph.get_edges_to(node),
            };
            for (nbr, edge) in nbrs {
                if matcher.matches(&edge.kind) && visited.insert((nbr.clone(), *q2)) {
                    frontier.push((nbr.clone(), *q2));
                }
            }
        }
    }
}

/// Every node reachable from `source` via a path whose edge-label sequence
/// matches `pattern`, in the given direction. Sorted for determinism. Includes
/// `source` itself iff the pattern matches the zero-length path (empty pattern,
/// or all atoms `Star`).
pub fn reachable_targets(
    graph: &CodeGraph,
    source: &NodeId,
    pattern: &[PatternAtom],
    direction: ClosureDirection,
) -> Vec<NodeId> {
    if !graph.contains_node(source) {
        return Vec::new();
    }
    let nfa = compile(pattern);
    let mut v: Vec<NodeId> = product_bfs(graph, source, &nfa, direction, None)
        .into_iter()
        .collect();
    v.sort();
    v
}

/// True iff `target` is reachable from `source` via a path matching `pattern`.
/// Short-circuits as soon as the target is accepted.
pub fn reachable(
    graph: &CodeGraph,
    source: &NodeId,
    target: &NodeId,
    pattern: &[PatternAtom],
    direction: ClosureDirection,
) -> bool {
    if !graph.contains_node(source) || !graph.contains_node(target) {
        return false;
    }
    let nfa = compile(pattern);
    product_bfs(graph, source, &nfa, direction, Some(target)).contains(target)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::edges::EdgeData;
    use crate::nodes::{NodeData, NodeKind, Span, Visibility};

    fn span() -> Span {
        Span {
            file: PathBuf::from("t.rs"),
            start_line: 1,
            start_col: 0,
            end_line: 1,
            end_col: 0,
            byte_range: 0..0,
        }
    }

    fn mk(name: &str, kind: NodeKind) -> NodeData {
        NodeData {
            id: NodeId::new("t.rs", name, kind),
            kind,
            name: name.into(),
            qualified_name: name.into(),
            file_path: PathBuf::from("t.rs"),
            span: span(),
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            birth_revision: 0,
            last_modified_revision: 0,
            complexity: None,
            cfg: None,
            dataflow: None,
        }
    }

    fn ed(k: EdgeKind) -> EdgeData {
        EdgeData {
            kind: k,
            source_span: span(),
            weight: 1.0,
        }
    }

    // Normal: (Calls)+ reaches the transitive callees but not the source.
    #[test]
    fn calls_plus_reaches_transitive_callees_normal() {
        let mut g = CodeGraph::new();
        let a = g.add_node(mk("a", NodeKind::Function));
        let b = g.add_node(mk("b", NodeKind::Function));
        let c = g.add_node(mk("c", NodeKind::Function));
        let d = g.add_node(mk("d", NodeKind::Function));
        g.add_edge(&a, &b, ed(EdgeKind::Calls)).unwrap();
        g.add_edge(&b, &c, ed(EdgeKind::Calls)).unwrap();
        g.add_edge(&c, &d, ed(EdgeKind::Calls)).unwrap();

        let pat = [PatternAtom::plus(EdgeKind::Calls)];
        let targets = reachable_targets(&g, &a, &pat, ClosureDirection::Outgoing);
        assert_eq!(targets.len(), 3);
        assert!(targets.contains(&b) && targets.contains(&c) && targets.contains(&d));
        assert!(!targets.contains(&a)); // Plus requires >= 1 edge
        assert!(reachable(&g, &a, &d, &pat, ClosureDirection::Outgoing));
    }

    // Normal: a two-stage constraint "Contains then (Calls)+" matches only the
    // functions reached after entering the module and making >=1 call.
    #[test]
    fn contains_then_calls_plus_two_stage_normal() {
        let mut g = CodeGraph::new();
        let m = g.add_node(mk("m", NodeKind::Module));
        let f0 = g.add_node(mk("f0", NodeKind::Function));
        let f1 = g.add_node(mk("f1", NodeKind::Function));
        let f2 = g.add_node(mk("f2", NodeKind::Function));
        g.add_edge(&m, &f0, ed(EdgeKind::Contains)).unwrap();
        g.add_edge(&f0, &f1, ed(EdgeKind::Calls)).unwrap();
        g.add_edge(&f1, &f2, ed(EdgeKind::Calls)).unwrap();

        let pat = [
            PatternAtom::one(EdgeKind::Contains),
            PatternAtom::plus(EdgeKind::Calls),
        ];
        let targets = reachable_targets(&g, &m, &pat, ClosureDirection::Outgoing);
        // f0 only satisfies Contains (zero Calls) -> not accepted.
        assert_eq!(targets, {
            let mut v = vec![f1.clone(), f2.clone()];
            v.sort();
            v
        });
        assert!(reachable(&g, &m, &f2, &pat, ClosureDirection::Outgoing));
        assert!(!reachable(&g, &m, &f0, &pat, ClosureDirection::Outgoing));
    }

    // Robust: a constraint whose first label can't be taken yields no matches.
    #[test]
    fn wrong_edge_sequence_not_reachable_robust() {
        let mut g = CodeGraph::new();
        let m = g.add_node(mk("m", NodeKind::Module));
        let f0 = g.add_node(mk("f0", NodeKind::Function));
        g.add_edge(&m, &f0, ed(EdgeKind::Contains)).unwrap();

        // m has only a Contains edge; demanding a Calls edge first matches nothing.
        let pat = [PatternAtom::one(EdgeKind::Calls)];
        assert!(reachable_targets(&g, &m, &pat, ClosureDirection::Outgoing).is_empty());
        assert!(!reachable(&g, &m, &f0, &pat, ClosureDirection::Outgoing));
    }

    // Robust: (Calls)* includes the source (zero occurrences) plus all callees.
    #[test]
    fn calls_star_includes_source_robust() {
        let mut g = CodeGraph::new();
        let a = g.add_node(mk("a", NodeKind::Function));
        let b = g.add_node(mk("b", NodeKind::Function));
        let c = g.add_node(mk("c", NodeKind::Function));
        g.add_edge(&a, &b, ed(EdgeKind::Calls)).unwrap();
        g.add_edge(&b, &c, ed(EdgeKind::Calls)).unwrap();

        let pat = [PatternAtom::star(EdgeKind::Calls)];
        let targets = reachable_targets(&g, &a, &pat, ClosureDirection::Outgoing);
        assert!(targets.contains(&a) && targets.contains(&b) && targets.contains(&c));
    }

    // Robust: a Calls cycle terminates (product visited-set bound).
    #[test]
    fn cycle_is_safe_robust() {
        let mut g = CodeGraph::new();
        let a = g.add_node(mk("a", NodeKind::Function));
        let b = g.add_node(mk("b", NodeKind::Function));
        g.add_edge(&a, &b, ed(EdgeKind::Calls)).unwrap();
        g.add_edge(&b, &a, ed(EdgeKind::Calls)).unwrap();

        let pat = [PatternAtom::plus(EdgeKind::Calls)];
        let targets = reachable_targets(&g, &a, &pat, ClosureDirection::Outgoing);
        // a reachable via 2 calls; b via 1 — both present, no infinite loop.
        assert!(targets.contains(&a) && targets.contains(&b));
    }

    // Normal: the Incoming direction answers "who reaches me via (Calls)+".
    #[test]
    fn incoming_direction_finds_callers_normal() {
        let mut g = CodeGraph::new();
        let caller = g.add_node(mk("caller", NodeKind::Function));
        let mid = g.add_node(mk("mid", NodeKind::Function));
        let target = g.add_node(mk("target", NodeKind::Function));
        g.add_edge(&caller, &mid, ed(EdgeKind::Calls)).unwrap();
        g.add_edge(&mid, &target, ed(EdgeKind::Calls)).unwrap();

        let pat = [PatternAtom::plus(EdgeKind::Calls)];
        let callers = reachable_targets(&g, &target, &pat, ClosureDirection::Incoming);
        assert!(callers.contains(&caller) && callers.contains(&mid));
    }

    // Robust: an empty pattern matches only the source (zero-length path).
    #[test]
    fn empty_pattern_matches_only_source_robust() {
        let mut g = CodeGraph::new();
        let a = g.add_node(mk("a", NodeKind::Function));
        let b = g.add_node(mk("b", NodeKind::Function));
        g.add_edge(&a, &b, ed(EdgeKind::Calls)).unwrap();

        let targets = reachable_targets(&g, &a, &[], ClosureDirection::Outgoing);
        assert_eq!(targets, vec![a.clone()]);
        assert!(!reachable(&g, &a, &b, &[], ClosureDirection::Outgoing));
    }

    // Normal: textual patterns parse to the expected atoms and round-trip
    // through format_pattern.
    #[test]
    fn parse_pattern_atoms_and_roundtrip_normal() {
        let pat = parse_pattern("Contains Calls+ any*").unwrap();
        assert_eq!(
            pat,
            vec![
                PatternAtom::one(EdgeKind::Contains),
                PatternAtom::plus(EdgeKind::Calls),
                PatternAtom::any(Rep::Star),
            ]
        );
        assert_eq!(format_pattern(&pat), "Contains Calls+ any*");
        assert_eq!(parse_pattern(&format_pattern(&pat)).unwrap(), pat);
    }

    // Normal: a parsed pattern drives the same product BFS as a hand-built
    // one (Contains then Calls+ over the two-stage fixture).
    #[test]
    fn parse_pattern_drives_reachability_normal() {
        let mut g = CodeGraph::new();
        let m = g.add_node(mk("m", NodeKind::Module));
        let f0 = g.add_node(mk("f0", NodeKind::Function));
        let f1 = g.add_node(mk("f1", NodeKind::Function));
        g.add_edge(&m, &f0, ed(EdgeKind::Contains)).unwrap();
        g.add_edge(&f0, &f1, ed(EdgeKind::Calls)).unwrap();

        let pat = parse_pattern("Contains Calls+").unwrap();
        let targets = reachable_targets(&g, &m, &pat, ClosureDirection::Outgoing);
        assert_eq!(targets, vec![f1.clone()]);
    }

    // Robust: unknown labels and dangling repetition suffixes are rejected
    // with the offending atom named; empty input is the empty pattern.
    #[test]
    fn parse_pattern_rejects_bad_atoms_robust() {
        let err = parse_pattern("Calls+ Bogus").unwrap_err();
        assert_eq!(err.atom, "Bogus");
        assert!(err.reason.contains("unknown edge label"));

        let err = parse_pattern("+").unwrap_err();
        assert_eq!(err.atom, "+");
        assert!(err.reason.contains("repetition suffix"));

        assert_eq!(parse_pattern("").unwrap(), Vec::new());
        assert_eq!(parse_pattern("   ").unwrap(), Vec::new());
    }

    // Robust: payload-carrying kinds parse to discriminant matchers — an
    // `UnresolvedCall` atom matches any payload.
    #[test]
    fn parse_pattern_unresolved_call_matches_by_discriminant_robust() {
        let mut g = CodeGraph::new();
        let a = g.add_node(mk("a", NodeKind::Function));
        let b = g.add_node(mk("b", NodeKind::Function));
        g.add_edge(&a, &b, ed(EdgeKind::UnresolvedCall("payload".into())))
            .unwrap();

        let pat = parse_pattern("UnresolvedCall").unwrap();
        let targets = reachable_targets(&g, &a, &pat, ClosureDirection::Outgoing);
        assert_eq!(targets, vec![b.clone()]);
    }

    // Robust: an Any-matcher Plus traverses a mixed-label path.
    #[test]
    fn any_matcher_traverses_mixed_labels_robust() {
        let mut g = CodeGraph::new();
        let m = g.add_node(mk("m", NodeKind::Module));
        let f = g.add_node(mk("f", NodeKind::Function));
        let s = g.add_node(mk("S", NodeKind::Struct));
        g.add_edge(&m, &f, ed(EdgeKind::Contains)).unwrap();
        g.add_edge(&f, &s, ed(EdgeKind::UsesType)).unwrap();

        let pat = [PatternAtom::any(Rep::Plus)];
        let targets = reachable_targets(&g, &m, &pat, ClosureDirection::Outgoing);
        assert!(targets.contains(&f) && targets.contains(&s));
    }
}
