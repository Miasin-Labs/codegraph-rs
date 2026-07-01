//! Inference-based vulnerability engine.
//!
//! The design principle (the lesson from hardcoded rule tables not scaling):
//! **separate the *property* from the *signature*.** A small fixed set of
//! property [`TemplateKind`]s is hardcoded; the per-codebase *signatures* —
//! which functions are sinks, guards, sources, sanitizers — are *inferred*
//! ([`PredicateSet`]) rather than listed. A "bug class" (IDOR, BAC, SSRF,
//! lossy-send) is then a template instantiated with an inferred predicate set,
//! not new engine code.
//!
//! Signatures are inferred by stacked mechanisms, each tagged with its
//! [`InferenceOrigin`] so confidence can be combined and audited:
//!
//! * **Frequency** — deviant-behavior mining over corpus consistency
//!   ([`mining`]). If almost every caller of a sink passes through some guard,
//!   the few that don't are anomalies, and the guard is *discovered* (not
//!   named). This is the rule-free core; see [`mining::mine_missing_guards`].
//! * **Name** — the `taint_naming` lexicon (source/sink-leaning identifiers).
//! * **History** — learned from fix-commits and tool-call traces.
//! * **Llm** — proposed by a model, then *verified against graph facts* before
//!   any finding is emitted.
//! * **Seed** — a small hardcoded anchor set to bootstrap (tier 0).
//!
//! The graph substrate this leans on already exists in the crate: the call
//! graph ([`crate::graph`]), dominators ([`crate::dominators`]), path
//! preconditions ([`crate::predicates`]), and taint ([`crate::taint_v2`]).

pub mod cfg_dominance;
pub mod classify;
pub mod fix_history;
pub mod learned;
pub mod mining;
pub mod taint_seed;

use crate::nodes::NodeId;

/// Where an inferred predicate role (sink/guard/source/sanitizer) came from.
/// Findings combine origins: a frequency-mined guard later confirmed by an LLM
/// or by fix-history carries more confidence than either alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceOrigin {
    /// Discovered from corpus consistency (deviant-frequency mining).
    Frequency,
    /// Inferred from the name lexicon (`taint_naming`-style).
    Name,
    /// Learned from fix-commit / tool-call history.
    History,
    /// Proposed by an LLM and verified against graph facts.
    Llm,
    /// A hardcoded seed anchor (tier 0 bootstrap).
    Seed,
}

impl InferenceOrigin {
    pub fn id(self) -> &'static str {
        match self {
            InferenceOrigin::Frequency => "frequency",
            InferenceOrigin::Name => "name",
            InferenceOrigin::History => "history",
            InferenceOrigin::Llm => "llm",
            InferenceOrigin::Seed => "seed",
        }
    }
}

/// The fixed library of vulnerability *property* templates. The engine is these
/// templates; a bug class is one of them + an inferred [`PredicateSet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    /// A sink reachable on a path that does not pass a guard most sibling call
    /// sites pass — missing authorization / validation (BAC, IDOR).
    MissingDominatorCheck,
    /// A tainted source reaches a sink with no sanitizer dominating the path
    /// (IDOR, SSRF, injection).
    ReachesWithoutSanitizer,
    /// Operation A occurs but a required follow B is absent on some path
    /// (lossy-send-without-delivery, lock-without-unlock).
    MustFollow,
    /// Pure statistical deviation from a learned norm.
    DeviantFrequency,
}

impl TemplateKind {
    pub fn id(self) -> &'static str {
        match self {
            TemplateKind::MissingDominatorCheck => "missing_dominator_check",
            TemplateKind::ReachesWithoutSanitizer => "reaches_without_sanitizer",
            TemplateKind::MustFollow => "must_follow",
            TemplateKind::DeviantFrequency => "deviant_frequency",
        }
    }
}

/// An inferred set of roles for a template instantiation — the discovered
/// "rule", as data rather than code. Persisted (with confidence) so the engine
/// improves over time as more code is indexed and feedback arrives.
#[derive(Debug, Clone, Default)]
pub struct PredicateSet {
    pub sinks: Vec<NodeId>,
    pub guards: Vec<NodeId>,
    pub sources: Vec<NodeId>,
    pub sanitizers: Vec<NodeId>,
    pub confidence: f64,
    pub origin_summary: Vec<InferenceOrigin>,
    pub evidence: String,
}

/// A candidate vulnerability: a deviation from an inferred norm.
#[derive(Debug, Clone, PartialEq)]
pub struct VulnFinding {
    pub template: TemplateKind,
    /// Heuristic human label derived from the inferred roles' names
    /// (e.g. "BAC", "IDOR"). This is a *label only* — detection does not depend
    /// on it. `None` when the deviation doesn't match a known class shape.
    pub class: Option<String>,
    /// The function where the anomaly sits (e.g. the caller missing the guard).
    pub site: NodeId,
    /// The sink the site reaches unguarded.
    pub sink: NodeId,
    /// The guard(s)/sanitizer(s) present at sibling sites but missing here.
    pub expected: Vec<NodeId>,
    /// How many sibling call sites *did* have the expected guard.
    pub support: u32,
    /// Total comparable call sites considered.
    pub total: u32,
    /// Strength of the inferred norm, in `[0, 1]`.
    pub confidence: f64,
    pub origin: InferenceOrigin,
    pub message: String,
}
