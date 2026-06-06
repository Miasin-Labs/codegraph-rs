//! Bridge from the SQLite knowledge graph into `codegraph-analysis`.
//!
//! Reads an already-indexed codegraph database through [`QueryBuilder`] and
//! materializes a [`codegraph_analysis::graph::CodeGraph`] so every analysis
//! the engine offers (communities, dominators, slicing, taint, cascade, DSL
//! queries, token-budgeted context, ...) can run over a codegraph index
//! WITHOUT re-parsing any source.
//!
//! ## Node-kind mapping (codegraph 22 kinds → analysis 5 kinds)
//!
//! | codegraph                          | analysis   |
//! |------------------------------------|------------|
//! | `function`, `method`               | `Function` |
//! | `class`, `struct`                  | `Struct`   |
//! | `enum`                             | `Enum`     |
//! | `file`, `module`, `namespace`      | `Module`   |
//! | `trait`, `interface`, `protocol`   | `Trait`    |
//! | everything else                    | skipped    |
//!
//! Skipped kinds are not dropped on the floor wholesale — the information
//! that matters to analyses is folded into metadata on the surviving nodes
//! (the analysis crate's documented well-known metadata keys):
//!
//! - `field`/`property` children of a mapped `Struct`/`Enum`/`Trait`
//!   (via `contains` edges) populate the parent's `fields` JSON array.
//! - `enum_member` children of a mapped `Enum` populate `variants`.
//! - `references`/`type_of` edges from a mapped `Function` to a skipped
//!   `field`/`property` populate the function's `accessed_fields` array.
//!
//! ## Edge-kind mapping
//!
//! | codegraph                  | analysis                                              |
//! |----------------------------|-------------------------------------------------------|
//! | `calls`                    | `Calls` (Function→Function only; else skipped)        |
//! | `contains`                 | `Contains` (source Module/Struct/Enum/Trait; else skipped) |
//! | `implements`               | `Implements` (Struct/Enum→Trait; else skipped)        |
//! | `extends`                  | `Implements` when Struct/Enum→Trait, else `References`|
//! | `references`, `imports`, `exports`, `instantiates`, `type_of`, `returns`, `overrides`, `decorates` | `UsesType` when Function→Struct/Enum/Trait, else `References` |
//!
//! Unresolved codegraph references (the `unresolved_refs` table) whose
//! `reference_kind` is `calls` and whose source maps to a `Function` become
//! `UnresolvedCall(name)` edges pointing at a deterministic placeholder
//! `Function` node under the [`UNRESOLVED_FILE`] pseudo-path — the same
//! shape the analysis crate's own adapters and LSP-enrichment layer use.
//!
//! Every row that cannot be represented under the analysis graph's edge
//! invariants is **skipped, counted, and logged** ([`BridgeStats`]) — never
//! inserted in a shape the engine rejects, and never a panic.
//!
//! ## Determinism
//!
//! Analysis `NodeId`s are content-addressed (`file_path + qualified_name +
//! kind`), all inputs are read in a stable sort order, and metadata arrays
//! are sorted before serialization — so bridging the same index twice (or
//! after a re-index of unchanged sources) yields the identical fingerprint
//! ([`codegraph_analysis::fingerprint::Fingerprintable`]).
//!
//! ## On-disk snapshot cache
//!
//! Bridging re-reads every node/edge/unresolved-ref row, which on large
//! indexes dominates `codegraph analyze` wall-clock. [`build_analysis_graph_cached`]
//! persists the bridged graph under `<project>/.codegraph/analysis/`:
//!
//! - `graph.bin` — the analysis engine's own postcard snapshot
//!   ([`codegraph_analysis::overlay::save_snapshot_bincode`], versioned by
//!   `OVERLAY_SCHEMA_VERSION`).
//! - `meta.json` — host-side envelope: cache schema version, the host crate
//!   version, the **index fingerprint** the snapshot was built from, the
//!   codegraph-id → analysis-id map, and the [`BridgeStats`].
//!
//! The index fingerprint ([`compute_index_fingerprint`]) is a cheap BLAKE3
//! digest of the SQLite store's row counts, max rowids, `max(updated_at)`,
//! and every file's `(path, content_hash)` pair — any re-index that changes
//! the store changes the fingerprint and invalidates the snapshot. All cache
//! failures (missing, corrupt, schema/version/fingerprint mismatch) degrade
//! to a silent rebuild; the cache is never load-bearing for correctness.
//!
//! `CODEGRAPH_ANALYSIS_CACHE_DIR` (the analysis engine's post-rebrand cache
//! env var) overrides the location: the snapshot then lives under
//! `<override>/<workspace-key>/`, where the key is a stable 16-hex digest of
//! the project root so multiple projects can share one override directory.
//! The default location needs no such key — `.codegraph/` is per-project by
//! construction, and its `.gitignore` (`*` + `!.gitignore`) already keeps
//! the cache out of user repositories.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use codegraph_analysis::edges::{EdgeData as AEdgeData, EdgeKind as AEdgeKind};
use codegraph_analysis::fingerprint::FingerprintHasher;
use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::nodes::{
    NodeData as ANodeData,
    NodeId as ANodeId,
    NodeKind as ANodeKind,
    Span as ASpan,
    Visibility as AVisibility,
};
use codegraph_analysis::overlay::{load_snapshot_bincode, save_snapshot_bincode};
use codegraph_analysis::session::GraphSession;
use serde::{Deserialize, Serialize};

use crate::db::QueryBuilder;
use crate::directory::get_codegraph_dir;
use crate::error::{Result, log_debug};
use crate::types::{EdgeKind, Node, NodeKind, Visibility};

/// Pseudo file path used for placeholder nodes that anchor
/// `UnresolvedCall` edges. Deterministic by construction so rebuilds
/// produce identical placeholder `NodeId`s.
pub const UNRESOLVED_FILE: &str = "<unresolved>";

// =============================================================================
// Stats
// =============================================================================

/// Counters describing what the bridge mapped, folded, and skipped.
///
/// `skipped_*` maps are keyed by kind/reason so callers (and tests) can see
/// exactly why a row didn't make it into the analysis graph.
///
/// Serde derives exist so the stats round-trip through the on-disk snapshot
/// cache (`meta.json`) — a cache hit returns the same [`BridgeResult`] shape
/// a fresh bridge would.
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
    /// They still resolve through [`BridgeResult::id_map`].
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
    /// Skipped node counts keyed by codegraph node kind.
    pub skipped_node_kinds: BTreeMap<String, usize>,
    /// Skipped edge counts keyed by reason.
    pub skipped_edge_reasons: BTreeMap<String, usize>,
}

// =============================================================================
// Result
// =============================================================================

/// Output of [`build_analysis_graph`].
pub struct BridgeResult {
    /// The fully-populated analysis graph.
    pub graph: AnalysisGraph,
    /// codegraph node id (SQLite `nodes.id`) → analysis `NodeId` for every
    /// mapped node (deduped nodes map to their surviving twin).
    pub id_map: HashMap<String, ANodeId>,
    /// What was mapped / folded / skipped.
    pub stats: BridgeStats,
}

impl BridgeResult {
    /// Wrap the bridged graph in a [`GraphSession`] facade so the full
    /// analysis surface (DSL queries, context engine, explore, cascade,
    /// validation, ...) is available without re-parsing source.
    pub fn into_session(self, workspace_root: &Path) -> (GraphSession, BridgeStats) {
        let session = GraphSession::from_snapshot(self.graph, workspace_root);
        (session, self.stats)
    }
}

// =============================================================================
// Kind mapping
// =============================================================================

/// Map a codegraph node kind onto the analysis engine's 5-kind model.
/// Returns `None` for kinds that are not represented as analysis nodes.
pub fn map_node_kind(kind: NodeKind) -> Option<ANodeKind> {
    match kind {
        NodeKind::Function | NodeKind::Method => Some(ANodeKind::Function),
        NodeKind::Class | NodeKind::Struct => Some(ANodeKind::Struct),
        NodeKind::Enum => Some(ANodeKind::Enum),
        NodeKind::File | NodeKind::Module | NodeKind::Namespace => Some(ANodeKind::Module),
        NodeKind::Trait | NodeKind::Interface | NodeKind::Protocol => Some(ANodeKind::Trait),
        NodeKind::Property
        | NodeKind::Field
        | NodeKind::Variable
        | NodeKind::Constant
        | NodeKind::EnumMember
        | NodeKind::TypeAlias
        | NodeKind::Parameter
        | NodeKind::Import
        | NodeKind::Export
        | NodeKind::Route
        | NodeKind::Component => None,
    }
}

/// Map a codegraph edge kind onto an analysis edge kind, given the already
/// mapped source/target node kinds. Returns `None` when the combination
/// cannot be represented under the analysis graph's insertion invariants.
///
/// The result is always a combination [`AEdgeKind::valid_for`] accepts —
/// [`build_analysis_graph`] still double-checks the insertion result so a
/// future invariant change can never turn into a panic.
pub fn map_edge_kind(kind: EdgeKind, source: ANodeKind, target: ANodeKind) -> Option<AEdgeKind> {
    let fn_to_type = source == ANodeKind::Function
        && matches!(
            target,
            ANodeKind::Struct | ANodeKind::Enum | ANodeKind::Trait
        );
    match kind {
        EdgeKind::Calls => (source == ANodeKind::Function && target == ANodeKind::Function)
            .then_some(AEdgeKind::Calls),
        EdgeKind::Contains => matches!(
            source,
            ANodeKind::Module | ANodeKind::Struct | ANodeKind::Enum | ANodeKind::Trait
        )
        .then_some(AEdgeKind::Contains),
        EdgeKind::Implements => (matches!(source, ANodeKind::Struct | ANodeKind::Enum)
            && target == ANodeKind::Trait)
            .then_some(AEdgeKind::Implements),
        // `extends` between Struct/Enum and a Trait is trait/interface
        // implementation; anything else (class→class inheritance, interface
        // →interface extension) is preserved as a general reference.
        EdgeKind::Extends => {
            if matches!(source, ANodeKind::Struct | ANodeKind::Enum) && target == ANodeKind::Trait {
                Some(AEdgeKind::Implements)
            } else {
                Some(AEdgeKind::References)
            }
        }
        // The reference family becomes the analysis crate's typed
        // `UsesType` whenever its invariant (Function → Struct/Enum/Trait)
        // holds, and a relaxed `References` otherwise.
        EdgeKind::References
        | EdgeKind::Imports
        | EdgeKind::Exports
        | EdgeKind::Instantiates
        | EdgeKind::TypeOf
        | EdgeKind::Returns
        | EdgeKind::Overrides
        | EdgeKind::Decorates => Some(if fn_to_type {
            AEdgeKind::UsesType
        } else {
            AEdgeKind::References
        }),
    }
}

fn map_visibility(v: Option<Visibility>) -> AVisibility {
    match v {
        Some(Visibility::Public) | None => AVisibility::Public,
        Some(Visibility::Private) => AVisibility::Private,
        Some(Visibility::Protected) => AVisibility::Super,
        Some(Visibility::Internal) => AVisibility::Crate,
    }
}

fn node_span(node: &Node) -> ASpan {
    ASpan {
        file: PathBuf::from(&node.file_path),
        start_line: node.start_line,
        start_col: node.start_column,
        end_line: node.end_line,
        end_col: node.end_column,
        // Byte offsets are not stored in the codegraph schema; 0..0 is the
        // documented "unknown" value (the analysis crate only needs byte
        // ranges for source-backed lowering, which the bridge skips).
        byte_range: 0..0,
    }
}

// =============================================================================
// Internal row shapes
// =============================================================================

/// One raw row from the `edges` table. Read with raw SQL (sorted) because
/// `QueryBuilder` exposes per-node edge lookups, not a bulk scan.
struct EdgeRow {
    source: String,
    target: String,
    kind: String,
    line: Option<u32>,
    col: Option<u32>,
}

fn read_all_edges(queries: &QueryBuilder) -> Result<Vec<EdgeRow>> {
    let conn = queries.db().conn();
    let mut stmt = conn.prepare(
        "SELECT source, target, kind, line, col FROM edges \
         ORDER BY source, target, kind, COALESCE(line, -1), COALESCE(col, -1)",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(EdgeRow {
            source: row.get(0)?,
            target: row.get(1)?,
            kind: row.get(2)?,
            line: row.get::<_, Option<i64>>(3)?.map(|v| v.max(0) as u32),
            col: row.get::<_, Option<i64>>(4)?.map(|v| v.max(0) as u32),
        })
    })?;
    rows.map(|r| r.map_err(Into::into)).collect()
}

// =============================================================================
// Bridge
// =============================================================================

/// Build a `codegraph-analysis` graph from an indexed codegraph database.
///
/// Pure read — the database is never mutated. See the module docs for the
/// kind mappings and the skip/fold rules; see [`BridgeStats`] for what was
/// counted along the way.
pub fn build_analysis_graph(queries: &QueryBuilder) -> Result<BridgeResult> {
    let mut stats = BridgeStats::default();
    let mut graph = AnalysisGraph::new();

    // --- 1. Read all nodes in a stable order --------------------------------
    let mut nodes = queries.get_all_nodes()?;
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    stats.nodes_total = nodes.len();

    let node_by_id: HashMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // --- 2. Compute the codegraph-id → analysis-id mapping (no inserts yet) -
    // `owner` records which codegraph node "wins" each analysis NodeId so
    // collisions (overloads) dedupe deterministically: the smallest
    // codegraph id wins because `nodes` is sorted.
    let mut mapped: HashMap<&str, (ANodeId, ANodeKind)> = HashMap::new();
    let mut owner: HashMap<ANodeId, &str> = HashMap::new();
    for node in &nodes {
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

    // --- 3. Walk edges: collect insertable edges + metadata enrichment ------
    let edge_rows = read_all_edges(queries)?;
    stats.edges_total = edge_rows.len();

    // Enrichment keyed by analysis NodeId so duplicate codegraph nodes that
    // collapse onto one analysis node still merge their contributions.
    let mut fields: HashMap<ANodeId, BTreeSet<String>> = HashMap::new();
    let mut variants: HashMap<ANodeId, BTreeSet<String>> = HashMap::new();
    let mut accessed_fields: HashMap<ANodeId, BTreeSet<String>> = HashMap::new();

    let mut pending_edges: Vec<(ANodeId, ANodeId, AEdgeData)> = Vec::new();

    let skip_edge = |stats: &mut BridgeStats, reason: &str| {
        stats.edges_skipped += 1;
        *stats
            .skipped_edge_reasons
            .entry(reason.to_string())
            .or_default() += 1;
    };

    for row in &edge_rows {
        let Ok(kind) = row.kind.parse::<EdgeKind>() else {
            skip_edge(&mut stats, "unknown_edge_kind");
            continue;
        };
        let (Some(src_node), Some(tgt_node)) = (
            node_by_id.get(row.source.as_str()),
            node_by_id.get(row.target.as_str()),
        ) else {
            skip_edge(&mut stats, "dangling_endpoint");
            continue;
        };

        match (
            mapped.get(row.source.as_str()),
            mapped.get(row.target.as_str()),
        ) {
            (Some((src_aid, src_akind)), Some((tgt_aid, tgt_akind))) => {
                let Some(akind) = map_edge_kind(kind, *src_akind, *tgt_akind) else {
                    skip_edge(
                        &mut stats,
                        &format!(
                            "invariant_{}_{:?}_to_{:?}",
                            kind.as_str(),
                            src_akind,
                            tgt_akind
                        ),
                    );
                    continue;
                };
                let span = ASpan {
                    file: PathBuf::from(&src_node.file_path),
                    start_line: row.line.unwrap_or(src_node.start_line),
                    start_col: row.col.unwrap_or(0),
                    end_line: row.line.unwrap_or(src_node.start_line),
                    end_col: row.col.unwrap_or(0),
                    byte_range: 0..0,
                };
                pending_edges.push((
                    src_aid.clone(),
                    tgt_aid.clone(),
                    AEdgeData {
                        kind: akind,
                        source_span: span,
                        weight: 1.0,
                    },
                ));
            }
            // Source survives, target was a skipped kind: fold the
            // relationship into the analysis crate's well-known metadata
            // keys where it carries analysis value.
            (Some((src_aid, src_akind)), None) => match kind {
                EdgeKind::Contains
                    if matches!(tgt_node.kind, NodeKind::Field | NodeKind::Property)
                        && matches!(
                            src_akind,
                            ANodeKind::Struct | ANodeKind::Enum | ANodeKind::Trait
                        ) =>
                {
                    fields
                        .entry(src_aid.clone())
                        .or_default()
                        .insert(tgt_node.name.clone());
                    stats.edges_enriched += 1;
                }
                EdgeKind::Contains
                    if tgt_node.kind == NodeKind::EnumMember && *src_akind == ANodeKind::Enum =>
                {
                    variants
                        .entry(src_aid.clone())
                        .or_default()
                        .insert(tgt_node.name.clone());
                    stats.edges_enriched += 1;
                }
                EdgeKind::References | EdgeKind::TypeOf
                    if *src_akind == ANodeKind::Function
                        && matches!(tgt_node.kind, NodeKind::Field | NodeKind::Property) =>
                {
                    accessed_fields
                        .entry(src_aid.clone())
                        .or_default()
                        .insert(tgt_node.name.clone());
                    stats.edges_enriched += 1;
                }
                _ => skip_edge(&mut stats, "target_not_mapped"),
            },
            _ => skip_edge(&mut stats, "source_not_mapped"),
        }
    }

    // --- 4. Insert nodes (first owner wins; metadata fully assembled) -------
    let mut id_map: HashMap<String, ANodeId> = HashMap::new();
    for node in &nodes {
        let Some((aid, akind)) = mapped.get(node.id.as_str()) else {
            continue;
        };
        id_map.insert(node.id.clone(), aid.clone());
        if owner.get(aid) != Some(&node.id.as_str()) {
            stats.nodes_deduped += 1;
            continue;
        }

        let mut metadata: HashMap<String, String> = HashMap::new();
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
        // BTreeSet iteration is sorted → deterministic JSON arrays →
        // deterministic graph fingerprints.
        if let Some(set) = fields.get(aid) {
            let arr: Vec<&String> = set.iter().collect();
            metadata.insert("fields".to_string(), serde_json::to_string(&arr)?);
        }
        if let Some(set) = variants.get(aid) {
            let arr: Vec<&String> = set.iter().collect();
            metadata.insert("variants".to_string(), serde_json::to_string(&arr)?);
        }
        if let Some(set) = accessed_fields.get(aid) {
            let arr: Vec<&String> = set.iter().collect();
            metadata.insert("accessed_fields".to_string(), serde_json::to_string(&arr)?);
        }

        graph.add_node(ANodeData {
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
        stats.nodes_mapped += 1;
    }

    // --- 5. Insert edges -----------------------------------------------------
    // `map_edge_kind` already respects the invariants, but the insertion
    // result is still checked: a rejected edge is counted, never a panic.
    for (from, to, data) in pending_edges {
        match graph.add_edge(&from, &to, data) {
            Ok(()) => stats.edges_mapped += 1,
            Err(e) => {
                log_debug(
                    "analysis bridge: edge rejected by analysis-graph invariant",
                    Some(&serde_json::json!({ "error": e.to_string() })),
                );
                skip_edge(&mut stats, "rejected_at_insert");
            }
        }
    }

    // --- 6. Unresolved references → UnresolvedCall edges ---------------------
    let mut unresolved = queries.get_unresolved_references()?;
    unresolved.sort_by(|a, b| {
        (&a.from_node_id, &a.reference_name, a.line, a.column).cmp(&(
            &b.from_node_id,
            &b.reference_name,
            b.line,
            b.column,
        ))
    });
    stats.unresolved_total = unresolved.len();

    let mut emitted: HashSet<(ANodeId, String, u32, u32)> = HashSet::new();
    for r in &unresolved {
        let mappable = r.reference_kind == EdgeKind::Calls
            && matches!(
                mapped.get(r.from_node_id.as_str()),
                Some((_, ANodeKind::Function))
            );
        if !mappable {
            stats.unresolved_skipped += 1;
            continue;
        }
        let (src_aid, _) = &mapped[r.from_node_id.as_str()];
        if !emitted.insert((src_aid.clone(), r.reference_name.clone(), r.line, r.column)) {
            stats.unresolved_skipped += 1;
            continue;
        }

        // Deterministic placeholder target (content-addressed from constant
        // pseudo-path + the referenced name).
        let placeholder_id = ANodeId::new(UNRESOLVED_FILE, &r.reference_name, ANodeKind::Function);
        if graph.get_node(&placeholder_id).is_none() {
            let span = ASpan {
                file: PathBuf::from(UNRESOLVED_FILE),
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 0,
                byte_range: 0..0,
            };
            graph.add_node(ANodeData {
                id: placeholder_id.clone(),
                kind: ANodeKind::Function,
                name: r.reference_name.clone(),
                qualified_name: r.reference_name.clone(),
                file_path: PathBuf::from(UNRESOLVED_FILE),
                span,
                visibility: AVisibility::Public,
                metadata: HashMap::from([("placeholder".to_string(), "true".to_string())]),
                birth_revision: 0,
                last_modified_revision: 0,
                complexity: None,
                cfg: None,
                dataflow: None,
            });
            stats.placeholder_nodes += 1;
        }

        let src_file = node_by_id
            .get(r.from_node_id.as_str())
            .map(|n| n.file_path.clone())
            .or_else(|| r.file_path.clone())
            .unwrap_or_default();
        let span = ASpan {
            file: PathBuf::from(src_file),
            start_line: r.line,
            start_col: r.column,
            end_line: r.line,
            end_col: r.column,
            byte_range: 0..0,
        };
        match graph.add_edge(
            src_aid,
            &placeholder_id,
            AEdgeData {
                kind: AEdgeKind::UnresolvedCall(r.reference_name.clone()),
                source_span: span,
                // Same convention as the analysis crate's own adapters:
                // unresolved calls carry half the weight of resolved ones.
                weight: 0.5,
            },
        ) {
            Ok(()) => stats.unresolved_mapped += 1,
            Err(e) => {
                log_debug(
                    "analysis bridge: unresolved-call edge rejected",
                    Some(&serde_json::json!({ "error": e.to_string() })),
                );
                stats.unresolved_skipped += 1;
            }
        }
    }

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

// =============================================================================
// On-disk snapshot cache
// =============================================================================

/// Environment variable that relocates the analysis snapshot cache (the
/// analysis engine's post-rebrand cache-dir name). When set and non-empty,
/// the cache lives under `<override>/<workspace-key>/` instead of
/// `<project>/.codegraph/analysis/`.
pub const ANALYSIS_CACHE_DIR_ENV: &str = "CODEGRAPH_ANALYSIS_CACHE_DIR";

/// Subdirectory of `.codegraph/` holding the snapshot cache.
const ANALYSIS_CACHE_SUBDIR: &str = "analysis";

/// Engine-format graph snapshot (postcard, written via
/// [`codegraph_analysis::overlay::save_snapshot_bincode`]).
const GRAPH_SNAPSHOT_FILE: &str = "graph.bin";

/// Host-side cache envelope (fingerprint, id map, stats). Written last —
/// it is the cache's commit point: readers validate it before touching
/// [`GRAPH_SNAPSHOT_FILE`].
const CACHE_META_FILE: &str = "meta.json";

/// Version of the `meta.json` envelope. Bump on any incompatible change to
/// [`CacheMeta`]; mismatches degrade to a cache miss (rebuild + overwrite).
const SNAPSHOT_CACHE_SCHEMA_VERSION: u32 = 1;

/// Host-side cache envelope persisted next to the graph snapshot.
///
/// `host_version` pins the snapshot to the exact crate version that produced
/// it: the bridge's kind-mapping rules are host logic, so a binary upgrade
/// must never serve a snapshot built by older mapping rules.
#[derive(Debug, Serialize, Deserialize)]
struct CacheMeta {
    schema_version: u32,
    host_version: String,
    index_fingerprint: u64,
    /// Sorted by codegraph id for deterministic bytes on disk.
    id_map: Vec<(String, ANodeId)>,
    stats: BridgeStats,
}

/// Output of [`build_analysis_graph_cached`].
pub struct CachedBridge {
    /// The bridged graph — identical shape whether rebuilt or loaded.
    pub result: BridgeResult,
    /// True when the result was served from the on-disk snapshot.
    pub from_cache: bool,
}

/// Cheap, deterministic fingerprint of the SQLite store's indexed state.
///
/// Folds (with BLAKE3, via the analysis engine's [`FingerprintHasher`]):
/// node/edge/unresolved-ref counts, the edges/unresolved max rowids
/// (AUTOINCREMENT — monotonic, so delete+reinsert churn is visible even at
/// stable counts), `max(nodes.updated_at)`, and every indexed file's
/// `(path, content_hash)` pair in sorted order. O(#files) rows read — far
/// cheaper than the full node/edge scan the bridge itself performs, which
/// is the whole point: validating the cache must cost much less than a
/// rebuild.
pub fn compute_index_fingerprint(queries: &QueryBuilder) -> Result<u64> {
    let conn = queries.db().conn();
    let scalar = |sql: &str| -> Result<i64> {
        let v: i64 = conn.query_row(sql, [], |row| row.get(0))?;
        Ok(v)
    };

    let node_count = scalar("SELECT COUNT(*) FROM nodes")?;
    let nodes_max_updated = scalar("SELECT COALESCE(MAX(updated_at), 0) FROM nodes")?;
    let edge_count = scalar("SELECT COUNT(*) FROM edges")?;
    let edge_max_id = scalar("SELECT COALESCE(MAX(id), 0) FROM edges")?;
    let unresolved_count = scalar("SELECT COUNT(*) FROM unresolved_refs")?;
    let unresolved_max_id = scalar("SELECT COALESCE(MAX(id), 0) FROM unresolved_refs")?;

    let mut stmt = conn.prepare("SELECT path, content_hash FROM files ORDER BY path")?;
    let files: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;

    let mut hasher = FingerprintHasher::new();
    // Domain separation so this digest can never collide with the engine's
    // graph-state fingerprints.
    hasher.update(&"codegraph::analysis-cache::index-fingerprint::v1");
    hasher.update(&node_count);
    hasher.update(&nodes_max_updated);
    hasher.update(&edge_count);
    hasher.update(&edge_max_id);
    hasher.update(&unresolved_count);
    hasher.update(&unresolved_max_id);
    hasher.update(&files.len());
    for (path, content_hash) in &files {
        hasher.update(path);
        hasher.update(content_hash);
    }
    Ok(hasher.finish().as_u64())
}

/// Stable 16-hex digest of the project root, used to namespace per-project
/// snapshots inside a shared `CODEGRAPH_ANALYSIS_CACHE_DIR` override.
fn workspace_cache_key(project_root: &Path) -> String {
    let mut hasher = FingerprintHasher::new();
    hasher.update(&"codegraph::analysis-cache::workspace-key::v1");
    hasher.update(&project_root.to_string_lossy().as_ref());
    format!("{:016x}", hasher.finish().as_u64())
}

/// Where the analysis snapshot cache for `project_root` lives. Default:
/// `<project>/.codegraph/analysis/`; relocated (and per-project namespaced)
/// by [`ANALYSIS_CACHE_DIR_ENV`].
pub fn analysis_cache_dir(project_root: &Path) -> PathBuf {
    analysis_cache_dir_with_override(project_root, std::env::var_os(ANALYSIS_CACHE_DIR_ENV))
}

/// Env-free core of [`analysis_cache_dir`] so tests can exercise the
/// override path without process-global env mutation.
fn analysis_cache_dir_with_override(
    project_root: &Path,
    override_dir: Option<OsString>,
) -> PathBuf {
    if let Some(dir) = override_dir {
        if !dir.is_empty() {
            return PathBuf::from(dir).join(workspace_cache_key(project_root));
        }
    }
    get_codegraph_dir(project_root).join(ANALYSIS_CACHE_SUBDIR)
}

/// Try to serve a [`BridgeResult`] from the on-disk snapshot. Any failure —
/// missing files, decode errors, schema/host-version/fingerprint mismatch —
/// returns `None` (cache miss), never an error.
fn load_cache(cache_dir: &Path, expected_fingerprint: u64) -> Option<BridgeResult> {
    let meta_bytes = fs::read(cache_dir.join(CACHE_META_FILE)).ok()?;
    let meta: CacheMeta = serde_json::from_slice(&meta_bytes).ok()?;
    if meta.schema_version != SNAPSHOT_CACHE_SCHEMA_VERSION
        || meta.host_version != env!("CARGO_PKG_VERSION")
        || meta.index_fingerprint != expected_fingerprint
    {
        return None;
    }
    let loaded = load_snapshot_bincode(&cache_dir.join(GRAPH_SNAPSHOT_FILE)).ok()?;
    Some(BridgeResult {
        graph: loaded.graph,
        id_map: meta.id_map.into_iter().collect(),
        stats: meta.stats,
    })
}

/// Persist a freshly-bridged result. Both files are written atomically
/// (tmp + rename); the graph snapshot lands first and `meta.json` last, so
/// a reader can never validate a meta that points at a half-written graph.
fn store_cache(
    cache_dir: &Path,
    project_root: &Path,
    index_fingerprint: u64,
    result: &BridgeResult,
) -> Result<()> {
    fs::create_dir_all(cache_dir)?;

    let graph_target = cache_dir.join(GRAPH_SNAPSHOT_FILE);
    let graph_tmp = cache_dir.join(format!("{GRAPH_SNAPSHOT_FILE}.tmp"));
    save_snapshot_bincode(&graph_tmp, &result.graph, project_root)
        .map_err(|e| crate::error::CodeGraphError::other(e.to_string()))?;
    if let Err(err) = fs::rename(&graph_tmp, &graph_target) {
        let _ = fs::remove_file(&graph_tmp);
        return Err(err.into());
    }

    let mut id_map: Vec<(String, ANodeId)> = result
        .id_map
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    id_map.sort_by(|a, b| a.0.cmp(&b.0));
    let meta = CacheMeta {
        schema_version: SNAPSHOT_CACHE_SCHEMA_VERSION,
        host_version: env!("CARGO_PKG_VERSION").to_string(),
        index_fingerprint,
        id_map,
        stats: result.stats.clone(),
    };
    let meta_target = cache_dir.join(CACHE_META_FILE);
    let meta_tmp = cache_dir.join(format!("{CACHE_META_FILE}.tmp"));
    fs::write(&meta_tmp, serde_json::to_vec(&meta)?)?;
    if let Err(err) = fs::rename(&meta_tmp, &meta_target) {
        let _ = fs::remove_file(&meta_tmp);
        return Err(err.into());
    }
    Ok(())
}

/// [`build_analysis_graph`] with the on-disk snapshot cache in front.
///
/// Computes the index fingerprint (cheap), and when `use_cache` is true
/// serves a valid snapshot from [`analysis_cache_dir`] instead of re-reading
/// the whole store. On a miss — or when `use_cache` is false (`--no-cache`)
/// — the graph is rebuilt from SQL and the cache is refreshed best-effort
/// (a store failure is logged and ignored; the fresh result is returned
/// regardless).
pub fn build_analysis_graph_cached(
    queries: &QueryBuilder,
    project_root: &Path,
    use_cache: bool,
) -> Result<CachedBridge> {
    let fingerprint = compute_index_fingerprint(queries)?;
    let cache_dir = analysis_cache_dir(project_root);

    if use_cache {
        if let Some(result) = load_cache(&cache_dir, fingerprint) {
            log_debug(
                "analysis cache: snapshot hit",
                Some(&serde_json::json!({
                    "cacheDir": cache_dir.display().to_string(),
                    "indexFingerprint": format!("{fingerprint:016x}"),
                })),
            );
            return Ok(CachedBridge {
                result,
                from_cache: true,
            });
        }
    }

    let result = build_analysis_graph(queries)?;
    if let Err(err) = store_cache(&cache_dir, project_root, fingerprint, &result) {
        log_debug(
            "analysis cache: store failed (continuing without cache)",
            Some(&serde_json::json!({
                "cacheDir": cache_dir.display().to_string(),
                "error": err.to_string(),
            })),
        );
    }
    Ok(CachedBridge {
        result,
        from_cache: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_kind_mapping_covers_all_22_kinds() {
        use crate::types::NODE_KINDS;
        let mapped: Vec<NodeKind> = NODE_KINDS
            .iter()
            .copied()
            .filter(|k| map_node_kind(*k).is_some())
            .collect();
        // 11 kinds map, 11 are skipped.
        assert_eq!(mapped.len(), 11);
        assert_eq!(map_node_kind(NodeKind::Method), Some(ANodeKind::Function));
        assert_eq!(map_node_kind(NodeKind::Class), Some(ANodeKind::Struct));
        assert_eq!(map_node_kind(NodeKind::File), Some(ANodeKind::Module));
        assert_eq!(map_node_kind(NodeKind::Interface), Some(ANodeKind::Trait));
        assert_eq!(map_node_kind(NodeKind::Protocol), Some(ANodeKind::Trait));
        assert_eq!(map_node_kind(NodeKind::Variable), None);
        assert_eq!(map_node_kind(NodeKind::Route), None);
        assert_eq!(map_node_kind(NodeKind::Parameter), None);
    }

    #[test]
    fn edge_kind_mapping_respects_invariants() {
        use ANodeKind::*;
        // calls: only Function→Function.
        assert_eq!(
            map_edge_kind(EdgeKind::Calls, Function, Function),
            Some(AEdgeKind::Calls)
        );
        assert_eq!(map_edge_kind(EdgeKind::Calls, Module, Function), None);
        assert_eq!(map_edge_kind(EdgeKind::Calls, Function, Struct), None);
        // contains: source must be a container.
        assert_eq!(
            map_edge_kind(EdgeKind::Contains, Module, Function),
            Some(AEdgeKind::Contains)
        );
        assert_eq!(map_edge_kind(EdgeKind::Contains, Function, Function), None);
        // implements: Struct/Enum → Trait only.
        assert_eq!(
            map_edge_kind(EdgeKind::Implements, Struct, Trait),
            Some(AEdgeKind::Implements)
        );
        assert_eq!(map_edge_kind(EdgeKind::Implements, Struct, Struct), None);
        // extends: Implements when it lands on a Trait, References otherwise
        // (Struct→Struct would violate the Implements invariant).
        assert_eq!(
            map_edge_kind(EdgeKind::Extends, Struct, Trait),
            Some(AEdgeKind::Implements)
        );
        assert_eq!(
            map_edge_kind(EdgeKind::Extends, Struct, Struct),
            Some(AEdgeKind::References)
        );
        assert_eq!(
            map_edge_kind(EdgeKind::Extends, Trait, Trait),
            Some(AEdgeKind::References)
        );
        // reference family: UsesType only when Function → type.
        assert_eq!(
            map_edge_kind(EdgeKind::Instantiates, Function, Struct),
            Some(AEdgeKind::UsesType)
        );
        assert_eq!(
            map_edge_kind(EdgeKind::Returns, Function, Enum),
            Some(AEdgeKind::UsesType)
        );
        assert_eq!(
            map_edge_kind(EdgeKind::References, Function, Trait),
            Some(AEdgeKind::UsesType)
        );
        assert_eq!(
            map_edge_kind(EdgeKind::References, Module, Module),
            Some(AEdgeKind::References)
        );
        assert_eq!(
            map_edge_kind(EdgeKind::Imports, Module, Module),
            Some(AEdgeKind::References)
        );
        assert_eq!(
            map_edge_kind(EdgeKind::Overrides, Function, Function),
            Some(AEdgeKind::References)
        );
    }

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

    #[test]
    fn workspace_cache_key_is_stable_and_distinct() {
        let a = workspace_cache_key(Path::new("/projects/a"));
        assert_eq!(a, workspace_cache_key(Path::new("/projects/a")));
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, workspace_cache_key(Path::new("/projects/b")));
    }

    #[test]
    fn cache_dir_defaults_to_codegraph_analysis_and_honors_override() {
        let root = Path::new("/projects/demo");
        assert_eq!(
            analysis_cache_dir_with_override(root, None),
            root.join(".codegraph").join("analysis")
        );
        // Empty override is ignored.
        assert_eq!(
            analysis_cache_dir_with_override(root, Some(OsString::new())),
            root.join(".codegraph").join("analysis")
        );
        // Non-empty override namespaces per project.
        let dir = analysis_cache_dir_with_override(root, Some(OsString::from("/tmp/shared")));
        assert_eq!(
            dir,
            Path::new("/tmp/shared").join(workspace_cache_key(root))
        );
    }

    #[test]
    fn snapshot_cache_round_trips_graph_id_map_and_stats() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        let result = sample_bridge_result();

        store_cache(&cache_dir, tmp.path(), 0xfeed, &result).expect("store");
        assert!(cache_dir.join(GRAPH_SNAPSHOT_FILE).exists());
        assert!(cache_dir.join(CACHE_META_FILE).exists());

        let loaded = load_cache(&cache_dir, 0xfeed).expect("fingerprint matches");
        assert_eq!(loaded.graph.node_count(), 1);
        assert_eq!(loaded.id_map.len(), 1);
        assert_eq!(loaded.stats.nodes_mapped, 1);
        let aid = loaded.id_map.get("cg-node-1").expect("id map entry");
        assert!(loaded.graph.get_node(aid).is_some());
    }

    #[test]
    fn snapshot_cache_misses_on_fingerprint_mismatch_or_corruption() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        let result = sample_bridge_result();
        store_cache(&cache_dir, tmp.path(), 0xfeed, &result).expect("store");

        // Different index fingerprint → miss.
        assert!(load_cache(&cache_dir, 0xdead).is_none());

        // Corrupt graph snapshot → miss, not a panic.
        fs::write(
            cache_dir.join(GRAPH_SNAPSHOT_FILE),
            b"not a postcard snapshot",
        )
        .unwrap();
        assert!(load_cache(&cache_dir, 0xfeed).is_none());

        // Corrupt meta → miss.
        store_cache(&cache_dir, tmp.path(), 0xfeed, &result).expect("re-store");
        fs::write(cache_dir.join(CACHE_META_FILE), b"{ not json").unwrap();
        assert!(load_cache(&cache_dir, 0xfeed).is_none());

        // Missing dir → miss.
        assert!(load_cache(&tmp.path().join("absent"), 0xfeed).is_none());
    }

    #[test]
    fn snapshot_cache_rejects_other_schema_or_host_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        let result = sample_bridge_result();
        store_cache(&cache_dir, tmp.path(), 0xfeed, &result).expect("store");

        let meta_path = cache_dir.join(CACHE_META_FILE);
        let mut meta: serde_json::Value =
            serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();

        meta["schema_version"] = serde_json::json!(SNAPSHOT_CACHE_SCHEMA_VERSION + 1);
        fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
        assert!(load_cache(&cache_dir, 0xfeed).is_none(), "future schema");

        meta["schema_version"] = serde_json::json!(SNAPSHOT_CACHE_SCHEMA_VERSION);
        meta["host_version"] = serde_json::json!("0.0.0-other");
        fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
        assert!(
            load_cache(&cache_dir, 0xfeed).is_none(),
            "other host version"
        );
    }

    /// Every combination `map_edge_kind` produces must pass the analysis
    /// crate's own `valid_for` invariant — exhaustively checked.
    #[test]
    fn mapped_edges_always_satisfy_analysis_invariants() {
        use crate::types::EDGE_KINDS;
        let akinds = [
            ANodeKind::Function,
            ANodeKind::Struct,
            ANodeKind::Enum,
            ANodeKind::Module,
            ANodeKind::Trait,
        ];
        for kind in EDGE_KINDS {
            for s in akinds {
                for t in akinds {
                    if let Some(mapped) = map_edge_kind(kind, s, t) {
                        assert!(
                            mapped.valid_for(s, t),
                            "map_edge_kind({kind:?}, {s:?}, {t:?}) = {mapped:?} violates valid_for"
                        );
                    }
                }
            }
        }
    }
}
