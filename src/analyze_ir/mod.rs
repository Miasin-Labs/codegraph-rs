//! IR-backed analyses over real byte ranges (gap-matrix Tier 2, items 14–16).
//!
//! Three capabilities of the `codegraph-analysis` engine that need on-disk
//! source (not just the bridged graph) become reachable here:
//!
//! - **`analyze cfg <symbol>`** ([`cfg_report`]) — per-function basic-block
//!   control-flow graphs (`cfg::build_cfg`), anchored by re-parsing the
//!   source file with the host grammars and locating the function at the
//!   node's recorded line/column — the same proven anchor pattern
//!   `analyze complexity` uses. CFG rules cover Rust, TypeScript/TSX,
//!   JavaScript/JSX, ArkTS, Python, Go, Java, C, C++, PHP, R, Solidity,
//!   Vyper, Move, Cairo, Sway, Fe,
//!   Nix, CFML/CFScript/CFQuery, and Erlang; other languages get an honest
//!   capability note, never a silent empty graph.
//! - **`analyze dataflow <symbol>`** ([`dataflow_report`]) — per-function
//!   dataflow facts (`dataflow::extract_dataflow`: params, returns,
//!   assignments, argument flows, mutations), same anchoring. Rules cover
//!   Rust, TypeScript/TSX, JavaScript/JSX, ArkTS, Python, Go, R, Solidity,
//!   Vyper, Move, Cairo, Sway, Fe,
//!   Nix, CFML/CFScript/CFQuery, and Erlang.
//! - **`analyze slice|taint --value-level`** ([`value_slice_report`],
//!   [`value_taint_report`]) — upgrades the call-graph oracle to the
//!   engine's interprocedural points-to oracle by lowering each function to
//!   dataflow IR over its **byte range** (host schema v5 stores tree-sitter
//!   byte offsets; the bridge carries them into analysis spans). When the
//!   index predates v5 (byte offsets NULL → spans degrade to `0..0`) the
//!   reports fall back to call-graph granularity and say exactly how to fix
//!   it (re-index), instead of returning a silently empty value slice.
//!
//! ## Why a host-side IR map builder?
//!
//! The engine's `ir_map::build_ir_map` reads `node.file_path` directly, but
//! the bridged graph stores **project-relative** paths — reading them only
//! works when the process cwd happens to be the project root. CLI commands
//! accept `--path`, so [`build_rooted_ir_map`] mirrors the engine driver
//! while resolving every file against the workspace root (and re-using the
//! host grammar registry, which covers TSX/JSX variants the engine's
//! extension routing does not).
//!
//! Every report is serde-serializable with the same stable camelCase JSON
//! conventions as [`crate::analyze`], and is wrapped by the CLI in the same
//! [`crate::analyze::ReportEnvelope`].

use std::collections::HashMap;
use std::path::Path;

use codegraph_analysis::cfg::{CfgBlockKind, CfgEdgeKind, build_cfg};
use codegraph_analysis::cfg_rules::CfgRules;
use codegraph_analysis::dataflow::{AssignSourceKind, extract_dataflow};
use codegraph_analysis::dataflow_rules::DataflowRules;
use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::ir::{IrFunction, lower_for_language};
use codegraph_analysis::nodes::{NodeData as ANodeData, NodeId as ANodeId, NodeKind as ANodeKind};
use codegraph_analysis::slicing::{PointsToOracle, backward_slice, forward_slice};
use codegraph_analysis::taint_v2::{TaintConfig, analyze as taint_analyze};
use serde::Serialize;
use tree_sitter::{Node as TsNode, Point, Tree};

use crate::analyze::{
    SliceDirection,
    SliceReport,
    SymbolRef,
    TaintPathSummary,
    TaintReport,
    edge_label_between,
    is_placeholder,
    slice_report,
    symbol_ref,
    symbol_sort_key,
    taint_report,
};
use crate::extraction::{create_parser, detect_language};
use crate::types::Language;

/// Human list of CFG-rule-covered languages, for capability notes.
const CFG_COVERED_LANGUAGES: &str = "Rust, TypeScript/TSX, JavaScript/JSX, ArkTS, Python, Go, Java, C, C++, PHP, R, Solidity, Vyper, Move, Cairo, Sway, Fe, Nix, CFML/CFScript/CFQuery, and Erlang";

/// Human list of dataflow-rule-covered languages, for capability notes.
const DATAFLOW_COVERED_LANGUAGES: &str = "Rust, TypeScript/TSX, JavaScript/JSX, ArkTS, Python, Go, R, Solidity, Vyper, Move, Cairo, Sway, Fe, Nix, CFML/CFScript/CFQuery, and Erlang";

/// Human list of IR-lowering-covered languages, for capability notes.
const IR_COVERED_LANGUAGES: &str = "Rust, Python, TypeScript/TSX, JavaScript/JSX, and Go";

// =============================================================================
// Language routing (host Language → engine rule ids)
// =============================================================================

/// Map a detected host language onto the engine's CFG-rule id
/// (`cfg_rules::CfgRules::for_language`).
fn cfg_lang_id(language: Language) -> Option<&'static str> {
    Some(match language {
        Language::Rust => "rust",
        Language::Typescript | Language::Tsx => "typescript",
        Language::Javascript | Language::Jsx => "javascript",
        Language::Arkts => "arkts",
        Language::Python => "python",
        Language::Go => "go",
        Language::Java => "java",
        Language::C => "c",
        Language::Cpp => "cpp",
        Language::Php => "php",
        Language::R => "r",
        Language::Solidity => "solidity",
        Language::Vyper => "vyper",
        Language::Move => "move",
        Language::Cairo => "cairo",
        Language::Sway => "sway",
        Language::Fe => "fe",
        Language::Nix => "nix",
        Language::Cfml => "cfml",
        Language::Cfscript => "cfscript",
        Language::Cfquery => "cfquery",
        Language::Erlang => "erlang",
        _ => return None,
    })
}

/// Map a detected host language onto the engine's dataflow-rule id
/// (`dataflow_rules::DataflowRules::for_language`).
fn dataflow_lang_id(language: Language) -> Option<&'static str> {
    Some(match language {
        Language::Rust => "rust",
        Language::Typescript | Language::Tsx => "typescript",
        Language::Javascript | Language::Jsx => "javascript",
        Language::Arkts => "arkts",
        Language::Python => "python",
        Language::Go => "go",
        Language::R => "r",
        Language::Solidity => "solidity",
        Language::Vyper => "vyper",
        Language::Move => "move",
        Language::Cairo => "cairo",
        Language::Sway => "sway",
        Language::Fe => "fe",
        Language::Nix => "nix",
        Language::Cfml => "cfml",
        Language::Cfscript => "cfscript",
        Language::Cfquery => "cfquery",
        Language::Erlang => "erlang",
        _ => return None,
    })
}

/// Map a detected host language onto the engine's IR-lowering id
/// (`ir::lower_for_language`).
fn ir_lang_id(language: Language) -> Option<&'static str> {
    Some(match language {
        Language::Rust => "rust",
        Language::Python => "python",
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx => "typescript",
        Language::Go => "go",
        _ => return None,
    })
}

// =============================================================================
// Source re-parse anchoring (the `analyze complexity` pattern)
// =============================================================================

/// A parsed source file (host grammar), cached per path so multiple
/// functions in one file reuse a single parse.
struct ParsedFile {
    tree: Tree,
    source: String,
    language: Language,
}

/// Read + parse one file with the host grammar for its detected language.
/// `rel_path` is resolved against `workspace_root` (absolute paths pass
/// through `Path::join` unchanged).
fn parse_source(workspace_root: &Path, rel_path: &Path) -> Option<ParsedFile> {
    let language = detect_language(&rel_path.to_string_lossy(), None);
    let source = std::fs::read_to_string(workspace_root.join(rel_path)).ok()?;
    let mut parser = create_parser(language)?;
    let tree = parser.parse(&source, None)?;
    Some(ParsedFile {
        tree,
        source,
        language,
    })
}

/// Walk up from the node at the function's recorded start position to the
/// nearest ancestor whose kind names a function per the language rules.
/// Point-based, like `analyze complexity` — the bridge always carries
/// line/column spans even when byte offsets are absent.
fn locate_function_node<'t>(
    root: TsNode<'t>,
    start_line: u32,
    start_col: u32,
    function_kinds: &[&str],
) -> Option<TsNode<'t>> {
    let point = Point {
        row: start_line.saturating_sub(1) as usize,
        column: start_col as usize,
    };
    let mut node = root.named_descendant_for_point_range(point, point)?;
    loop {
        if function_kinds.contains(&node.kind()) {
            return Some(node);
        }
        node = node.parent()?;
    }
}

/// tree-sitter node kinds denoting a function/method across the IR-covered
/// grammars (mirrors the engine's `ir_map` routing).
const IR_FUNCTION_NODE_KINDS: [&str; 6] = [
    "function_item",        // rust
    "function_definition",  // python
    "function_declaration", // go, js/ts
    "method_declaration",   // go, ts
    "method_definition",    // js/ts
    "arrow_function",       // js/ts
];

/// Find the function node covering `[start, end)` by byte range: descend to
/// the deepest named node containing the whole span, then climb to the
/// nearest function-like ancestor. Falls back to a function-like direct
/// child for wrapper nodes (export statements, decorated definitions) whose
/// recorded range covers the wrapper rather than the function itself.
fn find_covering_function_node<'t>(
    root: TsNode<'t>,
    start: usize,
    end: usize,
) -> Option<TsNode<'t>> {
    let mut node = root;
    loop {
        let mut cursor = node.walk();
        let child = node
            .named_children(&mut cursor)
            .find(|c| c.start_byte() <= start && c.end_byte() >= end);
        match child {
            Some(c) => node = c,
            None => break,
        }
    }
    let mut cur = Some(node);
    while let Some(n) = cur {
        if IR_FUNCTION_NODE_KINDS.contains(&n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    let mut cursor = node.walk();
    let fallback = node
        .named_children(&mut cursor)
        .find(|c| IR_FUNCTION_NODE_KINDS.contains(&c.kind()));
    fallback
}

// =============================================================================
// analyze cfg
// =============================================================================

/// One basic block of a function CFG.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CfgBlockSummary {
    pub id: u32,
    /// Builder label (`ENTRY`, `EXIT`, `if`, `loop_header`, `stmt`, ...).
    pub label: String,
    /// `entry`, `exit`, `normal`, `branch`, `loop`, or `exception`.
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// One typed edge between two basic blocks.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CfgEdgeSummary {
    pub from: u32,
    pub to: u32,
    /// `normal`, `branchTrue`, `branchFalse`, `loopBack`, `exception`,
    /// `break`, `continue`, or `return`.
    pub kind: String,
}

/// Result of [`cfg_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CfgReport {
    pub symbol: SymbolRef,
    pub language: String,
    /// True when a CFG was actually built; false rows carry `skipReason` +
    /// an honest `note` and empty block/edge lists.
    pub analyzed: bool,
    /// `notAFunction`, `placeholder`, `unsupportedLanguage`,
    /// `fileUnreadable`, `bodyNotLocated`, or `noBody`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub block_count: usize,
    pub edge_count: usize,
    pub blocks: Vec<CfgBlockSummary>,
    pub edges: Vec<CfgEdgeSummary>,
    pub note: String,
}

fn block_kind_label(kind: CfgBlockKind) -> &'static str {
    match kind {
        CfgBlockKind::Entry => "entry",
        CfgBlockKind::Exit => "exit",
        CfgBlockKind::Normal => "normal",
        CfgBlockKind::Branch => "branch",
        CfgBlockKind::Loop => "loop",
        CfgBlockKind::Exception => "exception",
    }
}

fn cfg_edge_kind_label(kind: CfgEdgeKind) -> &'static str {
    match kind {
        CfgEdgeKind::Normal => "normal",
        CfgEdgeKind::BranchTrue => "branchTrue",
        CfgEdgeKind::BranchFalse => "branchFalse",
        CfgEdgeKind::LoopBack => "loopBack",
        CfgEdgeKind::Exception => "exception",
        CfgEdgeKind::Break => "break",
        CfgEdgeKind::Continue => "continue",
        CfgEdgeKind::Return => "return",
    }
}

fn skipped_cfg(symbol: SymbolRef, language: String, reason: &str, note: String) -> CfgReport {
    CfgReport {
        symbol,
        language,
        analyzed: false,
        skip_reason: Some(reason.to_string()),
        block_count: 0,
        edge_count: 0,
        blocks: Vec::new(),
        edges: Vec::new(),
        note,
    }
}

/// Shared skip classification for cfg/dataflow: everything that can go
/// wrong before the engine analysis runs, with one honest note per reason.
/// `Ok` carries the located function node alongside its parsed file.
enum Anchor<'p> {
    Ok {
        parsed: &'p ParsedFile,
        lang_id: &'static str,
    },
    Skip {
        language: String,
        reason: &'static str,
        note: String,
    },
}

/// Resolve the language + parsed file for `node`, against the rule table
/// chosen by `lang_id_for` (cfg vs dataflow), classifying failures.
fn anchor_source<'p>(
    node: &ANodeData,
    workspace_root: &Path,
    cache: &'p mut Option<ParsedFile>,
    lang_id_for: fn(Language) -> Option<&'static str>,
    covered: &str,
    what: &str,
) -> Anchor<'p> {
    if node.kind != ANodeKind::Function {
        return Anchor::Skip {
            language: "unknown".to_string(),
            reason: "notAFunction",
            note: format!(
                "{what} is per-function; \"{}\" is a {:?} node with no function body to analyze.",
                node.name, node.kind
            ),
        };
    }
    if is_placeholder(node) {
        return Anchor::Skip {
            language: "unknown".to_string(),
            reason: "placeholder",
            note: format!(
                "\"{}\" is a placeholder anchoring an unresolved call — it has no source in \
                 this project to parse.",
                node.name
            ),
        };
    }
    let language = detect_language(&node.file_path.to_string_lossy(), None);
    let lang_name = language.as_str().to_string();
    if lang_id_for(language).is_none() {
        return Anchor::Skip {
            language: lang_name,
            reason: "unsupportedLanguage",
            note: format!(
                "{what} has rules for {covered}; \"{}\" is not covered yet, so nothing can be \
                 extracted for this function. This is a capability gap, not an empty result.",
                language.as_str()
            ),
        };
    }
    *cache = parse_source(workspace_root, &node.file_path);
    match cache {
        Some(parsed) => Anchor::Ok {
            lang_id: lang_id_for(parsed.language).expect("lang id checked above"),
            parsed,
        },
        None => Anchor::Skip {
            language: lang_name,
            reason: "fileUnreadable",
            note: format!(
                "Source file {} could not be read or parsed under the project root — the index \
                 may be stale (re-run \"codegraph sync\").",
                node.file_path.display()
            ),
        },
    }
}

/// Build the control-flow graph for `target` by re-parsing its source file
/// with the host grammars (engine entry points: `cfg::build_cfg` +
/// `cfg_rules::CfgRules::for_language`). Returns `None` only when `target`
/// is not in the graph; every other failure mode is an honest skip row.
pub fn cfg_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    target: &ANodeId,
) -> Option<CfgReport> {
    let node = graph.get_node(target)?;
    let symbol = symbol_ref(node);

    let mut cache: Option<ParsedFile> = None;
    let (parsed, lang_id) = match anchor_source(
        node,
        workspace_root,
        &mut cache,
        cfg_lang_id,
        CFG_COVERED_LANGUAGES,
        "CFG construction",
    ) {
        Anchor::Ok { parsed, lang_id } => (parsed, lang_id),
        Anchor::Skip {
            language,
            reason,
            note,
        } => return Some(skipped_cfg(symbol, language, reason, note)),
    };
    let lang_name = parsed.language.as_str().to_string();

    let rules = CfgRules::for_language(lang_id)?;
    let Some(fn_node) = locate_function_node(
        parsed.tree.root_node(),
        node.span.start_line,
        node.span.start_col,
        rules.function_nodes,
    ) else {
        return Some(skipped_cfg(
            symbol,
            lang_name,
            "bodyNotLocated",
            format!(
                "No function definition was found at the recorded position ({}:{}) — the file \
                 may have changed since indexing (re-run \"codegraph sync\").",
                node.file_path.display(),
                node.span.start_line
            ),
        ));
    };
    let Some(cfg) = build_cfg(fn_node, parsed.source.as_bytes(), lang_id) else {
        return Some(skipped_cfg(
            symbol,
            lang_name,
            "noBody",
            format!(
                "\"{}\" has no parseable body to build a control-flow graph from.",
                node.name
            ),
        ));
    };

    let blocks: Vec<CfgBlockSummary> = cfg
        .blocks
        .iter()
        .map(|b| CfgBlockSummary {
            id: b.id,
            label: b.label.clone(),
            kind: block_kind_label(b.kind).to_string(),
            start_line: b.start_line,
            end_line: b.end_line,
        })
        .collect();
    let edges: Vec<CfgEdgeSummary> = cfg
        .edges
        .iter()
        .map(|e| CfgEdgeSummary {
            from: e.from,
            to: e.to,
            kind: cfg_edge_kind_label(e.kind).to_string(),
        })
        .collect();

    Some(CfgReport {
        symbol,
        language: lang_name,
        analyzed: true,
        skip_reason: None,
        block_count: blocks.len(),
        edge_count: edges.len(),
        blocks,
        edges,
        note: "Basic blocks are line-anchored; edge kinds mark branch outcomes (branchTrue/\
               branchFalse), loop back-edges, break/continue jumps, exception edges, and \
               returns. Built by re-parsing the on-disk source, so it reflects the working \
               tree as of this run."
            .to_string(),
    })
}

// =============================================================================
// analyze dataflow
// =============================================================================

/// One function parameter.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataflowParamSummary {
    pub name: String,
    pub position: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_annotation: Option<String>,
    pub has_default: bool,
}

/// One return expression found in the body.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataflowReturnSummary {
    pub line: u32,
    pub expression: String,
}

/// One assignment / variable declaration.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataflowAssignmentSummary {
    pub target: String,
    /// `literal`, `param`, `callResult`, `fieldAccess`, or `other`.
    pub source_kind: String,
    pub line: u32,
}

/// A function parameter flowing directly into a callee argument.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataflowArgFlowSummary {
    pub callee: String,
    pub arg_position: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_param: Option<String>,
    pub line: u32,
}

/// A detected mutation (mutating method call on an identifier).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataflowMutationSummary {
    pub target: String,
    pub method: String,
    pub line: u32,
}

/// Result of [`dataflow_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataflowReport {
    pub symbol: SymbolRef,
    pub language: String,
    /// True when dataflow facts were extracted; false rows carry
    /// `skipReason` + an honest `note` and empty fact lists.
    pub analyzed: bool,
    /// Same reason vocabulary as [`CfgReport::skip_reason`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub params: Vec<DataflowParamSummary>,
    pub returns: Vec<DataflowReturnSummary>,
    pub assignments: Vec<DataflowAssignmentSummary>,
    pub arg_flows: Vec<DataflowArgFlowSummary>,
    pub mutations: Vec<DataflowMutationSummary>,
    pub note: String,
}

fn assign_source_kind_label(kind: AssignSourceKind) -> &'static str {
    match kind {
        AssignSourceKind::Literal => "literal",
        AssignSourceKind::Param => "param",
        AssignSourceKind::CallResult => "callResult",
        AssignSourceKind::FieldAccess => "fieldAccess",
        AssignSourceKind::Other => "other",
    }
}

fn skipped_dataflow(
    symbol: SymbolRef,
    language: String,
    reason: &str,
    note: String,
) -> DataflowReport {
    DataflowReport {
        symbol,
        language,
        analyzed: false,
        skip_reason: Some(reason.to_string()),
        params: Vec::new(),
        returns: Vec::new(),
        assignments: Vec::new(),
        arg_flows: Vec::new(),
        mutations: Vec::new(),
        note,
    }
}

/// Extract per-function dataflow facts for `target` by re-parsing its
/// source file with the host grammars (engine entry points:
/// `dataflow::extract_dataflow` + `dataflow_rules::DataflowRules`). Returns
/// `None` only when `target` is not in the graph; every other failure mode
/// is an honest skip row.
pub fn dataflow_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    target: &ANodeId,
) -> Option<DataflowReport> {
    let node = graph.get_node(target)?;
    let symbol = symbol_ref(node);

    let mut cache: Option<ParsedFile> = None;
    let (parsed, lang_id) = match anchor_source(
        node,
        workspace_root,
        &mut cache,
        dataflow_lang_id,
        DATAFLOW_COVERED_LANGUAGES,
        "Dataflow extraction",
    ) {
        Anchor::Ok { parsed, lang_id } => (parsed, lang_id),
        Anchor::Skip {
            language,
            reason,
            note,
        } => return Some(skipped_dataflow(symbol, language, reason, note)),
    };
    let lang_name = parsed.language.as_str().to_string();

    let rules = DataflowRules::for_language(lang_id)?;
    let Some(fn_node) = locate_function_node(
        parsed.tree.root_node(),
        node.span.start_line,
        node.span.start_col,
        rules.function_nodes,
    ) else {
        return Some(skipped_dataflow(
            symbol,
            lang_name,
            "bodyNotLocated",
            format!(
                "No function definition was found at the recorded position ({}:{}) — the file \
                 may have changed since indexing (re-run \"codegraph sync\").",
                node.file_path.display(),
                node.span.start_line
            ),
        ));
    };
    let Some(flow) = extract_dataflow(fn_node, parsed.source.as_bytes(), lang_id) else {
        return Some(skipped_dataflow(
            symbol,
            lang_name,
            "noBody",
            format!(
                "\"{}\" has no parseable body to extract dataflow from.",
                node.name
            ),
        ));
    };

    Some(DataflowReport {
        symbol,
        language: lang_name,
        analyzed: true,
        skip_reason: None,
        params: flow
            .params
            .iter()
            .map(|p| DataflowParamSummary {
                name: p.name.clone(),
                position: p.position,
                type_annotation: p.type_annotation.clone(),
                has_default: p.has_default,
            })
            .collect(),
        returns: flow
            .returns
            .iter()
            .map(|r| DataflowReturnSummary {
                line: r.line,
                expression: r.expression.clone(),
            })
            .collect(),
        assignments: flow
            .assignments
            .iter()
            .map(|a| DataflowAssignmentSummary {
                target: a.target.clone(),
                source_kind: assign_source_kind_label(a.source_kind).to_string(),
                line: a.line,
            })
            .collect(),
        arg_flows: flow
            .arg_flows
            .iter()
            .map(|f| DataflowArgFlowSummary {
                callee: f.callee.clone(),
                arg_position: f.arg_position,
                source_param: f.source_param.clone(),
                line: f.line,
            })
            .collect(),
        mutations: flow
            .mutations
            .iter()
            .map(|m| DataflowMutationSummary {
                target: m.target.clone(),
                method: m.method.clone(),
                line: m.line,
            })
            .collect(),
        note: "Defs are parameters and assignment targets; uses are returns, argument flows \
               into callees, and mutating method calls. Extracted intraprocedurally by \
               re-parsing the on-disk source, so it reflects the working tree as of this run."
            .to_string(),
    })
}

// =============================================================================
// Value-level oracle (slice/taint --value-level)
// =============================================================================

/// How much of the graph the IR lowering covered — embedded in value-level
/// reports so consumers can judge the oracle's reach.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IrCoverage {
    /// Non-placeholder `Function` nodes considered.
    pub functions_total: usize,
    /// Functions successfully lowered to dataflow IR.
    pub functions_lowered: usize,
    /// Functions whose index rows carry no byte offsets (indexed before
    /// schema v5) — re-indexing backfills them.
    pub functions_missing_byte_range: usize,
    /// Functions in languages without IR lowering.
    pub functions_unsupported_language: usize,
    /// Functions whose source file could not be read or parsed.
    pub functions_source_unavailable: usize,
    /// Functions whose definition could not be located at the recorded
    /// byte range (stale index) or whose body did not lower.
    pub functions_not_located: usize,
}

/// Build the interprocedural IR map for every `Function` node in the
/// bridged graph, resolving project-relative file paths against
/// `workspace_root` (see module docs for why the engine's own
/// `ir_map::build_ir_map` is not used directly). Functions without byte
/// ranges, in uncovered languages, or with unreadable sources are counted
/// in [`IrCoverage`] and simply get no IR entry.
pub fn build_rooted_ir_map(
    graph: &AnalysisGraph,
    workspace_root: &Path,
) -> (HashMap<ANodeId, IrFunction>, IrCoverage) {
    let mut out: HashMap<ANodeId, IrFunction> = HashMap::new();
    let mut coverage = IrCoverage::default();
    let mut file_cache: HashMap<String, Option<ParsedFile>> = HashMap::new();

    for node in graph.nodes_by_kind(ANodeKind::Function) {
        if is_placeholder(node) {
            continue;
        }
        coverage.functions_total += 1;

        let language = detect_language(&node.file_path.to_string_lossy(), None);
        let Some(lang_id) = ir_lang_id(language) else {
            coverage.functions_unsupported_language += 1;
            continue;
        };
        let range = node.span.byte_range.clone();
        if range.start == 0 && range.end == 0 {
            // The bridge's documented "unknown" value: the SQLite row carries
            // NULL byte offsets (pre-v5 index).
            coverage.functions_missing_byte_range += 1;
            continue;
        }

        let key = node.file_path.display().to_string();
        let parsed = file_cache
            .entry(key)
            .or_insert_with(|| parse_source(workspace_root, &node.file_path));
        let Some(parsed) = parsed.as_ref() else {
            coverage.functions_source_unavailable += 1;
            continue;
        };
        let Some(fn_node) =
            find_covering_function_node(parsed.tree.root_node(), range.start, range.end)
        else {
            coverage.functions_not_located += 1;
            continue;
        };
        match lower_for_language(lang_id, fn_node, &parsed.source) {
            Some(ir) => {
                out.insert(node.id.clone(), ir);
                coverage.functions_lowered += 1;
            }
            None => coverage.functions_not_located += 1,
        }
    }
    (out, coverage)
}

/// The honest note for a value-level request that could not build any IR —
/// names the dominant cause and the fix, and states the fallback.
fn value_level_unavailable_note(coverage: &IrCoverage) -> String {
    let fallback = "Falling back to call-graph granularity (hops follow resolved and \
                    unresolved call edges).";
    if coverage.functions_missing_byte_range > 0 {
        format!(
            "Value-level precision requested, but {} of {} indexed functions carry no byte \
             offsets — this index predates schema v5. Re-index the project (\"codegraph \
             index\") to store byte offsets and enable value-level analysis. {fallback}",
            coverage.functions_missing_byte_range, coverage.functions_total
        )
    } else if coverage.functions_total > 0
        && coverage.functions_unsupported_language == coverage.functions_total
    {
        format!(
            "Value-level precision requested, but IR lowering covers {IR_COVERED_LANGUAGES}; \
             none of the {} indexed functions are in a covered language. {fallback}",
            coverage.functions_total
        )
    } else {
        format!(
            "Value-level precision requested, but no function could be lowered to dataflow IR \
             (source files unreadable or definitions not located — re-run \"codegraph sync\" \
             if the working tree changed since indexing). {fallback}"
        )
    }
}

/// Coverage caveats appended to every value-level note.
fn coverage_caveats(coverage: &IrCoverage) -> String {
    let mut out = String::new();
    if coverage.functions_missing_byte_range > 0 {
        out.push_str(&format!(
            " {} functions lack byte offsets (indexed before schema v5) and contribute no \
             value-level edges — re-index (\"codegraph index\") to include them.",
            coverage.functions_missing_byte_range
        ));
    }
    if coverage.functions_unsupported_language > 0 {
        out.push_str(&format!(
            " IR lowering covers {IR_COVERED_LANGUAGES}; {} functions in other languages \
             contribute no value-level edges.",
            coverage.functions_unsupported_language
        ));
    }
    out
}

/// Value-level program slice: [`crate::analyze::slice_report`]'s shape with
/// the oracle upgraded from call-graph adjacency to the engine's
/// interprocedural points-to analysis over per-function dataflow IR
/// (engine entry points: `slicing::{backward_slice, forward_slice}` over
/// `slicing::PointsToOracle` built from [`build_rooted_ir_map`]).
///
/// When no IR can be built at all (pre-v5 index without byte offsets,
/// uncovered languages), the report **falls back to call-graph granularity**
/// with a note saying exactly why and how to enable value-level — never a
/// silently empty value slice. Returns `None` if `seed` is not in the graph.
pub fn value_slice_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    seed: &ANodeId,
    direction: SliceDirection,
    max_depth: usize,
) -> Option<SliceReport> {
    let seed_node = graph.get_node(seed)?;
    let (ir_map, coverage) = build_rooted_ir_map(graph, workspace_root);

    if ir_map.is_empty() {
        let mut report = slice_report(graph, seed, direction, max_depth)?;
        report.note = value_level_unavailable_note(&coverage);
        report.ir_coverage = Some(coverage);
        return Some(report);
    }

    let oracle = PointsToOracle::build(graph, &ir_map);
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
    nodes.sort_by(|a, b| symbol_sort_key(a).cmp(&symbol_sort_key(b)));

    let mut note = format!(
        "Slice computed at value-level granularity: hops follow interprocedural alias/\
         points-to flow derived from per-function dataflow IR ({} of {} functions lowered). \
         Callers/callees with no value flow into this symbol are excluded — compare with the \
         default call-graph slice to see pure reachability.",
        coverage.functions_lowered, coverage.functions_total
    );
    if !ir_map.contains_key(seed) {
        note.push_str(
            " The seed itself has no dataflow IR (uncovered language, missing byte offsets, \
             or unlocatable definition), so no value-level edges originate from it.",
        );
    }
    note.push_str(&coverage_caveats(&coverage));

    Some(SliceReport {
        seed: symbol_ref(seed_node),
        direction: direction.as_str().to_string(),
        max_depth,
        granularity: "value-level".to_string(),
        size: nodes.len(),
        nodes,
        ir_coverage: Some(coverage),
        note,
    })
}

/// Value-level source→sink taint tracing: [`crate::analyze::taint_report`]'s
/// shape with the engine's interprocedural taint analysis
/// (`taint_v2::analyze`) running over the points-to oracle instead of raw
/// call-graph reachability. Reports the shortest dataflow path per
/// source→sink pair (the engine deduplicates pairs by BFS layer order).
///
/// Same fallback contract as [`value_slice_report`] when no IR can be
/// built. Returns `None` if either endpoint is not in the graph.
pub fn value_taint_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    source: &ANodeId,
    sink: &ANodeId,
    max_intermediate_nodes: usize,
    max_paths: usize,
) -> Option<TaintReport> {
    let source_node = graph.get_node(source)?;
    let sink_node = graph.get_node(sink)?;
    let (ir_map, coverage) = build_rooted_ir_map(graph, workspace_root);

    if ir_map.is_empty() {
        let mut report = taint_report(graph, source, sink, max_intermediate_nodes, max_paths)?;
        report.note = value_level_unavailable_note(&coverage);
        report.ir_coverage = Some(coverage);
        return Some(report);
    }

    let oracle = PointsToOracle::build(graph, &ir_map);
    let sources = [source.clone()];
    let sinks = [sink.clone()];
    let config = TaintConfig {
        sources: &sources,
        sinks: &sinks,
        sanitizers: &[],
    };
    let flows = taint_analyze(graph, &oracle, &config);

    let path_count = flows.len();
    let truncated = path_count > max_paths;
    let paths: Vec<TaintPathSummary> = flows
        .into_iter()
        .take(max_paths)
        .map(|flow| {
            let edge_kinds = flow
                .path
                .windows(2)
                .map(|pair| edge_label_between(graph, &pair[0], &pair[1]))
                .collect();
            let nodes = flow
                .path
                .iter()
                .filter_map(|id| graph.get_node(id))
                .map(symbol_ref)
                .collect();
            TaintPathSummary { nodes, edge_kinds }
        })
        .collect();

    let mut note = format!(
        "Source-to-sink flow traced at value-level granularity: hops follow interprocedural \
         alias/points-to flow derived from per-function dataflow IR ({} of {} functions \
         lowered); one shortest dataflow path is reported per source\u{2192}sink pair (the \
         intermediate-node cap applies to call-graph mode only).",
        coverage.functions_lowered, coverage.functions_total
    );
    if path_count == 0 {
        note.push_str(
            " No value-level flow was found — data from the source does not reach the sink \
             through tracked aliases. A call-graph path may still exist; rerun without \
             --value-level to check pure reachability.",
        );
    }
    note.push_str(&coverage_caveats(&coverage));

    Some(TaintReport {
        source: symbol_ref(source_node),
        sink: symbol_ref(sink_node),
        max_intermediate_nodes,
        granularity: "value-level".to_string(),
        path_count,
        truncated,
        paths,
        ir_coverage: Some(coverage),
        note,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as Map;
    use std::io::Write;
    use std::path::PathBuf;

    use codegraph_analysis::edges::{EdgeData as AEdgeData, EdgeKind as AEdgeKind};
    use codegraph_analysis::nodes::{Span as ASpan, Visibility as AVisibility};

    use super::*;
    use crate::analysis_bridge::UNRESOLVED_FILE;

    fn write_temp(dir: &Path, name: &str, content: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    /// A Function node anchored at `decl`'s position within `source` (both
    /// line/col and byte range), with a workspace-relative `rel_path`.
    fn fn_node(rel_path: &str, name: &str, source: &str, decl: &str) -> ANodeData {
        let start = source.find(decl).expect("decl present in source");
        let end = start + decl.len();
        let start_line = source[..start].matches('\n').count() as u32 + 1;
        let line_start = source[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let start_col = (start - line_start) as u32;
        ANodeData {
            id: ANodeId::new(rel_path, name, ANodeKind::Function),
            kind: ANodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: PathBuf::from(rel_path),
            span: ASpan {
                file: PathBuf::from(rel_path),
                start_line,
                start_col,
                end_line: start_line,
                end_col: 0,
                byte_range: start..end,
            },
            visibility: AVisibility::Public,
            metadata: Map::new(),
            birth_revision: 0,
            last_modified_revision: 0,
            complexity: None,
            cfg: None,
            dataflow: None,
        }
    }

    fn calls_edge(file: &str) -> AEdgeData {
        AEdgeData {
            kind: AEdgeKind::Calls,
            source_span: ASpan {
                file: PathBuf::from(file),
                start_line: 1,
                start_col: 0,
                end_line: 1,
                end_col: 0,
                byte_range: 0..0,
            },
            weight: 1.0,
        }
    }

    // Normal: a Rust function with a branch + loop yields entry/exit/branch/
    // loop blocks and typed edges.
    #[test]
    fn cfg_report_builds_blocks_for_rust_function() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "pub fn branchy(n: i32) -> i32 {\n    let mut total = 0;\n    for i in 0..n {\n        if i % 2 == 0 {\n            total += i;\n        }\n    }\n    total\n}\n";
        write_temp(tmp.path(), "branchy.rs", src);

        let mut graph = AnalysisGraph::new();
        let node = fn_node("branchy.rs", "branchy", src, "fn branchy");
        let id = graph.add_node(node);

        let report = cfg_report(&graph, tmp.path(), &id).expect("node in graph");
        assert!(report.analyzed, "report: {report:?}");
        assert_eq!(report.language, "rust");
        assert!(report.block_count >= 4, "report: {report:?}");
        assert_eq!(report.block_count, report.blocks.len());
        assert_eq!(report.blocks[0].kind, "entry");
        assert_eq!(report.blocks[1].kind, "exit");
        assert!(report.blocks.iter().any(|b| b.kind == "branch"));
        assert!(report.blocks.iter().any(|b| b.kind == "loop"));
        assert!(report.edges.iter().any(|e| e.kind == "loopBack"));
        assert!(report.edges.iter().any(|e| e.kind == "branchFalse"));
    }

    #[test]
    fn cfg_report_models_move_if_branches() {
        let tmp = tempfile::tempdir().unwrap();
        let source = "module 0x1::flow {\n    public fun choose(value: u64): u64 {\n        if (value > 0) {\n            helper(value)\n        } else {\n            fallback(value)\n        }\n    }\n}\n";
        write_temp(tmp.path(), "flow.move", source);

        let mut graph = AnalysisGraph::new();
        let id = graph.add_node(fn_node("flow.move", "choose", source, "public fun choose"));
        let report = cfg_report(&graph, tmp.path(), &id).expect("node in graph");

        assert!(report.analyzed, "report: {report:?}");
        assert!(report.blocks.iter().any(|block| block.kind == "branch"));
        assert!(report.blocks.iter().any(|block| block.label == "else"));
        assert!(report.edges.iter().any(|edge| edge.kind == "branchFalse"));
    }

    // Honesty: a language without CFG rules is an explicit capability note,
    // not an empty graph.
    #[test]
    fn cfg_report_notes_unsupported_language_honestly() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "def greet\n  puts 'hi'\nend\n";
        write_temp(tmp.path(), "greet.rb", src);

        let mut graph = AnalysisGraph::new();
        let id = graph.add_node(fn_node("greet.rb", "greet", src, "def greet"));

        let report = cfg_report(&graph, tmp.path(), &id).expect("node in graph");
        assert!(!report.analyzed);
        assert_eq!(report.skip_reason.as_deref(), Some("unsupportedLanguage"));
        assert_eq!(report.block_count, 0);
        assert!(
            report.note.contains("Rust, TypeScript/TSX"),
            "covered languages listed: {}",
            report.note
        );
    }

    // Honesty: placeholders (unresolved library calls) have no source.
    #[test]
    fn cfg_report_skips_placeholder_with_note() {
        let mut graph = AnalysisGraph::new();
        let mut node = fn_node("x.rs", "exec", "pub fn exec() {}", "fn exec");
        node.file_path = PathBuf::from(UNRESOLVED_FILE);
        node.span.file = PathBuf::from(UNRESOLVED_FILE);
        node.id = ANodeId::new(UNRESOLVED_FILE, "exec", ANodeKind::Function);
        let id = graph.add_node(node);

        let report = cfg_report(&graph, Path::new("/nonexistent"), &id).unwrap();
        assert!(!report.analyzed);
        assert_eq!(report.skip_reason.as_deref(), Some("placeholder"));
        assert!(report.note.contains("unresolved call"));
    }

    // Normal: dataflow extraction returns defs (params, assignments) and
    // uses (returns, arg flows).
    #[test]
    fn dataflow_report_extracts_defs_and_uses() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "pub fn shape(x: i32) -> i32 {\n    let doubled = x * 2;\n    helper(x);\n    return doubled;\n}\npub fn helper(v: i32) {}\n";
        write_temp(tmp.path(), "shape.rs", src);

        let mut graph = AnalysisGraph::new();
        let id = graph.add_node(fn_node("shape.rs", "shape", src, "fn shape"));

        let report = dataflow_report(&graph, tmp.path(), &id).expect("node in graph");
        assert!(report.analyzed, "report: {report:?}");
        assert_eq!(report.params.len(), 1);
        assert_eq!(report.params[0].name, "x");
        assert_eq!(report.params[0].type_annotation.as_deref(), Some("i32"));
        assert!(
            report.assignments.iter().any(|a| a.target == "doubled"),
            "assignments: {:?}",
            report.assignments
        );
        assert!(!report.returns.is_empty(), "returns: {:?}", report.returns);
        assert!(
            report
                .arg_flows
                .iter()
                .any(|f| f.callee == "helper" && f.source_param.as_deref() == Some("x")),
            "arg flows: {:?}",
            report.arg_flows
        );
    }

    #[test]
    fn dataflow_report_handles_web3_grammar_shapes() {
        let cases = [
            (
                "flow.vy",
                "def relay(value: uint256) -> uint256:\n    return helper(value)\n",
                "def relay",
            ),
            (
                "flow.move",
                "module 0x1::flow {\n    public fun relay(value: u64): u64 {\n        helper(value)\n    }\n}\n",
                "public fun relay",
            ),
            (
                "flow.cairo",
                "fn relay(value: felt252) -> felt252 {\n    helper(value)\n}\n",
                "fn relay",
            ),
            (
                "flow.sw",
                "script;\nfn relay(value: u64) -> u64 {\n    helper(value)\n}\n",
                "fn relay",
            ),
            (
                "flow.fe",
                "pub fn relay(value: u256) -> u256 {\n    return helper(value)\n}\n",
                "pub fn relay",
            ),
        ];

        for (file, source, declaration) in cases {
            let tmp = tempfile::tempdir().unwrap();
            write_temp(tmp.path(), file, source);

            let mut graph = AnalysisGraph::new();
            let id = graph.add_node(fn_node(file, "relay", source, declaration));
            let report = dataflow_report(&graph, tmp.path(), &id).expect("node in graph");

            assert!(report.analyzed, "{file}: {report:?}");
            assert_eq!(report.params.len(), 1, "{file}: {report:?}");
            assert_eq!(report.params[0].name, "value", "{file}: {report:?}");
            assert!(
                report.arg_flows.iter().any(|flow| {
                    flow.callee == "helper" && flow.source_param.as_deref() == Some("value")
                }),
                "{file}: {report:?}"
            );
        }
    }

    // Honesty: dataflow rules cover fewer languages than CFG rules — Java is
    // CFG-covered but dataflow-uncovered.
    #[test]
    fn dataflow_report_notes_uncovered_language() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "class A { int f(int x) { return x; } }\n";
        write_temp(tmp.path(), "A.java", src);

        let mut graph = AnalysisGraph::new();
        let id = graph.add_node(fn_node("A.java", "f", src, "int f"));

        let report = dataflow_report(&graph, tmp.path(), &id).unwrap();
        assert!(!report.analyzed);
        assert_eq!(report.skip_reason.as_deref(), Some("unsupportedLanguage"));
        assert!(report.note.contains("Rust, TypeScript/TSX"));
    }

    /// Three-function fixture on disk: `passes_data` forwards its parameter
    /// into `target_fn`; `calls_no_data` calls it with a literal only.
    fn value_fixture(tmp: &Path) -> (AnalysisGraph, ANodeId, ANodeId, ANodeId) {
        let src = "pub fn target_fn(x: i32) -> i32 {\n    x + 1\n}\n\npub fn passes_data(v: i32) -> i32 {\n    let result = target_fn(v);\n    result\n}\n\npub fn calls_no_data() {\n    target_fn(1);\n}\n";
        write_temp(tmp, "flow.rs", src);

        let mut graph = AnalysisGraph::new();
        let target = graph.add_node(fn_node("flow.rs", "target_fn", src, "fn target_fn"));
        let passes = graph.add_node(fn_node("flow.rs", "passes_data", src, "fn passes_data"));
        let no_data = graph.add_node(fn_node("flow.rs", "calls_no_data", src, "fn calls_no_data"));
        graph
            .add_edge(&passes, &target, calls_edge("flow.rs"))
            .unwrap();
        graph
            .add_edge(&no_data, &target, calls_edge("flow.rs"))
            .unwrap();
        (graph, target, passes, no_data)
    }

    // The headline behavior: value-level slicing excludes the caller that
    // passes no data, while the call-graph slice includes both callers.
    #[test]
    fn value_slice_differs_from_call_graph_slice() {
        let tmp = tempfile::tempdir().unwrap();
        let (graph, target, _passes, _no_data) = value_fixture(tmp.path());

        let call_level = slice_report(&graph, &target, SliceDirection::Backward, 10).unwrap();
        let call_names: Vec<&str> = call_level.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(call_names.contains(&"passes_data"));
        assert!(
            call_names.contains(&"calls_no_data"),
            "call-graph slice includes every caller: {call_names:?}"
        );

        let value_level =
            value_slice_report(&graph, tmp.path(), &target, SliceDirection::Backward, 10).unwrap();
        assert_eq!(value_level.granularity, "value-level");
        let value_names: Vec<&str> = value_level.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            value_names.contains(&"passes_data"),
            "alias flow caller kept: {value_names:?}"
        );
        assert!(
            !value_names.contains(&"calls_no_data"),
            "literal-only caller excluded at value level: {value_names:?}"
        );
        let coverage = value_level.ir_coverage.expect("coverage embedded");
        assert_eq!(coverage.functions_total, 3);
        assert_eq!(coverage.functions_lowered, 3);
        assert!(value_level.note.contains("value-level"));
    }

    // Honesty: pre-v5-style spans (byte_range 0..0 everywhere) fall back to
    // call-graph granularity with the re-index note — never a silently
    // empty value slice.
    #[test]
    fn value_slice_pre_v5_byte_ranges_fall_back_with_reindex_note() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = value_fixture(tmp.path()); // writes flow.rs
        // Same shape, but with pre-v5-style degraded spans (byte_range 0..0).
        let graph = {
            let src = std::fs::read_to_string(tmp.path().join("flow.rs")).unwrap();
            let mut g = AnalysisGraph::new();
            let mut degraded = |name: &str, decl: &str| {
                let mut node = fn_node("flow.rs", name, &src, decl);
                node.span.byte_range = 0..0;
                g.add_node(node)
            };
            let target = degraded("target_fn", "fn target_fn");
            let passes = degraded("passes_data", "fn passes_data");
            let no_data = degraded("calls_no_data", "fn calls_no_data");
            g.add_edge(&passes, &target, calls_edge("flow.rs")).unwrap();
            g.add_edge(&no_data, &target, calls_edge("flow.rs"))
                .unwrap();
            g
        };
        let target = ANodeId::new("flow.rs", "target_fn", ANodeKind::Function);

        let report =
            value_slice_report(&graph, tmp.path(), &target, SliceDirection::Backward, 10).unwrap();
        assert_eq!(
            report.granularity, "call-graph",
            "degrades honestly: {report:?}"
        );
        assert!(
            report.note.contains("Re-index") || report.note.contains("re-index"),
            "re-index note present: {}",
            report.note
        );
        assert!(report.note.contains("byte offsets"));
        let coverage = report.ir_coverage.expect("coverage embedded");
        assert_eq!(coverage.functions_missing_byte_range, 3);
        assert_eq!(coverage.functions_lowered, 0);
        // The call-graph fallback still finds both callers.
        let names: Vec<&str> = report.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"passes_data") && names.contains(&"calls_no_data"));
    }

    // Value-level taint connects source→sink along real value flow and is
    // honest when there is none.
    #[test]
    fn value_taint_traces_alias_flow_and_reports_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let (graph, target, passes, no_data) = value_fixture(tmp.path());

        let flow = value_taint_report(&graph, tmp.path(), &passes, &target, 8, 25).unwrap();
        assert_eq!(flow.granularity, "value-level");
        assert_eq!(flow.path_count, 1, "report: {flow:?}");
        let names: Vec<&str> = flow.paths[0]
            .nodes
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(names, vec!["passes_data", "target_fn"]);
        assert_eq!(flow.paths[0].edge_kinds, vec!["calls"]);

        // calls_no_data passes only a literal — no value-level flow.
        let dry = value_taint_report(&graph, tmp.path(), &no_data, &target, 8, 25).unwrap();
        assert_eq!(dry.path_count, 0);
        assert!(
            dry.note.contains("No value-level flow"),
            "honest absence note: {}",
            dry.note
        );
    }

    #[test]
    fn lang_id_routing_matches_engine_rule_tables() {
        // Every cfg id resolves to engine rules.
        for lang in [
            Language::Rust,
            Language::Typescript,
            Language::Tsx,
            Language::Javascript,
            Language::Jsx,
            Language::Arkts,
            Language::Python,
            Language::Go,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::Php,
            Language::R,
            Language::Solidity,
            Language::Vyper,
            Language::Move,
            Language::Cairo,
            Language::Sway,
            Language::Fe,
            Language::Nix,
            Language::Cfml,
            Language::Cfscript,
            Language::Cfquery,
            Language::Erlang,
        ] {
            let id = cfg_lang_id(lang).expect("cfg-covered");
            assert!(CfgRules::for_language(id).is_some(), "cfg rules for {id}");
        }
        // Every dataflow id resolves to engine rules.
        for lang in [
            Language::Rust,
            Language::Typescript,
            Language::Javascript,
            Language::Arkts,
            Language::Python,
            Language::Go,
            Language::R,
            Language::Solidity,
            Language::Vyper,
            Language::Move,
            Language::Cairo,
            Language::Sway,
            Language::Fe,
            Language::Nix,
            Language::Cfml,
            Language::Cfscript,
            Language::Cfquery,
            Language::Erlang,
        ] {
            let id = dataflow_lang_id(lang).expect("dataflow-covered");
            assert!(
                DataflowRules::for_language(id).is_some(),
                "dataflow rules for {id}"
            );
        }
        // Uncovered languages return None for both (honesty gate).
        assert!(cfg_lang_id(Language::Ruby).is_none());
        assert!(dataflow_lang_id(Language::Java).is_none());
        assert!(ir_lang_id(Language::Cpp).is_none());
    }
}
