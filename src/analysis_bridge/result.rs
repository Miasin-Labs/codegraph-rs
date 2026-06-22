use std::collections::HashMap;
use std::path::Path;

use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::nodes::NodeId as ANodeId;
use codegraph_analysis::session::GraphSession;

use super::BridgeStats;

/// Output of [`crate::analysis_bridge::build_analysis_graph`].
pub struct BridgeResult {
    /// The fully-populated analysis graph.
    pub graph: AnalysisGraph,
    /// codegraph node id (SQLite `nodes.id`) -> analysis `NodeId` for every
    /// mapped node (deduped nodes map to their surviving twin).
    pub id_map: HashMap<String, ANodeId>,
    /// What was mapped / folded / skipped.
    pub stats: BridgeStats,
}

impl BridgeResult {
    /// Wrap the bridged graph in a [`GraphSession`] facade so the full
    /// analysis surface (DSL queries, context engine, explore, cascade,
    /// validation, ...) is available without re-parsing source.
    pub fn into_session(self, workspace_root: &Path) -> (GraphSession, BridgeStats) {
        let session = GraphSession::from_snapshot(self.graph, workspace_root);
        (session, self.stats)
    }
}
