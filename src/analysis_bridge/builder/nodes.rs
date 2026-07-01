use std::collections::HashMap;
use std::path::PathBuf;

use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::nodes::{NodeData as ANodeData, NodeId as ANodeId};
use codegraph_analysis::partial::{self, FieldInfo as AFieldInfo};

use crate::analysis_bridge::builder::edges::Enrichment;
use crate::analysis_bridge::builder::{MappedNodes, Owners};
use crate::analysis_bridge::mapping::{map_node_kind, map_visibility, node_span};
use crate::analysis_bridge::stats::BridgeStats;
use crate::error::{Result, log_debug};
use crate::types::Node;

pub(super) fn map_nodes<'a>(
    nodes: &'a [Node],
    stats: &mut BridgeStats,
) -> (MappedNodes<'a>, Owners<'a>) {
    let mut mapped = HashMap::new();
    let mut owner = HashMap::new();
    for node in nodes {
        let Some(akind) = map_node_kind(node.kind) else {
            stats.nodes_skipped += 1;
            *stats
                .skipped_node_kinds
                .entry(node.kind.as_str().to_string())
                .or_default() += 1;
            continue;
        };
        let aid = ANodeId::new(&node.file_path, &node.qualified_name, akind);
        owner.entry(aid.clone()).or_insert(node.id.as_str());
        mapped.insert(node.id.as_str(), (aid, akind));
    }
    (mapped, owner)
}

pub(super) struct NodeInsert<'a> {
    pub(super) graph: &'a mut AnalysisGraph,
    pub(super) nodes: &'a [Node],
    pub(super) mapped: &'a MappedNodes<'a>,
    pub(super) owner: &'a Owners<'a>,
    pub(super) enrichment: &'a Enrichment,
    pub(super) id_map: &'a mut HashMap<String, ANodeId>,
    pub(super) stats: &'a mut BridgeStats,
}

impl NodeInsert<'_> {
    pub(super) fn run(&mut self) -> Result<()> {
        for node in self.nodes {
            let Some((aid, akind)) = self.mapped.get(node.id.as_str()) else {
                continue;
            };
            self.id_map.insert(node.id.clone(), aid.clone());
            if self.owner.get(aid) != Some(&node.id.as_str()) {
                self.stats.nodes_deduped += 1;
                continue;
            }
            let metadata = self.metadata(node, aid)?;
            if node.byte_range().is_none() {
                self.stats.nodes_missing_byte_range += 1;
            }
            self.graph.add_node(ANodeData {
                id: aid.clone(),
                kind: *akind,
                name: node.name.clone(),
                qualified_name: node.qualified_name.clone(),
                file_path: PathBuf::from(&node.file_path),
                span: node_span(node),
                visibility: map_visibility(node.visibility),
                metadata,
                birth_revision: 0,
                last_modified_revision: 0,
                complexity: None,
                cfg: None,
                dataflow: None,
            });
            self.stats.nodes_mapped += 1;
        }
        self.register_engine_metadata();
        Ok(())
    }

    fn metadata(&self, node: &Node, aid: &ANodeId) -> Result<HashMap<String, String>> {
        let mut metadata = HashMap::new();
        metadata.insert("codegraph_id".to_string(), node.id.clone());
        metadata.insert("codegraph_kind".to_string(), node.kind.as_str().to_string());
        if let Some(is_async) = node.is_async {
            metadata.insert("async".to_string(), is_async.to_string());
        }
        if let Some(exported) = node.is_exported {
            metadata.insert("exported".to_string(), exported.to_string());
        }
        if let Some(sig) = &node.signature {
            metadata.insert("signature".to_string(), sig.clone());
        }
        if let Some(set) = self.enrichment.fields.get(aid) {
            if !self.enrichment.engine_fields.contains_key(aid) {
                let arr: Vec<&String> = set.iter().collect();
                metadata.insert("fields".to_string(), serde_json::to_string(&arr)?);
            }
        }
        if let Some(set) = self.enrichment.variants.get(aid) {
            let arr: Vec<&String> = set.iter().collect();
            metadata.insert("variants".to_string(), serde_json::to_string(&arr)?);
        }
        if let Some(set) = self.enrichment.accessed_fields.get(aid) {
            if !self.enrichment.engine_accessed.contains_key(aid) {
                let arr: Vec<&String> = set.iter().collect();
                metadata.insert("accessed_fields".to_string(), serde_json::to_string(&arr)?);
            }
        }
        if let Some(set) = self.enrichment.global_reads.get(aid) {
            let arr: Vec<&String> = set.iter().collect();
            metadata.insert("global_reads".to_string(), serde_json::to_string(&arr)?);
        }
        if let Some(set) = self.enrichment.global_writes.get(aid) {
            let arr: Vec<&String> = set.iter().collect();
            metadata.insert("global_writes".to_string(), serde_json::to_string(&arr)?);
        }
        if let Some(set) = self.enrichment.string_refs.get(aid) {
            let arr: Vec<&String> = set.iter().collect();
            metadata.insert("string_refs".to_string(), serde_json::to_string(&arr)?);
        }
        if let Some(items) = self.enrichment.memory_accesses.get(aid) {
            metadata.insert("memory_accesses".to_string(), serde_json::to_string(items)?);
        }
        if let Some(items) = self.enrichment.call_argument_roles.get(aid) {
            metadata.insert(
                "call_argument_roles".to_string(),
                serde_json::to_string(items)?,
            );
        }
        if let Some(items) = self.enrichment.ida_cfg.get(aid) {
            metadata.insert("ida_cfg".to_string(), serde_json::to_string(items)?);
        }
        Ok(metadata)
    }

    fn register_engine_metadata(&mut self) {
        for (aid, field_map) in &self.enrichment.engine_fields {
            let infos: Vec<AFieldInfo> = field_map.values().cloned().collect();
            match partial::set_struct_fields(self.graph, aid, &infos) {
                Ok(()) => self.stats.struct_fields_registered += infos.len(),
                Err(e) => log_debug(
                    "analysis bridge: struct-field registration rejected",
                    Some(&serde_json::json!({ "error": e.to_string() })),
                ),
            }
        }
        for (aid, names) in &self.enrichment.engine_accessed {
            match partial::set_accessed_fields(self.graph, aid, names) {
                Ok(()) => self.stats.accessed_fields_registered += 1,
                Err(e) => log_debug(
                    "analysis bridge: accessed-field registration rejected",
                    Some(&serde_json::json!({ "error": e.to_string() })),
                ),
            }
        }
    }
}
