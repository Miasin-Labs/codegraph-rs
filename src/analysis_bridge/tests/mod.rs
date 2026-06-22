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

use super::*;
use crate::analysis_bridge::cache::*;
use crate::analysis_bridge::mapping::*;
use crate::analysis_bridge::options::BridgeOptions;
use crate::analysis_bridge::sidecar::*;
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
