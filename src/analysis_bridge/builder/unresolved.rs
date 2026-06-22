use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use codegraph_analysis::edges::{EdgeData as AEdgeData, EdgeKind as AEdgeKind};
use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::nodes::{
    NodeData as ANodeData,
    NodeId as ANodeId,
    NodeKind as ANodeKind,
    Span as ASpan,
    Visibility as AVisibility,
};

use crate::analysis_bridge::UNRESOLVED_FILE;
use crate::analysis_bridge::builder::{MappedNodes, NodeById};
use crate::analysis_bridge::stats::BridgeStats;
use crate::db::QueryBuilder;
use crate::error::{Result, log_debug};
use crate::types::EdgeKind;

struct UnresolvedEdgeInput<'a> {
    src_aid: &'a ANodeId,
    placeholder_id: &'a ANodeId,
    reference_name: &'a str,
    src_file: String,
    line: u32,
    column: u32,
}

pub(super) struct UnresolvedPass<'a> {
    pub(super) graph: &'a mut AnalysisGraph,
    pub(super) queries: &'a QueryBuilder,
    pub(super) node_by_id: &'a NodeById<'a>,
    pub(super) mapped: &'a MappedNodes<'a>,
    pub(super) stats: &'a mut BridgeStats,
}

impl UnresolvedPass<'_> {
    pub(super) fn run(&mut self) -> Result<()> {
        let mut unresolved = self.queries.get_unresolved_references()?;
        unresolved.sort_by(|a, b| {
            (&a.from_node_id, &a.reference_name, a.line, a.column).cmp(&(
                &b.from_node_id,
                &b.reference_name,
                b.line,
                b.column,
            ))
        });
        self.stats.unresolved_total = unresolved.len();

        let mut emitted = HashSet::new();
        for r in &unresolved {
            let Some((src_aid, ANodeKind::Function)) = self.mapped.get(r.from_node_id.as_str())
            else {
                self.stats.unresolved_skipped += 1;
                continue;
            };
            if r.reference_kind != EdgeKind::Calls {
                self.stats.unresolved_skipped += 1;
                continue;
            }
            if !emitted.insert((src_aid.clone(), r.reference_name.clone(), r.line, r.column)) {
                self.stats.unresolved_skipped += 1;
                continue;
            }

            let placeholder_id =
                ANodeId::new(UNRESOLVED_FILE, &r.reference_name, ANodeKind::Function);
            self.insert_placeholder(&placeholder_id, &r.reference_name);
            let src_file = self
                .node_by_id
                .get(r.from_node_id.as_str())
                .map(|n| n.file_path.clone())
                .or_else(|| r.file_path.clone())
                .unwrap_or_default();
            self.insert_unresolved_edge(UnresolvedEdgeInput {
                src_aid,
                placeholder_id: &placeholder_id,
                reference_name: &r.reference_name,
                src_file,
                line: r.line,
                column: r.column,
            });
        }
        Ok(())
    }

    fn insert_placeholder(&mut self, placeholder_id: &ANodeId, name: &str) {
        if self.graph.get_node(placeholder_id).is_some() {
            return;
        }
        self.graph.add_node(ANodeData {
            id: placeholder_id.clone(),
            kind: ANodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: PathBuf::from(UNRESOLVED_FILE),
            span: ASpan {
                file: PathBuf::from(UNRESOLVED_FILE),
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 0,
                byte_range: 0..0,
            },
            visibility: AVisibility::Public,
            metadata: HashMap::from([("placeholder".to_string(), "true".to_string())]),
            birth_revision: 0,
            last_modified_revision: 0,
            complexity: None,
            cfg: None,
            dataflow: None,
        });
        self.stats.placeholder_nodes += 1;
    }

    fn insert_unresolved_edge(&mut self, input: UnresolvedEdgeInput<'_>) {
        let span = ASpan {
            file: PathBuf::from(input.src_file),
            start_line: input.line,
            start_col: input.column,
            end_line: input.line,
            end_col: input.column,
            byte_range: 0..0,
        };
        match self.graph.add_edge(
            input.src_aid,
            input.placeholder_id,
            AEdgeData {
                kind: AEdgeKind::UnresolvedCall(input.reference_name.to_string()),
                source_span: span,
                weight: 0.5,
            },
        ) {
            Ok(()) => self.stats.unresolved_mapped += 1,
            Err(e) => {
                log_debug(
                    "analysis bridge: unresolved-call edge rejected",
                    Some(&serde_json::json!({ "error": e.to_string() })),
                );
                self.stats.unresolved_skipped += 1;
            }
        }
    }
}
