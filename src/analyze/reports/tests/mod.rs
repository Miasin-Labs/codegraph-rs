use std::collections::HashMap;
use std::path::PathBuf;

use codegraph_analysis::edges::EdgeData as AEdgeData;
use codegraph_analysis::nodes::{Span as ASpan, Visibility as AVisibility};

use super::{
    AEdgeKind,
    ANodeData,
    ANodeId,
    ANodeKind,
    AnalysisGraph,
    REPORT_SCHEMA_VERSION,
    ReportEnvelope,
    SliceDirection,
    StoredComplexity,
    boundaries_report,
    capabilities_report,
    centrality_report,
    co_change_report,
    communities_report,
    critical_report,
    cycles_report,
    diff_report,
    dominators_report,
    explain_report,
    export_report,
    generics_report,
    impact_report,
    query_report,
    schema_text,
    signature_type_params,
    slice_report,
    stats_report,
    taint_report,
    taint_suggest_report,
    types_report,
    validate_report,
};

fn span(file: &str, line: u32) -> ASpan {
    ASpan {
        file: PathBuf::from(file),
        start_line: line,
        start_col: 0,
        end_line: line,
        end_col: 0,
        byte_range: 0..0,
    }
}

fn add_fn(graph: &mut AnalysisGraph, file: &str, name: &str, line: u32) -> ANodeId {
    let id = ANodeId::new(file, name, ANodeKind::Function);
    graph.add_node(ANodeData {
        id: id.clone(),
        kind: ANodeKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: PathBuf::from(file),
        span: span(file, line),
        visibility: AVisibility::Public,
        metadata: HashMap::new(),
        birth_revision: 0,
        last_modified_revision: 0,
        complexity: None,
        cfg: None,
        dataflow: None,
    });
    id
}

fn add_call(graph: &mut AnalysisGraph, from: &ANodeId, to: &ANodeId, file: &str) {
    graph
        .add_edge(
            from,
            to,
            AEdgeData {
                kind: AEdgeKind::Calls,
                source_span: span(file, 1),
                weight: 1.0,
            },
        )
        .expect("valid call edge");
}

/// a → b → c, plus the mutual pair d ↔ e.
fn fixture() -> (AnalysisGraph, ANodeId, ANodeId, ANodeId) {
    let mut graph = AnalysisGraph::new();
    let a = add_fn(&mut graph, "src/x.ts", "a", 1);
    let b = add_fn(&mut graph, "src/x.ts", "b", 5);
    let c = add_fn(&mut graph, "src/x.ts", "c", 9);
    let d = add_fn(&mut graph, "src/y.ts", "d", 1);
    let e = add_fn(&mut graph, "src/y.ts", "e", 5);
    add_call(&mut graph, &a, &b, "src/x.ts");
    add_call(&mut graph, &b, &c, "src/x.ts");
    add_call(&mut graph, &d, &e, "src/y.ts");
    add_call(&mut graph, &e, &d, "src/y.ts");
    (graph, a, b, c)
}

fn add_fn_span(graph: &mut AnalysisGraph, file: &str, name: &str, start: u32, end: u32) -> ANodeId {
    let id = ANodeId::new(file, name, ANodeKind::Function);
    graph.add_node(ANodeData {
        id: id.clone(),
        kind: ANodeKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: PathBuf::from(file),
        span: ASpan {
            file: PathBuf::from(file),
            start_line: start,
            start_col: 0,
            end_line: end,
            end_col: 0,
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
    id
}

fn base_snapshot(graph: AnalysisGraph) -> crate::analysis_bridge::BaseSnapshot {
    crate::analysis_bridge::BaseSnapshot {
        graph,
        index_fingerprint: Some(0xaaaa),
        generation: crate::analysis_bridge::BaseGeneration::Previous,
        complexity: HashMap::new(),
    }
}

#[cfg(feature = "vuln")]
use super::{VulnFindingOut, VulnReport, severity_for};

#[cfg(feature = "vuln")]
fn sample_vuln_report() -> VulnReport {
    VulnReport {
        findings: vec![
            VulnFindingOut {
                kind: "missing_dominator_check".to_owned(),
                template: "missing_dominator_check".to_owned(),
                class: Some("BAC".to_owned()),
                origin: "frequency".to_owned(),
                file: "src/handlers/order.rs".to_owned(),
                line: 42,
                symbol: "delete_order".to_owned(),
                confidence: 0.91,
                severity: severity_for(0.91).to_owned(),
                message: "reaches `db_delete` without `check_auth` & <tag>".to_owned(),
            },
            VulnFindingOut {
                kind: "lossy_send".to_owned(),
                template: "must_follow".to_owned(),
                class: None,
                origin: "concurrency".to_owned(),
                file: "src/queue.rs".to_owned(),
                line: 7,
                symbol: "enqueue".to_owned(),
                confidence: 0.9,
                severity: severity_for(0.9).to_owned(),
                message: "best-effort send result discarded".to_owned(),
            },
        ],
        missing_guard_count: 1,
        taint_count: 0,
        concurrency_count: 1,
        scanned_functions: 12,
    }
}

mod diff;
mod graph;
mod graph_algorithms;
mod query;
#[cfg(feature = "vuln")]
mod vuln;
