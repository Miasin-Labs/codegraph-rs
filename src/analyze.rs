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
//!   in its `note` field instead of pretending otherwise.
//! - **communities**, **dominators**, **cycles**, and **impact** are pure
//!   graph algorithms and run at full fidelity over the bridged graph.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use codegraph_analysis::analysis;
use codegraph_analysis::cascade::generate_cascade;
use codegraph_analysis::communities::louvain;
use codegraph_analysis::complexity::compute_complexity;
use codegraph_analysis::complexity_rules::LangRules;
use codegraph_analysis::dsl::aggregate::{AggExpr, parse_aggregate};
use codegraph_analysis::dsl::plan::{ScheduleStrategy, optimise_expr, pick_schedule_for_pipe};
use codegraph_analysis::dsl::provenance::trace_query;
use codegraph_analysis::dsl::{
    Expr,
    QueryConfig as DslQueryConfig,
    QueryError as DslQueryError,
    parse_expr,
    run_query_expr,
};
use codegraph_analysis::edges::EdgeKind as AEdgeKind;
use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::nodes::{NodeData as ANodeData, NodeId as ANodeId, NodeKind as ANodeKind};
use codegraph_analysis::slicing::{DataflowOracle, backward_slice, forward_slice};
use codegraph_analysis::traversal::{TraversalConfig, TraversalDirection, traverse};
use serde::Serialize;
use tree_sitter::{Node as TsNode, Point, Tree};

use crate::analysis_bridge::UNRESOLVED_FILE;
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

fn symbol_ref(node: &ANodeData) -> SymbolRef {
    SymbolRef {
        name: node.name.clone(),
        qualified_name: node.qualified_name.clone(),
        kind: kind_label(node.kind),
        file: node.file_path.display().to_string(),
        line: node.span.start_line,
    }
}

fn is_placeholder(node: &ANodeData) -> bool {
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
pub fn dominators_report(
    graph: &AnalysisGraph,
    entry: &ANodeId,
    limit: usize,
) -> Option<DominatorsReport> {
    let entry_node = graph.get_node(entry)?;

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
        let Some(chain) = analysis::dominator_chain(graph, entry, id) else {
            continue;
        };
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

/// Result of [`slice_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceReport {
    pub seed: SymbolRef,
    pub direction: String,
    pub max_depth: usize,
    /// Always `"call-graph"` over the SQLite bridge — see `note`.
    pub granularity: String,
    /// Slice size excluding the seed.
    pub size: usize,
    pub nodes: Vec<SymbolRef>,
    pub note: String,
}

/// Capability note shared by slice and taint reports.
fn call_graph_granularity_note(what: &str) -> String {
    format!(
        "{what} computed at call-graph granularity (hops follow resolved and \
         unresolved call edges). Value-level def-use precision requires the \
         analysis engine's per-function dataflow IR, which the SQLite bridge \
         does not carry (upstream it is produced by the Rust source adapter \
         only)."
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

/// Result of [`taint_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaintReport {
    pub source: SymbolRef,
    pub sink: SymbolRef,
    pub max_intermediate_nodes: usize,
    /// Always `"call-graph"` over the SQLite bridge — see `note`.
    pub granularity: String,
    /// Total simple paths found (before capping `paths`).
    pub path_count: usize,
    pub truncated: bool,
    pub paths: Vec<TaintPathSummary>,
    pub note: String,
}

fn edge_label_between(graph: &AnalysisGraph, from: &ANodeId, to: &ANodeId) -> String {
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
        note: call_graph_granularity_note("Source-to-sink paths"),
    })
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

fn symbol_sort_key(s: &SymbolRef) -> (&String, u32, &String) {
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

/// Run a DSL query over the bridged graph through the engine's unified
/// entry point (`run_query_expr`: pipe chains, set algebra, path patterns,
/// entrypoint/dominator selectors, and aggregations), including the plan
/// optimiser. Parse errors come back as the parser's own message (position
/// + offending token) so the CLI can show them verbatim.
pub fn query_report(
    graph: &AnalysisGraph,
    query: &str,
    max_nodes: usize,
    include_why: bool,
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
}
