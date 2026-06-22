use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use codegraph_analysis::edges::EdgeKind as AEdgeKind;
use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::nodes::{
    NodeData as ANodeData,
    NodeId as ANodeId,
    NodeKind as ANodeKind,
    Span as ASpan,
    Visibility as AVisibility,
};

use super::{BridgeResult, BridgeStats};
use crate::analysis_bridge::cache::{
    CACHE_META_FILE,
    COMPLEXITY_SIDECAR_FILE,
    GRAPH_SNAPSHOT_FILE,
    PREV_SUFFIX,
    SNAPSHOT_CACHE_SCHEMA_VERSION,
    analysis_cache_dir_with_override,
    load_cache,
    read_cache_meta,
    store_cache,
    workspace_cache_key,
};
use crate::analysis_bridge::mapping::{
    engine_safe_field_name,
    field_type_from,
    map_edge_kind,
    map_node_kind,
};
use crate::analysis_bridge::options::BridgeOptions;
use crate::analysis_bridge::sidecar::{
    ComplexitySidecar,
    StoredComplexity,
    load_complexity_sidecar,
    store_complexity_sidecar,
};
use crate::analysis_bridge::snapshot::{
    BaseGeneration,
    load_auto_base_snapshot,
    load_explicit_base_snapshot,
};
use crate::types::{EdgeKind, Node, NodeKind};

mod cache_behavior;
mod mapping_contract;
mod option_gate;
mod snapshot_base;

fn sample_bridge_result() -> BridgeResult {
    let mut graph = AnalysisGraph::new();
    let aid = ANodeId::new("src/a.ts", "alpha", ANodeKind::Function);
    graph.add_node(ANodeData {
        id: aid.clone(),
        kind: ANodeKind::Function,
        name: "alpha".to_string(),
        qualified_name: "alpha".to_string(),
        file_path: PathBuf::from("src/a.ts"),
        span: ASpan {
            file: PathBuf::from("src/a.ts"),
            start_line: 1,
            start_col: 0,
            end_line: 3,
            end_col: 1,
            byte_range: 0..0,
        },
        visibility: AVisibility::Public,
        metadata: HashMap::new(),
        birth_revision: 0,
        last_modified_revision: 0,
        complexity: None,
        cfg: None,
        dataflow: None,
    });
    let stats = BridgeStats {
        nodes_total: 1,
        nodes_mapped: 1,
        ..Default::default()
    };
    BridgeResult {
        graph,
        id_map: HashMap::from([("cg-node-1".to_string(), aid)]),
        stats,
    }
}
