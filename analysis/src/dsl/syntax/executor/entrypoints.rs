use super::super::EntrypointKind;
use super::{QueryConfig, QueryEngine, QueryResult};

impl<'a> QueryEngine<'a> {
    /// Execute the `entrypoints` selector by delegating to `CodeGraph::classify_entrypoints`.
    pub(super) fn execute_entrypoints(
        &self,
        kind_filter: Option<EntrypointKind>,
        config: &QueryConfig,
    ) -> QueryResult {
        let summaries = self.graph.classify_entrypoints();
        let mut nodes = Vec::new();
        let mut metadata = Vec::new();

        for summary in summaries {
            let dsl_kind = EntrypointKind::from_analysis(summary.kind);
            if let Some(want) = kind_filter
                && dsl_kind != want
            {
                continue;
            }
            let name = self
                .graph
                .get_node(&summary.node_id)
                .map(|n| n.qualified_name.clone())
                .unwrap_or_else(|| format!("{:?}", summary.node_id));
            metadata.push(format!(
                "{:?} {} fan_in={} fan_out={} reach={}",
                dsl_kind, name, summary.fan_in, summary.fan_out, summary.reach_size
            ));
            nodes.push(summary.node_id);
        }

        let total = nodes.len();
        let was_truncated = total > config.max_nodes;
        if was_truncated {
            nodes.truncate(config.max_nodes);
        }
        QueryResult {
            nodes,
            edges: Vec::new(),
            was_truncated,
            total_before_truncation: total,
            cycles_detected: Vec::new(),
            metadata,
        }
    }
}
