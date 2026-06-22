use super::{
    ANodeKind,
    AnalysisGraph,
    Serialize,
    SymbolRef,
    classify_name,
    flow_priority,
    symbol_ref,
    symbol_sort_key,
};

// =============================================================================
// analyze taint --suggest
// =============================================================================

/// A function whose name leans source-ish or sink-ish.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaintCandidate {
    pub symbol: SymbolRef,
    /// Fraction of name sub-tokens matching the lexicon, in [0, 1].
    pub score: f64,
}

/// A ranked candidate source→sink pair.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedTaintPair {
    pub source: SymbolRef,
    pub sink: SymbolRef,
    /// `taint_naming::flow_priority` of the pair (name evidence).
    pub priority: f64,
}

/// Result of [`taint_suggest_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaintSuggestReport {
    /// Functions classified (placeholders for unresolved calls included —
    /// library calls like `exec` are prime sink candidates).
    pub functions_classified: usize,
    pub source_count: usize,
    pub sink_count: usize,
    pub sources: Vec<TaintCandidate>,
    pub sinks: Vec<TaintCandidate>,
    pub pairs: Vec<SuggestedTaintPair>,
    pub note: String,
}

/// Candidates listed per side, and the source×sink pool size paired before
/// ranking (caps the cross product on big graphs).
const TAINT_SUGGEST_CANDIDATE_CAP: usize = 15;
const TAINT_SUGGEST_PAIR_POOL: usize = 25;

/// Name-based taint source/sink suggestion (engine entry points:
/// `taint_naming::classify_name`, `taint_naming::flow_priority`) for when no
/// source/sink arguments are given — Fluffy-style lexical priors.
pub fn taint_suggest_report(graph: &AnalysisGraph, top: usize) -> TaintSuggestReport {
    let mut sources: Vec<TaintCandidate> = Vec::new();
    let mut sinks: Vec<TaintCandidate> = Vec::new();
    let mut functions_classified = 0usize;

    for node in graph.nodes_by_kind(ANodeKind::Function) {
        functions_classified += 1;
        let class = classify_name(&node.name);
        if class.looks_like_source() {
            sources.push(TaintCandidate {
                symbol: symbol_ref(node),
                score: class.source_score,
            });
        } else if class.looks_like_sink() {
            sinks.push(TaintCandidate {
                symbol: symbol_ref(node),
                score: class.sink_score,
            });
        }
    }

    let rank = |list: &mut Vec<TaintCandidate>| {
        list.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)))
        });
    };
    rank(&mut sources);
    rank(&mut sinks);
    let source_count = sources.len();
    let sink_count = sinks.len();

    let mut pairs: Vec<SuggestedTaintPair> = Vec::new();
    for source in sources.iter().take(TAINT_SUGGEST_PAIR_POOL) {
        for sink in sinks.iter().take(TAINT_SUGGEST_PAIR_POOL) {
            if source.symbol == sink.symbol {
                continue;
            }
            pairs.push(SuggestedTaintPair {
                source: source.symbol.clone(),
                sink: sink.symbol.clone(),
                priority: flow_priority(&source.symbol.name, &sink.symbol.name),
            });
        }
    }
    pairs.sort_by(|a, b| {
        b.priority
            .total_cmp(&a.priority)
            .then_with(|| symbol_sort_key(&a.source).cmp(&symbol_sort_key(&b.source)))
            .then_with(|| symbol_sort_key(&a.sink).cmp(&symbol_sort_key(&b.sink)))
    });
    pairs.truncate(top);

    sources.truncate(TAINT_SUGGEST_CANDIDATE_CAP);
    sinks.truncate(TAINT_SUGGEST_CANDIDATE_CAP);

    let note = if source_count == 0 && sink_count == 0 {
        "No function name in this graph matches the source/sink lexicons (input/request/env/… \
         vs exec/query/write/…). Name-based suggestion has nothing to rank — pass an explicit \
         <source> <sink> pair instead."
            .to_string()
    } else {
        "Candidates are ranked purely by identifier naming (lexical priors), not by confirmed \
         data flow. Confirm a pair with `codegraph analyze taint <source> <sink>`. Unresolved \
         library calls (file <unresolved>) can rank as sinks but cannot be queried by name."
            .to_string()
    };

    TaintSuggestReport {
        functions_classified,
        source_count,
        sink_count,
        sources,
        sinks,
        pairs,
        note,
    }
}
