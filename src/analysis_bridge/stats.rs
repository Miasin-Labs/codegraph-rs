use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Counters describing what the bridge mapped, folded, and skipped.
///
/// `skipped_*` maps are keyed by kind/reason so callers (and tests) can see
/// exactly why a row did not make it into the analysis graph.
///
/// Serde derives exist so the stats round-trip through the on-disk snapshot
/// cache (`meta.json`) - a cache hit returns the same
/// [`crate::analysis_bridge::BridgeResult`] shape a fresh bridge would.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BridgeStats {
    /// Total node rows read from the database.
    pub nodes_total: usize,
    /// Nodes inserted into the analysis graph.
    pub nodes_mapped: usize,
    /// Nodes whose kind has no analysis equivalent (variable, import, ...).
    pub nodes_skipped: usize,
    /// Mapped nodes that collapsed onto an already-inserted analysis
    /// `NodeId` (e.g. overloads sharing file + qualified name + kind).
    /// They still resolve through [`crate::analysis_bridge::BridgeResult::id_map`].
    pub nodes_deduped: usize,
    /// Total edge rows read from the database.
    pub edges_total: usize,
    /// Edges inserted into the analysis graph.
    pub edges_mapped: usize,
    /// Edge rows that could not be represented (counted per reason in
    /// [`Self::skipped_edge_reasons`]).
    pub edges_skipped: usize,
    /// Edge rows folded into node metadata (`fields` / `variants` /
    /// `accessed_fields`) instead of becoming graph edges.
    pub edges_enriched: usize,
    /// Total unresolved-reference rows read from the database.
    pub unresolved_total: usize,
    /// Unresolved references that became `UnresolvedCall` edges.
    pub unresolved_mapped: usize,
    /// Unresolved references skipped (non-call kind, unmapped source, or
    /// duplicate of an already-emitted edge).
    pub unresolved_skipped: usize,
    /// Placeholder `Function` nodes created to anchor `UnresolvedCall` edges.
    pub placeholder_nodes: usize,
    /// Mapped nodes whose database row lacks byte offsets
    /// (`start_byte`/`end_byte` NULL). Their analysis spans carry the
    /// degraded `byte_range: 0..0`, so IR-backed analyses skip them.
    pub nodes_missing_byte_range: usize,
    /// Typed field entries registered onto `Struct` nodes via the engine's
    /// partial-struct contract. Always 0 unless
    /// [`crate::analysis_bridge::BridgeOptions::include_fields`] is on.
    #[serde(default)]
    pub struct_fields_registered: usize,
    /// `Function` nodes annotated with the engine's `accessed_fields`
    /// metadata. Always 0 unless
    /// [`crate::analysis_bridge::BridgeOptions::include_fields`] is on.
    #[serde(default)]
    pub accessed_fields_registered: usize,
    /// Field/accessed-field names skipped because they would corrupt the
    /// engine's metadata encoding (empty, or containing `;`/`:`/`,`).
    #[serde(default)]
    pub fields_skipped_invalid: usize,
    /// Skipped node counts keyed by codegraph node kind.
    pub skipped_node_kinds: BTreeMap<String, usize>,
    /// Skipped edge counts keyed by reason.
    pub skipped_edge_reasons: BTreeMap<String, usize>,
}
