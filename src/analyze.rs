//! Analysis-engine runners over the bridged graph.
//!
//! Each function here drives one public capability of the
//! `codegraph-analysis` crate against a graph produced by
//! [`crate::analysis_bridge::build_analysis_graph`] and returns a
//! serde-serializable report with a stable camelCase JSON shape. The CLI
//! (`codegraph analyze …`) renders these; library users can call them
//! directly.
//!
//! ## Honesty contract (what runs at which precision)
//!
//! The SQLite bridge carries symbols, spans (line/column, no byte ranges)
//! and structural edges — it does NOT carry the analysis crate's per-function
//! IR/CFG/dataflow (upstream those are produced by the Rust language adapter
//! only; see `analysis/ADAPTER_PARITY.md`). Consequences:
//!
//! - **complexity** re-parses the on-disk sources with the host tree-sitter
//!   grammars and runs the analysis crate's metrics on the located function
//!   bodies. Languages without complexity rules are counted as skipped.
//! - **slice** and **taint** run the analysis crate's slicing/path machinery
//!   over a *call-graph* oracle ([`CallGraphOracle`]) — function-level hops
//!   along call edges, not value-level def-use chains. Every report says so
//!   in its `note` field instead of pretending otherwise. With
//!   `--value-level` (schema v5 indexes that store byte offsets), the
//!   oracle upgrades to the engine's interprocedural points-to analysis
//!   over per-function dataflow IR and the reports' `granularity` flips to
//!   `"value-level"` — see [`crate::analyze_ir`]. Pre-v5 indexes degrade
//!   back to call-graph with an explicit re-index note.
//! - **cfg** and **dataflow** ([`crate::analyze_ir`]) use the same source
//!   re-parse anchor pattern as **complexity**; languages without engine
//!   rule tables get honest capability notes.
//! - **communities**, **dominators**, **cycles**, **impact**, **centrality**,
//!   **critical**, **export**, and **stats** are pure graph algorithms and run
//!   at full fidelity over the bridged graph.
//! - **co-change** mines `git log --name-only` at *file* granularity — every
//!   symbol in a touched file counts as changed; same-file pairs are
//!   tautological and summarized instead of listed.
//! - **coverage** is line-granular from LCOV `DA` records summed over each
//!   function's span; annotating also un-blinds the DSL `untested` operator.
//! - **validate** judges arity edits at call-graph level: the bridge carries
//!   no per-call-site argument counts, so an arity change marks every direct
//!   caller as needing review.
//! - **generics** and **boundaries** read the engine's metadata contract
//!   (`generic_params`/`callee_type_args`, `http_route`/`ffi_export`/
//!   `wasm_export`); the SQLite bridge does not populate those keys yet, so
//!   both report that honestly instead of silently returning nothing.
//!
//! ## JSON envelope
//!
//! Every `analyze … --json` payload is wrapped in [`ReportEnvelope`] —
//! `{"schemaVersion": N, "kind": "<report-kind>", "data": …}` — mirroring the
//! engine's `codegraph_analysis::schema::Envelope` wire shape (camelCase per
//! the CLI convention). The engine's own `Envelope` cannot be reused directly
//! because its `kind` is the closed four-variant `PayloadKind` enum; see
//! `notes/close-tier1-needs.md`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use codegraph_analysis::capabilities::{Capability, CapabilityTree};
use codegraph_analysis::cascade::generate_cascade;
use codegraph_analysis::co_change::{CommitInfo, co_changes_for_nodes, compute_co_changes};
use codegraph_analysis::communities::louvain;
use codegraph_analysis::complexity::compute_complexity;
use codegraph_analysis::complexity_rules::LangRules;
use codegraph_analysis::coverage::{annotate_graph_from_lcov, parse_lcov};
use codegraph_analysis::dsl::aggregate::{AggExpr, parse_aggregate};
use codegraph_analysis::dsl::plan::{ScheduleStrategy, optimise_expr, pick_schedule_for_pipe};
use codegraph_analysis::dsl::provenance::trace_query;
use codegraph_analysis::dsl::{
    DslOp,
    Expr,
    QueryConfig as DslQueryConfig,
    QueryError as DslQueryError,
    parse_expr,
    run_query_expr,
};
use codegraph_analysis::edges::EdgeKind as AEdgeKind;
use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::hll::approximate_reachability;
use codegraph_analysis::monomorphize::find_instantiations;
use codegraph_analysis::nodes::{NodeData as ANodeData, NodeId as ANodeId, NodeKind as ANodeKind};
use codegraph_analysis::pass::{GraphFlag, PassManager};
use codegraph_analysis::polyglot::{
    BoundaryKind,
    detect_ffi_exports,
    detect_http_routes,
    detect_wasm_exports,
    resolve_cross_language_calls,
};
use codegraph_analysis::possible_types::PossibleTypesPass;
use codegraph_analysis::predicates::{Predicate, extract_predicates};
use codegraph_analysis::schema::{PayloadKind, json_schema_for};
use codegraph_analysis::slicing::{DataflowOracle, backward_slice, forward_slice};
use codegraph_analysis::taint_naming::{classify_name, flow_priority};
use codegraph_analysis::traversal::{TraversalConfig, TraversalDirection, traverse};
use codegraph_analysis::validation::VirtualValidator;
use codegraph_analysis::{analysis, analysis_tools};
use serde::Serialize;
use tree_sitter::{Node as TsNode, Point, Tree};

use crate::analysis_bridge::{BaseSnapshot, StoredComplexity, UNRESOLVED_FILE};
use crate::extraction::{create_parser, detect_language};
use crate::types::Language;

/// Deterministic seed for Louvain community detection — same index, same
/// communities, every run.
const LOUVAIN_SEED: u64 = 42;

/// Louvain resolution parameter (1.0 = classic modularity).
const LOUVAIN_RESOLUTION: f64 = 1.0;

/// Depth bound for the reachability walk feeding dominator analysis.
const DOMINATOR_TRAVERSAL_DEPTH: usize = 64;

// =============================================================================
// Shared shapes
// =============================================================================

/// A symbol reference rendered into every report.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolRef {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
}

fn kind_label(kind: ANodeKind) -> String {
    match kind {
        ANodeKind::Function => "function".to_string(),
        ANodeKind::Struct => "struct".to_string(),
        ANodeKind::Enum => "enum".to_string(),
        ANodeKind::Module => "module".to_string(),
        ANodeKind::Trait => "trait".to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
}

pub(crate) fn symbol_ref(node: &ANodeData) -> SymbolRef {
    SymbolRef {
        name: node.name.clone(),
        qualified_name: node.qualified_name.clone(),
        kind: kind_label(node.kind),
        file: node.file_path.display().to_string(),
        line: node.span.start_line,
    }
}

pub(crate) fn is_placeholder(node: &ANodeData) -> bool {
    node.file_path.as_os_str() == UNRESOLVED_FILE
}

fn edge_kind_label(kind: &AEdgeKind) -> &'static str {
    match kind {
        AEdgeKind::Calls => "calls",
        AEdgeKind::UnresolvedCall(_) => "unresolvedCall",
        AEdgeKind::UsesType => "usesType",
        AEdgeKind::References => "references",
        AEdgeKind::Contains => "contains",
        AEdgeKind::Implements => "implements",
        AEdgeKind::ExternalCall(..) => "externalCall",
        AEdgeKind::Extends => "extends",
        AEdgeKind::Returns => "returns",
        AEdgeKind::TypeOf => "typeOf",
    }
}

// =============================================================================
// Call-graph dataflow oracle
// =============================================================================

/// Coarse interprocedural [`DataflowOracle`] derived from call edges — the
/// "coarse pass derived from `Calls` edges" the slicing module documents as
/// the drop-in until per-function IR is available.
///
/// Direction matches [`codegraph_analysis::slicing::PointsToOracle`]:
/// `def_uses` walks callers (values flow *into* a function from its
/// callers), `use_defs` walks callees. Unresolved calls are included so
/// slices surface where the static graph runs out.
pub struct CallGraphOracle {
    callers: HashMap<ANodeId, Vec<ANodeId>>,
    callees: HashMap<ANodeId, Vec<ANodeId>>,
}

impl CallGraphOracle {
    /// Cache caller/callee adjacency over `Calls` and `UnresolvedCall` edges.
    pub fn build(graph: &AnalysisGraph) -> Self {
        let mut callers: HashMap<ANodeId, Vec<ANodeId>> = HashMap::new();
        let mut callees: HashMap<ANodeId, Vec<ANodeId>> = HashMap::new();
        for id in graph.all_node_ids() {
            for (target, edge) in graph.get_edges_from(id) {
                if matches!(edge.kind, AEdgeKind::Calls | AEdgeKind::UnresolvedCall(_)) {
                    callees.entry(id.clone()).or_default().push(target.clone());
                    callers.entry(target.clone()).or_default().push(id.clone());
                }
            }
        }
        for list in callers.values_mut().chain(callees.values_mut()) {
            list.sort();
            list.dedup();
        }
        Self { callers, callees }
    }
}

impl DataflowOracle for CallGraphOracle {
    fn def_uses(&self, node: &ANodeId) -> Vec<ANodeId> {
        self.callers.get(node).cloned().unwrap_or_default()
    }

    fn use_defs(&self, node: &ANodeId) -> Vec<ANodeId> {
        self.callees.get(node).cloned().unwrap_or_default()
    }
}

// =============================================================================
// analyze complexity
// =============================================================================

/// Per-function complexity metrics for one analyzed function.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionComplexity {
    pub symbol: SymbolRef,
    pub language: String,
    pub cyclomatic: u32,
    pub cognitive: u32,
    pub max_nesting: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loc_total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loc_source: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maintainability_index: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halstead_volume: Option<f64>,
}

/// Result of [`complexity_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplexityReport {
    /// All `Function` nodes in the bridged graph (placeholders included in
    /// `skipped`, not here).
    pub functions_total: usize,
    /// Functions whose source was parsed and measured.
    pub functions_analyzed: usize,
    /// Skip counts keyed by reason (`placeholder`, `unsupportedLanguage`,
    /// `fileUnreadable`, `bodyNotLocated`, `noMetrics`).
    pub skipped: BTreeMap<String, usize>,
    /// Most complex functions, cyclomatic desc / cognitive desc.
    pub functions: Vec<FunctionComplexity>,
}

/// Map a detected host language onto the analysis crate's complexity-rule id.
fn complexity_lang_id(language: Language) -> Option<&'static str> {
    Some(match language {
        Language::Rust => "rust",
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx => "typescript",
        Language::Python => "python",
        Language::Go => "go",
        Language::Java => "java",
        Language::C => "c",
        Language::Cpp => "cpp",
        Language::Php => "php",
        Language::Kotlin => "kotlin",
        Language::Swift => "swift",
        Language::Csharp => "csharp",
        Language::Ruby => "ruby",
        _ => return None,
    })
}

/// Walk up from the node at the function's recorded start position to the
/// nearest ancestor with a function body per the language rules. The bridge
/// carries line/column spans (no byte ranges), so location is point-based.
fn locate_function_node<'t>(
    root: TsNode<'t>,
    start_line: u32,
    start_col: u32,
    rules: &LangRules,
) -> Option<TsNode<'t>> {
    let point = Point {
        row: start_line.saturating_sub(1) as usize,
        column: start_col as usize,
    };
    let mut node = root.named_descendant_for_point_range(point, point)?;
    loop {
        if rules
            .body_field_names
            .iter()
            .any(|f| node.child_by_field_name(f).is_some())
        {
            return Some(node);
        }
        node = node.parent()?;
    }
}

/// Compute the analysis crate's complexity metrics for every function in the
/// bridged graph by re-parsing the on-disk sources under `workspace_root`,
/// keeping the `top` most complex.
pub fn complexity_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    top: usize,
) -> ComplexityReport {
    struct ParsedFile {
        tree: Tree,
        source: String,
        lang_id: &'static str,
        language: Language,
    }

    fn parse_file(workspace_root: &Path, rel_path: &Path) -> Option<ParsedFile> {
        let language = detect_language(&rel_path.to_string_lossy(), None);
        let lang_id = complexity_lang_id(language)?;
        let source = std::fs::read_to_string(workspace_root.join(rel_path)).ok()?;
        let mut parser = create_parser(language)?;
        let tree = parser.parse(&source, None)?;
        Some(ParsedFile {
            tree,
            source,
            lang_id,
            language,
        })
    }

    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    let mut skip = |reason: &str| {
        *skipped.entry(reason.to_string()).or_default() += 1;
    };

    let mut functions = graph.nodes_by_kind(ANodeKind::Function);
    functions.sort_by(|a, b| {
        (&a.file_path, a.span.start_line, &a.qualified_name).cmp(&(
            &b.file_path,
            b.span.start_line,
            &b.qualified_name,
        ))
    });

    let mut cache: HashMap<String, Option<ParsedFile>> = HashMap::new();
    let mut measured: Vec<FunctionComplexity> = Vec::new();
    let mut functions_total = 0usize;

    for node in functions {
        if is_placeholder(node) {
            skip("placeholder");
            continue;
        }
        functions_total += 1;

        let key = node.file_path.display().to_string();
        let parsed = cache
            .entry(key)
            .or_insert_with(|| parse_file(workspace_root, &node.file_path));
        let Some(parsed) = parsed.as_ref() else {
            // Either no complexity rules for the language or the file is
            // gone/unreadable — distinguish the two for the report.
            if complexity_lang_id(detect_language(&node.file_path.to_string_lossy(), None))
                .is_none()
            {
                skip("unsupportedLanguage");
            } else {
                skip("fileUnreadable");
            }
            continue;
        };

        let Some(rules) = LangRules::for_language(parsed.lang_id) else {
            skip("unsupportedLanguage");
            continue;
        };
        let Some(fn_node) = locate_function_node(
            parsed.tree.root_node(),
            node.span.start_line,
            node.span.start_col,
            rules,
        ) else {
            skip("bodyNotLocated");
            continue;
        };
        let Some(metrics) = compute_complexity(fn_node, parsed.source.as_bytes(), parsed.lang_id)
        else {
            skip("noMetrics");
            continue;
        };

        measured.push(FunctionComplexity {
            symbol: symbol_ref(node),
            language: parsed.language.as_str().to_string(),
            cyclomatic: metrics.cyclomatic,
            cognitive: metrics.cognitive,
            max_nesting: metrics.max_nesting,
            loc_total: metrics.loc.as_ref().map(|l| l.total),
            loc_source: metrics.loc.as_ref().map(|l| l.source),
            maintainability_index: metrics.maintainability_index,
            halstead_volume: metrics.halstead.as_ref().map(|h| h.volume),
        });
    }

    let functions_analyzed = measured.len();
    measured.sort_by(|a, b| {
        b.cyclomatic
            .cmp(&a.cyclomatic)
            .then_with(|| b.cognitive.cmp(&a.cognitive))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    measured.truncate(top);

    ComplexityReport {
        functions_total,
        functions_analyzed,
        skipped,
        functions: measured,
    }
}

// =============================================================================
// analyze communities
// =============================================================================

/// One detected community (size ≥ 2).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunitySummary {
    /// Louvain community label.
    pub id: u32,
    pub size: usize,
    /// Up to 3 files with the most members.
    pub top_files: Vec<String>,
    /// Up to `sample` members, name-sorted.
    pub members: Vec<SymbolRef>,
    /// True when `members` was capped below `size`.
    pub truncated: bool,
}

/// Result of [`communities_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunitiesReport {
    /// Total Louvain communities (singletons included).
    pub community_count: u32,
    /// Final modularity score in [-0.5, 1.0].
    pub modularity: f64,
    /// Communities with ≥ 2 members (the interesting ones — Louvain runs on
    /// call edges, so symbols without call relationships stay singletons).
    pub multi_member_count: usize,
    pub singleton_count: usize,
    pub communities: Vec<CommunitySummary>,
}

/// Louvain community detection over the call graph (deterministic seed).
pub fn communities_report(graph: &AnalysisGraph, sample: usize) -> CommunitiesReport {
    let result = louvain(graph, LOUVAIN_RESOLUTION, LOUVAIN_SEED);

    let mut groups: BTreeMap<u32, Vec<&ANodeData>> = BTreeMap::new();
    for (id, label) in &result.assignments {
        if let Some(node) = graph.get_node(id) {
            groups.entry(*label).or_default().push(node);
        }
    }

    let singleton_count = groups.values().filter(|m| m.len() < 2).count();
    let mut communities: Vec<CommunitySummary> = groups
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .map(|(label, members)| {
            let mut file_counts: BTreeMap<String, usize> = BTreeMap::new();
            for m in &members {
                *file_counts
                    .entry(m.file_path.display().to_string())
                    .or_default() += 1;
            }
            let mut files: Vec<(String, usize)> = file_counts.into_iter().collect();
            files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let top_files: Vec<String> = files.into_iter().take(3).map(|(f, _)| f).collect();

            let size = members.len();
            let mut refs: Vec<SymbolRef> = members.iter().map(|m| symbol_ref(m)).collect();
            refs.sort();
            let truncated = refs.len() > sample;
            refs.truncate(sample);

            CommunitySummary {
                id: label,
                size,
                top_files,
                members: refs,
                truncated,
            }
        })
        .collect();
    communities.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.id.cmp(&b.id)));

    CommunitiesReport {
        community_count: result.community_count,
        modularity: result.modularity,
        multi_member_count: communities.len(),
        singleton_count,
        communities,
    }
}

// =============================================================================
// analyze dominators
// =============================================================================

/// Immediate-dominator record for one node reachable from the entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DominatorEntry {
    pub symbol: SymbolRef,
    /// The node every path from the entry must pass through last before
    /// reaching this one. `None` only for the entry itself (not listed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub immediate_dominator: Option<SymbolRef>,
    /// Length of the dominator chain back to the entry.
    pub dominator_depth: usize,
}

/// Result of [`dominators_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DominatorsReport {
    pub entry: SymbolRef,
    /// Nodes analyzed (reachable from the entry, capped by `--top`).
    pub analyzed: usize,
    /// True when the reachability walk hit the cap.
    pub truncated: bool,
    pub nodes: Vec<DominatorEntry>,
}

/// Dominator analysis rooted at `entry`: for each node reachable from the
/// entry (BFS order, capped at `limit`), report its immediate dominator and
/// chain depth. Returns `None` if `entry` is not in the graph.
///
/// Computes the dominator tree once (`analysis::dominator_tree`) and reads
/// every chain off it, instead of the previous per-node
/// `dominator_chain` recomputation.
pub fn dominators_report(
    graph: &AnalysisGraph,
    entry: &ANodeId,
    limit: usize,
) -> Option<DominatorsReport> {
    let entry_node = graph.get_node(entry)?;
    let tree = analysis::dominator_tree(graph, entry)?;

    let config = TraversalConfig {
        max_depth: DOMINATOR_TRAVERSAL_DEPTH,
        max_nodes: limit.saturating_add(1),
        direction: TraversalDirection::Outgoing,
        parallel: false,
    };
    let walk = traverse(graph, entry, &config);

    let mut nodes: Vec<DominatorEntry> = Vec::new();
    for id in walk.nodes.iter().filter(|id| *id != entry) {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        let chain = tree.chain_report(id);
        if chain.dominators.is_empty() {
            // Unreachable under dominance (shouldn't happen for BFS-reached
            // nodes, but never report a node without a chain to the entry).
            continue;
        }
        let immediate_dominator = chain
            .dominators
            .first()
            .and_then(|d| graph.get_node(d))
            .map(symbol_ref);
        nodes.push(DominatorEntry {
            symbol: symbol_ref(node),
            immediate_dominator,
            dominator_depth: chain.dominators.len(),
        });
    }

    Some(DominatorsReport {
        entry: symbol_ref(entry_node),
        analyzed: nodes.len(),
        truncated: walk.was_truncated,
        nodes,
    })
}

// =============================================================================
// analyze slice
// =============================================================================

/// Direction of a program slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceDirection {
    /// What the seed's values can influence (walks callees).
    Forward,
    /// What can affect the values reaching the seed (walks callers).
    Backward,
}

impl SliceDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            SliceDirection::Forward => "forward",
            SliceDirection::Backward => "backward",
        }
    }
}

/// Result of [`slice_report`] (and of
/// [`crate::analyze_ir::value_slice_report`], which upgrades the oracle).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceReport {
    pub seed: SymbolRef,
    pub direction: String,
    pub max_depth: usize,
    /// `"call-graph"` from [`slice_report`]; `"value-level"` when
    /// `analyze slice --value-level` runs over dataflow IR — see `note`.
    pub granularity: String,
    /// Slice size excluding the seed.
    pub size: usize,
    pub nodes: Vec<SymbolRef>,
    /// IR-lowering coverage, present on value-level runs only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ir_coverage: Option<crate::analyze_ir::IrCoverage>,
    pub note: String,
}

/// Capability note shared by slice and taint reports.
fn call_graph_granularity_note(what: &str) -> String {
    format!(
        "{what} computed at call-graph granularity (hops follow resolved and \
         unresolved call edges). Re-run with --value-level for def-use \
         precision: it lowers per-function dataflow IR by re-parsing the \
         working tree (needs byte offsets in the index — re-index pre-v5 \
         projects to enable)."
    )
}

/// Forward/backward program slice from `seed` using the analysis crate's
/// slicing algorithms over the [`CallGraphOracle`]. Returns `None` if `seed`
/// is not in the graph.
pub fn slice_report(
    graph: &AnalysisGraph,
    seed: &ANodeId,
    direction: SliceDirection,
    max_depth: usize,
) -> Option<SliceReport> {
    let seed_node = graph.get_node(seed)?;
    let oracle = CallGraphOracle::build(graph);
    let set = match direction {
        SliceDirection::Forward => forward_slice(graph, &oracle, seed, max_depth),
        SliceDirection::Backward => backward_slice(graph, &oracle, seed, max_depth),
    };

    let mut nodes: Vec<SymbolRef> = set
        .iter()
        .filter(|id| *id != seed)
        .filter_map(|id| graph.get_node(id))
        .map(symbol_ref)
        .collect();
    nodes.sort_by(|a, b| (&a.file, a.line, &a.name).cmp(&(&b.file, b.line, &b.name)));

    Some(SliceReport {
        seed: symbol_ref(seed_node),
        direction: direction.as_str().to_string(),
        max_depth,
        granularity: "call-graph".to_string(),
        size: nodes.len(),
        nodes,
        ir_coverage: None,
        note: call_graph_granularity_note("Slice"),
    })
}

// =============================================================================
// analyze cycles
// =============================================================================

/// One strongly-connected component with ≥ 2 members, or a self-recursive
/// node.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleSummary {
    pub size: usize,
    /// `mutualRecursion`, `selfRecursion`, `moduleCycle`, or `mixed`.
    pub kind: String,
    pub members: Vec<SymbolRef>,
}

/// A suggested edge removal that helps break a cycle.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleBreakSuggestion {
    pub from: SymbolRef,
    pub to: SymbolRef,
}

/// Result of [`cycles_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CyclesReport {
    pub cycle_count: usize,
    pub cycles: Vec<CycleSummary>,
    pub break_suggestions: Vec<CycleBreakSuggestion>,
}

fn classify_cycle(members: &[SymbolRef]) -> String {
    let all_functions = members.iter().all(|m| m.kind == "function");
    let all_modules = members.iter().all(|m| m.kind == "module");
    if all_functions {
        if members.len() == 1 {
            "selfRecursion".to_string()
        } else {
            "mutualRecursion".to_string()
        }
    } else if all_modules {
        "moduleCycle".to_string()
    } else {
        "mixed".to_string()
    }
}

/// Strongly-connected components of the bridged graph: mutual-recursion
/// clusters, self-recursive functions, and module/import dependency cycles —
/// plus the analysis crate's greedy cycle-break suggestions.
pub fn cycles_report(graph: &AnalysisGraph) -> CyclesReport {
    let clusters = analysis::find_mutual_recursion(graph);
    let mut cycles: Vec<CycleSummary> = clusters
        .into_iter()
        .map(|cluster| {
            let mut members: Vec<SymbolRef> = cluster
                .members
                .iter()
                .filter_map(|id| graph.get_node(id))
                .map(symbol_ref)
                .collect();
            members.sort();
            CycleSummary {
                size: members.len(),
                kind: classify_cycle(&members),
                members,
            }
        })
        .collect();
    cycles.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.members.cmp(&b.members)));

    let mut break_suggestions: Vec<CycleBreakSuggestion> = analysis::cycle_break_suggestions(graph)
        .into_iter()
        .filter_map(|edge| {
            let from = graph.get_node(&edge.from).map(symbol_ref)?;
            let to = graph.get_node(&edge.to).map(symbol_ref)?;
            Some(CycleBreakSuggestion { from, to })
        })
        .collect();
    break_suggestions.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));

    CyclesReport {
        cycle_count: cycles.len(),
        cycles,
        break_suggestions,
    }
}

// =============================================================================
// analyze impact (signature-edit cascade)
// =============================================================================

/// A call site that needs updating after a signature edit.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactCallSite {
    pub caller: String,
    pub file: String,
    pub line: u32,
}

/// One per-file cascade task.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CascadeTaskSummary {
    pub file: String,
    pub instruction: String,
    pub call_sites: Vec<ImpactCallSite>,
}

/// Result of [`impact_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactReport {
    pub target: SymbolRef,
    pub new_signature: String,
    pub task_count: usize,
    pub call_site_count: usize,
    pub tasks: Vec<CascadeTaskSummary>,
}

/// The analysis engine's signature-edit cascade for `target`: every direct
/// call site that must be updated, grouped into one task per file. This is
/// per-call-site precise and distinct from the BFS impact radius of
/// `codegraph impact`. Returns `None` if `target` is not in the graph.
pub fn impact_report(
    graph: &AnalysisGraph,
    target: &ANodeId,
    new_signature: Option<&str>,
) -> Option<ImpactReport> {
    let node = graph.get_node(target)?;
    let signature = new_signature
        .map(str::to_string)
        .or_else(|| node.metadata.get("signature").cloned())
        .unwrap_or_else(|| format!("(unchanged) {}", node.name));
    let description = format!("Signature edit to {}", node.qualified_name);

    let mut tasks: Vec<CascadeTaskSummary> =
        generate_cascade(graph, target, &signature, &description)
            .into_iter()
            .map(|task| {
                let file = task
                    .call_sites
                    .first()
                    .map(|s| s.file_path.display().to_string())
                    .unwrap_or_default();
                let mut call_sites: Vec<ImpactCallSite> = task
                    .call_sites
                    .iter()
                    .map(|s| ImpactCallSite {
                        caller: s.caller_name.clone(),
                        file: s.file_path.display().to_string(),
                        line: s.call_span.start_line,
                    })
                    .collect();
                call_sites
                    .sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.caller.cmp(&b.caller)));
                CascadeTaskSummary {
                    file,
                    instruction: task.instruction,
                    call_sites,
                }
            })
            .collect();
    tasks.sort_by(|a, b| a.file.cmp(&b.file));
    let call_site_count = tasks.iter().map(|t| t.call_sites.len()).sum();

    Some(ImpactReport {
        target: symbol_ref(node),
        new_signature: signature,
        task_count: tasks.len(),
        call_site_count,
        tasks,
    })
}

// =============================================================================
// analyze taint
// =============================================================================

/// One source-to-sink path.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaintPathSummary {
    pub nodes: Vec<SymbolRef>,
    /// Edge kind of each hop (`nodes.len() - 1` entries).
    pub edge_kinds: Vec<String>,
}

/// Result of [`taint_report`] (and of
/// [`crate::analyze_ir::value_taint_report`], which upgrades the oracle).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaintReport {
    pub source: SymbolRef,
    pub sink: SymbolRef,
    pub max_intermediate_nodes: usize,
    /// `"call-graph"` from [`taint_report`]; `"value-level"` when
    /// `analyze taint --value-level` runs over dataflow IR — see `note`.
    pub granularity: String,
    /// Total simple paths found (before capping `paths`).
    pub path_count: usize,
    pub truncated: bool,
    pub paths: Vec<TaintPathSummary>,
    /// IR-lowering coverage, present on value-level runs only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ir_coverage: Option<crate::analyze_ir::IrCoverage>,
    pub note: String,
}

pub(crate) fn edge_label_between(graph: &AnalysisGraph, from: &ANodeId, to: &ANodeId) -> String {
    let mut labels: Vec<&'static str> = graph
        .get_edges_from(from)
        .into_iter()
        .filter(|(target, _)| *target == to)
        .map(|(_, edge)| edge_kind_label(&edge.kind))
        .collect();
    if labels.contains(&"calls") {
        return "calls".to_string();
    }
    labels.sort();
    labels.first().copied().unwrap_or("unknown").to_string()
}

/// All simple paths from `source` to `sink` (the analysis crate's
/// `taint_paths` primitive), each hop annotated with its edge kind. This is
/// graph reachability between the two symbols — the engine's sanitizer-aware
/// value-level taint needs dataflow IR the bridge does not provide, and that
/// limitation is stated in the report instead of being papered over.
/// Returns `None` if either endpoint is not in the graph.
pub fn taint_report(
    graph: &AnalysisGraph,
    source: &ANodeId,
    sink: &ANodeId,
    max_intermediate_nodes: usize,
    max_paths: usize,
) -> Option<TaintReport> {
    let source_node = graph.get_node(source)?;
    let sink_node = graph.get_node(sink)?;

    let mut raw = analysis::taint_paths(graph, source, sink, max_intermediate_nodes);
    raw.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    let path_count = raw.len();
    let truncated = path_count > max_paths;
    raw.truncate(max_paths);

    let paths: Vec<TaintPathSummary> = raw
        .into_iter()
        .map(|path| {
            let edge_kinds = path
                .windows(2)
                .map(|pair| edge_label_between(graph, &pair[0], &pair[1]))
                .collect();
            let nodes = path
                .iter()
                .filter_map(|id| graph.get_node(id))
                .map(symbol_ref)
                .collect();
            TaintPathSummary { nodes, edge_kinds }
        })
        .collect();

    Some(TaintReport {
        source: symbol_ref(source_node),
        sink: symbol_ref(sink_node),
        max_intermediate_nodes,
        granularity: "call-graph".to_string(),
        path_count,
        truncated,
        paths,
        ir_coverage: None,
        note: call_graph_granularity_note("Source-to-sink paths"),
    })
}

// =============================================================================
// analyze slice|taint --source (engine CPG report façade)
// =============================================================================

/// Byte-offset presence over the bridged graph's non-placeholder `Function`
/// nodes — the cheap scan behind the `--source` honesty notes (no file IO,
/// no parsing). Returns `(total, missing_byte_range)`; `0..0` is the
/// bridge's documented "unknown" value for pre-v5 index rows.
fn function_byte_presence(graph: &AnalysisGraph) -> (usize, usize) {
    let mut total = 0usize;
    let mut missing = 0usize;
    for node in graph.nodes_by_kind(ANodeKind::Function) {
        if is_placeholder(node) {
            continue;
        }
        total += 1;
        let range = &node.span.byte_range;
        if range.start == 0 && range.end == 0 {
            missing += 1;
        }
    }
    (total, missing)
}

/// True when the process runs from `workspace_root` (the engine façade reads
/// the bridged graph's project-relative paths against the cwd). Unknowable
/// states (canonicalize failures) report `true` so no spurious warning is
/// emitted.
fn runs_from_workspace_root(workspace_root: &Path) -> bool {
    let cwd = std::env::current_dir().and_then(|d| d.canonicalize());
    let root = workspace_root.canonicalize();
    match (cwd, root) {
        (Ok(cwd), Ok(root)) => cwd == root,
        _ => true,
    }
}

/// Byte-offset coverage embedded in `--source` reports so consumers can
/// judge how much value-level fidelity backed the annotated text.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceReportCoverage {
    /// Non-placeholder `Function` nodes in the bridged graph.
    pub functions_total: usize,
    /// Functions whose index rows carry no byte offsets (indexed before
    /// schema v5) — they contribute no value-level dataflow hops.
    pub functions_missing_byte_range: usize,
}

/// Shared honesty note for the `--source` reports: states what the
/// annotations are, whether value-level fidelity is available (and how to
/// get it back when it is not), and warns when the process cwd is not the
/// project root (the engine façade resolves the graph's project-relative
/// paths against the cwd).
fn source_report_note(
    workspace_root: &Path,
    lead: &str,
    coverage: &SourceReportCoverage,
) -> String {
    let mut note = format!(
        "{lead} Annotations are `name (file:line)` from the indexed spans and work on any index."
    );
    if coverage.functions_total > 0
        && coverage.functions_missing_byte_range == coverage.functions_total
    {
        note.push_str(
            " Value-level fidelity is unavailable: no indexed function carries byte offsets \
             (indexed before schema v5), so the underlying points-to oracle sees no dataflow \
             — re-index (\"codegraph index\") to enable it.",
        );
    } else if coverage.functions_missing_byte_range > 0 {
        note.push_str(&format!(
            " {} of {} functions lack byte offsets (indexed before schema v5) and contribute \
             no value-level hops — re-index (\"codegraph index\") to include them.",
            coverage.functions_missing_byte_range, coverage.functions_total
        ));
    } else {
        note.push_str(" Value-level fidelity rides the index's byte offsets (schema v5).");
    }
    if !runs_from_workspace_root(workspace_root) {
        note.push_str(
            " Source files are resolved relative to the current working directory; run from \
             the project root for value-level fidelity (line annotations are unaffected).",
        );
    }
    note
}

/// Result of [`source_slice_report`] — `analyze slice --source`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSliceReport {
    /// The symbol as given (the engine façade resolves it by name,
    /// qualified-suffix aware).
    pub symbol: String,
    pub direction: String,
    /// Entry cap applied to both annotated lists.
    pub max_entries: usize,
    /// The engine's compact source-annotated slice report (one
    /// `name (file:line)` entry per line, capped at `max_entries`).
    pub report: String,
    /// The engine's one-hop incoming data-dependency report, same
    /// annotation and cap.
    pub data_dependencies: String,
    pub coverage: SourceReportCoverage,
    pub note: String,
}

/// Source-annotated program slice via the engine's CPG report façade
/// (engine entry points: `analysis_tools::program_slice` +
/// `analysis_tools::data_dependencies`). The façade resolves `symbol` by
/// name, builds the interprocedural IR map + points-to oracle itself, and
/// renders every slice node as `name (file:line)` — value-level fidelity
/// when the index carries byte offsets (schema v5), honest degradation
/// notes otherwise. The slice depth is the engine's fixed default
/// (currently 6 hops); `max_entries` caps the rendered lists.
pub fn source_slice_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    symbol: &str,
    direction: SliceDirection,
    max_entries: usize,
) -> SourceSliceReport {
    let backward = matches!(direction, SliceDirection::Backward);
    let report = analysis_tools::program_slice(graph, symbol, backward, max_entries);
    let data_dependencies = analysis_tools::data_dependencies(graph, symbol, max_entries);
    let (functions_total, functions_missing_byte_range) = function_byte_presence(graph);
    let coverage = SourceReportCoverage {
        functions_total,
        functions_missing_byte_range,
    };
    let lead = format!(
        "Slice depth is the engine's fixed default (6 hops); both lists cap at {max_entries} \
         entries."
    );
    let note = source_report_note(workspace_root, &lead, &coverage);
    SourceSliceReport {
        symbol: symbol.to_string(),
        direction: direction.as_str().to_string(),
        max_entries,
        report,
        data_dependencies,
        coverage,
        note,
    }
}

/// Result of [`source_taint_report`] — `analyze taint --source`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceTaintReport {
    pub source: String,
    pub sink: String,
    /// Rendered-flow cap (the engine appends a "… and N more flow(s)"
    /// trailer when more flows exist).
    pub max_paths: usize,
    /// The engine's source-annotated taint-flow report: each flow's full
    /// path rendered hop by hop with sanitizer status.
    pub report: String,
    pub coverage: SourceReportCoverage,
    pub note: String,
}

/// Source-annotated source→sink taint flow via the engine's CPG report
/// façade (engine entry point: `analysis_tools::taint_flow`). The façade
/// resolves both symbols by name, runs the engine's sanitizer-aware
/// value-level taint over the points-to oracle, and caps the rendered
/// flows at `max_paths` — flows beyond the cap are summarized, never
/// silently dropped.
pub fn source_taint_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    source: &str,
    sink: &str,
    max_paths: usize,
) -> SourceTaintReport {
    let report = analysis_tools::taint_flow(
        graph,
        &[source.to_string()],
        &[sink.to_string()],
        &[],
        max_paths,
    );
    let (functions_total, functions_missing_byte_range) = function_byte_presence(graph);
    let coverage = SourceReportCoverage {
        functions_total,
        functions_missing_byte_range,
    };
    let lead = format!("Rendered flows cap at {max_paths} paths.");
    let note = source_report_note(workspace_root, &lead, &coverage);
    SourceTaintReport {
        source: source.to_string(),
        sink: sink.to_string(),
        max_paths,
        report,
        coverage,
        note,
    }
}

// =============================================================================
// analyze query (pipe-based DSL)
// =============================================================================

/// An edge between two result nodes, by qualified name.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryEdge {
    pub from: String,
    pub to: String,
    /// Edge kind as described by the query engine (e.g. `Calls`, `Contains`).
    pub kind: String,
}

/// One why-provenance step: which DSL operator put a node into the result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyStep {
    /// The DSL operator that produced/kept the node.
    pub op: String,
    /// Qualified names of the previous working-set nodes that led here
    /// (empty for seed selectors like `fn("x")`).
    pub predecessors: Vec<String>,
    /// Pipeline stage at which the node entered the working set.
    pub stage: usize,
}

/// Why-provenance for one result node.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyEntry {
    pub symbol: SymbolRef,
    pub steps: Vec<WhyStep>,
}

/// Result of [`query_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryReport {
    pub query: String,
    /// Result rows, sorted by file/line/name. For path queries the path
    /// *order* lives in `edges` (hops are emitted in path order).
    pub nodes: Vec<SymbolRef>,
    pub node_count: usize,
    pub total_before_truncation: usize,
    pub truncated: bool,
    /// Edges between result nodes.
    pub edges: Vec<QueryEdge>,
    /// Engine metadata lines: aggregation scalars (`scalar = N`,
    /// `bool = …`), SCC/cluster/community descriptions, etc.
    pub metadata: Vec<String>,
    /// Nodes at which a traversal detected a cycle.
    pub cycles_detected: Vec<SymbolRef>,
    /// Why-provenance — present when requested *and* the query shape is
    /// traceable (aggregation queries are not; see [`query_report`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<Vec<WhyEntry>>,
    /// Source-level guarding conditions — present only when the query
    /// contains the `preconditions` operator and a workspace root was
    /// supplied (see [`query_report_with_sources`]). Never silently empty:
    /// the section's `note` explains absent or partial extraction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preconditions: Option<PreconditionsSection>,
}

/// Source-level enrichment of the DSL `preconditions` operator: the actual
/// guarding conditions (`if` / `match` / `while` / `for` / `loop`) wrapped
/// around each call site between result nodes, extracted by re-parsing the
/// on-disk sources (engine entry point: `predicates::extract_predicates`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreconditionsSection {
    /// Number of guarded call sites found (== `guards.len()`).
    pub guarded_call_count: usize,
    pub guards: Vec<PreconditionGuard>,
    /// Honesty note: reading order of the conditions, plus any extraction
    /// gaps (pre-v5 byte offsets, non-Rust call sites, unreadable files).
    pub note: String,
}

/// One guarded call site between two result nodes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreconditionGuard {
    pub caller: SymbolRef,
    pub callee: String,
    /// Call-site location (the caller's file).
    pub file: String,
    pub line: u32,
    /// Guarding conditions, outermost first (evaluation order: the first
    /// entry is the outermost gate control flow passed through).
    pub conditions: Vec<String>,
}

/// Render a [`DslQueryError`] without the redundant outer `parse error:`
/// wrapper — the inner parse error already carries position + offending
/// token, which is what a CLI user needs to fix the query.
fn friendly_query_error(err: DslQueryError) -> String {
    match err {
        DslQueryError::Parse(parse) => parse.to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn symbol_sort_key(s: &SymbolRef) -> (&String, u32, &String) {
    (&s.file, s.line, &s.name)
}

/// Why-provenance via the engine's `trace_query`, projected onto
/// [`WhyEntry`] rows for the final result nodes. Returns `None` when the
/// query shape cannot be traced (the aggregation grammar has no pipe to
/// replay) — callers surface that as "unavailable", never as an error.
fn build_why(graph: &AnalysisGraph, query: &str, config: &DslQueryConfig) -> Option<Vec<WhyEntry>> {
    let trace = trace_query(query, graph, config).ok()?;
    let mut entries: Vec<WhyEntry> = trace
        .result_nodes
        .iter()
        .filter_map(|id| {
            let node = graph.get_node(id)?;
            let prov = trace.entries.get(id)?;
            let steps = prov
                .steps
                .iter()
                .map(|step| WhyStep {
                    op: step.op_name.clone(),
                    predecessors: step
                        .predecessors
                        .iter()
                        .filter_map(|p| graph.get_node(p))
                        .map(|n| n.qualified_name.clone())
                        .collect(),
                    stage: step.depth,
                })
                .collect();
            Some(WhyEntry {
                symbol: symbol_ref(node),
                steps,
            })
        })
        .collect();
    entries.sort_by(|a, b| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)));
    Some(entries)
}

/// Does the parsed query contain the `preconditions` pipe operator
/// anywhere in its expression tree?
fn expr_contains_preconditions(expr: &Expr) -> bool {
    // Recursion guard — depth follows the nested query expression tree.
    crate::ensure_sufficient_stack(|| expr_contains_preconditions_inner(expr))
}

fn expr_contains_preconditions_inner(expr: &Expr) -> bool {
    let ops_have = |ops: &[DslOp]| ops.iter().any(|op| matches!(op, DslOp::Preconditions));
    match expr {
        Expr::Pipe(ops) => ops_have(ops),
        Expr::PipeFrom { base, ops } => expr_contains_preconditions(base) || ops_have(ops),
        Expr::SetOp { left, right, .. } => {
            expr_contains_preconditions(left) || expr_contains_preconditions(right)
        }
        Expr::PathQuery(pq) => {
            expr_contains_preconditions(&pq.from) || expr_contains_preconditions(&pq.to)
        }
        Expr::DominatorsOf(inner) | Expr::DominatesOf(inner) | Expr::TraitImplsOf(inner) => {
            expr_contains_preconditions(inner)
        }
        Expr::MultiPath { sources, to, .. } => {
            sources.iter().any(expr_contains_preconditions) || expr_contains_preconditions(to)
        }
        Expr::Entrypoints(_) => false,
    }
}

/// Mirror `run_query_expr`'s parse dispatch (the aggregation grammar first,
/// then the extended expression grammar) to decide whether `query` asks for
/// `preconditions`. Aggregations return scalars — there are no node rows to
/// enrich, so they report `false`.
fn query_requests_preconditions(query: &str) -> bool {
    match parse_aggregate(query) {
        Ok(AggExpr::Plain(expr)) => expr_contains_preconditions(&expr),
        Ok(_) => false,
        Err(_) => parse_expr(query)
            .map(|expr| expr_contains_preconditions(&expr))
            .unwrap_or(false),
    }
}

/// Byte offset of (1-based `line`, 0-based `col`) within `source`, clamped
/// into the line (and the file) so a stale column can never index past the
/// parsed text. `None` when the line is 0 (the bridge's "unknown" value) or
/// beyond the file.
fn byte_pos_of(source: &str, line: u32, col: u32) -> Option<usize> {
    let line_idx = line.checked_sub(1)? as usize;
    let mut offset = 0usize;
    for (i, l) in source.split_inclusive('\n').enumerate() {
        if i == line_idx {
            let within = (col as usize).min(l.len().saturating_sub(1));
            return Some((offset + within).min(source.len().saturating_sub(1)));
        }
        offset += l.len();
    }
    None
}

/// Render one engine predicate for display: bare `if` conditions get their
/// keyword back; `match`/`while`/`for`/`loop` texts already carry theirs.
fn predicate_text(p: &Predicate) -> String {
    if p.kind == "if_expression" {
        format!("if {}", p.text)
    } else {
        p.text.clone()
    }
}

/// Build the source-level preconditions section for a `… | preconditions`
/// query result: for every Calls/UnresolvedCall edge between two result
/// nodes, re-read the call site's source file under `workspace_root` and
/// extract the enclosing branch conditions (engine entry point:
/// `predicates::extract_predicates`, which needs source + a byte position —
/// real on v5 indexes). Extraction gaps are counted and surfaced in the
/// section note instead of being silently dropped.
fn build_preconditions_section(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    result_nodes: &[ANodeId],
) -> PreconditionsSection {
    let in_set: HashSet<&ANodeId> = result_nodes.iter().collect();
    let mut guards: Vec<PreconditionGuard> = Vec::new();
    let mut missing_byte_anchor = 0usize;
    let mut non_rust = 0usize;
    let mut unreadable = 0usize;
    let mut stale_position = 0usize;
    let mut unconditional = 0usize;
    let mut sources: HashMap<PathBuf, Option<String>> = HashMap::new();

    for id in result_nodes {
        let Some(caller) = graph.get_node(id) else {
            continue;
        };
        if is_placeholder(caller) {
            continue;
        }
        for (target, edge) in graph.get_edges_from(id) {
            if !matches!(edge.kind, AEdgeKind::Calls | AEdgeKind::UnresolvedCall(_)) {
                continue;
            }
            if !in_set.contains(target) {
                continue;
            }
            let callee = graph
                .get_node(target)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "?".to_string());
            // v5 honesty gate: source-level anchoring rides the index's byte
            // offsets. The bridge degrades pre-v5 rows to `0..0` — extracting
            // at a guessed position would be a fabricated answer.
            let caller_range = &caller.span.byte_range;
            if caller_range.start == 0 && caller_range.end == 0 {
                missing_byte_anchor += 1;
                continue;
            }
            let file = &edge.source_span.file;
            if file.extension().and_then(|e| e.to_str()) != Some("rs") {
                non_rust += 1;
                continue;
            }
            let cached = sources
                .entry(file.clone())
                .or_insert_with(|| std::fs::read_to_string(workspace_root.join(file)).ok());
            let Some(source) = cached.as_ref() else {
                unreadable += 1;
                continue;
            };
            let Some(byte_pos) = byte_pos_of(
                source,
                edge.source_span.start_line,
                edge.source_span.start_col,
            ) else {
                stale_position += 1;
                continue;
            };
            let preds = extract_predicates(source, byte_pos);
            if preds.is_empty() {
                unconditional += 1;
                continue;
            }
            // The engine returns innermost-first; report evaluation order.
            let conditions: Vec<String> = preds.iter().rev().map(predicate_text).collect();
            guards.push(PreconditionGuard {
                caller: symbol_ref(caller),
                callee,
                file: file.display().to_string(),
                line: edge.source_span.start_line,
                conditions,
            });
        }
    }
    guards.sort_by(|a, b| {
        (&a.file, a.line, &a.caller.name, &a.callee).cmp(&(
            &b.file,
            b.line,
            &b.caller.name,
            &b.callee,
        ))
    });

    let mut note = if guards.is_empty() {
        "No source-level guarding conditions were found on the call sites between the result \
         nodes."
            .to_string()
    } else {
        "Conditions are listed outermost first (evaluation order), extracted from the on-disk \
         sources at each call site — they reflect the working tree as of this run."
            .to_string()
    };
    if unconditional > 0 {
        note.push_str(&format!(
            " {unconditional} call site(s) have no enclosing branch — those calls are \
             unconditional."
        ));
    }
    if missing_byte_anchor > 0 {
        note.push_str(&format!(
            " {missing_byte_anchor} call site(s) could not be anchored: the index carries no \
             byte offsets there (indexed before schema v5) — re-index (\"codegraph index\") to \
             enable source-level precondition extraction."
        ));
    }
    if non_rust > 0 {
        note.push_str(&format!(
            " Source-level predicate extraction currently covers Rust; {non_rust} call site(s) \
             in other languages were skipped."
        ));
    }
    if unreadable > 0 {
        note.push_str(&format!(
            " {unreadable} call-site file(s) could not be read under the project root — the \
             index may be stale (re-run \"codegraph sync\")."
        ));
    }
    if stale_position > 0 {
        note.push_str(&format!(
            " {stale_position} call-site position(s) lie outside the current file contents — \
             the file changed since indexing (re-run \"codegraph sync\")."
        ));
    }

    PreconditionsSection {
        guarded_call_count: guards.len(),
        guards,
        note,
    }
}

/// Run a DSL query over the bridged graph through the engine's unified
/// entry point (`run_query_expr`: pipe chains, set algebra, path patterns,
/// entrypoint/dominator selectors, and aggregations), including the plan
/// optimiser. Parse errors come back as the parser's own message (position
/// + offending token) so the CLI can show them verbatim.
///
/// Equivalent to [`query_report_with_sources`] without source-level
/// enrichment (no workspace root to read files under).
pub fn query_report(
    graph: &AnalysisGraph,
    query: &str,
    max_nodes: usize,
    include_why: bool,
) -> Result<QueryReport, String> {
    query_report_with_sources(graph, query, max_nodes, include_why, None)
}

/// [`query_report`] plus source-level enrichment: when `source_root` is
/// given and the query contains the `preconditions` operator, the report
/// gains a [`PreconditionsSection`] with the actual guarding conditions
/// extracted from the on-disk sources.
pub fn query_report_with_sources(
    graph: &AnalysisGraph,
    query: &str,
    max_nodes: usize,
    include_why: bool,
    source_root: Option<&Path>,
) -> Result<QueryReport, String> {
    let config = DslQueryConfig {
        max_nodes,
        ..Default::default()
    };
    let result = run_query_expr(query, graph, &config).map_err(friendly_query_error)?;

    let mut nodes: Vec<SymbolRef> = result
        .nodes
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(symbol_ref)
        .collect();
    nodes.sort_by(|a, b| symbol_sort_key(a).cmp(&symbol_sort_key(b)));

    let edges: Vec<QueryEdge> = result
        .edges
        .iter()
        .filter_map(|(from, to, kind)| {
            let from = graph.get_node(from)?;
            let to = graph.get_node(to)?;
            Some(QueryEdge {
                from: from.qualified_name.clone(),
                to: to.qualified_name.clone(),
                kind: kind.clone(),
            })
        })
        .collect();

    let mut cycles: Vec<SymbolRef> = result
        .cycles_detected
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(symbol_ref)
        .collect();
    cycles.sort();
    cycles.dedup();

    let why = if include_why {
        build_why(graph, query, &config)
    } else {
        None
    };

    let preconditions = match source_root {
        Some(root) if query_requests_preconditions(query) => {
            Some(build_preconditions_section(graph, root, &result.nodes))
        }
        _ => None,
    };

    Ok(QueryReport {
        query: query.to_string(),
        node_count: nodes.len(),
        total_before_truncation: result.total_before_truncation,
        truncated: result.was_truncated,
        nodes,
        edges,
        metadata: result.metadata,
        cycles_detected: cycles,
        why,
        preconditions,
    })
}

// =============================================================================
// analyze query --explain
// =============================================================================

/// Result of [`explain_report`] — the optimised query plan, never executed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainReport {
    pub query: String,
    /// `pipe` (operator chain), `expression` (set algebra / path pattern /
    /// selector), or `aggregation` (count/sum/group_by…, which bypass the
    /// plan optimiser).
    pub kind: String,
    /// Optimised steps in execution order: one pipe operator per entry, or
    /// a single rendered expression. Optimiser rewrites are already applied
    /// (depth fusion, filter pushdown, intersect operand reordering).
    pub steps: Vec<String>,
    /// The optimiser's BFS schedule hint (`push`, `pull`, or `auto`).
    pub strategy: String,
    pub parallel: bool,
}

fn strategy_label(strategy: ScheduleStrategy) -> &'static str {
    match strategy {
        ScheduleStrategy::Push => "push",
        ScheduleStrategy::Pull => "pull",
        ScheduleStrategy::Auto => "auto",
    }
}

/// Parse and optimise `query` exactly the way [`query_report`] would before
/// executing it, and return the resulting plan. Pure function of the query
/// string — touches neither a graph nor the index.
pub fn explain_report(query: &str) -> Result<ExplainReport, String> {
    // Mirror `run_query_expr`'s dispatch: the aggregation grammar first
    // (it returns `Plain` for non-aggregation input), then the extended
    // expression grammar as the error-reporting fallback.
    let expr = match parse_aggregate(query) {
        Ok(AggExpr::Plain(plain)) => plain,
        Ok(aggregation) => {
            return Ok(ExplainReport {
                query: query.to_string(),
                kind: "aggregation".to_string(),
                steps: vec![format!("{aggregation:?}")],
                strategy: strategy_label(ScheduleStrategy::Auto).to_string(),
                parallel: false,
            });
        }
        Err(_) => parse_expr(query).map_err(|e| e.to_string())?,
    };

    let plan = optimise_expr(expr);
    let optimised = plan.expr().expect("optimise_expr yields Plan::Expr");
    Ok(match optimised {
        Expr::Pipe(ops) => {
            let schedule = pick_schedule_for_pipe(ops);
            ExplainReport {
                query: query.to_string(),
                kind: "pipe".to_string(),
                steps: ops.iter().map(|op| format!("{op:?}")).collect(),
                strategy: strategy_label(schedule.strategy).to_string(),
                parallel: schedule.parallel,
            }
        }
        other => {
            let schedule = plan.schedule();
            ExplainReport {
                query: query.to_string(),
                kind: "expression".to_string(),
                steps: vec![format!("{other:?}")],
                strategy: strategy_label(schedule.strategy).to_string(),
                parallel: schedule.parallel,
            }
        }
    })
}

// =============================================================================
// JSON report envelope
// =============================================================================

/// Version of every `analyze … --json` payload. Bumped when a report field is
/// renamed/removed or its semantics change; additive fields do not bump it
/// (same policy as `codegraph_analysis::schema::SCHEMA_VERSION`).
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Versioned envelope around every `analyze` JSON report:
/// `{"schemaVersion": N, "kind": "<kind>", "data": …}`.
///
/// Mirrors the engine's `codegraph_analysis::schema::Envelope` wire shape.
/// The engine type is not reused because its `kind` discriminator is the
/// closed [`PayloadKind`] enum (four engine payloads only) — host report
/// kinds are open strings instead.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportEnvelope<T: Serialize> {
    pub schema_version: u32,
    pub kind: &'static str,
    pub data: T,
}

impl<T: Serialize> ReportEnvelope<T> {
    pub fn new(kind: &'static str, data: T) -> Self {
        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            kind,
            data,
        }
    }
}

// =============================================================================
// analyze co-change
// =============================================================================

/// One temporally-coupled pair of symbols (cross-file).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoChangePairSummary {
    pub a: SymbolRef,
    pub b: SymbolRef,
    /// Commits in which both symbols' files changed together.
    pub times_changed_together: u32,
    pub total_changes_a: u32,
    pub total_changes_b: u32,
    /// `timesChangedTogether / max(totalChangesA, totalChangesB)` ∈ [0, 1].
    pub confidence: f64,
}

/// Result of [`co_change_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoChangeReport {
    /// Present when the analysis was seeded on one symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<SymbolRef>,
    /// Commits mined from `git log --name-only` (capped by `maxCommits`).
    pub commits_analyzed: usize,
    pub max_commits: usize,
    pub min_support: u32,
    /// Cross-file pairs found (before truncation to `pairs`).
    pub cross_file_pair_count: usize,
    /// Same-file pairs folded out of the listing — symbols in one file
    /// co-change by construction (git history is per-file), so they carry
    /// no coupling signal.
    pub same_file_pair_count: usize,
    pub truncated: bool,
    pub pairs: Vec<CoChangePairSummary>,
    pub note: String,
}

/// Mine commit history with `git log --name-only`, record-separated so the
/// parse is unambiguous. Returns an empty list when git is unavailable or
/// the directory is not a repository.
///
/// Host-side replacement for the engine's `co_change::fetch_git_history`:
/// that helper feeds `--format=%H` output (hash, *blank line*, files) into a
/// parser that expects hash-then-files-then-blank, so against real git it
/// mistakes the first file of every commit for the next commit's hash.
/// Recorded in `notes/close-tier1-needs.md`; swap back once fixed.
fn fetch_commit_history(workspace_root: &Path, max_commits: usize) -> Vec<CommitInfo> {
    let output = std::process::Command::new("git")
        .args([
            "log",
            "--name-only",
            // \x1e (ASCII record separator) marks each commit start.
            "--format=%x1e%H",
            &format!("-n{max_commits}"),
        ])
        .current_dir(workspace_root)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .split('\x1e')
        .filter_map(|record| {
            let mut lines = record.lines().map(str::trim).filter(|l| !l.is_empty());
            let hash = lines.next()?.to_string();
            let files: Vec<String> = lines.map(str::to_string).collect();
            if files.is_empty() {
                return None;
            }
            Some(CommitInfo { hash, files })
        })
        .collect()
}

/// Temporal coupling mined from git history via the analysis engine
/// (`co_change::{compute_co_changes, co_changes_for_nodes}` over
/// [`fetch_commit_history`]). Pure read of `git log`; the graph maps file
/// paths to symbols.
pub fn co_change_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    seed: Option<&ANodeId>,
    min_support: u32,
    max_commits: usize,
    top: usize,
) -> CoChangeReport {
    let commits = fetch_commit_history(workspace_root, max_commits);
    let seed_ref = seed.and_then(|id| graph.get_node(id)).map(symbol_ref);

    let result = match seed {
        Some(id) => co_changes_for_nodes(graph, &commits, std::slice::from_ref(id), min_support),
        None => compute_co_changes(graph, &commits, min_support),
    };

    let mut same_file_pair_count = 0usize;
    let mut pairs: Vec<CoChangePairSummary> = result
        .pairs
        .iter()
        .filter_map(|p| {
            let a = graph.get_node(&p.node_a)?;
            let b = graph.get_node(&p.node_b)?;
            if is_placeholder(a) || is_placeholder(b) {
                return None;
            }
            if a.file_path == b.file_path {
                same_file_pair_count += 1;
                return None;
            }
            let (mut a, mut b) = (symbol_ref(a), symbol_ref(b));
            if symbol_sort_key(&b) < symbol_sort_key(&a) {
                std::mem::swap(&mut a, &mut b);
            }
            Some(CoChangePairSummary {
                a,
                b,
                times_changed_together: p.times_changed_together,
                total_changes_a: p.total_changes_a,
                total_changes_b: p.total_changes_b,
                confidence: p.confidence,
            })
        })
        .collect();
    pairs.sort_by(|x, y| {
        y.confidence
            .total_cmp(&x.confidence)
            .then_with(|| y.times_changed_together.cmp(&x.times_changed_together))
            .then_with(|| symbol_sort_key(&x.a).cmp(&symbol_sort_key(&y.a)))
            .then_with(|| symbol_sort_key(&x.b).cmp(&symbol_sort_key(&y.b)))
    });
    let cross_file_pair_count = pairs.len();
    let truncated = pairs.len() > top;
    pairs.truncate(top);

    let note = if commits.is_empty() {
        "No git history available — the project is not a git repository, git is not on PATH, or \
         the history is empty. Co-change mining reads `git log --name-only`."
            .to_string()
    } else {
        "Co-change is mined from `git log --name-only` at file granularity: every symbol in a \
         touched file counts as changed. Same-file pairs are tautologically coupled and are \
         summarized in sameFilePairCount instead of listed."
            .to_string()
    };

    CoChangeReport {
        seed: seed_ref,
        commits_analyzed: commits.len(),
        max_commits,
        min_support,
        cross_file_pair_count,
        same_file_pair_count,
        truncated,
        pairs,
        note,
    }
}

// =============================================================================
// analyze coverage
// =============================================================================

/// One function with its summed LCOV line hits.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoveredFunction {
    pub symbol: SymbolRef,
    /// Total `DA` hit count across the function's line span.
    pub coverage_count: u64,
    pub tested: bool,
}

/// Result of [`coverage_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    pub lcov_path: String,
    /// Source files present in the LCOV data.
    pub lcov_files: usize,
    /// Malformed LCOV lines skipped by the parser.
    pub parse_warnings: usize,
    /// Non-placeholder functions in the bridged graph.
    pub functions_total: usize,
    pub functions_tested: usize,
    pub functions_untested: usize,
    /// True when the listing was filtered to untested functions.
    pub untested_only: bool,
    pub truncated: bool,
    pub functions: Vec<CoveredFunction>,
    pub note: String,
}

/// Parse an LCOV file and annotate every `Function` node with
/// `coverage_count`/`coverage_tested` metadata (the engine's coverage
/// contract — the same keys the DSL `untested` operator reads).
/// Returns `(lcov_file_count, parse_warnings)`.
pub fn annotate_coverage(
    graph: &mut AnalysisGraph,
    lcov_path: &Path,
    project_root: &Path,
) -> Result<(usize, usize), String> {
    let file = std::fs::File::open(lcov_path)
        .map_err(|e| format!("cannot read LCOV file {}: {e}", lcov_path.display()))?;
    let (lcov, warnings) = parse_lcov(std::io::BufReader::new(file));
    if lcov.files.is_empty() {
        return Err(format!(
            "{} contains no LCOV coverage records (no SF/DA lines)",
            lcov_path.display()
        ));
    }
    let file_count = lcov.files.len();
    annotate_graph_from_lcov(graph, &lcov, project_root);
    Ok((file_count, warnings))
}

/// Map LCOV line coverage onto the bridged graph and report per-function
/// tested/untested status (engine entry points: `coverage::parse_lcov` +
/// `coverage::annotate_graph_from_lcov`).
pub fn coverage_report(
    graph: &mut AnalysisGraph,
    lcov_path: &Path,
    project_root: &Path,
    untested_only: bool,
    top: usize,
) -> Result<CoverageReport, String> {
    let (lcov_files, parse_warnings) = annotate_coverage(graph, lcov_path, project_root)?;

    let mut functions: Vec<CoveredFunction> = graph
        .nodes_by_kind(ANodeKind::Function)
        .into_iter()
        .filter(|n| !is_placeholder(n))
        .map(|n| {
            let coverage_count = n
                .metadata
                .get("coverage_count")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let tested = n
                .metadata
                .get("coverage_tested")
                .map(|v| v == "true")
                .unwrap_or(false);
            CoveredFunction {
                symbol: symbol_ref(n),
                coverage_count,
                tested,
            }
        })
        .collect();

    let functions_total = functions.len();
    let functions_tested = functions.iter().filter(|f| f.tested).count();
    let functions_untested = functions_total - functions_tested;

    // Untested first, then fewest hits, then location — the actionable order.
    functions.sort_by(|a, b| {
        a.tested
            .cmp(&b.tested)
            .then_with(|| a.coverage_count.cmp(&b.coverage_count))
            .then_with(|| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)))
    });
    if untested_only {
        functions.retain(|f| !f.tested);
    }
    let truncated = functions.len() > top;
    functions.truncate(top);

    let mut note = "Coverage is line-granular: LCOV DA hit counts are summed over each \
                    function's line span. Annotating coverage also enables the DSL `untested` \
                    operator (`analyze query --lcov <path> '... | untested'`)."
        .to_string();
    if functions_tested == 0 && functions_total > 0 {
        note.push_str(
            " No function matched any covered line — check that the LCOV SF paths correspond \
             to the indexed file paths (relative to the project root).",
        );
    }

    Ok(CoverageReport {
        lcov_path: lcov_path.display().to_string(),
        lcov_files,
        parse_warnings,
        functions_total,
        functions_tested,
        functions_untested,
        untested_only,
        truncated,
        functions,
        note,
    })
}

// =============================================================================
// analyze validate
// =============================================================================

/// A caller judged incompatible with the proposed signature.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncompatibleCaller {
    pub symbol: SymbolRef,
    pub reason: String,
}

/// One previewed call site that an edit to the target would touch.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateCallSite {
    pub caller: String,
    pub file: String,
    pub line: u32,
}

/// Result of [`validate_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateReport {
    pub target: SymbolRef,
    pub params_before: usize,
    pub params_after: usize,
    /// True when no caller is judged incompatible.
    pub is_safe: bool,
    pub compatible: Vec<SymbolRef>,
    pub incompatible: Vec<IncompatibleCaller>,
    /// All call sites (resolved + unresolved) an edit would touch.
    pub call_sites: Vec<ValidateCallSite>,
    pub note: String,
}

/// Simulate a signature (arity) change before making it — the engine's
/// `validation::VirtualValidator` judging every direct caller, plus the
/// affected-call-site preview. Returns `None` if `target` is not in the
/// graph.
pub fn validate_report(
    graph: &AnalysisGraph,
    target: &ANodeId,
    params_before: usize,
    params_after: usize,
) -> Option<ValidateReport> {
    let target_node = graph.get_node(target)?;
    let validator = VirtualValidator::new(graph);
    let result = validator.validate_signature_change(target, params_before, params_after);

    let mut compatible: Vec<SymbolRef> = result
        .compatible
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(symbol_ref)
        .collect();
    compatible.sort();

    let mut incompatible: Vec<IncompatibleCaller> = result
        .incompatible
        .iter()
        .filter_map(|(id, reason)| {
            Some(IncompatibleCaller {
                symbol: graph.get_node(id).map(symbol_ref)?,
                reason: reason.clone(),
            })
        })
        .collect();
    incompatible.sort_by(|a, b| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)));

    let mut call_sites: Vec<ValidateCallSite> = validator
        .preview_affected_call_sites(target)
        .into_iter()
        .map(|s| ValidateCallSite {
            caller: s.caller_name,
            file: s.call_span.file.display().to_string(),
            line: s.call_span.start_line,
        })
        .collect();
    call_sites.sort_by(|a, b| (&a.file, a.line, &a.caller).cmp(&(&b.file, b.line, &b.caller)));

    Some(ValidateReport {
        target: symbol_ref(target_node),
        params_before,
        params_after,
        is_safe: result.is_safe,
        compatible,
        incompatible,
        call_sites,
        note: "Verdicts are call-graph-level: the bridge carries no per-call-site argument \
               counts, so an arity change marks every direct caller as needing review, and an \
               unchanged arity validates as safe. Unresolved calls appear in callSites but \
               receive no verdict."
            .to_string(),
    })
}

// =============================================================================
// analyze traits
// =============================================================================

/// One trait/interface/protocol and its direct implementors.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitHierarchySummary {
    #[serde(rename = "trait")]
    pub trait_ref: SymbolRef,
    pub implementor_count: usize,
    pub implementors: Vec<SymbolRef>,
}

/// A call edge that dispatches through a trait-declared method.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitDispatchSummary {
    pub caller: SymbolRef,
    pub callee: SymbolRef,
    #[serde(rename = "trait")]
    pub trait_ref: SymbolRef,
}

/// Functions clustered by the type they manipulate most.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeClusterSummary {
    pub primary_type: SymbolRef,
    pub function_count: usize,
    pub functions: Vec<SymbolRef>,
    pub truncated: bool,
}

/// Result of [`traits_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitsReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_filter: Option<String>,
    pub trait_count: usize,
    pub hierarchies: Vec<TraitHierarchySummary>,
    pub dispatch_call_count: usize,
    pub dispatch_calls: Vec<TraitDispatchSummary>,
    pub cluster_count: usize,
    pub clusters: Vec<TypeClusterSummary>,
    pub note: String,
}

/// Cap on dispatch-call rows and clusters listed (full data via `--json`
/// consumers can re-run with a bigger graph slice; these keep human output
/// readable).
const TRAITS_DISPATCH_CAP: usize = 50;
const TRAITS_CLUSTER_CAP: usize = 25;
const TRAITS_CLUSTER_MEMBER_SAMPLE: usize = 10;

/// Case-sensitive symbol filter: exact name, or qualified-name suffix.
fn matches_symbol_filter(s: &SymbolRef, filter: &str) -> bool {
    s.name == filter || s.qualified_name == filter || s.qualified_name.ends_with(filter)
}

/// Trait/type hierarchy analyses over the bridged Implements/Contains/
/// UsesType edges (engine entry points: `trait_hierarchies`,
/// `trait_dispatch_calls`, `cluster_by_primary_type`).
pub fn traits_report(graph: &AnalysisGraph, type_filter: Option<&str>) -> TraitsReport {
    let mut hierarchies: Vec<TraitHierarchySummary> = graph
        .trait_hierarchies()
        .into_iter()
        .filter_map(|h| {
            let trait_ref = graph.get_node(&h.trait_id).map(symbol_ref)?;
            let mut implementors: Vec<SymbolRef> = h
                .direct_impls
                .iter()
                .filter_map(|id| graph.get_node(id))
                .map(symbol_ref)
                .collect();
            implementors.sort();
            Some(TraitHierarchySummary {
                trait_ref,
                implementor_count: implementors.len(),
                implementors,
            })
        })
        .filter(|h| match type_filter {
            Some(f) => {
                matches_symbol_filter(&h.trait_ref, f)
                    || h.implementors.iter().any(|i| matches_symbol_filter(i, f))
            }
            None => true,
        })
        .collect();
    hierarchies.sort_by(|a, b| {
        b.implementor_count
            .cmp(&a.implementor_count)
            .then_with(|| symbol_sort_key(&a.trait_ref).cmp(&symbol_sort_key(&b.trait_ref)))
    });

    let mut dispatch_calls: Vec<TraitDispatchSummary> = graph
        .trait_dispatch_calls()
        .into_iter()
        .filter_map(|d| {
            Some(TraitDispatchSummary {
                caller: graph.get_node(&d.caller).map(symbol_ref)?,
                callee: graph.get_node(&d.callee).map(symbol_ref)?,
                trait_ref: graph.get_node(&d.trait_id).map(symbol_ref)?,
            })
        })
        .filter(|d| match type_filter {
            Some(f) => {
                matches_symbol_filter(&d.trait_ref, f)
                    || matches_symbol_filter(&d.caller, f)
                    || matches_symbol_filter(&d.callee, f)
            }
            None => true,
        })
        .collect();
    dispatch_calls.sort_by(|a, b| {
        (symbol_sort_key(&a.caller), symbol_sort_key(&a.callee))
            .cmp(&(symbol_sort_key(&b.caller), symbol_sort_key(&b.callee)))
    });
    let dispatch_call_count = dispatch_calls.len();
    dispatch_calls.truncate(TRAITS_DISPATCH_CAP);

    let mut clusters: Vec<TypeClusterSummary> = graph
        .cluster_by_primary_type()
        .into_iter()
        .filter_map(|c| {
            let primary_type = graph.get_node(&c.primary_type).map(symbol_ref)?;
            let mut functions: Vec<SymbolRef> = c
                .functions
                .iter()
                .filter_map(|id| graph.get_node(id))
                .map(symbol_ref)
                .collect();
            functions.sort();
            let function_count = functions.len();
            let truncated = functions.len() > TRAITS_CLUSTER_MEMBER_SAMPLE;
            functions.truncate(TRAITS_CLUSTER_MEMBER_SAMPLE);
            Some(TypeClusterSummary {
                primary_type,
                function_count,
                functions,
                truncated,
            })
        })
        .filter(|c| match type_filter {
            Some(f) => matches_symbol_filter(&c.primary_type, f),
            None => true,
        })
        .collect();
    let cluster_count = clusters.len();
    clusters.truncate(TRAITS_CLUSTER_CAP);

    let note = if graph.nodes_by_kind(ANodeKind::Trait).is_empty() {
        "The bridged graph contains no trait/interface/protocol nodes — hierarchies and \
         dispatch detection have nothing to work on in this index."
            .to_string()
    } else {
        "Dispatch detection is structural: a call counts as trait dispatch when the callee is \
         a method the trait itself declares (Trait → contains → method). Calls resolved \
         directly to a concrete implementation are regular call edges, not dispatch."
            .to_string()
    };

    TraitsReport {
        type_filter: type_filter.map(str::to_string),
        trait_count: hierarchies.len(),
        hierarchies,
        dispatch_call_count,
        dispatch_calls,
        cluster_count,
        clusters,
        note,
    }
}

// =============================================================================
// analyze centrality / analyze critical
// =============================================================================

/// PageRank damping factor (the standard 0.85).
const CENTRALITY_DAMPING: f32 = 0.85;

/// One node ranked by PageRank score.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RankedSymbol {
    pub symbol: SymbolRef,
    pub score: f64,
}

/// Result of [`centrality_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CentralityReport {
    pub damping_factor: f64,
    /// Nodes ranked (the whole graph; placeholders excluded from the listing).
    pub analyzed: usize,
    pub nodes: Vec<RankedSymbol>,
}

/// PageRank centrality over the bridged graph (engine entry point:
/// `analysis::centrality`) — the most depended-upon symbols first.
pub fn centrality_report(graph: &AnalysisGraph, top: usize) -> CentralityReport {
    let analyzed = graph.node_count();
    let mut nodes: Vec<RankedSymbol> = analysis::centrality(graph, analyzed, CENTRALITY_DAMPING)
        .into_iter()
        .filter_map(|r| {
            let node = graph.get_node(&r.id)?;
            if is_placeholder(node) {
                return None;
            }
            Some(RankedSymbol {
                symbol: symbol_ref(node),
                score: r.score,
            })
        })
        .collect();
    nodes.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)))
    });
    nodes.truncate(top);

    CentralityReport {
        damping_factor: CENTRALITY_DAMPING as f64,
        analyzed,
        nodes,
    }
}

/// A fragile coupling edge whose removal disconnects the graph.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeEdgeSummary {
    pub from: SymbolRef,
    pub to: SymbolRef,
}

/// Result of [`critical_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CriticalReport {
    /// Articulation nodes found (before truncation).
    pub articulation_count: usize,
    /// Bridge edges found (before truncation).
    pub bridge_count: usize,
    pub truncated: bool,
    pub nodes: Vec<SymbolRef>,
    pub bridges: Vec<BridgeEdgeSummary>,
    pub note: String,
}

/// Articulation nodes + bridge edges (engine entry points:
/// `analysis::critical_nodes`, `analysis::bridge_edges`) — the single points
/// of failure in the dependency structure.
pub fn critical_report(graph: &AnalysisGraph, top: usize) -> CriticalReport {
    let mut nodes: Vec<SymbolRef> = analysis::critical_nodes(graph)
        .into_iter()
        .filter_map(|id| {
            let node = graph.get_node(&id)?;
            if is_placeholder(node) {
                return None;
            }
            Some(symbol_ref(node))
        })
        .collect();
    nodes.sort();
    let articulation_count = nodes.len();

    let mut bridges: Vec<BridgeEdgeSummary> = analysis::bridge_edges(graph)
        .into_iter()
        .filter_map(|e| {
            let from = graph.get_node(&e.from)?;
            let to = graph.get_node(&e.to)?;
            if is_placeholder(from) || is_placeholder(to) {
                return None;
            }
            Some(BridgeEdgeSummary {
                from: symbol_ref(from),
                to: symbol_ref(to),
            })
        })
        .collect();
    bridges.sort_by(|a, b| {
        (symbol_sort_key(&a.from), symbol_sort_key(&a.to))
            .cmp(&(symbol_sort_key(&b.from), symbol_sort_key(&b.to)))
    });
    let bridge_count = bridges.len();

    let truncated = articulation_count > top || bridge_count > top;
    nodes.truncate(top);
    bridges.truncate(top);

    CriticalReport {
        articulation_count,
        bridge_count,
        truncated,
        nodes,
        bridges,
        note: "Computed over the graph treated as undirected: removing an articulation node \
               (or bridge edge) disconnects callers from callees regardless of call direction."
            .to_string(),
    }
}

// =============================================================================
// analyze export
// =============================================================================

/// Node cap for `--symbol` subgraph exports — keeps the DOT renderable.
const EXPORT_SUBGRAPH_MAX_NODES: usize = 2000;

/// Result of [`export_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    /// Output format (only `dot` today).
    pub format: String,
    /// `graph` (everything) or `subgraph` (neighborhood of `seed`).
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<SymbolRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    pub node_count: usize,
    /// The Graphviz DOT document (pipe to `dot -Tsvg`).
    pub dot: String,
}

/// Graphviz DOT export of the bridged graph (engine entry points:
/// `analysis::to_dot`, `analysis::to_dot_subgraph`). With a seed, exports the
/// `depth`-hop neighborhood (both directions). Returns `None` if a seed was
/// given but is not in the graph.
pub fn export_report(
    graph: &AnalysisGraph,
    seed: Option<&ANodeId>,
    depth: usize,
) -> Option<ExportReport> {
    match seed {
        Some(id) => {
            let seed_node = graph.get_node(id)?;
            let config = TraversalConfig {
                max_depth: depth,
                max_nodes: EXPORT_SUBGRAPH_MAX_NODES,
                direction: TraversalDirection::Both,
                parallel: false,
            };
            let walk = traverse(graph, id, &config);
            let dot = analysis::to_dot_subgraph(graph, &walk.nodes);
            Some(ExportReport {
                format: "dot".to_string(),
                scope: "subgraph".to_string(),
                seed: Some(symbol_ref(seed_node)),
                depth: Some(depth),
                node_count: walk.nodes.len(),
                dot,
            })
        }
        None => Some(ExportReport {
            format: "dot".to_string(),
            scope: "graph".to_string(),
            seed: None,
            depth: None,
            node_count: graph.node_count(),
            dot: analysis::to_dot(graph),
        }),
    }
}

// =============================================================================
// analyze types
// =============================================================================

/// Result of [`types_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypesReport {
    pub symbol: SymbolRef,
    /// Concrete types that can flow into the function (parameters).
    pub input_types: Vec<String>,
    /// Concrete types the function can produce.
    pub return_types: Vec<String>,
    /// Functions across the graph that received any possible-types
    /// annotation from the propagation pass.
    pub functions_annotated: usize,
    pub note: String,
}

/// Run the engine's possible-types propagation as an enrichment pass over
/// the bridged graph (through `pass.rs` — `PassManager` + `PossibleTypesPass`,
/// seeded `TreeParsed` the way the bridge's graph state warrants), then
/// report the concrete type sets for `symbol`. Returns `Ok(None)` if `symbol`
/// is not in the graph.
pub fn types_report(
    graph: &mut AnalysisGraph,
    symbol: &ANodeId,
) -> Result<Option<TypesReport>, String> {
    let mut manager = PassManager::new();
    manager.seed(GraphFlag::TreeParsed);
    manager.register(Box::new(PossibleTypesPass));
    manager
        .run(graph)
        .map_err(|e| format!("possible-types pass failed: {e}"))?;

    let Some(node) = graph.get_node(symbol) else {
        return Ok(None);
    };

    let parse_list = |key: &str| -> Vec<String> {
        node.metadata
            .get(key)
            .and_then(|v| serde_json::from_str::<Vec<String>>(v).ok())
            .unwrap_or_default()
    };
    let input_types = parse_list("possible_input_types");
    let return_types = parse_list("possible_return_types");

    let annotated: HashSet<&ANodeId> = graph
        .nodes_with_metadata_key("possible_input_types")
        .chain(graph.nodes_with_metadata_key("possible_return_types"))
        .collect();

    let note = if input_types.is_empty() && return_types.is_empty() {
        "No concrete types flow into or out of this function in the bridged graph. The \
         propagation is seeded from resolved UsesType edges and expanded through Implements \
         edges and the call graph — a function with no resolved type references stays empty."
            .to_string()
    } else {
        "Possible types over-approximate: they include every concrete type that could flow \
         here along UsesType/Implements/Calls edges, not just types a real execution \
         delivers. Generics are tracked without their arguments (Vec<T> is Vec)."
            .to_string()
    };

    Ok(Some(TypesReport {
        symbol: symbol_ref(node),
        input_types,
        return_types,
        functions_annotated: annotated.len(),
        note,
    }))
}

// =============================================================================
// analyze generics
// =============================================================================

/// A generic instantiation reported by the engine (callsite-supplied
/// concrete type arguments).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericInstantiationSummary {
    pub generic: SymbolRef,
    pub callsite: SymbolRef,
    pub type_args: Vec<String>,
}

/// A definition that *looks* generic based on its carried signature.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LikelyGenericDefinition {
    pub symbol: SymbolRef,
    /// Type-parameter-looking tokens found in the signature (e.g. `T`, `K`).
    pub type_params: Vec<String>,
}

/// Result of [`generics_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericsReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_filter: Option<String>,
    /// Engine-detected instantiations (callsite type args).
    pub instantiation_count: usize,
    pub instantiations: Vec<GenericInstantiationSummary>,
    /// Signature-heuristic generic definitions (see `note`).
    pub likely_generic_count: usize,
    pub truncated: bool,
    pub likely_generic_definitions: Vec<LikelyGenericDefinition>,
    pub note: String,
}

/// Extract type-parameter-looking tokens from a signature: standalone
/// identifiers of one uppercase letter optionally followed by a digit
/// (`T`, `U`, `K1`) — the conventional generic-parameter shape across
/// Rust/TS/Java/Go/C#.
fn signature_type_params(signature: &str) -> Vec<String> {
    let mut params: Vec<String> = Vec::new();
    let mut token = String::new();
    let flush = |token: &mut String, params: &mut Vec<String>| {
        let looks_generic = match token.len() {
            1 => token.chars().next().is_some_and(|c| c.is_ascii_uppercase()),
            2 => {
                let mut chars = token.chars();
                chars.next().is_some_and(|c| c.is_ascii_uppercase())
                    && chars.next().is_some_and(|c| c.is_ascii_digit())
            }
            _ => false,
        };
        if looks_generic && !params.contains(token) {
            params.push(std::mem::take(token));
        } else {
            token.clear();
        }
    };
    for c in signature.chars() {
        if c.is_alphanumeric() || c == '_' {
            token.push(c);
        } else {
            flush(&mut token, &mut params);
        }
    }
    flush(&mut token, &mut params);
    params.sort();
    params
}

/// Cap on likely-generic definitions listed.
const GENERICS_DEFINITION_CAP: usize = 50;

/// Generic instantiation detection (engine entry point:
/// `monomorphize::find_instantiations`) plus a signature-based heuristic
/// listing of likely generic definitions, since the bridge does not carry
/// the engine's `generic_params`/`callee_type_args` metadata yet.
pub fn generics_report(graph: &AnalysisGraph, symbol_filter: Option<&str>) -> GenericsReport {
    let instantiations: Vec<GenericInstantiationSummary> = find_instantiations(graph)
        .into_iter()
        .filter_map(|inst| {
            Some(GenericInstantiationSummary {
                generic: graph.get_node(&inst.generic_id).map(symbol_ref)?,
                callsite: graph.get_node(&inst.callsite_id).map(symbol_ref)?,
                type_args: inst.type_args,
            })
        })
        .filter(|inst| match symbol_filter {
            Some(f) => {
                matches_symbol_filter(&inst.generic, f) || matches_symbol_filter(&inst.callsite, f)
            }
            None => true,
        })
        .collect();

    let mut definitions: Vec<LikelyGenericDefinition> = graph
        .all_node_ids()
        .into_iter()
        .filter_map(|id| {
            let node = graph.get_node(id)?;
            if is_placeholder(node) {
                return None;
            }
            let signature = node.metadata.get("signature")?;
            let type_params = signature_type_params(signature);
            if type_params.is_empty() {
                return None;
            }
            Some(LikelyGenericDefinition {
                symbol: symbol_ref(node),
                type_params,
            })
        })
        .filter(|d| match symbol_filter {
            Some(f) => matches_symbol_filter(&d.symbol, f),
            None => true,
        })
        .collect();
    definitions.sort_by(|a, b| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)));
    let likely_generic_count = definitions.len();
    let truncated = definitions.len() > GENERICS_DEFINITION_CAP;
    definitions.truncate(GENERICS_DEFINITION_CAP);

    GenericsReport {
        symbol_filter: symbol_filter.map(str::to_string),
        instantiation_count: instantiations.len(),
        instantiations,
        likely_generic_count,
        truncated,
        likely_generic_definitions: definitions,
        note: "Instantiation detection reads the engine's generics metadata contract \
               (generic_params on declarations, callee_type_args on callers); the SQLite \
               bridge does not populate those keys yet, so instantiations over a bridged \
               index are empty until that enrichment lands. likelyGenericDefinitions is a \
               signature heuristic instead: standalone single-letter type tokens (T, U, K1) \
               found in carried signatures."
            .to_string(),
    }
}

// =============================================================================
// analyze taint --suggest
// =============================================================================

/// A function whose name leans source-ish or sink-ish.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaintCandidate {
    pub symbol: SymbolRef,
    /// Fraction of name sub-tokens matching the lexicon, in [0, 1].
    pub score: f64,
}

/// A ranked candidate source→sink pair.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedTaintPair {
    pub source: SymbolRef,
    pub sink: SymbolRef,
    /// `taint_naming::flow_priority` of the pair (name evidence).
    pub priority: f64,
}

/// Result of [`taint_suggest_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaintSuggestReport {
    /// Functions classified (placeholders for unresolved calls included —
    /// library calls like `exec` are prime sink candidates).
    pub functions_classified: usize,
    pub source_count: usize,
    pub sink_count: usize,
    pub sources: Vec<TaintCandidate>,
    pub sinks: Vec<TaintCandidate>,
    pub pairs: Vec<SuggestedTaintPair>,
    pub note: String,
}

/// Candidates listed per side, and the source×sink pool size paired before
/// ranking (caps the cross product on big graphs).
const TAINT_SUGGEST_CANDIDATE_CAP: usize = 15;
const TAINT_SUGGEST_PAIR_POOL: usize = 25;

/// Name-based taint source/sink suggestion (engine entry points:
/// `taint_naming::classify_name`, `taint_naming::flow_priority`) for when no
/// source/sink arguments are given — Fluffy-style lexical priors.
pub fn taint_suggest_report(graph: &AnalysisGraph, top: usize) -> TaintSuggestReport {
    let mut sources: Vec<TaintCandidate> = Vec::new();
    let mut sinks: Vec<TaintCandidate> = Vec::new();
    let mut functions_classified = 0usize;

    for node in graph.nodes_by_kind(ANodeKind::Function) {
        functions_classified += 1;
        let class = classify_name(&node.name);
        if class.looks_like_source() {
            sources.push(TaintCandidate {
                symbol: symbol_ref(node),
                score: class.source_score,
            });
        } else if class.looks_like_sink() {
            sinks.push(TaintCandidate {
                symbol: symbol_ref(node),
                score: class.sink_score,
            });
        }
    }

    let rank = |list: &mut Vec<TaintCandidate>| {
        list.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)))
        });
    };
    rank(&mut sources);
    rank(&mut sinks);
    let source_count = sources.len();
    let sink_count = sinks.len();

    let mut pairs: Vec<SuggestedTaintPair> = Vec::new();
    for source in sources.iter().take(TAINT_SUGGEST_PAIR_POOL) {
        for sink in sinks.iter().take(TAINT_SUGGEST_PAIR_POOL) {
            if source.symbol == sink.symbol {
                continue;
            }
            pairs.push(SuggestedTaintPair {
                source: source.symbol.clone(),
                sink: sink.symbol.clone(),
                priority: flow_priority(&source.symbol.name, &sink.symbol.name),
            });
        }
    }
    pairs.sort_by(|a, b| {
        b.priority
            .total_cmp(&a.priority)
            .then_with(|| symbol_sort_key(&a.source).cmp(&symbol_sort_key(&b.source)))
            .then_with(|| symbol_sort_key(&a.sink).cmp(&symbol_sort_key(&b.sink)))
    });
    pairs.truncate(top);

    sources.truncate(TAINT_SUGGEST_CANDIDATE_CAP);
    sinks.truncate(TAINT_SUGGEST_CANDIDATE_CAP);

    let note = if source_count == 0 && sink_count == 0 {
        "No function name in this graph matches the source/sink lexicons (input/request/env/… \
         vs exec/query/write/…). Name-based suggestion has nothing to rank — pass an explicit \
         <source> <sink> pair instead."
            .to_string()
    } else {
        "Candidates are ranked purely by identifier naming (lexical priors), not by confirmed \
         data flow. Confirm a pair with `codegraph analyze taint <source> <sink>`. Unresolved \
         library calls (file <unresolved>) can rank as sinks but cannot be queried by name."
            .to_string()
    };

    TaintSuggestReport {
        functions_classified,
        source_count,
        sink_count,
        sources,
        sinks,
        pairs,
        note,
    }
}

// =============================================================================
// analyze boundaries
// =============================================================================

/// An HTTP route provider.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRouteBoundary {
    pub method: String,
    pub path: String,
    pub provider: SymbolRef,
}

/// A C-ABI export.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfiBoundary {
    pub symbol_name: String,
    pub provider: SymbolRef,
}

/// A WASM export or import.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmBoundary {
    /// `export` or `import`.
    pub direction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub name: String,
    pub provider: SymbolRef,
}

/// Cross-language stitching counters from the engine's
/// `resolve_cross_language_calls`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossLanguageStitching {
    pub boundaries_seen: usize,
    pub clients_seen: usize,
    pub edges_emitted: usize,
    pub edge_errors: usize,
}

/// Result of [`boundaries_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundariesReport {
    pub boundary_count: usize,
    pub http_routes: Vec<HttpRouteBoundary>,
    pub ffi_exports: Vec<FfiBoundary>,
    pub wasm_boundaries: Vec<WasmBoundary>,
    pub cross_language_calls: CrossLanguageStitching,
    pub note: String,
}

/// Polyglot boundary detection + cross-language call stitching (engine entry
/// points: `polyglot::{detect_http_routes, detect_ffi_exports,
/// detect_wasm_exports, resolve_cross_language_calls}`). Stitched
/// `ExternalCall` edges land in the in-memory graph only — the SQLite index
/// is never mutated.
pub fn boundaries_report(graph: &mut AnalysisGraph) -> BoundariesReport {
    let mut boundaries = detect_http_routes(graph);
    boundaries.extend(detect_ffi_exports(graph));
    boundaries.extend(detect_wasm_exports(graph));

    let mut http_routes: Vec<HttpRouteBoundary> = Vec::new();
    let mut ffi_exports: Vec<FfiBoundary> = Vec::new();
    let mut wasm_boundaries: Vec<WasmBoundary> = Vec::new();
    for boundary in &boundaries {
        let Some(provider) = graph.get_node(&boundary.provider_node).map(symbol_ref) else {
            continue;
        };
        match &boundary.kind {
            BoundaryKind::HttpRoute { method, path } => http_routes.push(HttpRouteBoundary {
                method: method.clone(),
                path: path.clone(),
                provider,
            }),
            BoundaryKind::FfiExport { symbol } => ffi_exports.push(FfiBoundary {
                symbol_name: symbol.clone(),
                provider,
            }),
            BoundaryKind::WasmExport { name } => wasm_boundaries.push(WasmBoundary {
                direction: "export".to_string(),
                module: None,
                name: name.clone(),
                provider,
            }),
            BoundaryKind::WasmImport { module, name } => wasm_boundaries.push(WasmBoundary {
                direction: "import".to_string(),
                module: Some(module.clone()),
                name: name.clone(),
                provider,
            }),
            BoundaryKind::GrpcService { .. } => {}
        }
    }
    http_routes.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
    ffi_exports.sort_by(|a, b| a.symbol_name.cmp(&b.symbol_name));
    wasm_boundaries.sort_by(|a, b| (&a.direction, &a.name).cmp(&(&b.direction, &b.name)));

    let report = resolve_cross_language_calls(graph, &boundaries);
    let boundary_count = http_routes.len() + ffi_exports.len() + wasm_boundaries.len();

    let note = if boundary_count == 0 {
        "No cross-language boundaries detected. Detection reads the engine's metadata \
         contract on Function nodes (http_route/http_method, http_client_target, ffi_export, \
         wasm_export, wasm_import_module/name); the SQLite bridge does not populate these \
         keys yet — host route nodes are dropped by the 5-kind projection and extern/wasm \
         qualifiers are not carried in signatures — so a bridged index reports no boundaries \
         until that enrichment lands."
            .to_string()
    } else {
        "Stitched cross-language call edges (ExternalCall) connect HTTP clients to matching \
         route providers in the in-memory analysis graph only; the index is unchanged."
            .to_string()
    };

    BoundariesReport {
        boundary_count,
        http_routes,
        ffi_exports,
        wasm_boundaries,
        cross_language_calls: CrossLanguageStitching {
            boundaries_seen: report.boundaries_seen,
            clients_seen: report.clients_seen,
            edges_emitted: report.edges_emitted,
            edge_errors: report.edge_errors,
        },
        note,
    }
}

// =============================================================================
// analyze capabilities
// =============================================================================

/// Every engine capability, in display order. The engine's own
/// `ALL_CAPABILITIES` array is private (see `notes/close-tier1-needs.md`),
/// so the list is mirrored here — `Capability` is `#[non_exhaustive]`-free
/// and six-variant today.
const ALL_CAPABILITIES: [(Capability, &str); 6] = [
    (Capability::CallGraph, "callGraph"),
    (Capability::TypeUsage, "typeUsage"),
    (Capability::PartialStruct, "partialStruct"),
    (Capability::VirtualValidation, "virtualValidation"),
    (Capability::Persistence, "persistence"),
    (Capability::SymbolEditing, "symbolEditing"),
];

/// One capability's resolved status.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityStatus {
    pub name: String,
    /// The `CODEGRAPH_ANALYSIS_CAP_*` kill-switch env var.
    pub env_var: String,
    /// Resolved state after env overrides and dependency cascading.
    pub enabled: bool,
    /// Raw env var value, when set in the current environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_value: Option<String>,
    /// Capabilities additionally disabled when this one is turned off
    /// (dependency cascade).
    pub disables: Vec<String>,
}

/// Result of [`capabilities_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesReport {
    pub capabilities: Vec<CapabilityStatus>,
    pub note: String,
}

fn capability_display_name(cap: Capability) -> &'static str {
    ALL_CAPABILITIES
        .iter()
        .find(|(c, _)| *c == cap)
        .map(|(_, name)| *name)
        .unwrap_or("unknown")
}

/// The engine's capability tree resolved against the current environment
/// (engine entry points: `CapabilityTree::from_env`, `Capability::env_name`).
/// Pure environment read — touches no index.
pub fn capabilities_report() -> CapabilitiesReport {
    let resolved = CapabilityTree::from_env();

    let capabilities: Vec<CapabilityStatus> = ALL_CAPABILITIES
        .iter()
        .map(|(cap, name)| {
            // Probe the dependency cascade against a fresh default tree:
            // `disable` returns exactly the dependents it switched off.
            let mut probe = CapabilityTree::new();
            let mut disables: Vec<String> = probe
                .disable(*cap)
                .into_iter()
                .map(|c| capability_display_name(c).to_string())
                .collect();
            disables.sort();

            CapabilityStatus {
                name: (*name).to_string(),
                env_var: cap.env_name().to_string(),
                enabled: resolved.is_enabled(*cap),
                env_value: std::env::var(cap.env_name()).ok(),
                disables,
            }
        })
        .collect();

    CapabilitiesReport {
        capabilities,
        note: "All capabilities are enabled by default. Set <ENV_VAR>=0|false|off|no|disabled \
               to disable one; disabling cascades to its dependents (listed in disables)."
            .to_string(),
    }
}

// =============================================================================
// analyze schema
// =============================================================================

/// The payload kinds `analyze schema` accepts, with their engine enum values.
const SCHEMA_KINDS: [(&str, PayloadKind); 4] = [
    ("query_result", PayloadKind::QueryResult),
    ("entrypoint_summary", PayloadKind::EntrypointSummary),
    ("context_result", PayloadKind::ContextResult),
    ("formatted_output", PayloadKind::FormattedOutput),
];

/// JSON Schema (draft-07) for one of the engine's stabilised payload kinds
/// (engine entry point: `schema::json_schema_for`). The returned text is the
/// engine's own schema document, printed verbatim. Unknown kinds list the
/// accepted names.
pub fn schema_text(kind: &str) -> Result<&'static str, String> {
    let normalized = kind.trim().to_ascii_lowercase().replace('-', "_");
    SCHEMA_KINDS
        .iter()
        .find(|(name, _)| *name == normalized)
        .map(|(_, payload)| json_schema_for(*payload))
        .ok_or_else(|| {
            let known: Vec<&str> = SCHEMA_KINDS.iter().map(|(name, _)| *name).collect();
            format!(
                "unknown schema kind \"{kind}\" — known kinds: {}",
                known.join(", ")
            )
        })
}

// =============================================================================
// analyze stats
// =============================================================================

/// Above this node count, reachability estimates use the engine's
/// HyperLogLog sketches (ANF-style, ~2% standard error); at or below it,
/// exact per-node BFS counts are computed instead — small graphs get exact
/// numbers, huge graphs get the estimator they need.
const HLL_EXACT_THRESHOLD: usize = 5_000;

/// Per-node reachability counts (exact or estimated — see `method`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReachabilityEntry {
    pub symbol: SymbolRef,
    /// Nodes reachable from this one (any edge kind).
    pub descendants: f64,
    /// Nodes that can reach this one.
    pub ancestors: f64,
}

/// Reachability section of [`StatsReport`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReachabilitySection {
    /// `exact` (BFS per node) or `hyperloglog` (ANF sketches).
    pub method: String,
    /// Node count above which the HLL estimator is used.
    pub exact_threshold: usize,
    /// Top nodes by descendant count.
    pub top: Vec<ReachabilityEntry>,
    pub note: String,
}

/// Result of [`stats_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsReport {
    pub node_count: usize,
    pub edge_count: usize,
    /// Distinct files contributing nodes (placeholders excluded).
    pub file_count: usize,
    /// Placeholder nodes anchoring unresolved calls.
    pub placeholder_count: usize,
    pub nodes_by_kind: BTreeMap<String, usize>,
    pub edges_by_kind: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reachability: Option<ReachabilitySection>,
}

/// Exact per-node reachability via BFS in both directions. O(V·(V+E)) —
/// only used at or below [`HLL_EXACT_THRESHOLD`].
fn exact_reachability(graph: &AnalysisGraph) -> HashMap<ANodeId, (f64, f64)> {
    let ids: Vec<&ANodeId> = graph.all_node_ids();
    let index_of: HashMap<&ANodeId, usize> =
        ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();

    let mut forward: Vec<Vec<usize>> = vec![Vec::new(); ids.len()];
    let mut backward: Vec<Vec<usize>> = vec![Vec::new(); ids.len()];
    for (i, id) in ids.iter().enumerate() {
        for (target, _) in graph.get_edges_from(id) {
            if let Some(&j) = index_of.get(target) {
                forward[i].push(j);
                backward[j].push(i);
            }
        }
    }

    let bfs_count = |adjacency: &[Vec<usize>], start: usize| -> usize {
        let mut seen = vec![false; adjacency.len()];
        seen[start] = true;
        let mut queue = std::collections::VecDeque::from([start]);
        let mut count = 0usize;
        while let Some(current) = queue.pop_front() {
            for &next in &adjacency[current] {
                if !seen[next] {
                    seen[next] = true;
                    count += 1;
                    queue.push_back(next);
                }
            }
        }
        count
    };

    ids.iter()
        .enumerate()
        .map(|(i, id)| {
            (
                (*id).clone(),
                (
                    bfs_count(&forward, i) as f64,
                    bfs_count(&backward, i) as f64,
                ),
            )
        })
        .collect()
}

/// Bridged-graph statistics, with optional reachability profiling — exact at
/// small scale, the engine's HyperLogLog estimator
/// (`hll::approximate_reachability`) above [`HLL_EXACT_THRESHOLD`].
pub fn stats_report(graph: &AnalysisGraph, estimate_reachability: bool, top: usize) -> StatsReport {
    let mut nodes_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut edges_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut files: HashSet<&Path> = HashSet::new();
    let mut placeholder_count = 0usize;

    for id in graph.all_node_ids() {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        if is_placeholder(node) {
            placeholder_count += 1;
        } else {
            files.insert(node.file_path.as_path());
        }
        *nodes_by_kind.entry(kind_label(node.kind)).or_default() += 1;
        for (_, edge) in graph.get_edges_from(id) {
            *edges_by_kind
                .entry(edge_kind_label(&edge.kind).to_string())
                .or_default() += 1;
        }
    }

    let reachability = estimate_reachability.then(|| {
        let node_count = graph.node_count();
        let (method, counts, note) = if node_count <= HLL_EXACT_THRESHOLD {
            (
                "exact",
                exact_reachability(graph),
                format!(
                    "Exact BFS counts — the graph ({node_count} nodes) is at or below the \
                     {HLL_EXACT_THRESHOLD}-node threshold where the HyperLogLog estimator \
                     takes over."
                ),
            )
        } else {
            let estimates = approximate_reachability(graph);
            let mut counts: HashMap<ANodeId, (f64, f64)> = HashMap::new();
            for (id, descendants) in estimates.descendant_count {
                counts.entry(id).or_insert((0.0, 0.0)).0 = descendants.max(0.0);
            }
            for (id, ancestors) in estimates.ancestor_count {
                counts.entry(id).or_insert((0.0, 0.0)).1 = ancestors.max(0.0);
            }
            (
                "hyperloglog",
                counts,
                "HyperLogLog (ANF) estimates with ~2% standard error — exact per-node counts \
                 would cost O(V·(V+E)) at this scale."
                    .to_string(),
            )
        };

        let mut entries: Vec<ReachabilityEntry> = counts
            .into_iter()
            .filter_map(|(id, (descendants, ancestors))| {
                let node = graph.get_node(&id)?;
                if is_placeholder(node) {
                    return None;
                }
                Some(ReachabilityEntry {
                    symbol: symbol_ref(node),
                    descendants,
                    ancestors,
                })
            })
            .collect();
        entries.sort_by(|a, b| {
            b.descendants
                .total_cmp(&a.descendants)
                .then_with(|| b.ancestors.total_cmp(&a.ancestors))
                .then_with(|| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)))
        });
        entries.truncate(top);

        ReachabilitySection {
            method: method.to_string(),
            exact_threshold: HLL_EXACT_THRESHOLD,
            top: entries,
            note,
        }
    });

    StatsReport {
        node_count: graph.node_count(),
        edge_count: graph.edge_count(),
        file_count: files.len(),
        placeholder_count,
        nodes_by_kind,
        edges_by_kind,
        reachability,
    }
}

// =============================================================================
// analyze diff
// =============================================================================

/// Node cap for each per-seed impact walk — bounds the BFS on huge deltas.
const DIFF_IMPACT_WALK_CAP: usize = 5_000;

/// Measure cyclomatic/cognitive complexity for every non-placeholder
/// `Function` node by re-parsing the on-disk sources (the `analyze
/// complexity` anchor pattern). Functions whose language has no complexity
/// rules, whose file is unreadable, or whose body cannot be located are
/// simply absent from the map — `analyze diff` reports those as
/// "complexity unavailable", never as zero.
pub fn measure_complexity_map(
    graph: &AnalysisGraph,
    workspace_root: &Path,
) -> HashMap<ANodeId, StoredComplexity> {
    struct ParsedFile {
        tree: Tree,
        source: String,
        lang_id: &'static str,
    }
    let mut cache: HashMap<String, Option<ParsedFile>> = HashMap::new();
    let mut measured: HashMap<ANodeId, StoredComplexity> = HashMap::new();

    for node in graph.nodes_by_kind(ANodeKind::Function) {
        if is_placeholder(node) {
            continue;
        }
        let key = node.file_path.display().to_string();
        let parsed = cache.entry(key).or_insert_with(|| {
            let language = detect_language(&node.file_path.to_string_lossy(), None);
            let lang_id = complexity_lang_id(language)?;
            let source = std::fs::read_to_string(workspace_root.join(&node.file_path)).ok()?;
            let mut parser = create_parser(language)?;
            let tree = parser.parse(&source, None)?;
            Some(ParsedFile {
                tree,
                source,
                lang_id,
            })
        });
        let Some(parsed) = parsed.as_ref() else {
            continue;
        };
        let Some(rules) = LangRules::for_language(parsed.lang_id) else {
            continue;
        };
        let Some(fn_node) = locate_function_node(
            parsed.tree.root_node(),
            node.span.start_line,
            node.span.start_col,
            rules,
        ) else {
            continue;
        };
        let Some(metrics) = compute_complexity(fn_node, parsed.source.as_bytes(), parsed.lang_id)
        else {
            continue;
        };
        measured.insert(
            node.id.clone(),
            StoredComplexity {
                cyclomatic: metrics.cyclomatic,
                cognitive: metrics.cognitive,
                max_nesting: metrics.max_nesting,
            },
        );
    }
    measured
}

/// Where the diff base came from.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffBaseDescriptor {
    /// `cache` (stale current generation), `cache-prev` (rotated previous
    /// generation), or `file` (explicit `--base <path>`).
    pub source: String,
    /// Hex index fingerprint of the base, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_fingerprint: Option<String>,
}

/// One node present in both states whose structure differs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedNode {
    pub symbol: SymbolRef,
    /// What differs: `spanLines`, `byteLength`, `signature`, `visibility`,
    /// `fields`, `variants`, `accessedFields`, `async`, `exported`.
    pub reasons: Vec<String>,
}

/// An edge present in exactly one of the two states.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

/// Complexity before/after for one changed function.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFunctionDelta {
    pub symbol: SymbolRef,
    pub lines_before: u32,
    pub lines_after: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cyclomatic_before: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cyclomatic_after: Option<u32>,
    /// Present only when both ends were measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cyclomatic_delta: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cognitive_before: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cognitive_after: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cognitive_delta: Option<i64>,
}

/// Impact section of [`DiffReport`]: what the delta can break.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaImpact {
    /// BFS depth walked from each delta node along incoming edges.
    pub depth: usize,
    /// Impacted symbols found (before truncation; delta nodes excluded).
    pub impacted_count: usize,
    pub truncated: bool,
    pub nodes: Vec<SymbolRef>,
}

/// Result of [`diff_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReport {
    pub base: DiffBaseDescriptor,
    pub nodes_added_count: usize,
    pub nodes_removed_count: usize,
    pub nodes_changed_count: usize,
    pub nodes_added: Vec<SymbolRef>,
    pub nodes_removed: Vec<SymbolRef>,
    pub nodes_changed: Vec<ChangedNode>,
    pub edges_added_count: usize,
    pub edges_removed_count: usize,
    pub edges_added: Vec<DiffEdge>,
    pub edges_removed: Vec<DiffEdge>,
    /// True when any listing above was capped at `top`.
    pub truncated: bool,
    /// Changed/added `Function` nodes with complexity before/after.
    pub changed_functions: Vec<ChangedFunctionDelta>,
    /// SCC clusters present now but not in the base.
    pub new_cycle_count: usize,
    pub new_cycles: Vec<CycleSummary>,
    /// SCC clusters present in the base but not anymore.
    pub resolved_cycle_count: usize,
    pub impact: DeltaImpact,
    pub note: String,
}

/// Structural comparison of one node present in both states. Pure position
/// shifts (a function pushed down by an edit above it) are deliberately NOT
/// changes — only span *length*, byte *length*, and carried structure count.
fn node_change_reasons(base: &ANodeData, current: &ANodeData) -> Vec<String> {
    let mut reasons: Vec<String> = Vec::new();
    let line_len = |n: &ANodeData| n.span.end_line.saturating_sub(n.span.start_line);
    if line_len(base) != line_len(current) {
        reasons.push("spanLines".to_string());
    }
    // Byte lengths only when both states carry real ranges — a pre-v5 base
    // (degraded 0..0) against a v5 current is a schema artifact, not an edit.
    if !base.span.byte_range.is_empty()
        && !current.span.byte_range.is_empty()
        && base.span.byte_range.len() != current.span.byte_range.len()
    {
        reasons.push("byteLength".to_string());
    }
    for (key, label) in [
        ("signature", "signature"),
        ("fields", "fields"),
        ("variants", "variants"),
        ("accessed_fields", "accessedFields"),
        ("async", "async"),
        ("exported", "exported"),
    ] {
        if base.metadata.get(key) != current.metadata.get(key) {
            reasons.push(label.to_string());
        }
    }
    if base.visibility != current.visibility {
        reasons.push("visibility".to_string());
    }
    reasons
}

/// All non-placeholder node ids of a graph, with their data.
fn diffable_nodes(graph: &AnalysisGraph) -> HashMap<&ANodeId, &ANodeData> {
    graph
        .all_node_ids()
        .into_iter()
        .filter_map(|id| {
            let node = graph.get_node(id)?;
            if is_placeholder(node) {
                return None;
            }
            Some((id, node))
        })
        .collect()
}

/// Distinct edge triples of a graph, keyed for set comparison. The kind key
/// uses the engine's `Debug` form so `UnresolvedCall("name")` edges to
/// different names stay distinct; placeholder-anchored unresolved calls are
/// included (an added call to an unknown function is a real delta).
fn edge_set(graph: &AnalysisGraph) -> HashSet<(ANodeId, ANodeId, String)> {
    let mut set = HashSet::new();
    for id in graph.all_node_ids() {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        if is_placeholder(node) {
            continue;
        }
        for (target, edge) in graph.get_edges_from(id) {
            set.insert((id.clone(), target.clone(), format!("{:?}", edge.kind)));
        }
    }
    set
}

/// Human/JSON label for an edge-set kind key (`Debug` form → camelCase).
fn edge_key_label(kind_key: &str) -> String {
    match kind_key.split('(').next().unwrap_or(kind_key) {
        "Calls" => "calls".to_string(),
        "UnresolvedCall" => {
            // Surface the callee name: UnresolvedCall("foo") → unresolvedCall(foo)
            let name = kind_key
                .trim_start_matches("UnresolvedCall(\"")
                .trim_end_matches("\")");
            format!("unresolvedCall({name})")
        }
        "UsesType" => "usesType".to_string(),
        "References" => "references".to_string(),
        "Contains" => "contains".to_string(),
        "Implements" => "implements".to_string(),
        "ExternalCall" => "externalCall".to_string(),
        "Extends" => "extends".to_string(),
        "Returns" => "returns".to_string(),
        "TypeOf" => "typeOf".to_string(),
        other => other.to_string(),
    }
}

/// SCC clusters keyed by their sorted member ids, for set comparison.
fn cycle_keys(graph: &AnalysisGraph) -> HashMap<Vec<ANodeId>, Vec<ANodeId>> {
    analysis::find_mutual_recursion(graph)
        .into_iter()
        .map(|cluster| {
            let mut key = cluster.members.clone();
            key.sort();
            (key, cluster.members)
        })
        .collect()
}

/// Compare the current bridged graph against a base snapshot: nodes/edges
/// added/removed/changed, complexity deltas for changed functions,
/// newly-introduced cycles, and the impact set of the delta (incoming-edge
/// BFS from every delta node — added/changed walked in the current graph,
/// removed walked in the base).
///
/// `current_complexity` is the working tree's measurement
/// ([`measure_complexity_map`]); base complexity comes from the snapshot's
/// sidecar ([`BaseSnapshot::complexity`]) when a prior `analyze diff` wrote
/// one, and is honestly reported as unavailable otherwise.
pub fn diff_report(
    base: &BaseSnapshot,
    current: &AnalysisGraph,
    current_complexity: &HashMap<ANodeId, StoredComplexity>,
    impact_depth: usize,
    top: usize,
) -> DiffReport {
    let base_nodes = diffable_nodes(&base.graph);
    let current_nodes = diffable_nodes(current);

    let mut nodes_added: Vec<SymbolRef> = Vec::new();
    let mut nodes_removed: Vec<SymbolRef> = Vec::new();
    let mut nodes_changed: Vec<ChangedNode> = Vec::new();
    let mut delta_current_ids: Vec<ANodeId> = Vec::new(); // added + changed
    let mut delta_base_ids: Vec<ANodeId> = Vec::new(); // removed
    let mut changed_fn_ids: Vec<ANodeId> = Vec::new();

    for (id, node) in &current_nodes {
        match base_nodes.get(*id) {
            None => {
                nodes_added.push(symbol_ref(node));
                delta_current_ids.push((*id).clone());
                if node.kind == ANodeKind::Function {
                    changed_fn_ids.push((*id).clone());
                }
            }
            Some(base_node) => {
                let reasons = node_change_reasons(base_node, node);
                if !reasons.is_empty() {
                    nodes_changed.push(ChangedNode {
                        symbol: symbol_ref(node),
                        reasons,
                    });
                    delta_current_ids.push((*id).clone());
                    if node.kind == ANodeKind::Function {
                        changed_fn_ids.push((*id).clone());
                    }
                }
            }
        }
    }
    for (id, node) in &base_nodes {
        if !current_nodes.contains_key(*id) {
            nodes_removed.push(symbol_ref(node));
            delta_base_ids.push((*id).clone());
        }
    }
    nodes_added.sort();
    nodes_removed.sort();
    nodes_changed.sort_by(|a, b| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)));

    // --- Edges ---------------------------------------------------------------
    let base_edges = edge_set(&base.graph);
    let current_edges = edge_set(current);
    let render_edge = |graph_a: &AnalysisGraph,
                       graph_b: &AnalysisGraph,
                       (from, to, kind): &(ANodeId, ANodeId, String)|
     -> Option<DiffEdge> {
        let lookup = |id: &ANodeId| {
            graph_a
                .get_node(id)
                .or_else(|| graph_b.get_node(id))
                .map(|n| n.qualified_name.clone())
        };
        Some(DiffEdge {
            from: lookup(from)?,
            to: lookup(to)?,
            kind: edge_key_label(kind),
        })
    };
    let mut edges_added: Vec<DiffEdge> = current_edges
        .difference(&base_edges)
        .filter_map(|e| render_edge(current, &base.graph, e))
        .collect();
    let mut edges_removed: Vec<DiffEdge> = base_edges
        .difference(&current_edges)
        .filter_map(|e| render_edge(&base.graph, current, e))
        .collect();
    let edge_sort =
        |a: &DiffEdge, b: &DiffEdge| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind));
    edges_added.sort_by(edge_sort);
    edges_removed.sort_by(edge_sort);

    // --- Complexity deltas for changed/added functions ------------------------
    changed_fn_ids.sort();
    let mut changed_functions: Vec<ChangedFunctionDelta> = changed_fn_ids
        .iter()
        .filter_map(|id| {
            let node = current.get_node(id)?;
            let line_len = |n: &ANodeData| n.span.end_line.saturating_sub(n.span.start_line) + 1;
            let before = base.complexity.get(id);
            let after = current_complexity.get(id);
            let delta = |b: Option<u32>, a: Option<u32>| match (b, a) {
                (Some(b), Some(a)) => Some(i64::from(a) - i64::from(b)),
                _ => None,
            };
            Some(ChangedFunctionDelta {
                symbol: symbol_ref(node),
                lines_before: base.graph.get_node(id).map(line_len).unwrap_or(0),
                lines_after: line_len(node),
                cyclomatic_before: before.map(|c| c.cyclomatic),
                cyclomatic_after: after.map(|c| c.cyclomatic),
                cyclomatic_delta: delta(before.map(|c| c.cyclomatic), after.map(|c| c.cyclomatic)),
                cognitive_before: before.map(|c| c.cognitive),
                cognitive_after: after.map(|c| c.cognitive),
                cognitive_delta: delta(before.map(|c| c.cognitive), after.map(|c| c.cognitive)),
            })
        })
        .collect();
    changed_functions.sort_by(|a, b| symbol_sort_key(&a.symbol).cmp(&symbol_sort_key(&b.symbol)));

    // --- Newly-introduced cycles ----------------------------------------------
    let base_cycles = cycle_keys(&base.graph);
    let current_cycles = cycle_keys(current);
    let mut new_cycles: Vec<CycleSummary> = current_cycles
        .iter()
        .filter(|(key, _)| !base_cycles.contains_key(*key))
        .map(|(_, members)| {
            let mut refs: Vec<SymbolRef> = members
                .iter()
                .filter_map(|id| current.get_node(id))
                .map(symbol_ref)
                .collect();
            refs.sort();
            CycleSummary {
                size: refs.len(),
                kind: classify_cycle(&refs),
                members: refs,
            }
        })
        .collect();
    new_cycles.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.members.cmp(&b.members)));
    let resolved_cycle_count = base_cycles
        .keys()
        .filter(|key| !current_cycles.contains_key(*key))
        .count();

    // --- Impact set of the delta ----------------------------------------------
    let delta_ids: HashSet<&ANodeId> = delta_current_ids
        .iter()
        .chain(delta_base_ids.iter())
        .collect();
    let config = TraversalConfig {
        max_depth: impact_depth,
        max_nodes: DIFF_IMPACT_WALK_CAP,
        direction: TraversalDirection::Incoming,
        parallel: false,
    };
    let mut impacted: BTreeMap<ANodeId, SymbolRef> = BTreeMap::new();
    let mut walk_into = |graph: &AnalysisGraph, seed: &ANodeId| {
        for id in traverse(graph, seed, &config).nodes {
            if delta_ids.contains(&id) || impacted.contains_key(&id) {
                continue;
            }
            let Some(node) = graph.get_node(&id) else {
                continue;
            };
            if is_placeholder(node) {
                continue;
            }
            impacted.insert(id, symbol_ref(node));
        }
    };
    for id in &delta_current_ids {
        walk_into(current, id);
    }
    for id in &delta_base_ids {
        walk_into(&base.graph, id);
    }
    let mut impact_nodes: Vec<SymbolRef> = impacted.into_values().collect();
    impact_nodes.sort_by(|a, b| symbol_sort_key(a).cmp(&symbol_sort_key(b)));
    let impacted_count = impact_nodes.len();
    let impact_truncated = impact_nodes.len() > top;
    impact_nodes.truncate(top);

    // --- Counts, truncation, note ----------------------------------------------
    let nodes_added_count = nodes_added.len();
    let nodes_removed_count = nodes_removed.len();
    let nodes_changed_count = nodes_changed.len();
    let edges_added_count = edges_added.len();
    let edges_removed_count = edges_removed.len();
    let truncated = nodes_added_count > top
        || nodes_removed_count > top
        || nodes_changed_count > top
        || edges_added_count > top
        || edges_removed_count > top;
    nodes_added.truncate(top);
    nodes_removed.truncate(top);
    nodes_changed.truncate(top);
    edges_added.truncate(top);
    edges_removed.truncate(top);

    let mut note = "Change detection is structural: span/byte length, signature, carried \
                    fields/variants, visibility. Pure position shifts (code moved by edits \
                    elsewhere in the file) are not counted; a same-length in-place edit that \
                    alters none of those is invisible to the diff."
        .to_string();
    if base.complexity.is_empty() && !changed_functions.is_empty() {
        note.push_str(
            " Base complexity is unavailable (the base snapshot predates a diff run); this \
             run annotated the current snapshot, so the next `analyze diff` against it will \
             report full before/after deltas.",
        );
    }

    DiffReport {
        base: DiffBaseDescriptor {
            source: base.generation.as_str().to_string(),
            index_fingerprint: base.index_fingerprint.map(|fp| format!("{fp:016x}")),
        },
        nodes_added_count,
        nodes_removed_count,
        nodes_changed_count,
        nodes_added,
        nodes_removed,
        nodes_changed,
        edges_added_count,
        edges_removed_count,
        edges_added,
        edges_removed,
        truncated,
        changed_functions,
        new_cycle_count: new_cycles.len(),
        new_cycles,
        resolved_cycle_count,
        impact: DeltaImpact {
            depth: impact_depth,
            impacted_count,
            truncated: impact_truncated,
            nodes: impact_nodes,
        },
        note,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use codegraph_analysis::edges::EdgeData as AEdgeData;
    use codegraph_analysis::nodes::{Span as ASpan, Visibility as AVisibility};

    use super::*;

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

    #[test]
    fn forward_slice_walks_callees_and_backward_walks_callers() {
        let (graph, a, _b, c) = fixture();

        let fwd = slice_report(&graph, &a, SliceDirection::Forward, 10).unwrap();
        assert_eq!(fwd.direction, "forward");
        assert_eq!(fwd.size, 2, "a influences b and c");
        assert!(fwd.nodes.iter().any(|n| n.name == "c"));

        let bwd = slice_report(&graph, &c, SliceDirection::Backward, 10).unwrap();
        assert_eq!(bwd.size, 2, "c is affected by a and b");
        assert!(bwd.nodes.iter().any(|n| n.name == "a"));
        assert!(bwd.note.contains("call-graph"));
    }

    #[test]
    fn cycles_report_finds_mutual_recursion_only() {
        let (graph, _, _, _) = fixture();
        let report = cycles_report(&graph);
        assert_eq!(report.cycle_count, 1);
        assert_eq!(report.cycles[0].kind, "mutualRecursion");
        let names: Vec<&str> = report.cycles[0]
            .members
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, vec!["d", "e"]);
        assert_eq!(report.break_suggestions.len(), 1);
    }

    #[test]
    fn dominators_chain_back_to_entry() {
        let (graph, a, b, c) = fixture();
        let report = dominators_report(&graph, &a, 50).unwrap();
        assert_eq!(report.entry.name, "a");
        assert_eq!(report.analyzed, 2);
        let c_entry = report
            .nodes
            .iter()
            .find(|n| n.symbol.name == "c")
            .expect("c analyzed");
        assert_eq!(
            c_entry
                .immediate_dominator
                .as_ref()
                .map(|s| s.name.as_str()),
            Some("b"),
            "every path from a to c passes through b"
        );
        assert_eq!(c_entry.dominator_depth, 2);
        let _ = (b, c);
    }

    #[test]
    fn taint_report_annotates_call_hops() {
        let (graph, a, _b, c) = fixture();
        let report = taint_report(&graph, &a, &c, 8, 25).unwrap();
        assert_eq!(report.path_count, 1);
        let path = &report.paths[0];
        let names: Vec<&str> = path.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert_eq!(path.edge_kinds, vec!["calls", "calls"]);
        assert!(report.note.contains("dataflow IR"));
    }

    #[test]
    fn impact_report_groups_call_sites_by_file() {
        let (graph, _a, b, _c) = fixture();
        let report = impact_report(&graph, &b, Some("fn b(x: i32)")).unwrap();
        assert_eq!(report.new_signature, "fn b(x: i32)");
        assert_eq!(report.call_site_count, 1, "only a calls b");
        assert_eq!(report.tasks.len(), 1);
        assert_eq!(report.tasks[0].call_sites[0].caller, "a");
    }

    #[test]
    fn communities_report_is_deterministic_and_groups_call_clusters() {
        let (graph, _, _, _) = fixture();
        let one = communities_report(&graph, 8);
        let two = communities_report(&graph, 8);
        assert_eq!(one.community_count, two.community_count);
        assert_eq!(one.multi_member_count, two.multi_member_count);
        assert!(one.multi_member_count >= 1, "call clusters detected");
    }

    #[test]
    fn query_report_runs_pipe_dsl_over_call_edges() {
        let (graph, _a, _b, _c) = fixture();
        let report = query_report(&graph, r#"fn("a") | callees"#, 50, false).unwrap();
        assert_eq!(report.node_count, 1, "a's only callee is b");
        assert_eq!(report.nodes[0].name, "b");
        assert!(!report.truncated);
        assert!(report.why.is_none(), "why is opt-in");
    }

    #[test]
    fn query_report_why_records_seed_predecessor() {
        let (graph, _a, _b, _c) = fixture();
        let report = query_report(&graph, r#"fn("a") | callees"#, 50, true).unwrap();
        let why = report.why.expect("pipe queries are traceable");
        let entry = why
            .iter()
            .find(|w| w.symbol.name == "b")
            .expect("result node b is explained");
        assert!(
            entry
                .steps
                .iter()
                .any(|s| s.predecessors.iter().any(|p| p == "a")),
            "b's provenance references seed a: {why:?}"
        );
    }

    #[test]
    fn query_report_parse_error_quotes_bad_token() {
        let (graph, _, _, _) = fixture();
        let err = query_report(&graph, r#"fn("a") | bogus_op"#, 50, false).unwrap_err();
        assert!(err.contains("bogus_op"), "offending token quoted: {err}");
        assert!(err.contains("position"), "position included: {err}");
    }

    #[test]
    fn explain_report_fuses_depth_without_executing() {
        let report = explain_report(r#"fn("a") | callees | callees | callees"#).unwrap();
        assert_eq!(report.kind, "pipe");
        assert!(
            report.steps.iter().any(|s| s.contains("Depth(3)")),
            "depth fusion applied: {:?}",
            report.steps
        );
    }

    #[test]
    fn explain_report_classifies_aggregations_and_rejects_bad_queries() {
        let agg = explain_report(r#"count fn("a")"#).unwrap();
        assert_eq!(agg.kind, "aggregation");

        let err = explain_report(r#"fn("a") | bogus_op"#).unwrap_err();
        assert!(err.contains("bogus_op"), "offending token quoted: {err}");
    }

    #[test]
    fn report_envelope_serializes_camel_case_wire_shape() {
        let envelope = ReportEnvelope::new("cycles", serde_json::json!({"cycleCount": 1}));
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["schemaVersion"], REPORT_SCHEMA_VERSION);
        assert_eq!(value["kind"], "cycles");
        assert_eq!(value["data"]["cycleCount"], 1);
    }

    #[test]
    fn validate_report_judges_arity_changes_per_caller() {
        let (graph, _a, b, _c) = fixture();

        // Arity change: every direct caller flagged incompatible.
        let changed = validate_report(&graph, &b, 1, 2).unwrap();
        assert!(!changed.is_safe);
        assert_eq!(changed.incompatible.len(), 1, "only a calls b");
        assert_eq!(changed.incompatible[0].symbol.name, "a");
        assert_eq!(changed.call_sites.len(), 1);

        // Unchanged arity: safe, callers compatible.
        let unchanged = validate_report(&graph, &b, 2, 2).unwrap();
        assert!(unchanged.is_safe);
        assert_eq!(unchanged.compatible.len(), 1);
        assert!(unchanged.note.contains("call-graph"));
    }

    #[test]
    fn centrality_and_critical_run_over_call_fixture() {
        let (graph, _a, _b, _c) = fixture();

        let centrality = centrality_report(&graph, 3);
        assert_eq!(centrality.analyzed, 5);
        assert_eq!(centrality.nodes.len(), 3, "--top caps the listing");
        // Scores are sorted descending.
        for pair in centrality.nodes.windows(2) {
            assert!(pair[0].score >= pair[1].score);
        }

        // b is the only path between a and c → articulation node.
        let critical = critical_report(&graph, 10);
        assert!(
            critical.nodes.iter().any(|n| n.name == "b"),
            "b articulates a-c: {critical:?}"
        );
        assert!(critical.bridge_count >= 1, "a->b / b->c are bridges");
    }

    #[test]
    fn export_report_emits_dot_for_graph_and_subgraph() {
        let (graph, a, _b, _c) = fixture();

        let whole = export_report(&graph, None, 0).unwrap();
        assert_eq!(whole.scope, "graph");
        assert_eq!(whole.node_count, 5);
        assert!(whole.dot.starts_with("digraph"));
        assert!(whole.dot.contains("calls"));

        let sub = export_report(&graph, Some(&a), 1).unwrap();
        assert_eq!(sub.scope, "subgraph");
        assert!(sub.node_count >= 2, "a plus its 1-hop neighborhood");
        assert!(sub.dot.starts_with("digraph"));
    }

    #[test]
    fn taint_suggest_ranks_named_sources_and_sinks() {
        let mut graph = AnalysisGraph::new();
        let read = add_fn(&mut graph, "src/io.ts", "readUserInput", 1);
        let exec = add_fn(&mut graph, "src/db.ts", "execQuery", 1);
        let plain = add_fn(&mut graph, "src/x.ts", "tally", 1);
        add_call(&mut graph, &read, &exec, "src/io.ts");
        let _ = plain;

        let report = taint_suggest_report(&graph, 10);
        assert_eq!(report.source_count, 1);
        assert_eq!(report.sink_count, 1);
        assert_eq!(report.sources[0].symbol.name, "readUserInput");
        assert_eq!(report.sinks[0].symbol.name, "execQuery");
        assert_eq!(report.pairs.len(), 1);
        assert!(report.pairs[0].priority > 0.0);

        // No lexicon match at all → honest note, no panic.
        let mut empty = AnalysisGraph::new();
        add_fn(&mut empty, "src/x.ts", "tally", 1);
        let report = taint_suggest_report(&empty, 10);
        assert_eq!(report.pairs.len(), 0);
        assert!(report.note.contains("nothing to rank"));
    }

    #[test]
    fn boundaries_report_is_honestly_empty_over_metadata_free_graph() {
        let (mut graph, _, _, _) = fixture();
        let report = boundaries_report(&mut graph);
        assert_eq!(report.boundary_count, 0);
        assert!(report.note.contains("does not populate these keys"));
        assert_eq!(report.cross_language_calls.edges_emitted, 0);
    }

    #[test]
    fn capabilities_report_lists_all_six_with_cascades() {
        let report = capabilities_report();
        assert_eq!(report.capabilities.len(), 6);
        let validation = report
            .capabilities
            .iter()
            .find(|c| c.name == "virtualValidation")
            .unwrap();
        assert_eq!(
            validation.env_var,
            "CODEGRAPH_ANALYSIS_CAP_VIRTUAL_VALIDATION"
        );
        let call_graph = report
            .capabilities
            .iter()
            .find(|c| c.name == "callGraph")
            .unwrap();
        assert!(
            call_graph
                .disables
                .contains(&"virtualValidation".to_string()),
            "disabling callGraph cascades: {call_graph:?}"
        );
    }

    #[test]
    fn schema_text_returns_engine_schemas_and_rejects_unknown() {
        for kind in [
            "query_result",
            "entrypoint_summary",
            "context_result",
            "formatted_output",
        ] {
            let schema = schema_text(kind).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(schema).unwrap();
            assert!(parsed["title"].is_string(), "{kind} schema parses");
        }
        // Hyphens and case are normalized.
        assert!(schema_text("Query-Result").is_ok());

        let err = schema_text("bogus").unwrap_err();
        assert!(err.contains("known kinds"));
    }

    #[test]
    fn stats_report_counts_kinds_and_exact_reachability() {
        let (graph, _a, _b, _c) = fixture();
        let report = stats_report(&graph, true, 10);
        assert_eq!(report.node_count, 5);
        assert_eq!(report.nodes_by_kind.get("function"), Some(&5));
        assert_eq!(report.edges_by_kind.get("calls"), Some(&4));
        assert_eq!(report.file_count, 2);
        assert_eq!(report.placeholder_count, 0);

        let reach = report.reachability.expect("requested");
        assert_eq!(reach.method, "exact", "small graph gets exact numbers");
        let a_entry = reach
            .top
            .iter()
            .find(|e| e.symbol.name == "a")
            .expect("a listed");
        assert_eq!(a_entry.descendants, 2.0, "a reaches b and c");
        assert_eq!(a_entry.ancestors, 0.0);
        // d/e form a 2-cycle: each reaches the other.
        let d_entry = reach.top.iter().find(|e| e.symbol.name == "d").unwrap();
        assert_eq!(d_entry.descendants, 1.0);
        assert_eq!(d_entry.ancestors, 1.0);
    }

    #[test]
    fn co_change_report_is_honest_without_git_history() {
        let (graph, _a, _b, _c) = fixture();
        let tmp = tempfile::tempdir().unwrap();
        // Not a git repository → zero commits, honest note, exit-0 shape.
        let report = co_change_report(&graph, tmp.path(), None, 2, 100, 10);
        assert_eq!(report.commits_analyzed, 0);
        assert!(report.pairs.is_empty());
        assert!(report.note.contains("No git history"));
    }

    #[test]
    fn signature_type_params_extracts_generic_tokens() {
        assert_eq!(signature_type_params("(x: T) -> T"), vec!["T"]);
        assert_eq!(
            signature_type_params("(map: HashMap<K, V>) -> V"),
            vec!["K", "V"]
        );
        assert!(signature_type_params("(x: number): number").is_empty());
        assert!(signature_type_params("(s: &str) -> String").is_empty());
    }

    #[test]
    fn generics_report_reports_metadata_gap_honestly() {
        let (graph, _a, _b, _c) = fixture();
        let report = generics_report(&graph, None);
        assert_eq!(report.instantiation_count, 0);
        assert!(report.note.contains("does not populate"));
    }

    #[test]
    fn types_report_propagates_via_pass_manager() {
        let (mut graph, a, _b, _c) = fixture();
        let report = types_report(&mut graph, &a).unwrap().unwrap();
        assert_eq!(report.symbol.name, "a");
        // Fixture has no UsesType edges — honest empty, not an error.
        assert!(report.input_types.is_empty());
        assert!(report.note.contains("No concrete types"));
    }

    fn add_fn_span(
        graph: &mut AnalysisGraph,
        file: &str,
        name: &str,
        start: u32,
        end: u32,
    ) -> ANodeId {
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

    #[test]
    fn diff_report_finds_added_removed_changed_and_impact() {
        // Base: a → b → c, plus doomed.
        let mut base = AnalysisGraph::new();
        let a = add_fn_span(&mut base, "src/x.ts", "a", 1, 3);
        let b = add_fn_span(&mut base, "src/x.ts", "b", 5, 8);
        let c = add_fn_span(&mut base, "src/x.ts", "c", 10, 12);
        add_call(&mut base, &a, &b, "src/x.ts");
        add_call(&mut base, &b, &c, "src/x.ts");
        let doomed = add_fn_span(&mut base, "src/y.ts", "doomed", 1, 2);
        add_call(&mut base, &a, &doomed, "src/x.ts");

        // Current: b grew (5..8 → 5..11), c/a shifted but same length,
        // doomed removed, fresh added (called by c).
        let mut current = AnalysisGraph::new();
        let a2 = add_fn_span(&mut current, "src/x.ts", "a", 1, 3);
        let b2 = add_fn_span(&mut current, "src/x.ts", "b", 5, 11);
        let c2 = add_fn_span(&mut current, "src/x.ts", "c", 13, 15);
        add_call(&mut current, &a2, &b2, "src/x.ts");
        add_call(&mut current, &b2, &c2, "src/x.ts");
        let fresh = add_fn_span(&mut current, "src/x.ts", "fresh", 17, 18);
        add_call(&mut current, &c2, &fresh, "src/x.ts");

        let base = base_snapshot(base);
        let current_complexity = HashMap::from([(
            b2.clone(),
            StoredComplexity {
                cyclomatic: 4,
                cognitive: 3,
                max_nesting: 2,
            },
        )]);
        let report = diff_report(&base, &current, &current_complexity, 3, 50);

        assert_eq!(report.nodes_added_count, 1);
        assert_eq!(report.nodes_added[0].name, "fresh");
        assert_eq!(report.nodes_removed_count, 1);
        assert_eq!(report.nodes_removed[0].name, "doomed");
        // Exactly b changed — c moved but kept its length.
        assert_eq!(report.nodes_changed_count, 1);
        assert_eq!(report.nodes_changed[0].symbol.name, "b");
        assert_eq!(report.nodes_changed[0].reasons, vec!["spanLines"]);

        // Edge delta: a→doomed gone, c→fresh new.
        assert_eq!(report.edges_removed_count, 1);
        assert_eq!(report.edges_removed[0].to, "doomed");
        assert_eq!(report.edges_added_count, 1);
        assert_eq!(report.edges_added[0].from, "c");
        assert_eq!(report.edges_added[0].kind, "calls");

        // Changed/added functions carry complexity: after measured, before
        // honestly absent (no sidecar in the base).
        let b_delta = report
            .changed_functions
            .iter()
            .find(|f| f.symbol.name == "b")
            .expect("b listed");
        assert_eq!(b_delta.cyclomatic_after, Some(4));
        assert_eq!(b_delta.cyclomatic_before, None);
        assert_eq!(b_delta.cyclomatic_delta, None);
        assert_eq!(b_delta.lines_before, 4);
        assert_eq!(b_delta.lines_after, 7);
        assert!(report.note.contains("Base complexity is unavailable"));

        // Impact: b changed → a (its caller); fresh added → c, b, a;
        // doomed removed → a (in the base graph).
        let impacted: Vec<&str> = report
            .impact
            .nodes
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert!(impacted.contains(&"a"), "callers impacted: {impacted:?}");
        assert!(impacted.contains(&"c"), "fresh's caller impacted");
        assert!(
            !impacted.contains(&"b") && !impacted.contains(&"fresh"),
            "delta nodes are not their own impact: {impacted:?}"
        );
        assert!(report.new_cycles.is_empty());
        assert_eq!(report.base.source, "cache-prev");
    }

    #[test]
    fn diff_report_complexity_delta_when_base_sidecar_present() {
        let mut base_graph = AnalysisGraph::new();
        let b = add_fn_span(&mut base_graph, "src/x.ts", "b", 5, 8);
        let mut base = base_snapshot(base_graph);
        base.complexity.insert(
            b.clone(),
            StoredComplexity {
                cyclomatic: 2,
                cognitive: 1,
                max_nesting: 1,
            },
        );

        let mut current = AnalysisGraph::new();
        let b2 = add_fn_span(&mut current, "src/x.ts", "b", 5, 11);
        let current_complexity = HashMap::from([(
            b2,
            StoredComplexity {
                cyclomatic: 5,
                cognitive: 4,
                max_nesting: 2,
            },
        )]);
        let report = diff_report(&base, &current, &current_complexity, 3, 50);
        let delta = &report.changed_functions[0];
        assert_eq!(delta.cyclomatic_before, Some(2));
        assert_eq!(delta.cyclomatic_after, Some(5));
        assert_eq!(delta.cyclomatic_delta, Some(3));
        assert_eq!(delta.cognitive_delta, Some(3));
        assert!(!report.note.contains("Base complexity is unavailable"));
    }

    #[test]
    fn diff_report_surfaces_newly_introduced_cycles_only() {
        // Base already has d ↔ e; current adds g ↔ h.
        let mut base_graph = AnalysisGraph::new();
        let d = add_fn_span(&mut base_graph, "src/y.ts", "d", 1, 2);
        let e = add_fn_span(&mut base_graph, "src/y.ts", "e", 4, 5);
        add_call(&mut base_graph, &d, &e, "src/y.ts");
        add_call(&mut base_graph, &e, &d, "src/y.ts");

        let mut current = AnalysisGraph::new();
        let d2 = add_fn_span(&mut current, "src/y.ts", "d", 1, 2);
        let e2 = add_fn_span(&mut current, "src/y.ts", "e", 4, 5);
        add_call(&mut current, &d2, &e2, "src/y.ts");
        add_call(&mut current, &e2, &d2, "src/y.ts");
        let g = add_fn_span(&mut current, "src/z.ts", "g", 1, 2);
        let h = add_fn_span(&mut current, "src/z.ts", "h", 4, 5);
        add_call(&mut current, &g, &h, "src/z.ts");
        add_call(&mut current, &h, &g, "src/z.ts");

        let report = diff_report(&base_snapshot(base_graph), &current, &HashMap::new(), 3, 50);
        assert_eq!(report.new_cycle_count, 1, "only g↔h is new");
        let names: Vec<&str> = report.new_cycles[0]
            .members
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, vec!["g", "h"]);
        assert_eq!(report.resolved_cycle_count, 0);
    }

    #[test]
    fn diff_report_is_empty_for_identical_graphs() {
        let (graph, _a, _b, _c) = fixture();
        let (same, _, _, _) = fixture();
        let report = diff_report(&base_snapshot(same), &graph, &HashMap::new(), 3, 50);
        assert_eq!(report.nodes_added_count, 0);
        assert_eq!(report.nodes_removed_count, 0);
        assert_eq!(report.nodes_changed_count, 0);
        assert_eq!(report.edges_added_count, 0);
        assert_eq!(report.edges_removed_count, 0);
        assert!(report.changed_functions.is_empty());
        assert_eq!(report.impact.impacted_count, 0);
    }
}
