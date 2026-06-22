use super::*;

// =============================================================================
// analyze types
// =============================================================================

/// Result of [`types_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypesReport {
    pub symbol: SymbolRef,
    /// Concrete types that can flow into the function (parameters).
    pub input_types: Vec<String>,
    /// Concrete types the function can produce.
    pub return_types: Vec<String>,
    /// Functions across the graph that received any possible-types
    /// annotation from the propagation pass.
    pub functions_annotated: usize,
    pub note: String,
}

/// Run the engine's possible-types propagation as an enrichment pass over
/// the bridged graph (through `pass.rs` — `PassManager` + `PossibleTypesPass`,
/// seeded `TreeParsed` the way the bridge's graph state warrants), then
/// report the concrete type sets for `symbol`. Returns `Ok(None)` if `symbol`
/// is not in the graph.
pub fn types_report(
    graph: &mut AnalysisGraph,
    symbol: &ANodeId,
) -> Result<Option<TypesReport>, String> {
    let mut manager = PassManager::new();
    manager.seed(GraphFlag::TreeParsed);
    manager.register(Box::new(PossibleTypesPass));
    manager
        .run(graph)
        .map_err(|e| format!("possible-types pass failed: {e}"))?;

    let Some(node) = graph.get_node(symbol) else {
        return Ok(None);
    };

    let parse_list = |key: &str| -> Vec<String> {
        node.metadata
            .get(key)
            .and_then(|v| serde_json::from_str::<Vec<String>>(v).ok())
            .unwrap_or_default()
    };
    let input_types = parse_list("possible_input_types");
    let return_types = parse_list("possible_return_types");

    let annotated: HashSet<&ANodeId> = graph
        .nodes_with_metadata_key("possible_input_types")
        .chain(graph.nodes_with_metadata_key("possible_return_types"))
        .collect();

    let note = if input_types.is_empty() && return_types.is_empty() {
        "No concrete types flow into or out of this function in the bridged graph. The \
         propagation is seeded from resolved UsesType edges and expanded through Implements \
         edges and the call graph — a function with no resolved type references stays empty."
            .to_string()
    } else {
        "Possible types over-approximate: they include every concrete type that could flow \
         here along UsesType/Implements/Calls edges, not just types a real execution \
         delivers. Generics are tracked without their arguments (Vec<T> is Vec)."
            .to_string()
    };

    Ok(Some(TypesReport {
        symbol: symbol_ref(node),
        input_types,
        return_types,
        functions_annotated: annotated.len(),
        note,
    }))
}
