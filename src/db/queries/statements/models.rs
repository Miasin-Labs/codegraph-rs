use crate::types::UnresolvedReference;

/// Result of [`QueryBuilder::get_dominant_file`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DominantFile {
    pub file_path: String,
    pub edge_count: u64,
    pub next_edge_count: u64,
}

/// Result of [`QueryBuilder::get_top_route_file`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopRouteFile {
    pub file_path: String,
    pub route_count: u64,
    pub total_routes: u64,
}

/// One URL → handler mapping from [`QueryBuilder::get_routing_manifest`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingManifestEntry {
    pub url: String,
    pub handler: String,
    pub handler_file: String,
    pub handler_line: u32,
    pub handler_kind: String,
}

/// Result of [`QueryBuilder::get_routing_manifest`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingManifest {
    pub entries: Vec<RoutingManifestEntry>,
    pub top_handler_file: Option<String>,
    pub top_handler_file_count: u64,
    pub total_routes: u64,
}

/// Lightweight (nodes, edges) count snapshot.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct NodeEdgeCount {
    pub nodes: u64,
    pub edges: u64,
}

/// A page of unresolved references plus the last stable row id seen.
#[derive(Debug, Clone)]
pub struct UnresolvedBatch {
    pub refs: Vec<UnresolvedReference>,
    pub last_id: i64,
}

/// Key identifying a resolved reference for precise deletion
/// (TS `{ fromNodeId, referenceName, referenceKind }`).
#[derive(Debug, Clone)]
pub struct ResolvedRefKey {
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: String,
}
