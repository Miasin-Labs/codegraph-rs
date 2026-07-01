use std::collections::{BTreeMap, BTreeSet, HashMap};

use codegraph_analysis::edges::EdgeData as AEdgeData;
use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::nodes::{
    NodeId as ANodeId,
    NodeKind as ANodeKind,
    Span as ASpan,
    Visibility as AVisibility,
};
use codegraph_analysis::partial::FieldInfo as AFieldInfo;
use serde_json::Value;

use super::{MappedNodes, NodeById};
use crate::analysis_bridge::mapping::{
    engine_safe_field_name,
    field_type_from,
    map_edge_kind,
    map_visibility,
};
use crate::analysis_bridge::options::BridgeOptions;
use crate::analysis_bridge::rows::EdgeRow;
use crate::analysis_bridge::stats::BridgeStats;
use crate::error::log_debug;
use crate::types::{EdgeKind, Node, NodeKind};

pub(super) type PendingEdge = (ANodeId, ANodeId, AEdgeData);

#[derive(Default)]
pub(super) struct Enrichment {
    pub(super) fields: HashMap<ANodeId, BTreeSet<String>>,
    pub(super) variants: HashMap<ANodeId, BTreeSet<String>>,
    pub(super) accessed_fields: HashMap<ANodeId, BTreeSet<String>>,
    pub(super) global_reads: HashMap<ANodeId, BTreeSet<String>>,
    pub(super) global_writes: HashMap<ANodeId, BTreeSet<String>>,
    pub(super) string_refs: HashMap<ANodeId, BTreeSet<String>>,
    pub(super) memory_accesses: HashMap<ANodeId, Vec<Value>>,
    pub(super) call_argument_roles: HashMap<ANodeId, Vec<Value>>,
    pub(super) ida_cfg: HashMap<ANodeId, Vec<Value>>,
    pub(super) engine_fields: BTreeMap<ANodeId, BTreeMap<String, AFieldInfo>>,
    pub(super) engine_accessed: BTreeMap<ANodeId, Vec<String>>,
}

impl Enrichment {
    pub(super) fn finalize_engine_accessed(
        &mut self,
        options: &BridgeOptions,
        stats: &mut BridgeStats,
    ) {
        if !options.include_fields {
            return;
        }
        for (aid, names) in &self.accessed_fields {
            let valid: Vec<String> = names
                .iter()
                .filter(|n| engine_safe_field_name(n))
                .cloned()
                .collect();
            stats.fields_skipped_invalid += names.len() - valid.len();
            if !valid.is_empty() {
                self.engine_accessed.insert(aid.clone(), valid);
            }
        }
    }
}

pub(super) struct EdgePass<'a> {
    pub(super) rows: &'a [EdgeRow],
    pub(super) node_by_id: &'a NodeById<'a>,
    pub(super) mapped: &'a MappedNodes<'a>,
    pub(super) options: &'a BridgeOptions,
    pub(super) enrichment: &'a mut Enrichment,
    pub(super) pending_edges: &'a mut Vec<PendingEdge>,
    pub(super) stats: &'a mut BridgeStats,
}

struct GraphEdgeInput<'a> {
    row: &'a EdgeRow,
    kind: EdgeKind,
    src_node: &'a Node,
    tgt_node: &'a Node,
    src_aid: &'a ANodeId,
    src_akind: ANodeKind,
    tgt_aid: &'a ANodeId,
    tgt_akind: ANodeKind,
}

struct SkippedTargetInput<'a> {
    kind: EdgeKind,
    tgt_node: &'a Node,
    src_aid: &'a ANodeId,
    src_akind: ANodeKind,
}

impl EdgePass<'_> {
    pub(super) fn run(&mut self) {
        for row in self.rows {
            let Ok(kind) = row.kind.parse::<EdgeKind>() else {
                skip_edge(self.stats, "unknown_edge_kind");
                continue;
            };
            let (Some(src_node), Some(tgt_node)) = (
                self.node_by_id.get(row.source.as_str()),
                self.node_by_id.get(row.target.as_str()),
            ) else {
                skip_edge(self.stats, "dangling_endpoint");
                continue;
            };

            match (
                self.mapped.get(row.source.as_str()),
                self.mapped.get(row.target.as_str()),
            ) {
                (Some((src_aid, src_akind)), Some((tgt_aid, tgt_akind))) => {
                    self.queue_graph_edge(GraphEdgeInput {
                        row,
                        kind,
                        src_node,
                        tgt_node,
                        src_aid,
                        src_akind: *src_akind,
                        tgt_aid,
                        tgt_akind: *tgt_akind,
                    });
                }
                (Some((src_aid, src_akind)), None) => {
                    self.fold_skipped_target(SkippedTargetInput {
                        kind,
                        tgt_node,
                        src_aid,
                        src_akind: *src_akind,
                    });
                }
                _ => skip_edge(self.stats, "source_not_mapped"),
            }
        }
    }

    fn queue_graph_edge(&mut self, input: GraphEdgeInput<'_>) {
        self.fold_data_and_fact_metadata(&input);
        let Some(akind) = map_edge_kind(input.kind, input.src_akind, input.tgt_akind) else {
            skip_edge(
                self.stats,
                &format!(
                    "invariant_{}_{:?}_to_{:?}",
                    input.kind.as_str(),
                    input.src_akind,
                    input.tgt_akind
                ),
            );
            return;
        };
        let span = ASpan {
            file: input.src_node.file_path.clone().into(),
            start_line: input.row.line.unwrap_or(input.src_node.start_line),
            start_col: input.row.col.unwrap_or(0),
            end_line: input.row.line.unwrap_or(input.src_node.start_line),
            end_col: input.row.col.unwrap_or(0),
            byte_range: 0..0,
        };
        self.pending_edges.push((
            input.src_aid.clone(),
            input.tgt_aid.clone(),
            AEdgeData {
                kind: akind,
                source_span: span,
                weight: 1.0,
            },
        ));
    }

    fn fold_data_and_fact_metadata(&mut self, input: &GraphEdgeInput<'_>) {
        if input.src_akind != ANodeKind::Function {
            return;
        }
        if let Some(metadata) = &input.row.metadata {
            let fact = Value::Object(metadata.clone());
            match metadata.get("kind").and_then(Value::as_str) {
                Some("memory_access") => {
                    self.enrichment
                        .memory_accesses
                        .entry(input.src_aid.clone())
                        .or_default()
                        .push(fact);
                    return;
                }
                Some("call_argument_roles") => {
                    self.enrichment
                        .call_argument_roles
                        .entry(input.src_aid.clone())
                        .or_default()
                        .push(fact);
                    return;
                }
                Some("ida_cfg") => {
                    self.enrichment
                        .ida_cfg
                        .entry(input.src_aid.clone())
                        .or_default()
                        .push(fact);
                    return;
                }
                _ => {}
            }
        }
        match (input.kind, input.tgt_node.kind) {
            (EdgeKind::Reads, NodeKind::DataSymbol)
                if is_real_data_symbol(&input.tgt_node.name) =>
            {
                self.enrichment
                    .global_reads
                    .entry(input.src_aid.clone())
                    .or_default()
                    .insert(input.tgt_node.name.clone());
            }
            (EdgeKind::Writes, NodeKind::DataSymbol)
                if is_real_data_symbol(&input.tgt_node.name) =>
            {
                self.enrichment
                    .global_writes
                    .entry(input.src_aid.clone())
                    .or_default()
                    .insert(input.tgt_node.name.clone());
            }
            (EdgeKind::References, NodeKind::StringLiteral) => {
                self.enrichment
                    .string_refs
                    .entry(input.src_aid.clone())
                    .or_default()
                    .insert(input.tgt_node.qualified_name.clone());
            }
            _ => {}
        }
    }

    fn fold_skipped_target(&mut self, input: SkippedTargetInput<'_>) {
        match input.kind {
            EdgeKind::Contains
                if matches!(input.tgt_node.kind, NodeKind::Field | NodeKind::Property)
                    && matches!(
                        input.src_akind,
                        ANodeKind::Struct | ANodeKind::Enum | ANodeKind::Trait
                    ) =>
            {
                self.enrichment
                    .fields
                    .entry(input.src_aid.clone())
                    .or_default()
                    .insert(input.tgt_node.name.clone());
                self.fold_engine_field(input.tgt_node, input.src_aid, input.src_akind);
                self.stats.edges_enriched += 1;
            }
            EdgeKind::Contains
                if input.tgt_node.kind == NodeKind::EnumMember
                    && input.src_akind == ANodeKind::Enum =>
            {
                self.enrichment
                    .variants
                    .entry(input.src_aid.clone())
                    .or_default()
                    .insert(input.tgt_node.name.clone());
                self.stats.edges_enriched += 1;
            }
            EdgeKind::References | EdgeKind::TypeOf
                if input.src_akind == ANodeKind::Function
                    && matches!(input.tgt_node.kind, NodeKind::Field | NodeKind::Property) =>
            {
                self.enrichment
                    .accessed_fields
                    .entry(input.src_aid.clone())
                    .or_default()
                    .insert(input.tgt_node.name.clone());
                self.stats.edges_enriched += 1;
            }
            _ => skip_edge(self.stats, "target_not_mapped"),
        }
    }

    fn fold_engine_field(&mut self, tgt_node: &Node, src_aid: &ANodeId, src_akind: ANodeKind) {
        if !self.options.include_fields || src_akind != ANodeKind::Struct {
            return;
        }
        if !engine_safe_field_name(&tgt_node.name) {
            self.stats.fields_skipped_invalid += 1;
            return;
        }
        self.enrichment
            .engine_fields
            .entry(src_aid.clone())
            .or_default()
            .entry(tgt_node.name.clone())
            .or_insert_with(|| AFieldInfo {
                name: tgt_node.name.clone(),
                type_str: field_type_from(tgt_node),
                is_public: matches!(map_visibility(tgt_node.visibility), AVisibility::Public),
            });
    }
}

fn is_real_data_symbol(name: &str) -> bool {
    !(name.starts_with("mem:")
        || name.starts_with("callarg:")
        || name.starts_with("label:")
        || name.starts_with("switch:"))
}

pub(super) fn insert_pending_edges(
    graph: &mut AnalysisGraph,
    pending_edges: Vec<PendingEdge>,
    stats: &mut BridgeStats,
) {
    for (from, to, data) in pending_edges {
        match graph.add_edge(&from, &to, data) {
            Ok(()) => stats.edges_mapped += 1,
            Err(e) => {
                log_debug(
                    "analysis bridge: edge rejected by analysis-graph invariant",
                    Some(&serde_json::json!({ "error": e.to_string() })),
                );
                skip_edge(stats, "rejected_at_insert");
            }
        }
    }
}

pub(super) fn skip_edge(stats: &mut BridgeStats, reason: &str) {
    stats.edges_skipped += 1;
    *stats
        .skipped_edge_reasons
        .entry(reason.to_string())
        .or_default() += 1;
}
