use super::*;

// =============================================================================
// analyze generics
// =============================================================================

/// A generic instantiation reported by the engine (callsite-supplied
/// concrete type arguments).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericInstantiationSummary {
    pub generic: SymbolRef,
    pub callsite: SymbolRef,
    pub type_args: Vec<String>,
}

/// A definition that *looks* generic based on its carried signature.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LikelyGenericDefinition {
    pub symbol: SymbolRef,
    /// Type-parameter-looking tokens found in the signature (e.g. `T`, `K`).
    pub type_params: Vec<String>,
}

/// Result of [`generics_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericsReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_filter: Option<String>,
    /// Engine-detected instantiations (callsite type args).
    pub instantiation_count: usize,
    pub instantiations: Vec<GenericInstantiationSummary>,
    /// Signature-heuristic generic definitions (see `note`).
    pub likely_generic_count: usize,
    pub truncated: bool,
    pub likely_generic_definitions: Vec<LikelyGenericDefinition>,
    pub note: String,
}

/// Extract type-parameter-looking tokens from a signature: standalone
/// identifiers of one uppercase letter optionally followed by a digit
/// (`T`, `U`, `K1`) — the conventional generic-parameter shape across
/// Rust/TS/Java/Go/C#.
pub(crate) fn signature_type_params(signature: &str) -> Vec<String> {
    let mut params: Vec<String> = Vec::new();
    let mut token = String::new();
    let flush = |token: &mut String, params: &mut Vec<String>| {
        let looks_generic = match token.len() {
            1 => token.chars().next().is_some_and(|c| c.is_ascii_uppercase()),
            2 => {
                let mut chars = token.chars();
                chars.next().is_some_and(|c| c.is_ascii_uppercase())
                    && chars.next().is_some_and(|c| c.is_ascii_digit())
            }
            _ => false,
        };
        if looks_generic && !params.contains(token) {
            params.push(std::mem::take(token));
        } else {
            token.clear();
        }
    };
    for c in signature.chars() {
        if c.is_alphanumeric() || c == '_' {
            token.push(c);
        } else {
            flush(&mut token, &mut params);
        }
    }
    flush(&mut token, &mut params);
    params.sort();
    params
}

/// Cap on likely-generic definitions listed.
const GENERICS_DEFINITION_CAP: usize = 50;

/// Generic instantiation detection (engine entry point:
/// `monomorphize::find_instantiations`) plus a signature-based heuristic
/// listing of likely generic definitions, since the bridge does not carry
/// the engine's `generic_params`/`callee_type_args` metadata yet.
pub fn generics_report(graph: &AnalysisGraph, symbol_filter: Option<&str>) -> GenericsReport {
    let instantiations: Vec<GenericInstantiationSummary> = find_instantiations(graph)
        .into_iter()
        .filter_map(|inst| {
            Some(GenericInstantiationSummary {
                generic: graph.get_node(&inst.generic_id).map(symbol_ref)?,
                callsite: graph.get_node(&inst.callsite_id).map(symbol_ref)?,
                type_args: inst.type_args,
            })
        })
        .filter(|inst| match symbol_filter {
            Some(f) => {
                matches_symbol_filter(&inst.generic, f) || matches_symbol_filter(&inst.callsite, f)
            }
            None => true,
        })
        .collect();

    let mut definitions: Vec<LikelyGenericDefinition> = graph
        .all_node_ids()
        .into_iter()
        .filter_map(|id| {
            let node = graph.get_node(id)?;
            if is_placeholder(node) {
                return None;
            }
            let signature = node.metadata.get("signature")?;
            let type_params = signature_type_params(signature);
            if type_params.is_empty() {
                return None;
            }
            Some(LikelyGenericDefinition {
                symbol: symbol_ref(node),
                type_params,
            })
        })
        .filter(|d| match symbol_filter {
            Some(f) => matches_symbol_filter(&d.symbol, f),
            None => true,
        })
        .collect();
    definitions.sort_by(|a, b| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)));
    let likely_generic_count = definitions.len();
    let truncated = definitions.len() > GENERICS_DEFINITION_CAP;
    definitions.truncate(GENERICS_DEFINITION_CAP);

    GenericsReport {
        symbol_filter: symbol_filter.map(str::to_string),
        instantiation_count: instantiations.len(),
        instantiations,
        likely_generic_count,
        truncated,
        likely_generic_definitions: definitions,
        note: "Instantiation detection reads the engine's generics metadata contract \
               (generic_params on declarations, callee_type_args on callers); the SQLite \
               bridge does not populate those keys yet, so instantiations over a bridged \
               index are empty until that enrichment lands. likelyGenericDefinitions is a \
               signature heuristic instead: standalone single-letter type tokens (T, U, K1) \
               found in carried signatures."
            .to_string(),
    }
}
