use super::*;

// =============================================================================
// Call-graph dataflow oracle
// =============================================================================

/// Coarse interprocedural [`DataflowOracle`] derived from call edges — the
/// "coarse pass derived from `Calls` edges" the slicing module documents as
/// the drop-in until per-function IR is available.
///
/// Direction matches [`codegraph_analysis::slicing::PointsToOracle`]:
/// `def_uses` walks callers (values flow *into* a function from its
/// callers), `use_defs` walks callees. Unresolved calls are included so
/// slices surface where the static graph runs out.
pub struct CallGraphOracle {
    callers: HashMap<ANodeId, Vec<ANodeId>>,
    callees: HashMap<ANodeId, Vec<ANodeId>>,
}

impl CallGraphOracle {
    /// Cache caller/callee adjacency over `Calls` and `UnresolvedCall` edges.
    pub fn build(graph: &AnalysisGraph) -> Self {
        let mut callers: HashMap<ANodeId, Vec<ANodeId>> = HashMap::new();
        let mut callees: HashMap<ANodeId, Vec<ANodeId>> = HashMap::new();
        for id in graph.all_node_ids() {
            for (target, edge) in graph.get_edges_from(id) {
                if matches!(edge.kind, AEdgeKind::Calls | AEdgeKind::UnresolvedCall(_)) {
                    callees.entry(id.clone()).or_default().push(target.clone());
                    callers.entry(target.clone()).or_default().push(id.clone());
                }
            }
        }
        for list in callers.values_mut().chain(callees.values_mut()) {
            list.sort();
            list.dedup();
        }
        Self { callers, callees }
    }
}

impl DataflowOracle for CallGraphOracle {
    fn def_uses(&self, node: &ANodeId) -> Vec<ANodeId> {
        self.callers.get(node).cloned().unwrap_or_default()
    }

    fn use_defs(&self, node: &ANodeId) -> Vec<ANodeId> {
        self.callees.get(node).cloned().unwrap_or_default()
    }
}
