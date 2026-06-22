mod edges;
mod nodes;
mod unresolved;

use std::collections::HashMap;

use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::nodes::{NodeId as ANodeId, NodeKind as ANodeKind};

use super::options::BridgeOptions;
use super::result::BridgeResult;
use super::rows::read_all_edges;
use super::stats::BridgeStats;
use crate::db::QueryBuilder;
use crate::error::{Result, log_debug};
use crate::types::Node;

type MappedNodes<'a> = HashMap<&'a str, (ANodeId, ANodeKind)>;
type NodeById<'a> = HashMap<&'a str, &'a Node>;
type Owners<'a> = HashMap<ANodeId, &'a str>;

/// Build a `codegraph-analysis` graph from an indexed codegraph database
/// with default [`BridgeOptions`] (no field carrying).
///
/// Pure read; the database is never mutated. See the module docs for the
/// kind mappings and the skip/fold rules; see [`BridgeStats`] for what was
/// counted along the way.
pub fn build_analysis_graph(queries: &QueryBuilder) -> Result<BridgeResult> {
    build_analysis_graph_with_options(queries, &BridgeOptions::default())
}

/// [`build_analysis_graph`] with explicit [`BridgeOptions`].
pub fn build_analysis_graph_with_options(
    queries: &QueryBuilder,
    options: &BridgeOptions,
) -> Result<BridgeResult> {
    let mut stats = BridgeStats::default();
    let mut graph = AnalysisGraph::new();

    let mut db_nodes = queries.get_all_nodes()?;
    db_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    stats.nodes_total = db_nodes.len();

    let node_by_id: NodeById<'_> = db_nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let (mapped, owner) = nodes::map_nodes(&db_nodes, &mut stats);

    let edge_rows = read_all_edges(queries)?;
    stats.edges_total = edge_rows.len();
    let mut enrichment = edges::Enrichment::default();
    let mut pending_edges = Vec::new();
    edges::EdgePass {
        rows: &edge_rows,
        node_by_id: &node_by_id,
        mapped: &mapped,
        options,
        enrichment: &mut enrichment,
        pending_edges: &mut pending_edges,
        stats: &mut stats,
    }
    .run();
    enrichment.finalize_engine_accessed(options, &mut stats);

    let mut id_map = HashMap::new();
    nodes::NodeInsert {
        graph: &mut graph,
        nodes: &db_nodes,
        mapped: &mapped,
        owner: &owner,
        enrichment: &enrichment,
        id_map: &mut id_map,
        stats: &mut stats,
    }
    .run()?;

    edges::insert_pending_edges(&mut graph, pending_edges, &mut stats);
    unresolved::UnresolvedPass {
        graph: &mut graph,
        queries,
        node_by_id: &node_by_id,
        mapped: &mapped,
        stats: &mut stats,
    }
    .run()?;

    log_debug(
        "analysis bridge: graph built",
        Some(&serde_json::json!({
            "nodesTotal": stats.nodes_total,
            "nodesMapped": stats.nodes_mapped,
            "nodesSkipped": stats.nodes_skipped,
            "nodesDeduped": stats.nodes_deduped,
            "edgesTotal": stats.edges_total,
            "edgesMapped": stats.edges_mapped,
            "edgesSkipped": stats.edges_skipped,
            "edgesEnriched": stats.edges_enriched,
            "unresolvedMapped": stats.unresolved_mapped,
            "placeholderNodes": stats.placeholder_nodes,
            "nodesMissingByteRange": stats.nodes_missing_byte_range,
            "includeFields": options.include_fields,
            "structFieldsRegistered": stats.struct_fields_registered,
            "accessedFieldsRegistered": stats.accessed_fields_registered,
            "fieldsSkippedInvalid": stats.fields_skipped_invalid,
            "skippedNodeKinds": stats.skipped_node_kinds,
            "skippedEdgeReasons": stats.skipped_edge_reasons,
        })),
    );

    Ok(BridgeResult {
        graph,
        id_map,
        stats,
    })
}
