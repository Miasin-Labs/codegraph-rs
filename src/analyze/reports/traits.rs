use super::{ANodeKind, AnalysisGraph, Serialize, SymbolRef, symbol_ref, symbol_sort_key};

// =============================================================================
// analyze traits
// =============================================================================

/// One trait/interface/protocol and its direct implementors.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitHierarchySummary {
    #[serde(rename = "trait")]
    pub trait_ref: SymbolRef,
    pub implementor_count: usize,
    pub implementors: Vec<SymbolRef>,
}

/// A call edge that dispatches through a trait-declared method.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitDispatchSummary {
    pub caller: SymbolRef,
    pub callee: SymbolRef,
    #[serde(rename = "trait")]
    pub trait_ref: SymbolRef,
}

/// Functions clustered by the type they manipulate most.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeClusterSummary {
    pub primary_type: SymbolRef,
    pub function_count: usize,
    pub functions: Vec<SymbolRef>,
    pub truncated: bool,
}

/// Result of [`traits_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitsReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_filter: Option<String>,
    pub trait_count: usize,
    pub hierarchies: Vec<TraitHierarchySummary>,
    pub dispatch_call_count: usize,
    pub dispatch_calls: Vec<TraitDispatchSummary>,
    pub cluster_count: usize,
    pub clusters: Vec<TypeClusterSummary>,
    pub note: String,
}

/// Cap on dispatch-call rows and clusters listed (full data via `--json`
/// consumers can re-run with a bigger graph slice; these keep human output
/// readable).
const TRAITS_DISPATCH_CAP: usize = 50;
const TRAITS_CLUSTER_CAP: usize = 25;
const TRAITS_CLUSTER_MEMBER_SAMPLE: usize = 10;

/// Case-sensitive symbol filter: exact name, or qualified-name suffix.
pub(crate) fn matches_symbol_filter(s: &SymbolRef, filter: &str) -> bool {
    s.name == filter || s.qualified_name == filter || s.qualified_name.ends_with(filter)
}

/// Trait/type hierarchy analyses over the bridged Implements/Contains/
/// UsesType edges (engine entry points: `trait_hierarchies`,
/// `trait_dispatch_calls`, `cluster_by_primary_type`).
pub fn traits_report(graph: &AnalysisGraph, type_filter: Option<&str>) -> TraitsReport {
    let mut hierarchies: Vec<TraitHierarchySummary> = graph
        .trait_hierarchies()
        .into_iter()
        .filter_map(|h| {
            let trait_ref = graph.get_node(&h.trait_id).map(symbol_ref)?;
            let mut implementors: Vec<SymbolRef> = h
                .direct_impls
                .iter()
                .filter_map(|id| graph.get_node(id))
                .map(symbol_ref)
                .collect();
            implementors.sort();
            Some(TraitHierarchySummary {
                trait_ref,
                implementor_count: implementors.len(),
                implementors,
            })
        })
        .filter(|h| match type_filter {
            Some(f) => {
                matches_symbol_filter(&h.trait_ref, f)
                    || h.implementors.iter().any(|i| matches_symbol_filter(i, f))
            }
            None => true,
        })
        .collect();
    hierarchies.sort_by(|a, b| {
        b.implementor_count
            .cmp(&a.implementor_count)
            .then_with(|| symbol_sort_key(&a.trait_ref).cmp(&symbol_sort_key(&b.trait_ref)))
    });

    let mut dispatch_calls: Vec<TraitDispatchSummary> = graph
        .trait_dispatch_calls()
        .into_iter()
        .filter_map(|d| {
            Some(TraitDispatchSummary {
                caller: graph.get_node(&d.caller).map(symbol_ref)?,
                callee: graph.get_node(&d.callee).map(symbol_ref)?,
                trait_ref: graph.get_node(&d.trait_id).map(symbol_ref)?,
            })
        })
        .filter(|d| match type_filter {
            Some(f) => {
                matches_symbol_filter(&d.trait_ref, f)
                    || matches_symbol_filter(&d.caller, f)
                    || matches_symbol_filter(&d.callee, f)
            }
            None => true,
        })
        .collect();
    dispatch_calls.sort_by(|a, b| {
        (symbol_sort_key(&a.caller), symbol_sort_key(&a.callee))
            .cmp(&(symbol_sort_key(&b.caller), symbol_sort_key(&b.callee)))
    });
    let dispatch_call_count = dispatch_calls.len();
    dispatch_calls.truncate(TRAITS_DISPATCH_CAP);

    let mut clusters: Vec<TypeClusterSummary> = graph
        .cluster_by_primary_type()
        .into_iter()
        .filter_map(|c| {
            let primary_type = graph.get_node(&c.primary_type).map(symbol_ref)?;
            let mut functions: Vec<SymbolRef> = c
                .functions
                .iter()
                .filter_map(|id| graph.get_node(id))
                .map(symbol_ref)
                .collect();
            functions.sort();
            let function_count = functions.len();
            let truncated = functions.len() > TRAITS_CLUSTER_MEMBER_SAMPLE;
            functions.truncate(TRAITS_CLUSTER_MEMBER_SAMPLE);
            Some(TypeClusterSummary {
                primary_type,
                function_count,
                functions,
                truncated,
            })
        })
        .filter(|c| match type_filter {
            Some(f) => matches_symbol_filter(&c.primary_type, f),
            None => true,
        })
        .collect();
    let cluster_count = clusters.len();
    clusters.truncate(TRAITS_CLUSTER_CAP);

    let note = if graph.nodes_by_kind(ANodeKind::Trait).is_empty() {
        "The bridged graph contains no trait/interface/protocol nodes — hierarchies and \
         dispatch detection have nothing to work on in this index."
            .to_string()
    } else {
        "Dispatch detection is structural: a call counts as trait dispatch when the callee is \
         a method the trait itself declares (Trait → contains → method). Calls resolved \
         directly to a concrete implementation are regular call edges, not dispatch."
            .to_string()
    };

    TraitsReport {
        type_filter: type_filter.map(str::to_string),
        trait_count: hierarchies.len(),
        hierarchies,
        dispatch_call_count,
        dispatch_calls,
        cluster_count,
        clusters,
        note,
    }
}
