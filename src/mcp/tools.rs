//! MCP Tool Definitions
//!
//! Defines the tools exposed by the CodeGraph MCP server.
//! Port of `src/mcp/tools.ts`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::codegraph::CodeGraph;
use crate::directory::find_nearest_codegraph_root;
use crate::error::{CodeGraphError, Result};
use crate::extraction::is_generated_file;
use crate::search::is_test_file;
use crate::sync::PendingFile;
use crate::sync::worktree::{
    WorktreeIndexMismatch,
    detect_worktree_index_mismatch,
    worktree_mismatch_notice,
    worktree_mismatch_warning,
};
use crate::types::{
    Edge,
    EdgeKind,
    FindRelevantContextOptions,
    Node,
    NodeKind,
    Provenance,
    SearchOptions,
    SearchResult,
};
use crate::utils::{clamp, lexical_resolve, validate_path_within_root, validate_project_path};

/// Maximum output length to prevent context bloat (characters)
const MAX_OUTPUT_LENGTH: usize = 15000;

/// Maximum length for free-form string inputs (query, task, symbol).
/// Bounds memory and CPU when a buggy or hostile MCP client sends a
/// huge payload — without this an attacker could ship a 100MB string
/// and force a full FTS5 scan / OOM the server. 10 000 characters is
/// far beyond any realistic legitimate query.
const MAX_INPUT_LENGTH: usize = 10_000;

/// Maximum length for path-like string inputs (projectPath, path
/// filter, glob pattern). Paths beyond a few thousand chars are
/// never legitimate and signal abuse or a bug upstream.
const MAX_PATH_LENGTH: usize = 4_096;

/// Rust path roots that have no file-system equivalent — `crate` is the
/// current crate, `super` is the parent module, `self` is the current
/// module. Used by `matches_symbol` to strip these before file-path
/// matching so `crate::configurator::stage_apply::run` resolves the
/// same as `configurator::stage_apply::run`.
const RUST_PATH_PREFIXES: [&str; 3] = ["crate", "super", "self"];

/// Node kinds that contain other symbols. For these, `codegraph_node` with
/// `includeCode=true` returns a structural outline (member names + signatures +
/// line numbers) instead of the full body, which for a large class is a
/// multi-thousand-character wall of source that bloats the agent's context.
fn is_container_node_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
            | NodeKind::Namespace
            | NodeKind::Module
    )
}

/// Callable kinds — TS also lists `'constructor'`, which is not a `NodeKind`
/// in either implementation (dead-letter entry kept out of the Rust enum).
fn is_callable_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::Component
    )
}

// =============================================================================
// Shared regexes (JS regex literals → compiled once)
// =============================================================================

static QUALIFIER_SPLIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"::|[./]").unwrap());
static QUAL_DOT_SPLIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"::|\.").unwrap());
static TOKEN_SPLIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s,()\[\]]+").unwrap());
static FILE_EXT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(?:java|kt|kts|ts|tsx|js|jsx|mjs|cjs|cs|py|go|rb|php|swift|rs|cpp|cc|cxx|c|h|hpp|scala|lua|dart|vue|svelte)$").unwrap()
});
static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*(?:(?:::|\.)[A-Za-z0-9_$]+)*$").unwrap()
});
static TYPE_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][A-Za-z0-9]{3,}").unwrap());
static TEST_PATH_DIR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|/)(tests?|specs?|__tests__|testdata|mocks?|fixtures?)/").unwrap()
});
static TEST_PATH_EXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(test|spec)\.[a-z]+$").unwrap());
static QUERY_MENTIONS_TESTS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(test|tests|testing|spec|verify|verifies)\b").unwrap());
static EXT_STRIP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.[^.]+$").unwrap());
static LEADING_DOT_SLASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\.?/+)+").unwrap());
static LOW_VALUE_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"/(tests?|__tests?__|spec)/",
        r"_test\.go$",
        r"(?:^|/)test_[^/]+\.py$",
        r"_test\.py$",
        r"_spec\.rb$",
        r"_test\.rb$",
        r"\.(test|spec)\.[jt]sx?$",
        r"(test|spec|tests)\.(java|kt|scala)$",
        r"(tests?|spec)\.cs$",
        r"tests?\.swift$",
        r"_test\.dart$",
        r"\bicons?\b",
        r"\bi18n\b",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

/// Last `::` / `.` / `/`-separated segment of a qualified symbol.
fn last_qualifier_part(symbol: &str) -> String {
    QUALIFIER_SPLIT_RE
        .split(symbol)
        .filter(|p| !p.is_empty())
        .last()
        .map(|s| s.to_string())
        .unwrap_or_else(|| symbol.to_string())
}

/// Calculate the recommended number of codegraph_explore calls based on project size.
/// Larger codebases need more exploration calls to cover their surface area,
/// but smaller ones should use fewer to avoid unnecessary overhead.
pub fn get_explore_budget(file_count: u64) -> u32 {
    if file_count < 500 {
        return 1;
    }
    if file_count < 5000 {
        return 2;
    }
    if file_count < 15000 {
        return 3;
    }
    if file_count < 25000 {
        return 4;
    }
    5
}

/// Adaptive output budget for `codegraph_explore`, scaled to project size.
///
/// Smaller codebases get a tighter total cap, fewer default files, smaller
/// per-file cap, and tighter clustering — so a focused query on a 100-file
/// project doesn't dump a whole file's worth of source into the agent's
/// context. Larger codebases keep the generous defaults because the
/// agent's native discovery cost (grep + find + many Reads) genuinely
/// dwarfs a fat explore call at that scale.
///
/// Tier breakpoints mirror `get_explore_budget` so a project sits in the
/// same tier across both knobs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExploreOutputBudget {
    /// Hard cap on total output characters.
    pub max_output_chars: usize,
    /// Default `maxFiles` when the caller didn't specify one.
    pub default_max_files: usize,
    /// Cap on contiguous source returned per file (across all its clusters).
    pub max_chars_per_file: usize,
    /// Cluster gap threshold in lines — tighter clustering on small projects.
    pub gap_threshold: i64,
    /// Max symbols listed in the per-file header (`#### path — sym(kind), ...`).
    pub max_symbols_in_file_header: usize,
    /// Max edges shown per relationship kind in the Relationships section.
    pub max_edges_per_relationship_kind: usize,
    /// Include the "Relationships" section.
    pub include_relationships: bool,
    /// Include the "Additional relevant files (not shown)" trailing list.
    pub include_additional_files: bool,
    /// Include the "Complete source code is included above…" reminder.
    pub include_completeness_signal: bool,
    /// Include the explore-budget reminder at the end.
    pub include_budget_note: bool,
    /// Hard-drop test/spec/icon/i18n files from the relevant-file set unless
    /// the query itself mentions tests.
    pub exclude_low_value_files: bool,
}

pub fn get_explore_output_budget(file_count: u64) -> ExploreOutputBudget {
    // Tiered budget, scaled to project size. The budget is a CEILING (relevance
    // still gates WHAT is included), and it MUST stay under the agent's INLINE
    // tool-result cap (~25K chars). Invariant: a larger tier must never get a
    // smaller `max_chars_per_file` than a smaller tier.
    if file_count < 150 {
        return ExploreOutputBudget {
            max_output_chars: 13000,
            default_max_files: 4,
            max_chars_per_file: 3800,
            gap_threshold: 7,
            max_symbols_in_file_header: 5,
            max_edges_per_relationship_kind: 4,
            include_relationships: false,
            include_additional_files: false,
            include_completeness_signal: false,
            include_budget_note: false,
            exclude_low_value_files: true,
        };
    }
    if file_count < 500 {
        return ExploreOutputBudget {
            max_output_chars: 18000,
            default_max_files: 5,
            max_chars_per_file: 3800,
            gap_threshold: 8,
            max_symbols_in_file_header: 6,
            max_edges_per_relationship_kind: 6,
            include_relationships: false,
            include_additional_files: false,
            include_completeness_signal: false,
            include_budget_note: false,
            exclude_low_value_files: true,
        };
    }
    if file_count < 5000 {
        return ExploreOutputBudget {
            max_output_chars: 24000,
            default_max_files: 8,
            max_chars_per_file: 6500,
            gap_threshold: 12,
            max_symbols_in_file_header: 10,
            max_edges_per_relationship_kind: 10,
            include_relationships: true,
            include_additional_files: true,
            include_completeness_signal: true,
            include_budget_note: true,
            exclude_low_value_files: false,
        };
    }
    if file_count < 15000 {
        return ExploreOutputBudget {
            max_output_chars: 24000,
            default_max_files: 8,
            max_chars_per_file: 7000,
            gap_threshold: 15,
            max_symbols_in_file_header: 15,
            max_edges_per_relationship_kind: 15,
            include_relationships: true,
            include_additional_files: true,
            include_completeness_signal: true,
            include_budget_note: true,
            exclude_low_value_files: false,
        };
    }
    ExploreOutputBudget {
        max_output_chars: 24000,
        default_max_files: 8,
        max_chars_per_file: 7000,
        gap_threshold: 15,
        max_symbols_in_file_header: 15,
        max_edges_per_relationship_kind: 15,
        include_relationships: true,
        include_additional_files: true,
        include_completeness_signal: true,
        include_budget_note: true,
        exclude_low_value_files: false,
    }
}

/// Whether `codegraph_explore` should prefix source lines with their line
/// numbers (cat -n style: `<num>\t<code>`). Defaults ON. Set
/// `CODEGRAPH_EXPLORE_LINENUMS=0` to disable.
fn explore_line_numbers_enabled() -> bool {
    std::env::var("CODEGRAPH_EXPLORE_LINENUMS")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// Adaptive explore sizing (default ON). Set `CODEGRAPH_ADAPTIVE_EXPLORE=0`
/// to disable.
fn adaptive_explore_enabled() -> bool {
    match std::env::var("CODEGRAPH_ADAPTIVE_EXPLORE") {
        Ok(v) => v != "0" && v != "false",
        Err(_) => true,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Prefix each line of a source slice with its 1-based line number, matching
/// the Read tool's `cat -n` convention (number + tab).
fn number_source_lines(slice: &str, first_line_number: usize) -> String {
    slice
        .split('\n')
        .enumerate()
        .map(|(i, l)| format!("{}\t{}", first_line_number + i, l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `Number.prototype.toLocaleString()` parity for non-negative integers
/// (en-US grouping: `12345` → `"12,345"`).
fn to_locale_string(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Largest byte index `<= idx` that is a char boundary of `s`.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Per-file staleness banner emitted at the top of a tool response when the
/// file watcher has pending events for files referenced by the response (#403).
pub fn format_stale_banner(stale: &[PendingFile]) -> String {
    let now = now_ms();
    let lines: Vec<String> = stale
        .iter()
        .map(|p| {
            let age_ms = (now - p.last_seen_ms).max(0);
            let label = if p.indexing {
                "indexing in progress"
            } else {
                "pending sync"
            };
            format!("  - {} (edited {}ms ago, {})", p.path, age_ms, label)
        })
        .collect();
    format!(
        "⚠️ Some files referenced below were edited since the last index sync — their codegraph entries may be stale:\n{}\nFor accurate content of those specific files, Read them directly. The rest of this response is fresh.",
        lines.join("\n")
    )
}

/// Compact footer listing pending files that are NOT referenced in this
/// response.
pub fn format_stale_footer(stale: &[PendingFile]) -> String {
    const MAX: usize = 5;
    let now = now_ms();
    let shown = &stale[..stale.len().min(MAX)];
    let lines: Vec<String> = shown
        .iter()
        .map(|p| {
            let age_ms = (now - p.last_seen_ms).max(0);
            format!("  - {} (edited {}ms ago)", p.path, age_ms)
        })
        .collect();
    let more = if stale.len() > MAX {
        format!("\n  - …and {} more", stale.len() - MAX)
    } else {
        String::new()
    };
    format!(
        "(Note: {} file(s) elsewhere in this project are pending index sync but were not referenced above:\n{}{})",
        stale.len(),
        lines.join("\n"),
        more
    )
}

// =============================================================================
// Tool definitions
// =============================================================================

/// MCP Tool definition. Serializes to the same JSON shape as the TS
/// `ToolDefinition` (camelCase `inputSchema`, ordered properties).
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: InputSchema,
    /// EXCEEDS TS: optional behavior hints (spec `ToolAnnotations`) — the TS
    /// parent ships none. Hosts use these for permission UX / auto-approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// Spec tool behavior hints — field names/casing mirror rmcp `ToolAnnotations`
/// (`model/tool.rs`, camelCase, skip-if-none).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// The annotation set every CodeGraph tool shares: all 8 tools are pure
/// reads over the local index — read-only, non-destructive, idempotent,
/// closed-world.
fn read_only_annotations() -> Option<ToolAnnotations> {
    Some(ToolAnnotations {
        read_only_hint: Some(true),
        destructive_hint: Some(false),
        idempotent_hint: Some(true),
        open_world_hint: Some(false),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct InputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    /// Ordered (serde_json `preserve_order`) map of property name → schema.
    pub properties: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

/// Tool execution result (TS `ToolResult`).
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

impl ToolResult {
    /// First text content (convenience for the server/tests).
    pub fn text(&self) -> &str {
        self.content.first().map(|c| c.text.as_str()).unwrap_or("")
    }
}

/// Build a `{ type, description }` property schema (ordered keys:
/// type, description, enum?, default? — matching the TS literal order).
fn prop(prop_type: &str, description: &str) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), Value::String(prop_type.into()));
    m.insert("description".into(), Value::String(description.into()));
    Value::Object(m)
}

fn prop_enum(prop_type: &str, description: &str, enum_values: &[&str]) -> Value {
    let mut v = prop(prop_type, description);
    v.as_object_mut().unwrap().insert(
        "enum".into(),
        Value::Array(
            enum_values
                .iter()
                .map(|e| Value::String((*e).into()))
                .collect(),
        ),
    );
    v
}

fn prop_default(prop_type: &str, description: &str, default: Value) -> Value {
    let mut v = prop(prop_type, description);
    v.as_object_mut().unwrap().insert("default".into(), default);
    v
}

fn prop_enum_default(
    prop_type: &str,
    description: &str,
    enum_values: &[&str],
    default: Value,
) -> Value {
    let mut v = prop_enum(prop_type, description, enum_values);
    v.as_object_mut().unwrap().insert("default".into(), default);
    v
}

/// Common projectPath property for cross-project queries.
fn project_path_property() -> Value {
    prop(
        "string",
        "Path to a different project with .codegraph/ initialized. If omitted, uses current project. Use this to query other codebases.",
    )
}

/// All CodeGraph MCP tools (mirrors the TS `tools` array, same order).
pub fn tools() -> Vec<ToolDefinition> {
    let mut out = Vec::with_capacity(8);

    // codegraph_search
    {
        let mut props = Map::new();
        props.insert(
            "query".into(),
            prop(
                "string",
                "Symbol name or partial name (e.g., \"auth\", \"signIn\", \"UserService\")",
            ),
        );
        props.insert(
            "kind".into(),
            prop_enum(
                "string",
                "Filter by node kind",
                &[
                    "function",
                    "method",
                    "class",
                    "interface",
                    "type",
                    "variable",
                    "route",
                    "component",
                ],
            ),
        );
        props.insert(
            "limit".into(),
            prop_default("number", "Maximum results (default: 10)", Value::from(10)),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_search".into(),
            description: "Quick symbol search by name. Returns locations only (no code). Use codegraph_explore instead to get the actual source / understand an area in one call.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["query".into()]),
            },
            annotations: read_only_annotations(),
        });
    }

    // codegraph_callers
    {
        let mut props = Map::new();
        props.insert(
            "symbol".into(),
            prop(
                "string",
                "Name of the function, method, or class to find callers for",
            ),
        );
        props.insert(
            "limit".into(),
            prop_default(
                "number",
                "Maximum number of callers to return (default: 20)",
                Value::from(20),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_callers".into(),
            description:
                "List functions that call <symbol>. For the full flow, use codegraph_explore."
                    .into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["symbol".into()]),
            },
            annotations: read_only_annotations(),
        });
    }

    // codegraph_callees
    {
        let mut props = Map::new();
        props.insert(
            "symbol".into(),
            prop(
                "string",
                "Name of the function, method, or class to find callees for",
            ),
        );
        props.insert(
            "limit".into(),
            prop_default(
                "number",
                "Maximum number of callees to return (default: 20)",
                Value::from(20),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_callees".into(),
            description:
                "List functions that <symbol> calls. For the full flow, use codegraph_explore."
                    .into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["symbol".into()]),
            },
            annotations: read_only_annotations(),
        });
    }

    // codegraph_impact
    {
        let mut props = Map::new();
        props.insert(
            "symbol".into(),
            prop("string", "Name of the symbol to analyze impact for"),
        );
        props.insert(
            "depth".into(),
            prop_default(
                "number",
                "How many levels of dependencies to traverse (default: 2)",
                Value::from(2),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_impact".into(),
            description: "List symbols affected by changing <symbol>. Use before a refactor."
                .into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["symbol".into()]),
            },
            annotations: read_only_annotations(),
        });
    }

    // codegraph_node
    {
        let mut props = Map::new();
        props.insert(
            "symbol".into(),
            prop("string", "Name of the symbol to get details for"),
        );
        props.insert(
            "includeCode".into(),
            prop_default(
                "boolean",
                "Include full source code (default: false to minimize context)",
                Value::from(false),
            ),
        );
        props.insert(
            "file".into(),
            prop(
                "string",
                "Optional: disambiguate an overloaded name to the definition in this file (path or basename, e.g. \"harness.rs\").",
            ),
        );
        props.insert(
            "line".into(),
            prop(
                "number",
                "Optional: disambiguate to the definition at/around this line (use with the file:line a trail showed you).",
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_node".into(),
            description: "SECONDARY (after codegraph_explore): get ONE symbol in full — its location, signature, callers/callees trail, and verbatim body (includeCode=true). When the name is AMBIGUOUS (an overloaded method, or the same method name on different types), it returns EVERY matching definition's full body in a single call — so you never need to Read a file to find the specific overload you want. For a heavily-overloaded name, pass `file` (and/or `line`) to pin the exact definition — e.g. the `file:line` a trail or another tool already showed you. Reach for this when explore trimmed a body you need. Use codegraph_explore for several related symbols or the full flow.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["symbol".into()]),
            },
            annotations: read_only_annotations(),
        });
    }

    // codegraph_explore
    {
        let mut props = Map::new();
        props.insert(
            "query".into(),
            prop(
                "string",
                "Symbol names, file names, or short code terms to explore (e.g., \"AuthService loginUser session-manager\", \"GraphTraverser BFS impact traversal.ts\"). Use codegraph_search first to find relevant names.",
            ),
        );
        props.insert(
            "maxFiles".into(),
            prop_default(
                "number",
                "Maximum number of files to include source code from (default: 12)",
                Value::from(12),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_explore".into(),
            description: "PRIMARY TOOL — call FIRST for almost any question: how does X work, architecture, a bug, where/what is X, or surveying an area. Returns the verbatim source of the relevant symbols grouped by file in ONE capped call (Read-equivalent — do NOT re-open shown files). Query can be a natural-language question OR a bag of symbol/file names. Usually the ONLY call you need — answers without further search/node/Read/Grep.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["query".into()]),
            },
            annotations: read_only_annotations(),
        });
    }

    // codegraph_status
    {
        let mut props = Map::new();
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_status".into(),
            description: "Index health check (files / nodes / edges). Skip unless debugging."
                .into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: None,
            },
            annotations: read_only_annotations(),
        });
    }

    // codegraph_files
    {
        let mut props = Map::new();
        props.insert(
            "path".into(),
            prop(
                "string",
                "Filter to files under this directory path (e.g., \"src/components\"). Returns all files if not specified.",
            ),
        );
        props.insert(
            "pattern".into(),
            prop(
                "string",
                "Filter files matching this glob pattern (e.g., \"*.tsx\", \"**/*.test.ts\")",
            ),
        );
        props.insert(
            "format".into(),
            prop_enum_default(
                "string",
                "Output format: \"tree\" (hierarchical, default), \"flat\" (simple list), \"grouped\" (by language)",
                &["tree", "flat", "grouped"],
                Value::from("tree"),
            ),
        );
        props.insert(
            "includeMetadata".into(),
            prop_default(
                "boolean",
                "Include file metadata like language and symbol count (default: true)",
                Value::from(true),
            ),
        );
        props.insert(
            "maxDepth".into(),
            prop(
                "number",
                "Maximum directory depth to show (default: unlimited)",
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_files".into(),
            description: "Indexed file tree with language + symbol counts. Faster than Glob for project layout.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: None,
            },
            annotations: read_only_annotations(),
        });
    }

    out
}

/// Strip the optional `codegraph_` prefix (allowlist short-name form).
fn short_tool_name(name: &str) -> &str {
    name.strip_prefix("codegraph_").unwrap_or(name)
}

/// Optional allowlist of exposed tools, parsed from the CODEGRAPH_MCP_TOOLS
/// env var (comma-separated short names). Unset/empty → every tool exposed.
fn tool_allowlist() -> Option<HashSet<String>> {
    let raw = std::env::var("CODEGRAPH_MCP_TOOLS").ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let set: HashSet<String> = raw
        .split(',')
        .map(|s| short_tool_name(s.trim()).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if set.is_empty() { None } else { Some(set) }
}

/// Allowlist-filtered tool definitions WITHOUT an engine — the static surface
/// the proxy answers `tools/list` with before any project is open.
pub fn get_static_tools() -> Vec<ToolDefinition> {
    match tool_allowlist() {
        Some(allow) => tools()
            .into_iter()
            .filter(|t| allow.contains(short_tool_name(&t.name)))
            .collect(),
        None => tools(),
    }
}

// =============================================================================
// Internal helpers (ordered node map, flow info)
// =============================================================================

/// Insertion-ordered node map (TS `Map<string, Node>` parity: `set` on an
/// existing key replaces the value but keeps its original position).
struct OrderedNodeMap {
    order: Vec<String>,
    map: HashMap<String, Node>,
}

impl OrderedNodeMap {
    fn new() -> Self {
        OrderedNodeMap {
            order: Vec::new(),
            map: HashMap::new(),
        }
    }

    fn contains(&self, id: &str) -> bool {
        self.map.contains_key(id)
    }

    fn get(&self, id: &str) -> Option<&Node> {
        self.map.get(id)
    }

    fn insert(&mut self, node: Node) {
        if !self.map.contains_key(&node.id) {
            self.order.push(node.id.clone());
        }
        self.map.insert(node.id.clone(), node);
    }

    fn values(&self) -> impl Iterator<Item = &Node> {
        self.order.iter().filter_map(|id| self.map.get(id))
    }

    fn keys(&self) -> impl Iterator<Item = &String> {
        self.order.iter()
    }

    fn len(&self) -> usize {
        self.order.len()
    }
}

/// Deterministic ordering for a `Subgraph`'s nodes. TS Maps preserve the
/// builder's insertion order; Rust's `Subgraph.nodes` is a `HashMap`, so we
/// impose roots-first (in `roots` order) then (filePath, startLine, name, id).
/// Tie ordering downstream may differ from TS in unpinned cases — see
/// notes/mcp-tools.md.
fn ordered_nodes_from_subgraph(sg: &crate::types::Subgraph) -> OrderedNodeMap {
    let mut out = OrderedNodeMap::new();
    for id in &sg.roots {
        if let Some(n) = sg.nodes.get(id) {
            out.insert(n.clone());
        }
    }
    let mut rest: Vec<&Node> = sg.nodes.values().filter(|n| !out.contains(&n.id)).collect();
    rest.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.start_line.cmp(&b.start_line))
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    for n in rest {
        out.insert(n.clone());
    }
    out
}

/// Result of `build_flow_from_named_symbols`.
struct FlowInfo {
    text: String,
    path_node_ids: HashSet<String>,
    named_node_ids: HashSet<String>,
    unique_named_node_ids: HashSet<String>,
}

impl FlowInfo {
    fn empty() -> Self {
        FlowInfo {
            text: String::new(),
            path_node_ids: HashSet::new(),
            named_node_ids: HashSet::new(),
            unique_named_node_ids: HashSet::new(),
        }
    }
}

struct SynthNote {
    #[allow(dead_code)]
    label: String,
    compact: String,
    #[allow(dead_code)]
    registered_at: Option<String>,
}

/// JS truthiness for a metadata value rendered with `String(...)` — returns
/// the rendered string only when the value is truthy.
fn truthy_meta_string(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f != 0.0 && !f.is_nan() {
                Some(n.to_string())
            } else {
                None
            }
        }
        Value::Bool(true) => Some("true".to_string()),
        _ => None,
    }
}

/// Symbol-ish tokens extracted from an explore query (shared by the flow
/// builder and named-symbol seeding — identical pipeline in TS).
fn extract_symbol_tokens(query: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for part in TOKEN_SPLIT_RE.split(query) {
        let t = FILE_EXT_RE.replace(part, "").trim().to_string();
        if t.chars().count() >= 3 && TOKEN_RE.is_match(&t) && seen.insert(t.clone()) {
            out.push(t);
        }
    }
    out.truncate(16);
    out
}

fn is_qualified_token(t: &str) -> bool {
    t.contains('.') || t.contains('/') || t.contains("::")
}

fn is_test_path(p: &str) -> bool {
    TEST_PATH_DIR_RE.is_match(p) || TEST_PATH_EXT_RE.is_match(p)
}

fn is_low_value(p: &str) -> bool {
    let lp = p.to_lowercase();
    LOW_VALUE_RES.iter().any(|re| re.is_match(&lp))
}

/// `path.resolve(p)` parity for project-root comparisons.
fn resolve_path(p: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    lexical_resolve(&cwd, &p.to_string_lossy())
}

/// `localeCompare` approximation: case-insensitive primary, byte-order tiebreak.
fn locale_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// TS `fileLines.slice(start - 1, end).join('\n')` with clamped bounds.
fn slice_lines(lines: &[&str], start_1based: i64, end_1based: i64) -> String {
    let s = ((start_1based - 1).max(0) as usize).min(lines.len());
    let e = (end_1based.max(0) as usize).min(lines.len());
    if s >= e {
        String::new()
    } else {
        lines[s..e].join("\n")
    }
}

/// JS `Number(x) || default` (also used where TS does `(x as number) || d`).
fn num_or(args: &Map<String, Value>, key: &str, default: f64) -> f64 {
    match args.get(key) {
        Some(Value::Number(n)) => {
            let v = n.as_f64().unwrap_or(f64::NAN);
            if v != 0.0 && !v.is_nan() { v } else { default }
        }
        Some(Value::String(s)) => match s.trim().parse::<f64>() {
            Ok(v) if v != 0.0 => v,
            _ => default,
        },
        Some(Value::Bool(true)) => 1.0,
        _ => default,
    }
}

// =============================================================================
// ToolHandler
// =============================================================================

/// Tool handler that executes tools against a CodeGraph instance.
///
/// Supports cross-project queries via the projectPath parameter.
/// Other projects are opened on-demand and cached for performance.
///
/// Like `CodeGraph` itself this is single-threaded (`!Send`/`!Sync`).
pub struct ToolHandler {
    /// The default CodeGraph instance (None until a project is opened).
    cg: RefCell<Option<Rc<CodeGraph>>>,
    /// Cache of opened CodeGraph instances for cross-project queries.
    project_cache: RefCell<HashMap<String, Rc<CodeGraph>>>,
    /// The directory the server last searched for a default project.
    default_project_hint: RefCell<Option<String>>,
    /// Per-start-path cache of the git worktree/index mismatch (issue #155).
    worktree_mismatch_cache: RefCell<HashMap<String, Option<WorktreeIndexMismatch>>>,
    /// Gate the MCP engine pokes after `open()` so the first tool call blocks
    /// on the post-open filesystem reconcile (catch-up sync). The TS gate is a
    /// Promise; here it is a one-shot closure run (and cleared) on the next
    /// `execute()`. Failures inside the closure are the engine's to log.
    catch_up_gate: RefCell<Option<Box<dyn FnOnce()>>>,
    /// EXCEEDS TS: per-call context (progress emitter + cooperative cancel
    /// flag) the engine sets around each `execute()` — see [`CallContext`].
    call_context: Rc<CallContext>,
}

/// Progress callback the session plumbs through the engine when a `tools/call`
/// carried a `_meta.progressToken` (rmcp `ProgressNotificationParam` fields:
/// progress, total?, message?). Never installed unsolicited.
pub type ProgressEmitter = Arc<dyn Fn(f64, Option<f64>, Option<&str>) + Send + Sync>;

/// EXCEEDS TS: per-call execution context — set by the engine thread for the
/// duration of one `ToolHandler::execute()`. The catch-up gate closure (built
/// before any call arrives) reads the *current* call's progress emitter and
/// cancel flag through this shared cell, mirroring rmcp's
/// `RequestContext { ct, peer, .. }` made available to handlers.
#[derive(Default)]
pub struct CallContext {
    progress: RefCell<Option<ProgressEmitter>>,
    cancel: RefCell<Option<Arc<AtomicBool>>>,
}

impl CallContext {
    /// Install the per-call progress emitter / cancel flag (engine thread).
    pub fn set(&self, progress: Option<ProgressEmitter>, cancel: Option<Arc<AtomicBool>>) {
        *self.progress.borrow_mut() = progress;
        *self.cancel.borrow_mut() = cancel;
    }

    /// Clear after the call completes.
    pub fn clear(&self) {
        *self.progress.borrow_mut() = None;
        *self.cancel.borrow_mut() = None;
    }

    /// Emit one `notifications/progress` if (and only if) the current call
    /// asked for progress.
    pub fn emit_progress(&self, progress: f64, total: Option<f64>, message: Option<&str>) {
        if let Some(emit) = self.progress.borrow().as_ref() {
            emit(progress, total, message);
        }
    }

    /// Whether the current call was cancelled via `notifications/cancelled`.
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
    }
}

impl ToolHandler {
    pub fn new(cg: Option<Rc<CodeGraph>>) -> ToolHandler {
        ToolHandler {
            cg: RefCell::new(cg),
            project_cache: RefCell::new(HashMap::new()),
            default_project_hint: RefCell::new(None),
            worktree_mismatch_cache: RefCell::new(HashMap::new()),
            catch_up_gate: RefCell::new(None),
            call_context: Rc::new(CallContext::default()),
        }
    }

    /// Shared per-call context — the engine sets/clears it around `execute()`
    /// and shares it with the catch-up gate closure.
    pub fn call_context(&self) -> Rc<CallContext> {
        Rc::clone(&self.call_context)
    }

    /// Update the default CodeGraph instance (e.g. after lazy initialization).
    pub fn set_default_code_graph(&self, cg: Rc<CodeGraph>) {
        *self.cg.borrow_mut() = Some(cg);
    }

    /// Engine-only: register the catch-up sync gate so the next `execute()`
    /// call runs it before serving. Cleared on first use.
    pub fn set_catch_up_gate(&self, gate: Option<Box<dyn FnOnce()>>) {
        *self.catch_up_gate.borrow_mut() = gate;
    }

    /// Record the directory the server tried to resolve the default project
    /// from. Used only to make the "no default project" error actionable.
    pub fn set_default_project_hint(&self, searched_path: impl Into<String>) {
        *self.default_project_hint.borrow_mut() = Some(searched_path.into());
    }

    /// Whether a default CodeGraph instance is available.
    pub fn has_default_code_graph(&self) -> bool {
        self.cg.borrow().is_some()
    }

    /// Whether a tool name passes the CODEGRAPH_MCP_TOOLS allowlist (if any).
    fn is_tool_allowed(&self, name: &str) -> bool {
        match tool_allowlist() {
            Some(allow) => allow.contains(short_tool_name(name)),
            None => true,
        }
    }

    /// Get tool definitions with dynamic descriptions based on project size.
    pub fn get_tools(&self) -> Vec<ToolDefinition> {
        let allow = tool_allowlist();
        let mut visible: Vec<ToolDefinition> = match &allow {
            Some(set) => tools()
                .into_iter()
                .filter(|t| set.contains(short_tool_name(&t.name)))
                .collect(),
            None => tools(),
        };
        let cg_ref = self.cg.borrow();
        let Some(cg) = cg_ref.as_ref() else {
            return visible;
        };

        let Ok(stats) = cg.get_stats() else {
            return visible;
        };
        let budget = get_explore_budget(stats.file_count);

        // Tiny-repo tool gating: on projects under TINY_REPO_FILE_THRESHOLD
        // files, only expose the core tools — the omitted tools reduce to one
        // grep at this scale (see the TS source for the full A/B rationale).
        const TINY_REPO_FILE_THRESHOLD: u64 = 500;
        if stats.file_count < TINY_REPO_FILE_THRESHOLD {
            visible.retain(|t| {
                matches!(
                    t.name.as_str(),
                    "codegraph_explore" | "codegraph_search" | "codegraph_node"
                )
            });
        }

        for tool in &mut visible {
            if tool.name == "codegraph_explore" {
                tool.description = format!(
                    "{} Budget: make at most {} calls for this project ({} files indexed).",
                    tool.description,
                    budget,
                    to_locale_string(stats.file_count)
                );
            }
        }
        visible
    }

    /// Get CodeGraph instance for a project.
    ///
    /// If projectPath is provided, opens that project's CodeGraph (cached).
    /// Otherwise returns the default CodeGraph instance. Walks up parent
    /// directories to find the nearest .codegraph/ folder.
    fn get_code_graph(&self, project_path: Option<&str>) -> Result<Rc<CodeGraph>> {
        let Some(project_path) = project_path else {
            return match &*self.cg.borrow() {
                Some(cg) => Ok(Rc::clone(cg)),
                None => {
                    let searched =
                        self.default_project_hint
                            .borrow()
                            .clone()
                            .unwrap_or_else(|| {
                                std::env::current_dir()
                                    .map(|p| p.to_string_lossy().to_string())
                                    .unwrap_or_default()
                            });
                    Err(CodeGraphError::other(format!(
                        "No CodeGraph project is loaded for this session.\nSearched for a .codegraph/ directory starting from: {searched}\nThe index is likely fine — this is a working-directory detection issue: the MCP client launched the server outside your project and didn't report the workspace root. Fix it either way:\n  • Pass projectPath to the tool call, e.g. projectPath: \"/absolute/path/to/your/project\"\n  • Or add --path to the server's MCP config args: [\"serve\", \"--mcp\", \"--path\", \"/absolute/path/to/your/project\"]"
                    )))
                }
            };
        };

        // Check cache first (using original path as key)
        if let Some(cg) = self.project_cache.borrow().get(project_path) {
            return Ok(Rc::clone(cg));
        }

        // Reject sensitive system directories before opening. Only validate a
        // path that actually exists — a nested or not-yet-created sub-path of
        // a real project must still be allowed to resolve UP to its
        // .codegraph/ root below (issue #238).
        let pp = Path::new(project_path);
        if pp.exists() {
            if let Some(path_error) = validate_project_path(pp) {
                return Err(CodeGraphError::other(path_error));
            }
        }

        // Walk up parent directories to find nearest .codegraph/
        let resolved_root = find_nearest_codegraph_root(pp).ok_or_else(|| {
            CodeGraphError::other(format!(
                "CodeGraph not initialized in {project_path}. Run 'codegraph init' in that project first."
            ))
        })?;

        // If the path resolves to the default project, reuse the already-open
        // default instance rather than opening a SECOND connection to the same
        // DB (issue #238). Deliberately not cached under projectPath — the
        // server owns and closes the default instance.
        if let Some(cg) = &*self.cg.borrow() {
            if cg.get_project_root() == resolved_root.as_path() {
                return Ok(Rc::clone(cg));
            }
        }

        // Check if we already have this resolved root cached (different path,
        // same project)
        let resolved_key = resolved_root.to_string_lossy().to_string();
        if let Some(cg) = self
            .project_cache
            .borrow()
            .get(&resolved_key)
            .map(Rc::clone)
        {
            self.project_cache
                .borrow_mut()
                .insert(project_path.to_string(), Rc::clone(&cg));
            return Ok(cg);
        }

        // Open and cache under both paths
        let cg = Rc::new(CodeGraph::open_sync(&resolved_root)?);
        self.project_cache
            .borrow_mut()
            .insert(resolved_key.clone(), Rc::clone(&cg));
        if project_path != resolved_key {
            self.project_cache
                .borrow_mut()
                .insert(project_path.to_string(), Rc::clone(&cg));
        }
        Ok(cg)
    }

    /// Close all cached project connections.
    pub fn close_all(&self) {
        // The same Rc may be cached under multiple paths; close() is idempotent.
        for cg in self.project_cache.borrow().values() {
            cg.close();
        }
        self.project_cache.borrow_mut().clear();
        self.worktree_mismatch_cache.borrow_mut().clear();
    }

    /// Validate that a value is a non-empty string within length bounds.
    fn validate_string(
        &self,
        value: Option<&Value>,
        name: &str,
    ) -> std::result::Result<String, ToolResult> {
        match value {
            Some(Value::String(s)) if !s.is_empty() => {
                let len = s.chars().count();
                if len > MAX_INPUT_LENGTH {
                    Err(self.error_result(&format!(
                        "{name} exceeds maximum length of {MAX_INPUT_LENGTH} characters (got {len})"
                    )))
                } else {
                    Ok(s.clone())
                }
            }
            _ => Err(self.error_result(&format!("{name} must be a non-empty string"))),
        }
    }

    /// Validate an optional path-like string input.
    fn validate_optional_path(
        &self,
        value: Option<&Value>,
        name: &str,
    ) -> std::result::Result<Option<String>, ToolResult> {
        match value {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => {
                let len = s.chars().count();
                if len > MAX_PATH_LENGTH {
                    Err(self.error_result(&format!(
                        "{name} exceeds maximum length of {MAX_PATH_LENGTH} characters (got {len})"
                    )))
                } else {
                    Ok(Some(s.clone()))
                }
            }
            Some(_) => Err(self.error_result(&format!("{name} must be a string"))),
        }
    }

    /// Cached git worktree/index mismatch for a tool call's effective project.
    fn worktree_mismatch_for(&self, project_path: Option<&str>) -> Option<WorktreeIndexMismatch> {
        let start_path = project_path
            .map(String::from)
            .or_else(|| self.default_project_hint.borrow().clone())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
        if let Some(cached) = self.worktree_mismatch_cache.borrow().get(&start_path) {
            return cached.clone();
        }

        let mismatch = match self.get_code_graph(project_path) {
            Ok(cg) => detect_worktree_index_mismatch(Path::new(&start_path), cg.get_project_root()),
            // No resolvable project → nothing to warn.
            Err(_) => None,
        };
        self.worktree_mismatch_cache
            .borrow_mut()
            .insert(start_path, mismatch.clone());
        mismatch
    }

    /// Prefix a successful read-tool result with a compact worktree-mismatch
    /// notice when the resolved index belongs to a different git working tree
    /// than the caller's (issue #155).
    fn with_worktree_notice(&self, result: ToolResult, project_path: Option<&str>) -> ToolResult {
        if result.is_error == Some(true) {
            return result;
        }
        let Some(mismatch) = self.worktree_mismatch_for(project_path) else {
            return result;
        };

        let notice = worktree_mismatch_notice(&mismatch);
        let mut content = result.content;
        if let Some(first) = content.first_mut() {
            if first.content_type == "text" {
                first.text = format!("{}\n\n{}", notice, first.text);
            }
        }
        ToolResult {
            content,
            is_error: result.is_error,
        }
    }

    /// Annotate a successful read-tool result with per-file staleness (#403).
    fn with_staleness_notice(&self, result: ToolResult, project_path: Option<&str>) -> ToolResult {
        if result.is_error == Some(true) {
            return result;
        }

        let Ok(mut cg) = self.get_code_graph(project_path) else {
            return result; // no default project — leave as is
        };

        // Cross-project `projectPath` calls open a cached CodeGraph WITHOUT a
        // watcher. When that path is actually the default project, prefer the
        // default instance so the staleness signal still fires.
        if let Some(default_cg) = &*self.cg.borrow() {
            if !Rc::ptr_eq(default_cg, &cg)
                && resolve_path(default_cg.get_project_root())
                    == resolve_path(cg.get_project_root())
            {
                cg = Rc::clone(default_cg);
            }
        }

        let pending = cg.get_pending_files();
        if pending.is_empty() {
            return result;
        }

        let Some(first) = result.content.first() else {
            return result;
        };
        if first.content_type != "text" {
            return result;
        }

        let text = first.text.clone();
        let mut in_response: Vec<PendingFile> = Vec::new();
        let mut elsewhere: Vec<PendingFile> = Vec::new();
        for p in pending {
            if text.contains(&p.path) {
                in_response.push(p);
            } else {
                elsewhere.push(p);
            }
        }

        let banner = if in_response.is_empty() {
            String::new()
        } else {
            format_stale_banner(&in_response)
        };
        let footer = if elsewhere.is_empty() {
            String::new()
        } else {
            format_stale_footer(&elsewhere)
        };
        if banner.is_empty() && footer.is_empty() {
            return result;
        }

        let composed = [banner, text, footer]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut content = result.content;
        content[0] = ToolContent {
            content_type: "text".into(),
            text: composed,
        };
        ToolResult {
            content,
            is_error: result.is_error,
        }
    }

    /// Execute a tool by name. `args` is the JSON object of tool arguments
    /// (a non-object is treated as `{}`).
    pub fn execute(&self, tool_name: &str, args: &Value) -> ToolResult {
        static EMPTY: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);
        let args = args.as_object().unwrap_or(&EMPTY);

        // Run the engine's post-open reconcile gate once.
        if let Some(gate) = self.catch_up_gate.borrow_mut().take() {
            gate();
        }

        // EXCEEDS TS: cooperative cancellation between pipeline stages — the
        // catch-up sync above is the long first-call stage; if the client
        // cancelled while it ran, stop here. The session suppresses the
        // response (this placeholder is never sent).
        if self.call_context.is_cancelled() {
            return self.error_result("Request cancelled by client");
        }

        // Honor the optional tool allowlist (CODEGRAPH_MCP_TOOLS).
        if !self.is_tool_allowed(tool_name) {
            return self.error_result(&format!(
                "Tool {tool_name} is disabled via CODEGRAPH_MCP_TOOLS"
            ));
        }

        // Cross-cutting input validation.
        if let Err(r) = self.validate_optional_path(args.get("projectPath"), "projectPath") {
            return r;
        }
        if args.contains_key("path") {
            if let Err(r) = self.validate_optional_path(args.get("path"), "path") {
                return r;
            }
        }
        if args.contains_key("pattern") {
            if let Err(r) = self.validate_optional_path(args.get("pattern"), "pattern") {
                return r;
            }
        }

        let project_path: Option<String> = args
            .get("projectPath")
            .and_then(|v| v.as_str())
            .map(String::from);

        let result = match tool_name {
            "codegraph_search" => self.handle_search(args),
            "codegraph_callers" => self.handle_callers(args),
            "codegraph_callees" => self.handle_callees(args),
            "codegraph_impact" => self.handle_impact(args),
            "codegraph_explore" => self.handle_explore(args),
            "codegraph_node" => self.handle_node(args),
            "codegraph_status" => {
                // status embeds the pending-files list as a first-class section,
                // so skip the auto-banner wrappers.
                return match self.handle_status(args) {
                    Ok(r) => r,
                    Err(e) => self.error_result(&format!("Tool execution failed: {e}")),
                };
            }
            "codegraph_files" => self.handle_files(args),
            _ => return self.error_result(&format!("Unknown tool: {tool_name}")),
        };
        let result = match result {
            Ok(r) => r,
            Err(e) => return self.error_result(&format!("Tool execution failed: {e}")),
        };
        let with_worktree = self.with_worktree_notice(result, project_path.as_deref());
        self.with_staleness_notice(with_worktree, project_path.as_deref())
    }

    // =========================================================================
    // codegraph_search
    // =========================================================================

    fn handle_search(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let query = match self.validate_string(args.get("query"), "query") {
            Ok(q) => q,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let kind = args
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let raw_limit = num_or(args, "limit", 10.0);
        let limit = clamp(raw_limit, 1.0, 100.0) as usize;

        // TS passes the raw kind string through; an unknown kind (e.g. "type")
        // matches no rows. NodeKind can't represent it, so short-circuit the
        // same empty-result outcome.
        let kinds: Option<Vec<NodeKind>> = match kind {
            Some(k) => match k.parse::<NodeKind>() {
                Ok(nk) => Some(vec![nk]),
                Err(_) => {
                    return Ok(self.text_result(&format!("No results found for \"{query}\"")));
                }
            },
            None => None,
        };

        let results = cg.search_nodes(
            &query,
            Some(&SearchOptions {
                limit: Some(limit),
                kinds,
                ..Default::default()
            }),
        )?;

        if results.is_empty() {
            return Ok(self.text_result(&format!("No results found for \"{query}\"")));
        }

        // Down-rank generated files within the FTS-returned set. Stable.
        let mut ranked = results;
        ranked.sort_by_key(|r| {
            if is_generated_file(&r.node.file_path) {
                1
            } else {
                0
            }
        });

        let formatted = self.format_search_results(&ranked);
        Ok(self.text_result(&self.truncate_output(&formatted)))
    }

    // =========================================================================
    // codegraph_callers / codegraph_callees
    // =========================================================================

    fn handle_callers(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let limit = clamp(num_or(args, "limit", 20.0), 1.0, 100.0) as usize;

        let all_matches = self.find_all_symbols(&cg, &symbol)?;
        if all_matches.nodes.is_empty() {
            return Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")));
        }

        // Aggregate callers across all matching symbols
        let mut seen: HashSet<String> = HashSet::new();
        let mut all_callers: Vec<Node> = Vec::new();
        for node in &all_matches.nodes {
            for c in cg.get_callers(&node.id, None)? {
                if seen.insert(c.node.id.clone()) {
                    all_callers.push(c.node);
                }
            }
        }

        if all_callers.is_empty() {
            return Ok(self.text_result(&format!(
                "No callers found for \"{symbol}\"{}",
                all_matches.note
            )));
        }

        all_callers.truncate(limit);
        let formatted = format!(
            "{}{}",
            self.format_node_list(&all_callers, &format!("Callers of {symbol}")),
            all_matches.note
        );
        Ok(self.text_result(&self.truncate_output(&formatted)))
    }

    fn handle_callees(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let limit = clamp(num_or(args, "limit", 20.0), 1.0, 100.0) as usize;

        let all_matches = self.find_all_symbols(&cg, &symbol)?;
        if all_matches.nodes.is_empty() {
            return Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")));
        }

        // Aggregate callees across all matching symbols
        let mut seen: HashSet<String> = HashSet::new();
        let mut all_callees: Vec<Node> = Vec::new();
        for node in &all_matches.nodes {
            for c in cg.get_callees(&node.id, None)? {
                if seen.insert(c.node.id.clone()) {
                    all_callees.push(c.node);
                }
            }
        }

        if all_callees.is_empty() {
            return Ok(self.text_result(&format!(
                "No callees found for \"{symbol}\"{}",
                all_matches.note
            )));
        }

        all_callees.truncate(limit);
        let formatted = format!(
            "{}{}",
            self.format_node_list(&all_callees, &format!("Callees of {symbol}")),
            all_matches.note
        );
        Ok(self.text_result(&self.truncate_output(&formatted)))
    }

    // =========================================================================
    // codegraph_impact
    // =========================================================================

    fn handle_impact(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let depth = clamp(num_or(args, "depth", 2.0), 1.0, 10.0) as u32;

        let all_matches = self.find_all_symbols(&cg, &symbol)?;
        if all_matches.nodes.is_empty() {
            return Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")));
        }

        // Aggregate impact across all matching symbols
        let mut merged_nodes = OrderedNodeMap::new();
        let mut seen_edges: HashSet<String> = HashSet::new();

        for node in &all_matches.nodes {
            let impact = cg.get_impact_radius(&node.id, Some(depth))?;
            // Subgraph.nodes is a HashMap (the TS Map preserved insertion
            // order) — impose deterministic ordering. See notes/mcp-tools.md.
            let ordered = ordered_nodes_from_subgraph(&impact);
            for n in ordered.values() {
                merged_nodes.insert(n.clone());
            }
            for e in &impact.edges {
                let key = format!("{}->{}:{}", e.source, e.target, e.kind.as_str());
                seen_edges.insert(key);
            }
        }

        let formatted = format!(
            "{}{}",
            self.format_impact(&symbol, &merged_nodes),
            all_matches.note
        );
        Ok(self.text_result(&self.truncate_output(&formatted)))
    }

    /// Describe a synthesized (dynamic-dispatch) edge for human output.
    /// Returns None for ordinary static edges.
    fn synth_edge_note(&self, edge: Option<&Edge>) -> Option<SynthNote> {
        let edge = edge?;
        if edge.provenance != Some(Provenance::Heuristic) {
            return None;
        }
        let empty = Map::new();
        let m = edge.metadata.as_ref().unwrap_or(&empty);
        let registered_at = m
            .get("registeredAt")
            .and_then(|v| v.as_str())
            .map(String::from);
        let at = registered_at
            .as_ref()
            .map(|r| format!(" @{r}"))
            .unwrap_or_default();
        let synthesized_by = m
            .get("synthesizedBy")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match synthesized_by {
            "callback" => {
                let via = truthy_meta_string(m.get("via"))
                    .map(|v| format!("`{v}`"))
                    .unwrap_or_else(|| "a registrar".to_string());
                let field = truthy_meta_string(m.get("field"))
                    .map(|f| format!(" on .{f}"))
                    .unwrap_or_default();
                Some(SynthNote {
                    label: format!("callback — registered via {via}{field} (dynamic dispatch)"),
                    compact: format!("dynamic: callback via {via}{at}"),
                    registered_at,
                })
            }
            "event-emitter" => {
                let ev = truthy_meta_string(m.get("event"))
                    .map(|e| format!("`{e}`"))
                    .unwrap_or_else(|| "an event".to_string());
                Some(SynthNote {
                    label: format!("event {ev} — emit → handler (dynamic dispatch)"),
                    compact: format!("dynamic: event {ev}{at}"),
                    registered_at,
                })
            }
            "react-render" => Some(SynthNote {
                label: "React re-render — `setState` re-runs render() (dynamic dispatch)".to_string(),
                compact: format!("dynamic: React re-render via setState{at}"),
                registered_at,
            }),
            "jsx-render" => {
                let child = truthy_meta_string(m.get("via"))
                    .map(|v| format!("<{v}>"))
                    .unwrap_or_else(|| "a child component".to_string());
                Some(SynthNote {
                    label: format!("renders {child} (JSX child — dynamic dispatch)"),
                    compact: format!("dynamic: renders {child}"),
                    registered_at,
                })
            }
            "vue-handler" => {
                let ev = truthy_meta_string(m.get("event"))
                    .map(|e| format!("@{e}"))
                    .unwrap_or_else(|| "a template event".to_string());
                Some(SynthNote {
                    label: format!("Vue template handler — bound to {ev} (dynamic dispatch)"),
                    compact: format!("dynamic: Vue {ev} handler"),
                    registered_at,
                })
            }
            "interface-impl" => Some(SynthNote {
                label: "interface/abstract dispatch — runs the implementation override (dynamic dispatch)"
                    .to_string(),
                compact: format!("dynamic: interface → impl{at}"),
                registered_at,
            }),
            "closure-collection" => {
                let field = truthy_meta_string(m.get("field"))
                    .map(|f| format!("`{f}`"))
                    .unwrap_or_else(|| "a collection".to_string());
                Some(SynthNote {
                    label: format!("closure collection — runs handlers appended to {field} (dynamic dispatch)"),
                    compact: format!("dynamic: runs {field} handlers{at}"),
                    registered_at,
                })
            }
            _ => None,
        }
    }

    /// Flow-from-named-symbols: surface the longest call chain AMONG the
    /// symbols named in an explore query. Returns the empty flow if no chain
    /// of >= 3 nodes (and no synthesized links) exists. Mirrors the TS
    /// behavior of swallowing every error into the empty flow.
    fn build_flow_from_named_symbols(&self, cg: &CodeGraph, query: &str) -> FlowInfo {
        self.build_flow_inner(cg, query)
            .unwrap_or_else(|_| FlowInfo::empty())
    }

    fn build_flow_inner(&self, cg: &CodeGraph, query: &str) -> Result<FlowInfo> {
        const MAX_HOPS: usize = 7;
        const MAX_BRIDGE: usize = 1; // ≤1 consecutive UNNAMED hop

        let tokens = extract_symbol_tokens(query);
        if tokens.len() < 2 {
            return Ok(FlowInfo::empty());
        }

        // Pool of name SEGMENTS (Class + method from every token) used to
        // disambiguate an ambiguous SIMPLE name.
        let mut seg_pool: HashSet<String> = HashSet::new();
        for t in &tokens {
            for s in QUAL_DOT_SPLIT_RE.split(&t.to_lowercase()) {
                if !s.is_empty() {
                    seg_pool.insert(s.to_string());
                }
            }
        }

        let mut named = OrderedNodeMap::new();
        // Nodes whose token is SPECIFIC — a (near-)unique callable name
        // (<= 3 defs in the whole graph).
        let mut unique_named_node_ids: HashSet<String> = HashSet::new();
        for t in &tokens {
            let cands: Vec<Node> = self
                .find_all_symbols(cg, t)?
                .nodes
                .into_iter()
                .filter(|n| is_callable_kind(n.kind))
                .collect();
            // A qualified or otherwise-specific name (<=3 hits) keeps all; an
            // ambiguous simple name keeps only candidates whose container is
            // named.
            let specific = cands.len() <= 3;
            let pick: Vec<Node> = if specific {
                cands
            } else {
                cands
                    .into_iter()
                    .filter(|n| {
                        let q = n.qualified_name.to_lowercase();
                        let segs: Vec<&str> = QUAL_DOT_SPLIT_RE
                            .split(&q)
                            .filter(|s| !s.is_empty())
                            .collect();
                        let container = if segs.len() >= 2 {
                            segs[segs.len() - 2]
                        } else {
                            ""
                        };
                        !container.is_empty() && seg_pool.contains(container)
                    })
                    .collect()
            };
            for n in pick.into_iter().take(6) {
                let id = n.id.clone();
                named.insert(n);
                if specific {
                    unique_named_node_ids.insert(id);
                }
            }
            if named.len() > 40 {
                break;
            }
        }
        if named.len() < 2 {
            return Ok(FlowInfo::empty());
        }

        struct ParentEntry {
            prev: Option<String>,
            edge: Option<Edge>,
            node: Node,
        }

        let mut best: Option<Vec<(Node, Option<Edge>)>> = None;
        // BFS the full call graph (incl. synth edges) from each named seed,
        // but only ACCEPT a sink that is also named.
        let seeds: Vec<Node> = named.values().take(8).cloned().collect();
        for seed in &seeds {
            let mut parent: HashMap<String, ParentEntry> = HashMap::new();
            parent.insert(
                seed.id.clone(),
                ParentEntry {
                    prev: None,
                    edge: None,
                    node: seed.clone(),
                },
            );
            let mut q: Vec<(String, usize, usize)> = vec![(seed.id.clone(), 0, 0)];
            let mut deep: Option<String> = None;
            let mut deep_depth = 0usize;
            let mut h = 0usize;
            while h < q.len() && parent.len() < 1500 {
                let (id, depth, streak) = q[h].clone();
                h += 1;
                if id != seed.id && named.contains(&id) && depth > deep_depth {
                    deep = Some(id.clone());
                    deep_depth = depth;
                }
                if depth >= MAX_HOPS - 1 {
                    continue;
                }
                for c in cg.get_callees(&id, None)? {
                    if c.edge.kind != EdgeKind::Calls || parent.contains_key(&c.node.id) {
                        continue;
                    }
                    let new_streak = if named.contains(&c.node.id) {
                        0
                    } else {
                        streak + 1
                    };
                    if new_streak > MAX_BRIDGE {
                        continue;
                    }
                    let cid = c.node.id.clone();
                    parent.insert(
                        cid.clone(),
                        ParentEntry {
                            prev: Some(id.clone()),
                            edge: Some(c.edge),
                            node: c.node,
                        },
                    );
                    q.push((cid, depth + 1, new_streak));
                }
            }
            let Some(deep) = deep else { continue };
            let mut chain: Vec<(Node, Option<Edge>)> = Vec::new();
            let mut cur: Option<String> = Some(deep);
            while let Some(c) = cur {
                let Some(p) = parent.get(&c) else { break };
                chain.push((p.node.clone(), p.edge.clone()));
                cur = p.prev.clone();
            }
            chain.reverse();
            if best.as_ref().map(|b| chain.len() > b.len()).unwrap_or(true) {
                best = Some(chain);
            }
        }
        let has_main = best.as_ref().map(|b| b.len() >= 3).unwrap_or(false);
        let path_ids: HashSet<String> = best
            .as_ref()
            .map(|b| b.iter().map(|(n, _)| n.id.clone()).collect())
            .unwrap_or_default();

        // Supplementary: dynamic-dispatch (synthesized) edges incident to a
        // NAMED symbol.
        let mut synth_lines: Vec<String> = Vec::new();
        let mut synth_seen: HashSet<String> = HashSet::new();
        let named_nodes: Vec<Node> = named.values().cloned().collect();
        'outer: for n in &named_nodes {
            if synth_lines.len() >= 6 {
                break;
            }
            let mut refs = cg.get_callers(&n.id, None)?;
            refs.extend(cg.get_callees(&n.id, None)?);
            for r in refs {
                if synth_lines.len() >= 6 {
                    break 'outer;
                }
                let other = r.node;
                let edge = r.edge;
                if edge.provenance != Some(Provenance::Heuristic) || other.id == n.id {
                    continue;
                }
                if path_ids.contains(&edge.source) && path_ids.contains(&edge.target) {
                    continue; // already in the main chain
                }
                let (src, tgt) = if edge.source == n.id {
                    (n, &other)
                } else {
                    (&other, n)
                };
                let key = format!("{}>{}", src.name, tgt.name);
                if !synth_seen.insert(key) {
                    continue;
                }
                let note = self.synth_edge_note(Some(&edge));
                let tag = note
                    .map(|sn| sn.compact)
                    .unwrap_or_else(|| edge.kind.as_str().to_string());
                synth_lines.push(format!("- {} → {}   [{}]", src.name, tgt.name, tag));
            }
        }

        if !has_main && synth_lines.is_empty() {
            return Ok(FlowInfo::empty());
        }
        let mut out: Vec<String> = Vec::new();
        if has_main {
            out.push("## Flow (call path among the symbols you queried)".to_string());
            out.push(String::new());
            let best = best.as_ref().unwrap();
            for (i, (node, edge)) in best.iter().enumerate() {
                if let Some(e) = edge {
                    let sy = self.synth_edge_note(Some(e));
                    let tag = sy
                        .map(|s| s.compact)
                        .unwrap_or_else(|| e.kind.as_str().to_string());
                    out.push(format!("   ↓ {tag}"));
                }
                out.push(format!(
                    "{}. {} ({}:{})",
                    i + 1,
                    node.name,
                    node.file_path,
                    node.start_line
                ));
            }
            out.push(String::new());
        }
        if !synth_lines.is_empty() {
            out.push("## Dynamic-dispatch links among your symbols".to_string());
            out.push(
                "(synthesized — the indirect hops grep/Read would reconstruct; the `@file:line` is the wiring site)"
                    .to_string(),
            );
            out.push(String::new());
            out.extend(synth_lines);
            out.push(String::new());
        }
        out.push(
            "> Full source for these symbols is below — the call flow among them, followed by their bodies."
                .to_string(),
        );
        out.push(String::new());

        let named_node_ids: HashSet<String> = named.keys().cloned().collect();
        Ok(FlowInfo {
            text: out.join("\n"),
            path_node_ids: path_ids,
            named_node_ids,
            unique_named_node_ids,
        })
    }

    /// Compact "blast radius" for the entry symbols of an explore result.
    fn build_blast_radius_section(
        &self,
        cg: &CodeGraph,
        roots: &[String],
        nodes: &OrderedNodeMap,
    ) -> String {
        const ROOT_CAP: usize = 5; // only the symbols the query actually targeted
        const FILE_CAP: usize = 4; // caller files listed per symbol before "+N more"
        fn meaningful(kind: NodeKind) -> bool {
            matches!(
                kind,
                NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::Class
                    | NodeKind::Interface
                    | NodeKind::Struct
                    | NodeKind::Trait
                    | NodeKind::Protocol
                    | NodeKind::Enum
                    | NodeKind::TypeAlias
                    | NodeKind::Component
                    | NodeKind::Constant
                    | NodeKind::Variable
                    | NodeKind::Property
                    | NodeKind::Field
            )
        }
        let rel = |p: &str| p.replace('\\', "/");

        let root_nodes: Vec<&Node> = roots
            .iter()
            .filter_map(|id| nodes.get(id))
            .filter(|n| meaningful(n.kind))
            .take(ROOT_CAP)
            .collect();
        if root_nodes.is_empty() {
            return String::new();
        }

        let mut entries: Vec<String> = Vec::new();
        for root in root_nodes {
            let callers = cg.get_callers(&root.id, None).unwrap_or_default();

            let mut seen: HashSet<String> = HashSet::new();
            let mut uniq: Vec<Node> = Vec::new();
            for c in callers {
                if seen.insert(c.node.id.clone()) {
                    uniq.push(c.node);
                }
            }
            if uniq.is_empty() {
                continue; // no blast radius → nothing to flag
            }

            let mut caller_files: Vec<String> = Vec::new();
            let mut file_seen: HashSet<String> = HashSet::new();
            for n in &uniq {
                let f = rel(&n.file_path);
                if file_seen.insert(f.clone()) {
                    caller_files.push(f);
                }
            }
            let test_files: Vec<&String> =
                caller_files.iter().filter(|f| is_test_file(f)).collect();
            let non_test: Vec<&String> = caller_files.iter().filter(|f| !is_test_file(f)).collect();

            let shown = non_test
                .iter()
                .take(FILE_CAP)
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let more = if non_test.len() > FILE_CAP {
                format!(" +{} more", non_test.len() - FILE_CAP)
            } else {
                String::new()
            };
            let where_part = if !non_test.is_empty() {
                format!(" in {shown}{more}")
            } else {
                String::new()
            };
            let tests = if !test_files.is_empty() {
                format!(
                    "; tests: {}{}",
                    test_files
                        .iter()
                        .take(FILE_CAP)
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    if test_files.len() > FILE_CAP {
                        format!(" +{}", test_files.len() - FILE_CAP)
                    } else {
                        String::new()
                    }
                )
            } else {
                "; ⚠️ no covering tests found".to_string()
            };

            entries.push(format!(
                "- `{}` ({}:{}) — {} caller{}{}{}",
                root.name,
                rel(&root.file_path),
                root.start_line,
                uniq.len(),
                if uniq.len() == 1 { "" } else { "s" },
                where_part,
                tests
            ));
        }
        if entries.is_empty() {
            return String::new();
        }

        let mut lines = vec![
            "### Blast radius — what depends on these (update/verify before editing)".to_string(),
            String::new(),
        ];
        lines.extend(entries);
        lines.push(String::new());
        lines.join("\n")
    }

    /// Graph-connectivity relevance via Random-Walk-with-Restart (personalized
    /// PageRank) from the query's matched SEED nodes over the call/reference
    /// graph.
    fn compute_graph_relevance(
        &self,
        node_ids: &[String],
        edges: &[Edge],
        seed_ids: &HashSet<String>,
    ) -> HashMap<String, f64> {
        let mut out: HashMap<String, f64> = HashMap::new();
        let n = node_ids.len();
        if n == 0 {
            return out;
        }
        let mut idx: HashMap<&str, usize> = HashMap::new();
        for (i, id) in node_ids.iter().enumerate() {
            idx.insert(id.as_str(), i);
        }

        fn rank_edge(kind: EdgeKind) -> bool {
            matches!(
                kind,
                EdgeKind::Calls
                    | EdgeKind::References
                    | EdgeKind::Extends
                    | EdgeKind::Implements
                    | EdgeKind::Overrides
                    | EdgeKind::Instantiates
                    | EdgeKind::Returns
                    | EdgeKind::TypeOf
                    | EdgeKind::Imports
            )
        }

        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for e in edges {
            if !rank_edge(e.kind) {
                continue;
            }
            let (Some(&i), Some(&j)) = (idx.get(e.source.as_str()), idx.get(e.target.as_str()))
            else {
                continue;
            };
            if i == j {
                continue;
            }
            adj[i].push(j);
            adj[j].push(i); // undirected — reachable either direction
        }

        // Restart vector: uniform over seeds present in the candidate set.
        let mut r = vec![0.0f64; n];
        let mut rsum = 0.0f64;
        for id in seed_ids {
            if let Some(&i) = idx.get(id.as_str()) {
                r[i] = 1.0;
                rsum += 1.0;
            }
        }
        if rsum == 0.0 {
            for v in r.iter_mut() {
                *v = 1.0;
            }
            rsum = n as f64;
        }
        for v in r.iter_mut() {
            *v /= rsum;
        }

        let alpha = 0.25f64;
        let mut s = r.clone();
        for _ in 0..25 {
            let mut next = vec![0.0f64; n];
            for i in 0..n {
                let si = s[i];
                if si == 0.0 {
                    continue;
                }
                let d = adj[i].len();
                if d == 0 {
                    next[i] += si; // dangling: keep its mass
                    continue;
                }
                let share = si / d as f64;
                for &j in &adj[i] {
                    next[j] += share;
                }
            }
            for i in 0..n {
                s[i] = (1.0 - alpha) * next[i] + alpha * r[i];
            }
        }
        for (i, id) in node_ids.iter().enumerate() {
            out.insert(id.clone(), s[i]);
        }
        out
    }

    // =========================================================================
    // codegraph_explore — deep exploration in a single call
    // =========================================================================

    fn handle_explore(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let query = match self.validate_string(args.get("query"), "query") {
            Ok(q) => q,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let project_root = cg.get_project_root().to_path_buf();

        // Resolve adaptive output budget from project size; fall back to the
        // largest-tier defaults if stats aren't available.
        let budget = match cg.get_stats() {
            Ok(stats) => get_explore_output_budget(stats.file_count),
            Err(_) => get_explore_output_budget(u64::MAX),
        };
        let max_files = clamp(
            num_or(args, "maxFiles", budget.default_max_files as f64),
            1.0,
            20.0,
        ) as usize;
        let with_line_numbers = explore_line_numbers_enabled();

        // Step 1: Find relevant context with generous parameters.
        let subgraph = cg.find_relevant_context(
            &query,
            Some(&FindRelevantContextOptions {
                search_limit: Some(8),
                traversal_depth: Some(3),
                max_nodes: Some(200),
                min_score: Some(0.2),
                ..Default::default()
            }),
        )?;

        if subgraph.nodes.is_empty() {
            return Ok(self.text_result(&format!("No relevant code found for \"{query}\"")));
        }

        let roots = subgraph.roots.clone();
        let edges = subgraph.edges.clone();
        let mut nodes = ordered_nodes_from_subgraph(&subgraph);

        // Graph-aware glue: pull in the callers/callees of the entry (root)
        // nodes, but ONLY those that live in files the subgraph already
        // surfaces.
        let mut glue_node_ids: HashSet<String> = HashSet::new();
        let subgraph_files: HashSet<String> = nodes.values().map(|n| n.file_path.clone()).collect();
        const GLUE_NODE_CAP: usize = 60;
        for root_id in &roots {
            if glue_node_ids.len() >= GLUE_NODE_CAP {
                break;
            }
            let mut neighbors: Vec<Node> = Vec::new();
            let callers = cg.get_callers(root_id, None);
            let callees = cg.get_callees(root_id, None);
            match (callers, callees) {
                (Ok(cr), Ok(ce)) => {
                    neighbors.extend(cr.into_iter().map(|c| c.node));
                    neighbors.extend(ce.into_iter().map(|c| c.node));
                }
                _ => continue,
            }
            for nb in neighbors {
                if glue_node_ids.len() >= GLUE_NODE_CAP {
                    break;
                }
                if nodes.contains(&nb.id) {
                    continue;
                }
                if !subgraph_files.contains(&nb.file_path) {
                    continue;
                }
                glue_node_ids.insert(nb.id.clone());
                nodes.insert(nb);
            }
        }

        // Named-symbol seeding: resolve EACH named token to its substantive
        // definition and inject it as an entry.
        let mut named_seed_ids: HashSet<String> = HashSet::new();
        {
            let body_lines = |n: &Node| (n.end_line as i64 - n.start_line as i64).max(0);
            let tokens = extract_symbol_tokens(&query);
            // PascalCase tokens in the query are type/file disambiguators.
            let type_tokens: Vec<&String> = tokens
                .iter()
                .filter(|t| TYPE_TOKEN_RE.is_match(t))
                .collect();
            let in_named_context = |n: &Node| {
                type_tokens.iter().any(|ct| {
                    let lc = ct.to_lowercase();
                    n.file_path.to_lowercase().contains(&lc)
                        || n.qualified_name.to_lowercase().contains(&lc)
                })
            };
            for t in &tokens {
                // Enumerate ALL defs of a bare token via the direct index, not
                // FTS; qualified tokens keep findAllSymbols.
                let raw: Vec<Node> = if is_qualified_token(t) {
                    self.find_all_symbols(&cg, t)?.nodes
                } else {
                    cg.get_nodes_by_name(t)?
                };
                let mut cands: Vec<Node> = raw
                    .into_iter()
                    .filter(|n| is_callable_kind(n.kind) && !is_test_path(&n.file_path))
                    .collect();
                cands.sort_by(|a, b| {
                    let a_sub = if body_lines(a) > 1 { 1 } else { 0 };
                    let b_sub = if body_lines(b) > 1 { 1 } else { 0 };
                    b_sub.cmp(&a_sub).then(body_lines(b).cmp(&body_lines(a)))
                });
                let picks: Vec<Node> = if cands.len() <= 3 {
                    cands
                } else {
                    let ctx: Vec<Node> = cands
                        .iter()
                        .filter(|n| in_named_context(n))
                        .cloned()
                        .collect();
                    if !ctx.is_empty() {
                        ctx.into_iter().take(4).collect()
                    } else {
                        cands.into_iter().take(1).collect()
                    }
                };
                for n in picks {
                    // Mark as a named seed EVEN IF the FTS gather already had it.
                    named_seed_ids.insert(n.id.clone());
                    if !nodes.contains(&n.id) {
                        nodes.insert(n);
                    }
                }
            }
        }

        // Step 2: Group nodes by file, score by relevance
        struct FileGroup {
            nodes: Vec<Node>,
            score: i64,
        }
        let mut file_order: Vec<String> = Vec::new();
        let mut file_groups: HashMap<String, FileGroup> = HashMap::new();
        let entry_node_ids: HashSet<String> = roots
            .iter()
            .cloned()
            .chain(named_seed_ids.iter().cloned())
            .collect();

        // Build a set of nodes directly connected to entry points (depth 1)
        let mut connected_to_entry: HashSet<String> = HashSet::new();
        for edge in &edges {
            if entry_node_ids.contains(&edge.source) {
                connected_to_entry.insert(edge.target.clone());
            }
            if entry_node_ids.contains(&edge.target) {
                connected_to_entry.insert(edge.source.clone());
            }
        }

        for node in nodes.values() {
            // Skip import/export nodes — they add noise without information
            if node.kind == NodeKind::Import || node.kind == NodeKind::Export {
                continue;
            }
            if !file_groups.contains_key(&node.file_path) {
                file_order.push(node.file_path.clone());
                file_groups.insert(
                    node.file_path.clone(),
                    FileGroup {
                        nodes: Vec::new(),
                        score: 0,
                    },
                );
            }
            let group = file_groups.get_mut(&node.file_path).unwrap();
            group.nodes.push(node.clone());
            // Definition ≫ reference: a NAMED-SEED node is worth far more.
            if named_seed_ids.contains(&node.id) {
                group.score += 50;
            } else if entry_node_ids.contains(&node.id) {
                group.score += 10;
            } else if connected_to_entry.contains(&node.id) {
                group.score += 3;
            } else {
                group.score += 1;
            }
        }

        // Only include files that have entry points or nodes directly
        // connected to entry points
        let mut relevant_files: Vec<String> = file_order
            .iter()
            .filter(|fp| file_groups[*fp].score >= 3)
            .cloned()
            .collect();

        // Extract query terms for relevance checking
        let query_terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|t| t.chars().count() >= 3)
            .map(String::from)
            .collect();

        // Hard-exclude test/spec files (ALL tiers) unless the query is about
        // tests — and only when >= 2 non-test candidates remain.
        {
            let query_mentions_tests = QUERY_MENTIONS_TESTS_RE.is_match(&query);
            if !query_mentions_tests {
                let non_low: Vec<String> = relevant_files
                    .iter()
                    .filter(|p| !is_low_value(p))
                    .cloned()
                    .collect();
                if non_low.len() >= 2 {
                    relevant_files = non_low;
                }
            }
        }

        // Secondary signal: how many DISTINCT query terms each file matches.
        let unique_query_terms: Vec<String> = {
            let mut seen = HashSet::new();
            query_terms
                .iter()
                .filter(|t| t.chars().count() >= 3 && seen.insert((*t).clone()))
                .cloned()
                .collect()
        };
        let mut file_term_hits: HashMap<String, usize> = HashMap::new();
        for fp in &relevant_files {
            let group = &file_groups[fp];
            let hay = format!(
                "{} {}",
                fp.to_lowercase(),
                group
                    .nodes
                    .iter()
                    .map(|n| n.name.to_lowercase())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let hits = unique_query_terms
                .iter()
                .filter(|t| hay.contains(t.as_str()))
                .count();
            file_term_hits.insert(fp.clone(), hits);
        }

        // PRIMARY relevance: graph connectivity (RWR from the matched seeds).
        let node_ids: Vec<String> = nodes.keys().cloned().collect();
        let node_rwr = self.compute_graph_relevance(&node_ids, &edges, &entry_node_ids);
        let mut graph_score_order: Vec<String> = Vec::new();
        let mut file_graph_score: HashMap<String, f64> = HashMap::new();
        for node in nodes.values() {
            if !file_graph_score.contains_key(&node.file_path) {
                graph_score_order.push(node.file_path.clone());
            }
            *file_graph_score
                .entry(node.file_path.clone())
                .or_insert(0.0) += node_rwr.get(&node.id).copied().unwrap_or(0.0);
        }
        let max_graph = file_graph_score.values().fold(0.0f64, |a, &b| a.max(b));

        // Central file(s): the 1-2 most graph-central files that also match
        // the query textually.
        let central_files: HashSet<String> = {
            let mut entries: Vec<(&String, f64)> = graph_score_order
                .iter()
                .map(|fp| (fp, file_graph_score[fp]))
                .filter(|(fp, g)| *g > 0.0 && file_term_hits.get(*fp).copied().unwrap_or(0) >= 1)
                .collect();
            entries.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        file_term_hits
                            .get(b.0)
                            .copied()
                            .unwrap_or(0)
                            .cmp(&file_term_hits.get(a.0).copied().unwrap_or(0))
                    })
            });
            entries
                .into_iter()
                .take(2)
                .map(|(f, _)| f.clone())
                .collect()
        };

        // Files that DEFINE a symbol the agent named (or a subgraph root).
        let mut entry_files: HashSet<String> = HashSet::new();
        for id in &entry_node_ids {
            if let Some(n) = nodes.get(id) {
                entry_files.insert(n.file_path.clone());
            }
        }

        // Relevance gate (the generous budget is a CEILING, not a target).
        if max_graph > 0.0 {
            let gated: Vec<String> = relevant_files
                .iter()
                .filter(|fp| {
                    file_graph_score.get(*fp).copied().unwrap_or(0.0) >= max_graph * 0.06
                        || central_files.contains(*fp)
                        || entry_files.contains(*fp)
                        || file_term_hits.get(*fp).copied().unwrap_or(0) >= 2
                })
                .cloned()
                .collect();
            if gated.len() >= 2 {
                relevant_files = gated;
            }
        }

        // Files that DEFINE a symbol the agent NAMED — these sort first.
        let mut named_seed_files: HashSet<String> = HashSet::new();
        for id in &named_seed_ids {
            if let Some(n) = nodes.get(id) {
                named_seed_files.insert(n.file_path.clone());
            }
        }

        let mut sorted_files = relevant_files;
        sorted_files.sort_by(|a, b| {
            use std::cmp::Ordering;
            let a_path = a.to_lowercase();
            let b_path = b.to_lowercase();

            // Agent-named files first.
            let a_named = named_seed_files.contains(a);
            let b_named = named_seed_files.contains(b);
            if a_named != b_named {
                return b_named.cmp(&a_named);
            }

            // Graph connectivity next (epsilon so near-ties fall through).
            let a_g = file_graph_score.get(a).copied().unwrap_or(0.0);
            let b_g = file_graph_score.get(b).copied().unwrap_or(0.0);
            if (a_g - b_g).abs() > max_graph * 0.01 {
                return b_g.partial_cmp(&a_g).unwrap_or(Ordering::Equal);
            }

            let a_hits = file_term_hits.get(a).copied().unwrap_or(0);
            let b_hits = file_term_hits.get(b).copied().unwrap_or(0);
            if a_hits != b_hits {
                return b_hits.cmp(&a_hits);
            }

            let a_low = is_low_value(&a_path);
            let b_low = is_low_value(&b_path);
            if a_low != b_low {
                return if a_low {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }

            // Deprioritize generated source.
            let a_gen = is_generated_file(a);
            let b_gen = is_generated_file(b);
            if a_gen != b_gen {
                return if a_gen {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }

            let a_score = file_groups[a].score;
            let b_score = file_groups[b].score;
            if a_score != b_score {
                return b_score.cmp(&a_score);
            }
            file_groups[b].nodes.len().cmp(&file_groups[a].nodes.len())
        });

        // Step 3: Build relationship map
        let mut lines: Vec<String> = vec![
            format!("## Exploration: {query}"),
            String::new(),
            format!(
                "Found {} symbols across {} files.",
                nodes.len(),
                file_order.len()
            ),
            String::new(),
        ];

        // Blast radius (always-on, compact).
        let blast_radius = self.build_blast_radius_section(&cg, &roots, &nodes);
        if !blast_radius.is_empty() {
            lines.push(blast_radius);
        }

        // Relationship map — show how symbols connect
        let significant_edges: Vec<&Edge> = edges
            .iter()
            .filter(|e| e.kind != EdgeKind::Contains)
            .collect();

        if budget.include_relationships && !significant_edges.is_empty() {
            lines.push("### Relationships".to_string());
            lines.push(String::new());

            // Group edges by kind for readability
            let mut kind_order: Vec<String> = Vec::new();
            let mut by_kind: HashMap<String, Vec<(String, String)>> = HashMap::new();
            for edge in &significant_edges {
                let (Some(source_node), Some(target_node)) =
                    (nodes.get(&edge.source), nodes.get(&edge.target))
                else {
                    continue;
                };
                let kind = edge.kind.as_str().to_string();
                if !by_kind.contains_key(&kind) {
                    kind_order.push(kind.clone());
                }
                by_kind
                    .entry(kind)
                    .or_default()
                    .push((source_node.name.clone(), target_node.name.clone()));
            }

            for kind in &kind_order {
                let group = &by_kind[kind];
                let cap = budget.max_edges_per_relationship_kind;
                lines.push(format!("**{kind}:**"));
                for (source, target) in group.iter().take(cap) {
                    lines.push(format!("- {source} → {target}"));
                }
                if group.len() > cap {
                    lines.push(format!("- ... and {} more", group.len() - cap));
                }
                lines.push(String::new());
            }
        }

        // Step 4: Read contiguous file sections.
        // Compute the flow spine once.
        let flow = self.build_flow_from_named_symbols(&cg, &query);

        // Polymorphic-sibling caches.
        const MIN_SIBLINGS: usize = 3;
        let mut sibling_super: HashMap<String, bool> = HashMap::new();
        let mut super_many: HashMap<String, bool> = HashMap::new();

        lines.push("### Source Code".to_string());
        lines.push(String::new());
        lines.push("> The code below is the **verbatim, current on-disk source** of these files — re-read from disk on this call and line-numbered, byte-for-byte identical to what the Read tool returns. It is NOT a summary, outline, or stale cache. Treat each block as a Read you have already performed: do not Read a file shown here.".to_string());
        lines.push(String::new());

        let mut total_chars: usize = lines.join("\n").len();
        let mut files_included: usize = 0;
        let mut any_file_trimmed = false;

        for file_path in &sorted_files {
            let group = &file_groups[file_path];
            if files_included >= max_files {
                break;
            }
            // A file DEFINES a named/spine symbol (the answer) vs merely
            // references the flow. Past 90% budget, stop pulling INCIDENTAL
            // files — but keep scanning for necessary ones.
            let file_necessary = group.nodes.iter().any(|n| {
                entry_node_ids.contains(&n.id)
                    || flow.path_node_ids.contains(&n.id)
                    || flow.unique_named_node_ids.contains(&n.id)
            });
            if !file_necessary && total_chars as f64 > budget.max_output_chars as f64 * 0.9 {
                continue;
            }

            let Some(abs_path) = validate_path_within_root(&project_root, file_path) else {
                continue;
            };
            if !abs_path.exists() {
                continue;
            }

            let Ok(file_content) = std::fs::read_to_string(&abs_path) else {
                continue;
            };

            let file_lines: Vec<&str> = file_content.split('\n').collect();
            let lang = group
                .nodes
                .first()
                .map(|n| n.language.as_str())
                .unwrap_or("");

            // Adaptive sizing (CODEGRAPH_ADAPTIVE_EXPLORE, default on):
            // collapse a file to a per-symbol view when it's a redundant
            // member of a polymorphic family (see the TS source for the full
            // decision table).
            let spare_named = group
                .nodes
                .iter()
                .any(|n| flow.unique_named_node_ids.contains(&n.id));
            let file_defines_super = {
                let mut found = false;
                for n in &group.nodes {
                    if !matches!(
                        n.kind,
                        NodeKind::Class
                            | NodeKind::Interface
                            | NodeKind::Struct
                            | NodeKind::Trait
                            | NodeKind::Protocol
                            | NodeKind::TypeAlias
                    ) {
                        continue;
                    }
                    let many = match super_many.get(&n.id) {
                        Some(&m) => m,
                        None => {
                            let m = cg
                                .get_incoming_edges(&n.id)?
                                .iter()
                                .filter(|x| {
                                    x.kind == EdgeKind::Implements || x.kind == EdgeKind::Extends
                                })
                                .count()
                                >= MIN_SIBLINGS;
                            super_many.insert(n.id.clone(), m);
                            m
                        }
                    };
                    if many {
                        found = true;
                        break;
                    }
                }
                found
            };
            let spared = spare_named && !file_defines_super;
            fn callable_body(kind: NodeKind) -> bool {
                matches!(
                    kind,
                    NodeKind::Method | NodeKind::Function | NodeKind::Component
                )
            }
            let has_spine_node = group
                .nodes
                .iter()
                .any(|n| flow.path_node_ids.contains(&n.id));
            // On-spine god-file detection.
            let named_body_chars: usize = group
                .nodes
                .iter()
                .filter(|n| {
                    callable_body(n.kind)
                        && (flow.path_node_ids.contains(&n.id)
                            || flow.unique_named_node_ids.contains(&n.id))
                })
                .map(|n| slice_lines(&file_lines, n.start_line as i64, n.end_line as i64).len())
                .sum();
            let on_spine_god_file = has_spine_node
                && named_body_chars > budget.max_chars_per_file
                && group.nodes.iter().any(|n| {
                    callable_body(n.kind)
                        && flow.unique_named_node_ids.contains(&n.id)
                        && !flow.path_node_ids.contains(&n.id)
                });
            let is_sibling = if !has_spine_node {
                let mut found = false;
                'sib: for n in &group.nodes {
                    for e in cg.get_outgoing_edges(&n.id)? {
                        if e.kind != EdgeKind::Implements && e.kind != EdgeKind::Extends {
                            continue;
                        }
                        let many = match sibling_super.get(&e.target) {
                            Some(&m) => m,
                            None => {
                                let m = cg
                                    .get_incoming_edges(&e.target)?
                                    .iter()
                                    .filter(|x| {
                                        x.kind == EdgeKind::Implements
                                            || x.kind == EdgeKind::Extends
                                    })
                                    .count()
                                    >= MIN_SIBLINGS;
                                sibling_super.insert(e.target.clone(), m);
                                m
                            }
                        };
                        if many {
                            found = true;
                            break 'sib;
                        }
                    }
                }
                found
            } else {
                false
            };
            if adaptive_explore_enabled()
                && !flow.path_node_ids.is_empty()
                && (on_spine_god_file || (!has_spine_node && is_sibling && !spared))
            {
                let mut syms: Vec<&Node> = group
                    .nodes
                    .iter()
                    .filter(|n| {
                        n.kind != NodeKind::Import && n.kind != NodeKind::Export && n.start_line > 0
                    })
                    .collect();
                syms.sort_by_key(|n| n.start_line);
                // Pass 1: choose which symbols get a FULL body, by priority,
                // greedily within a per-file body cap.
                let prio = |n: &Node| -> i32 {
                    if !callable_body(n.kind) {
                        99
                    } else if flow.path_node_ids.contains(&n.id) {
                        0
                    } else if flow.unique_named_node_ids.contains(&n.id) {
                        1
                    } else if file_defines_super && flow.named_node_ids.contains(&n.id) {
                        2
                    } else {
                        99
                    }
                };
                let body_cap = budget.max_chars_per_file as f64 * 1.5;
                let mut body_ids: HashSet<String> = HashSet::new();
                let mut body_chars: f64 = 0.0;
                let mut prio_sorted: Vec<&&Node> = syms
                    .iter()
                    .filter(|n| prio(n) < 99 && n.end_line >= n.start_line)
                    .collect();
                prio_sorted.sort_by_key(|n| prio(n));
                for n in prio_sorted {
                    let sz = slice_lines(&file_lines, n.start_line as i64, n.end_line as i64).len()
                        as f64;
                    if body_chars + sz > body_cap && !body_ids.is_empty() {
                        continue;
                    }
                    body_ids.insert(n.id.clone());
                    body_chars += sz;
                }
                // Pass 2: render in line order — full body for chosen symbols,
                // else the signature line (capped, "+N more" tail).
                let mut skel: Vec<String> = Vec::new();
                let mut covered_until: i64 = 0;
                let mut sig_count = 0usize;
                let mut sig_dropped = 0usize;
                let sig_max = 12usize.max(budget.max_symbols_in_file_header * 2);
                for n in &syms {
                    if (n.start_line as i64) <= covered_until {
                        continue;
                    }
                    if body_ids.contains(&n.id) {
                        let end = n.end_line as i64;
                        let body = slice_lines(&file_lines, n.start_line as i64, end);
                        skel.push(if with_line_numbers {
                            number_source_lines(&body, n.start_line as usize)
                        } else {
                            body
                        });
                        covered_until = end;
                    } else {
                        // node.startLine can point at a decorator/annotation —
                        // scan forward for the line that names the symbol.
                        let mut line_no = n.start_line as i64;
                        for k in 0..4i64 {
                            let idx = n.start_line as i64 - 1 + k;
                            let l = if idx >= 0 && (idx as usize) < file_lines.len() {
                                file_lines[idx as usize]
                            } else {
                                ""
                            };
                            if l.contains(&n.name) {
                                line_no = n.start_line as i64 + k;
                                break;
                            }
                        }
                        if line_no <= covered_until {
                            continue;
                        }
                        if sig_count >= sig_max {
                            sig_dropped += 1;
                            continue;
                        }
                        let sig = if line_no >= 1 && (line_no as usize) <= file_lines.len() {
                            file_lines[line_no as usize - 1].trim()
                        } else {
                            ""
                        };
                        if !sig.is_empty() {
                            skel.push(if with_line_numbers {
                                format!("{line_no}\t{sig}")
                            } else {
                                sig.to_string()
                            });
                            sig_count += 1;
                        }
                    }
                }
                if sig_dropped > 0 {
                    skel.push(format!("… +{sig_dropped} more (signatures elided)"));
                }
                if !skel.is_empty() {
                    let mut name_seen: HashSet<String> = HashSet::new();
                    let names = group
                        .nodes
                        .iter()
                        .filter(|n| n.kind != NodeKind::Import && n.kind != NodeKind::Export)
                        .filter(|n| name_seen.insert(n.name.clone()))
                        .map(|n| n.name.clone())
                        .take(budget.max_symbols_in_file_header)
                        .collect::<Vec<_>>()
                        .join(", ");
                    // Steer the agent to codegraph_explore — NEVER to Read.
                    let tag = if !body_ids.is_empty() {
                        "focused (the methods you named in full, the rest as signatures — codegraph_explore a signature by name for its body; do NOT Read)"
                    } else {
                        "skeleton (signatures only — codegraph_explore a name for its full body; do NOT Read)"
                    };
                    let skel_text = skel.join("\n");
                    lines.push(format!("#### {file_path} — {names} · {tag}"));
                    lines.push(String::new());
                    lines.push(format!("```{lang}"));
                    lines.push(skel_text.clone());
                    lines.push("```".to_string());
                    lines.push(String::new());
                    total_chars += skel_text.len() + 120;
                    files_included += 1;
                    continue;
                }
            }

            // Whole-file rule: a small relevant file is returned ENTIRELY.
            let is_central_file = central_files.contains(file_path);
            let whole_file_max_lines = if is_central_file { 280 } else { 220 };
            let whole_file_max_chars = if is_central_file {
                (budget.max_output_chars as i64 - total_chars as i64 - 200)
                    .max(0)
                    .min((budget.max_chars_per_file as f64 * 1.5).round() as i64)
                    as usize
            } else {
                budget.max_chars_per_file * 3
            };
            if file_lines.len() <= whole_file_max_lines
                && file_content.len() <= whole_file_max_chars
            {
                let body = file_content.trim_end_matches('\n');
                let whole_section = if with_line_numbers {
                    number_source_lines(body, 1)
                } else {
                    body.to_string()
                };
                let mut sym_seen: HashSet<String> = HashSet::new();
                let uniq_symbols: Vec<String> = group
                    .nodes
                    .iter()
                    .filter(|n| n.kind != NodeKind::Import && n.kind != NodeKind::Export)
                    .map(|n| format!("{}({})", n.name, n.kind.as_str()))
                    .filter(|s| sym_seen.insert(s.clone()))
                    .collect();
                let header_names: Vec<String> = uniq_symbols
                    .iter()
                    .take(budget.max_symbols_in_file_header)
                    .cloned()
                    .collect();
                let omitted = uniq_symbols.len() as i64 - header_names.len() as i64;
                let whole_header = if omitted > 0 {
                    format!(
                        "#### {} — {}, +{} more",
                        file_path,
                        header_names.join(", "),
                        omitted
                    )
                } else {
                    format!("#### {} — {}", file_path, header_names.join(", "))
                };

                if !file_necessary
                    && total_chars + whole_section.len() + 200 > budget.max_output_chars
                {
                    // Don't slice a whole file mid-method.
                    any_file_trimmed = true;
                    continue;
                }
                lines.push(whole_header);
                lines.push(String::new());
                lines.push(format!("```{lang}"));
                lines.push(whole_section.clone());
                lines.push("```".to_string());
                lines.push(String::new());
                total_chars += whole_section.len() + 200;
                files_included += 1;
                continue;
            }

            // Cluster nearby symbols.
            fn envelope_kind(kind: NodeKind) -> bool {
                matches!(
                    kind,
                    NodeKind::File
                        | NodeKind::Module
                        | NodeKind::Class
                        | NodeKind::Struct
                        | NodeKind::Interface
                        | NodeKind::Enum
                        | NodeKind::Namespace
                        | NodeKind::Protocol
                        | NodeKind::Trait
                        | NodeKind::Component
                )
            }
            // Cluster from this file's gathered nodes PLUS any callable the
            // agent NAMED that lives here.
            let mut range_nodes = OrderedNodeMap::new();
            for n in &group.nodes {
                if n.start_line > 0 && n.end_line > 0 {
                    range_nodes.insert(n.clone());
                }
            }
            for id in &flow.named_node_ids {
                if range_nodes.contains(id) {
                    continue;
                }
                if let Some(n) = cg.get_node(id)? {
                    if n.file_path == *file_path && n.start_line > 0 && n.end_line > 0 {
                        range_nodes.insert(n);
                    }
                }
            }
            #[derive(Clone)]
            struct LineRange {
                start: i64,
                end: i64,
                name: String,
                kind: String,
                importance: i64,
            }
            let mut ranges: Vec<LineRange> = range_nodes
                .values()
                // Drop whole-file envelope nodes (containers covering >50%).
                .filter(|n| {
                    !(envelope_kind(n.kind)
                        && (n.end_line as i64 - n.start_line as i64 + 1) as f64
                            > file_lines.len() as f64 * 0.5)
                })
                .map(|n| {
                    let importance = if entry_node_ids.contains(&n.id) {
                        10
                    } else if flow.named_node_ids.contains(&n.id) {
                        9 // agent named it → keep its cluster
                    } else if glue_node_ids.contains(&n.id) {
                        6 // bridging caller/callee of an entry
                    } else if connected_to_entry.contains(&n.id) {
                        3
                    } else {
                        1
                    };
                    LineRange {
                        start: n.start_line as i64,
                        end: n.end_line as i64,
                        name: n.name.clone(),
                        kind: n.kind.as_str().to_string(),
                        importance,
                    }
                })
                .collect();

            // Add edge source locations in this file (template references).
            let mut edge_lines: HashSet<String> = HashSet::new();
            for node in &group.nodes {
                for edge in cg.get_outgoing_edges(&node.id)? {
                    let Some(line) = edge.line else { continue };
                    if line == 0 || edge.kind == EdgeKind::Contains {
                        continue;
                    }
                    let key = format!("{}:{}", line, edge.target);
                    if !edge_lines.insert(key) {
                        continue;
                    }
                    let target_name = nodes
                        .get(&edge.target)
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| edge.kind.as_str().to_string());
                    ranges.push(LineRange {
                        start: line as i64,
                        end: line as i64,
                        name: target_name,
                        kind: edge.kind.as_str().to_string(),
                        importance: 2,
                    });
                }
            }

            ranges.sort_by_key(|r| r.start);

            if ranges.is_empty() {
                continue;
            }

            let gap_threshold = budget.gap_threshold;
            #[derive(Clone)]
            struct Cluster {
                start: i64,
                end: i64,
                symbols: Vec<String>,
                score: i64,
                max_importance: i64,
            }
            let mut clusters: Vec<Cluster> = Vec::new();
            let mut current = Cluster {
                start: ranges[0].start,
                end: ranges[0].end,
                symbols: vec![format!("{}({})", ranges[0].name, ranges[0].kind)],
                score: ranges[0].importance,
                max_importance: ranges[0].importance,
            };

            for r in ranges.iter().skip(1) {
                if r.start <= current.end + gap_threshold {
                    current.end = current.end.max(r.end);
                    current.symbols.push(format!("{}({})", r.name, r.kind));
                    current.score += r.importance;
                    current.max_importance = current.max_importance.max(r.importance);
                } else {
                    clusters.push(current);
                    current = Cluster {
                        start: r.start,
                        end: r.end,
                        symbols: vec![format!("{}({})", r.name, r.kind)],
                        score: r.importance,
                        max_importance: r.importance,
                    };
                }
            }
            clusters.push(current);

            // Build file section output from clusters, capped by per-file
            // budget.
            let context_padding: i64 = 3;
            let build_section = |c: &Cluster| -> String {
                let start_idx = (c.start - 1 - context_padding).max(0) as usize;
                let end_idx = ((c.end + context_padding).max(0) as usize).min(file_lines.len());
                let slice = if start_idx >= end_idx {
                    String::new()
                } else {
                    file_lines[start_idx..end_idx].join("\n")
                };
                if with_line_numbers {
                    number_source_lines(&slice, start_idx + 1)
                } else {
                    slice
                }
            };
            // Language-neutral separator.
            const GAP_MARKER: &str = "\n\n... (gap) ...\n\n";

            // Rank clusters for inclusion under the per-file cap.
            struct RankedCluster {
                idx: usize,
                span: i64,
            }
            let mut ranked_clusters: Vec<RankedCluster> = clusters
                .iter()
                .enumerate()
                .map(|(i, c)| RankedCluster {
                    idx: i,
                    span: c.end - c.start + 1,
                })
                .collect();
            ranked_clusters.sort_by(|a, b| {
                use std::cmp::Ordering;
                let ca = &clusters[a.idx];
                let cb = &clusters[b.idx];
                if cb.max_importance != ca.max_importance {
                    return cb.max_importance.cmp(&ca.max_importance);
                }
                let density_a = ca.score as f64 / a.span as f64;
                let density_b = cb.score as f64 / b.span as f64;
                if density_b != density_a {
                    return density_b.partial_cmp(&density_a).unwrap_or(Ordering::Equal);
                }
                if cb.score != ca.score {
                    return cb.score.cmp(&ca.score);
                }
                a.span.cmp(&b.span)
            });

            // Per-file budget is the SMALLER of the per-file cap and what's
            // left of the total output cap.
            let file_budget = budget
                .max_chars_per_file
                .min((budget.max_output_chars as i64 - total_chars as i64 - 200).max(0) as usize);
            let mut chosen_indices: HashSet<usize> = HashSet::new();
            let mut projected_chars: usize = 0;
            for rc in &ranked_clusters {
                let section_len = build_section(&clusters[rc.idx]).len()
                    + if !chosen_indices.is_empty() {
                        GAP_MARKER.len()
                    } else {
                        0
                    };
                // Always take the top-ranked cluster, even if oversize.
                if chosen_indices.is_empty() {
                    chosen_indices.insert(rc.idx);
                    projected_chars += section_len;
                    continue;
                }
                if projected_chars + section_len > file_budget {
                    continue;
                }
                chosen_indices.insert(rc.idx);
                projected_chars += section_len;
            }

            // Emit chosen clusters in source order.
            let mut file_section = String::new();
            let mut all_symbols: Vec<String> = Vec::new();
            for (i, cluster) in clusters.iter().enumerate() {
                if !chosen_indices.contains(&i) {
                    continue;
                }
                let section = build_section(cluster);
                if !file_section.is_empty() {
                    file_section.push_str(GAP_MARKER);
                }
                file_section.push_str(&section);
                all_symbols.extend(cluster.symbols.iter().cloned());
            }

            if chosen_indices.len() < clusters.len() {
                any_file_trimmed = true;
            }

            // Dedupe + cap the symbols list shown in the per-file header.
            let mut count_order: Vec<String> = Vec::new();
            let mut symbol_counts: HashMap<String, usize> = HashMap::new();
            for s in &all_symbols {
                if !symbol_counts.contains_key(s) {
                    count_order.push(s.clone());
                }
                *symbol_counts.entry(s.clone()).or_insert(0) += 1;
            }
            let mut sorted_symbols: Vec<String> = count_order;
            sorted_symbols.sort_by(|a, b| symbol_counts[b].cmp(&symbol_counts[a]));
            let header_cap = budget.max_symbols_in_file_header;
            let header_symbols: Vec<String> =
                sorted_symbols.iter().take(header_cap).cloned().collect();
            let omitted_count = sorted_symbols.len() as i64 - header_symbols.len() as i64;
            let header_suffix = if omitted_count > 0 {
                format!("{}, +{} more", header_symbols.join(", "), omitted_count)
            } else {
                header_symbols.join(", ")
            };
            let file_header = format!("#### {file_path} — {header_suffix}");

            // The total cap bounds INCIDENTAL files only.
            if !file_necessary && total_chars + file_section.len() + 200 > budget.max_output_chars {
                any_file_trimmed = true;
                continue;
            }

            lines.push(file_header);
            lines.push(String::new());
            lines.push(format!("```{lang}"));
            lines.push(file_section.clone());
            lines.push("```".to_string());
            lines.push(String::new());

            total_chars += file_section.len() + 200;
            files_included += 1;
        }

        // Add remaining files as references.
        if budget.include_additional_files {
            let remaining_relevant: Vec<String> =
                sorted_files.iter().skip(files_included).cloned().collect();
            let mut peripheral_files: Vec<String> = file_order
                .iter()
                .filter(|fp| file_groups[*fp].score < 3)
                .cloned()
                .collect();
            peripheral_files.sort_by(|a, b| file_groups[b].score.cmp(&file_groups[a].score));
            let remaining_files: Vec<String> = remaining_relevant
                .into_iter()
                .chain(peripheral_files)
                .collect();
            if !remaining_files.is_empty() {
                lines
                    .push("### Not shown above — explore these names for their source".to_string());
                lines.push(String::new());
                for file_path in remaining_files.iter().take(10) {
                    let group = &file_groups[file_path];
                    let symbols = group
                        .nodes
                        .iter()
                        .map(|n| format!("{}:{}", n.name, n.start_line))
                        .collect::<Vec<_>>()
                        .join(", ");
                    lines.push(format!("- {file_path}: {symbols}"));
                }
                if remaining_files.len() > 10 {
                    lines.push(format!(
                        "- ... and {} more files",
                        remaining_files.len() - 10
                    ));
                }
            }
        }

        // Add completeness signal so agents know they don't need to re-read.
        if budget.include_completeness_signal {
            lines.push(String::new());
            lines.push("---".to_string());
            lines.push(format!(
                "> **Complete source for {files_included} files is included above — do NOT re-read them.** If your question also needs files/symbols listed under \"Not shown above\" (or any area this call didn't cover), make ANOTHER codegraph_explore targeting those names — it returns the same source with line numbers and is cheaper and more complete than reading. Reserve Read for a single specific line range explore can't surface."
            ));
        } else if any_file_trimmed {
            lines.push(String::new());
            lines.push("> Some file sections were trimmed for size. For a specific symbol you still need, run another `codegraph_explore` (or `codegraph_node`) with its exact name — line-numbered source, cheaper and more complete than Read.".to_string());
        }

        // Add explore budget note based on project size
        if budget.include_budget_note {
            if let Ok(stats) = cg.get_stats() {
                let call_budget = get_explore_budget(stats.file_count);
                lines.push(String::new());
                lines.push(format!(
                    "> **Explore budget: {} calls for this project ({} files indexed).** Each call covers ~6 files; if your question spans more, spend your remaining calls on the uncovered area BEFORE falling back to Read — another explore is cheaper and more complete than reading those files. Synthesize once you've used {}.",
                    call_budget,
                    to_locale_string(stats.file_count),
                    call_budget
                ));
            }
        }

        // Final ceiling — an ABSOLUTE inline cap.
        let output = format!("{}{}", flow.text, lines.join("\n"));
        let hard_ceiling = (((budget.max_output_chars as f64) * 1.5).round() as usize).min(25000);
        if output.len() > hard_ceiling {
            // Cut at a FILE-SECTION boundary (the last `#### ` header before
            // the ceiling); fall back to a line boundary.
            let cut = &output[..floor_char_boundary(&output, hard_ceiling)];
            let last_section = cut.rfind("\n#### ");
            let boundary = match last_section {
                Some(pos) if (pos as f64) > hard_ceiling as f64 * 0.5 => Some(pos),
                _ => cut.rfind('\n'),
            };
            let safe = match boundary {
                Some(pos) if pos > 0 => &cut[..pos],
                _ => cut,
            };
            return Ok(self.text_result(&format!(
                "{safe}\n\n... (output truncated to budget; the source above is complete and verbatim — treat it as already Read. For any area not covered, run another codegraph_explore with the specific names — do NOT Read these files.)"
            )));
        }
        Ok(self.text_result(&output))
    }

    // =========================================================================
    // codegraph_node
    // =========================================================================

    fn handle_node(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        // Default to false to minimize context usage
        let include_code = args.get("includeCode") == Some(&Value::Bool(true));
        let file_hint: Option<String> = args
            .get("file")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let line_hint: Option<f64> = args
            .get("line")
            .and_then(|v| v.as_f64())
            .filter(|&l| l > 0.0);

        let mut matches = self.find_symbol_matches(&cg, &symbol)?;
        if matches.is_empty() {
            return Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")));
        }

        // Disambiguate a heavily-overloaded name to a specific definition the
        // caller pinned by file/line. Only narrows (never empties).
        if matches.len() > 1 && (file_hint.is_some() || line_hint.is_some()) {
            let norm = |p: &str| p.replace('\\', "/").to_lowercase();
            let mut narrowed = matches.clone();
            if let Some(fh) = &file_hint {
                let fh = norm(fh);
                let by_file: Vec<Node> = narrowed
                    .iter()
                    .filter(|n| {
                        let np = norm(&n.file_path);
                        np.ends_with(&fh) || np.contains(&fh)
                    })
                    .cloned()
                    .collect();
                if !by_file.is_empty() {
                    narrowed = by_file;
                }
            }
            if let Some(lh) = line_hint {
                if narrowed.len() > 1 {
                    let containing: Vec<Node> = narrowed
                        .iter()
                        .filter(|n| (n.start_line as f64) <= lh && (n.end_line as f64) >= lh)
                        .cloned()
                        .collect();
                    narrowed = if !containing.is_empty() {
                        containing
                    } else {
                        let mut sorted = narrowed.clone();
                        sorted.sort_by(|a, b| {
                            let da = (a.start_line as f64 - lh).abs();
                            let db = (b.start_line as f64 - lh).abs();
                            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        sorted.truncate(1);
                        sorted
                    };
                }
            }
            if !narrowed.is_empty() {
                matches = narrowed;
            }
        }

        // Single definition — the common case.
        if matches.len() == 1 {
            let section = self.render_node_section(&cg, &matches[0], include_code)?;
            return Ok(self.text_result(&self.truncate_output(&section)));
        }

        // Multiple definitions share this name — return them ALL.
        let header = format!("**{} definitions named \"{}\"**", matches.len(), symbol);
        if !include_code {
            let list: Vec<String> = matches
                .iter()
                .map(|n| {
                    format!(
                        "- `{}` ({}) — {}:{}",
                        n.name,
                        n.kind.as_str(),
                        n.file_path,
                        n.start_line
                    )
                })
                .collect();
            let mut out = vec![
                header,
                String::new(),
                "Re-query with `includeCode: true` to get every body in one call — no need to pick one first."
                    .to_string(),
                String::new(),
            ];
            out.extend(list);
            return Ok(self.text_result(&self.truncate_output(&out.join("\n"))));
        }

        const BODY_BUDGET: usize = 12000; // room under MAX_OUTPUT_LENGTH for header + list
        const HARD_CAP: usize = 16;
        let mut rendered: Vec<String> = Vec::new();
        let mut listed: Vec<Node> = Vec::new();
        let mut used: usize = 0;
        for n in &matches {
            if rendered.len() >= HARD_CAP {
                listed.push(n.clone());
                continue;
            }
            let section = self.render_node_section(&cg, n, true)?;
            // Always emit the first; emit the rest only while within budget.
            if rendered.is_empty() || used + section.len() <= BODY_BUDGET {
                used += section.len();
                rendered.push(section);
            } else {
                listed.push(n.clone());
            }
        }

        let mut out: Vec<String> = vec![
            header,
            format!(
                "Returning {} in full{} — pick the one you need (no Read required).",
                rendered.len(),
                if !listed.is_empty() {
                    format!("; {} more listed below", listed.len())
                } else {
                    String::new()
                }
            ),
            String::new(),
            rendered.join("\n\n---\n\n"),
        ];
        if !listed.is_empty() {
            const LIST_CAP: usize = 20;
            out.push(String::new());
            out.push("### Other definitions".to_string());
            for n in listed.iter().take(LIST_CAP) {
                out.push(format!(
                    "- `{}` ({}) — {}:{}",
                    n.name,
                    n.kind.as_str(),
                    n.file_path,
                    n.start_line
                ));
            }
            if listed.len() > LIST_CAP {
                out.push(format!("- … +{} more", listed.len() - LIST_CAP));
            }
            let basename = listed[0]
                .file_path
                .split('/')
                .next_back()
                .unwrap_or(&listed[0].file_path);
            out.push(String::new());
            out.push(format!(
                "> Need one of these in full? Call codegraph_node again with `file` (e.g. `\"{basename}\"`) or `line` — do NOT Read it."
            ));
        }
        Ok(self.text_result(&self.truncate_output(&out.join("\n"))))
    }

    /// Render one symbol: details + (optional) body/outline + its trail.
    fn render_node_section(
        &self,
        cg: &CodeGraph,
        node: &Node,
        include_code: bool,
    ) -> Result<String> {
        let mut code: Option<String> = None;
        let mut outline: Option<String> = None;
        if include_code {
            // For container symbols, return a structural outline instead.
            if is_container_node_kind(node.kind) {
                let o = self.build_container_outline(cg, node)?;
                if !o.is_empty() {
                    outline = Some(o);
                }
            }
            if outline.is_none() {
                code = cg.get_code(&node.id)?;
            }
        }
        Ok(format!(
            "{}{}",
            self.format_node_details(node, code.as_deref(), outline.as_deref()),
            self.format_trail(cg, node)?
        ))
    }

    /// Build the "trail" for a symbol: direct callees and callers with
    /// file:line.
    fn format_trail(&self, cg: &CodeGraph, node: &Node) -> Result<String> {
        const TRAIL_CAP: usize = 12;
        let fmt = |e: &crate::types::NodeRef| -> String {
            let base = format!(
                "{} ({}:{})",
                e.node.name, e.node.file_path, e.node.start_line
            );
            match self.synth_edge_note(Some(&e.edge)) {
                Some(synth) => format!("{} [{}]", base, synth.compact),
                None => base,
            }
        };
        let collect = |edges: Vec<crate::types::NodeRef>| -> Vec<crate::types::NodeRef> {
            let mut seen: HashSet<String> = HashSet::new();
            seen.insert(node.id.clone());
            let mut out = Vec::new();
            for e in edges {
                if seen.insert(e.node.id.clone()) {
                    out.push(e);
                }
            }
            out
        };
        let callees = collect(cg.get_callees(&node.id, None)?);
        let callers = collect(cg.get_callers(&node.id, None)?);
        if callees.is_empty() && callers.is_empty() {
            return Ok(String::new());
        }
        let mut lines: Vec<String> = vec![
            String::new(),
            "### Trail — codegraph_node any of these to follow it (no Read needed)".to_string(),
        ];
        if !callees.is_empty() {
            lines.push(format!(
                "**Calls →** {}{}",
                callees
                    .iter()
                    .take(TRAIL_CAP)
                    .map(&fmt)
                    .collect::<Vec<_>>()
                    .join(", "),
                if callees.len() > TRAIL_CAP {
                    format!(", +{} more", callees.len() - TRAIL_CAP)
                } else {
                    String::new()
                }
            ));
        }
        if !callers.is_empty() {
            lines.push(format!(
                "**Called by ←** {}{}",
                callers
                    .iter()
                    .take(TRAIL_CAP)
                    .map(&fmt)
                    .collect::<Vec<_>>()
                    .join(", "),
                if callers.len() > TRAIL_CAP {
                    format!(", +{} more", callers.len() - TRAIL_CAP)
                } else {
                    String::new()
                }
            ));
        }
        Ok(lines.join("\n"))
    }

    // =========================================================================
    // codegraph_status
    // =========================================================================

    fn handle_status(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let project_path = args.get("projectPath").and_then(|v| v.as_str());
        let mut cg = self.get_code_graph(project_path)?;
        // Same trick as with_staleness_notice — prefer the default instance
        // when an explicit projectPath resolves to the same project.
        if let Some(default_cg) = &*self.cg.borrow() {
            if !Rc::ptr_eq(default_cg, &cg)
                && resolve_path(default_cg.get_project_root())
                    == resolve_path(cg.get_project_root())
            {
                cg = Rc::clone(default_cg);
            }
        }
        let stats = cg.get_stats()?;

        let mismatch = self.worktree_mismatch_for(project_path);

        let mut lines: Vec<String> = vec!["## CodeGraph Status".to_string(), String::new()];
        if let Some(m) = &mismatch {
            lines.push(format!(
                "> ⚠ {}",
                worktree_mismatch_warning(m).replace('\n', "\n> ")
            ));
            lines.push(String::new());
        }
        lines.push(format!("**Files indexed:** {}", stats.file_count));
        lines.push(format!("**Total nodes:** {}", stats.node_count));
        lines.push(format!("**Total edges:** {}", stats.edge_count));
        lines.push(format!(
            "**Database size:** {:.2} MB",
            stats.db_size_bytes as f64 / 1024.0 / 1024.0
        ));

        // Surface the active SQLite backend. The TS line names node:sqlite;
        // the Rust build embeds SQLite via rusqlite — report "native" per the
        // porting convention (PORTING.md rule 12) while keeping the line shape.
        lines.push(format!(
            "**Backend:** {} (rusqlite bundled SQLite) — full WAL + FTS5",
            cg.get_backend().as_str()
        ));

        // Effective journal mode.
        let journal_mode = cg.get_journal_mode()?;
        if journal_mode == "wal" {
            lines.push("**Journal mode:** wal (concurrent reads safe)".to_string());
        } else {
            let mode = if journal_mode.is_empty() {
                "unknown".to_string()
            } else {
                journal_mode
            };
            lines.push(format!(
                "**Journal mode:** ⚠ {mode} — WAL not active, so reads can block on a concurrent write (WAL appears unsupported on this filesystem)"
            ));
        }

        lines.push(String::new());
        lines.push("### Nodes by Kind:".to_string());

        // TS iterates Object.entries insertion order (the SQL GROUP BY order);
        // sort keys for determinism here.
        let mut kinds: Vec<(&String, &u64)> = stats.nodes_by_kind.iter().collect();
        kinds.sort_by(|a, b| a.0.cmp(b.0));
        for (kind, count) in kinds {
            if *count > 0 {
                lines.push(format!("- {kind}: {count}"));
            }
        }

        lines.push(String::new());
        lines.push("### Languages:".to_string());
        let mut langs: Vec<(&String, &u64)> = stats.files_by_language.iter().collect();
        langs.sort_by(|a, b| a.0.cmp(b.0));
        for (lang, count) in langs {
            if *count > 0 {
                lines.push(format!("- {lang}: {count}"));
            }
        }

        // Per-file freshness (#403).
        let pending = cg.get_pending_files();
        if !pending.is_empty() {
            lines.push(String::new());
            lines.push("### Pending sync:".to_string());
            let now = now_ms();
            for p in &pending {
                let age_ms = (now - p.last_seen_ms).max(0);
                let label = if p.indexing {
                    "indexing in progress"
                } else {
                    "pending sync"
                };
                lines.push(format!("- {} (edited {}ms ago, {})", p.path, age_ms, label));
            }
        }

        Ok(self.text_result(&lines.join("\n")))
    }

    // =========================================================================
    // codegraph_files
    // =========================================================================

    fn handle_files(&self, args: &Map<String, Value>) -> Result<ToolResult> {
        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let path_filter = args.get("path").and_then(|v| v.as_str());
        let pattern = args.get("pattern").and_then(|v| v.as_str());
        let format = args
            .get("format")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("tree");
        let include_metadata = args.get("includeMetadata") != Some(&Value::Bool(false));
        let max_depth: Option<usize> = match args.get("maxDepth") {
            None | Some(Value::Null) => None,
            Some(v) => v.as_f64().map(|d| clamp(d, 1.0, 20.0) as usize),
        };

        // Get all files from the index
        struct FileEntry {
            path: String,
            language: String,
            node_count: u32,
        }
        let all_files: Vec<FileEntry> = cg
            .get_files()?
            .into_iter()
            .map(|f| FileEntry {
                path: f.path,
                language: f.language.as_str().to_string(),
                node_count: f.node_count,
            })
            .collect();

        if all_files.is_empty() {
            return Ok(self.text_result("No files indexed. Run `codegraph index` first."));
        }

        // Filter by path prefix, normalizing root-ish and Windows-style
        // variants (#426).
        let normalized_filter: String = match path_filter {
            Some(pf) if !pf.is_empty() => {
                let s = pf.replace('\\', "/");
                let s = LEADING_DOT_SLASH_RE.replace(&s, "").to_string();
                let s = if s == "." { String::new() } else { s };
                s.trim_end_matches('/').to_string()
            }
            _ => String::new(),
        };
        let mut files: Vec<&FileEntry> = if !normalized_filter.is_empty() {
            all_files
                .iter()
                .filter(|f| {
                    f.path == normalized_filter
                        || f.path.starts_with(&format!("{normalized_filter}/"))
                })
                .collect()
        } else {
            all_files.iter().collect()
        };

        // Filter by glob pattern
        if let Some(pattern) = pattern.filter(|p| !p.is_empty()) {
            let regex = glob_to_regex(pattern)?;
            files.retain(|f| regex.is_match(&f.path));
        }

        if files.is_empty() {
            return Ok(self.text_result("No files found matching the criteria."));
        }

        let triples: Vec<(&str, &str, u32)> = files
            .iter()
            .map(|f| (f.path.as_str(), f.language.as_str(), f.node_count))
            .collect();

        // Format output
        let output = match format {
            "flat" => self.format_files_flat(&triples, include_metadata),
            "grouped" => self.format_files_grouped(&triples, include_metadata),
            _ => self.format_files_tree(&triples, include_metadata, max_depth),
        };

        Ok(self.text_result(&self.truncate_output(&output)))
    }

    /// Format files as a flat list.
    fn format_files_flat(&self, files: &[(&str, &str, u32)], include_metadata: bool) -> String {
        let mut lines: Vec<String> = vec![format!("## Files ({})", files.len()), String::new()];

        let mut sorted: Vec<&(&str, &str, u32)> = files.iter().collect();
        sorted.sort_by(|a, b| locale_cmp(a.0, b.0));
        for (path, language, node_count) in sorted {
            if include_metadata {
                lines.push(format!("- {path} ({language}, {node_count} symbols)"));
            } else {
                lines.push(format!("- {path}"));
            }
        }

        lines.join("\n")
    }

    /// Format files grouped by language.
    fn format_files_grouped(&self, files: &[(&str, &str, u32)], include_metadata: bool) -> String {
        let mut lang_order: Vec<String> = Vec::new();
        let mut by_lang: HashMap<String, Vec<(&str, &str, u32)>> = HashMap::new();
        for f in files {
            if !by_lang.contains_key(f.1) {
                lang_order.push(f.1.to_string());
            }
            by_lang.entry(f.1.to_string()).or_default().push(*f);
        }

        let mut lines: Vec<String> = vec![
            format!("## Files by Language ({} total)", files.len()),
            String::new(),
        ];

        // Sort languages by file count (descending), stable.
        let mut sorted_langs = lang_order;
        sorted_langs.sort_by(|a, b| by_lang[b].len().cmp(&by_lang[a].len()));

        for lang in &sorted_langs {
            let lang_files = &by_lang[lang];
            lines.push(format!("### {} ({})", lang, lang_files.len()));
            let mut sorted: Vec<&(&str, &str, u32)> = lang_files.iter().collect();
            sorted.sort_by(|a, b| locale_cmp(a.0, b.0));
            for (path, _language, node_count) in sorted {
                if include_metadata {
                    lines.push(format!("- {path} ({node_count} symbols)"));
                } else {
                    lines.push(format!("- {path}"));
                }
            }
            lines.push(String::new());
        }

        lines.join("\n")
    }

    /// Format files as a tree structure.
    fn format_files_tree(
        &self,
        files: &[(&str, &str, u32)],
        include_metadata: bool,
        max_depth: Option<usize>,
    ) -> String {
        struct TreeNode {
            name: String,
            children: Vec<TreeNode>,
            child_index: HashMap<String, usize>,
            file: Option<(String, u32)>,
        }
        impl TreeNode {
            fn new(name: &str) -> Self {
                TreeNode {
                    name: name.to_string(),
                    children: Vec::new(),
                    child_index: HashMap::new(),
                    file: None,
                }
            }
        }

        let mut root = TreeNode::new("");

        for (path, language, node_count) in files {
            let parts: Vec<&str> = path.split('/').collect();
            let mut current = &mut root;

            for (i, part) in parts.iter().enumerate() {
                if part.is_empty() {
                    continue;
                }
                let idx = match current.child_index.get(*part) {
                    Some(&idx) => idx,
                    None => {
                        current.children.push(TreeNode::new(part));
                        let idx = current.children.len() - 1;
                        current.child_index.insert(part.to_string(), idx);
                        idx
                    }
                };
                current = &mut current.children[idx];

                // If this is the last part, it's a file
                if i == parts.len() - 1 {
                    current.file = Some((language.to_string(), *node_count));
                }
            }
        }

        let mut lines: Vec<String> = vec![
            format!("## Project Structure ({} files)", files.len()),
            String::new(),
        ];

        fn render_node(
            node: &TreeNode,
            prefix: &str,
            is_last: bool,
            depth: usize,
            max_depth: Option<usize>,
            include_metadata: bool,
            lines: &mut Vec<String>,
        ) {
            if let Some(md) = max_depth {
                if depth > md {
                    return;
                }
            }

            let connector = if is_last { "└── " } else { "├── " };
            let child_prefix = if is_last { "    " } else { "│   " };

            if !node.name.is_empty() {
                let mut line = format!("{prefix}{connector}{}", node.name);
                if let (Some((language, node_count)), true) = (&node.file, include_metadata) {
                    line.push_str(&format!(" ({language}, {node_count} symbols)"));
                }
                lines.push(line);
            }

            let mut children: Vec<&TreeNode> = node.children.iter().collect();
            // Sort: directories first, then files, both alphabetically
            children.sort_by(|a, b| {
                let a_is_dir = !a.children.is_empty() && a.file.is_none();
                let b_is_dir = !b.children.is_empty() && b.file.is_none();
                if a_is_dir != b_is_dir {
                    return if a_is_dir {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Greater
                    };
                }
                locale_cmp(&a.name, &b.name)
            });

            let count = children.len();
            for (i, child) in children.into_iter().enumerate() {
                let next_prefix = if !node.name.is_empty() {
                    format!("{prefix}{child_prefix}")
                } else {
                    prefix.to_string()
                };
                render_node(
                    child,
                    &next_prefix,
                    i == count - 1,
                    depth + 1,
                    max_depth,
                    include_metadata,
                    lines,
                );
            }
        }

        render_node(&root, "", true, 0, max_depth, include_metadata, &mut lines);

        lines.join("\n")
    }

    // =========================================================================
    // Symbol resolution helpers
    // =========================================================================

    /// Check if a node matches a symbol query (simple names plus dotted /
    /// colon-pair / slash qualifiers; Rust path prefixes stripped).
    fn matches_symbol(&self, node: &Node, symbol: &str) -> bool {
        // Simple name match
        if node.name == symbol {
            return true;
        }
        // File basename match (e.g., "product-card" matches "product-card.liquid")
        if node.kind == NodeKind::File && EXT_STRIP_RE.replace(&node.name, "") == symbol {
            return true;
        }

        // Qualified-name lookups
        if !(symbol.contains('.') || symbol.contains('/') || symbol.contains("::")) {
            return false;
        }
        let parts: Vec<&str> = QUALIFIER_SPLIT_RE
            .split(symbol)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() < 2 {
            return false;
        }

        let last_part = parts[parts.len() - 1];
        if node.name != last_part {
            return false;
        }

        // Stage 1: qualified-name suffix match.
        let colon_suffix = parts.join("::");
        if node.qualified_name.contains(&colon_suffix) {
            return true;
        }

        // Stage 2: file-path containment.
        let container_hints: Vec<&str> = parts[..parts.len() - 1]
            .iter()
            .filter(|p| !RUST_PATH_PREFIXES.contains(p))
            .copied()
            .collect();
        if container_hints.is_empty() {
            return false;
        }

        let segments: Vec<&str> = node
            .file_path
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        container_hints.iter().all(|hint| {
            segments
                .iter()
                .any(|seg| seg == hint || EXT_STRIP_RE.replace(seg, "") == *hint)
        })
    }

    /// Find ALL definitions matching a name, ranked, so codegraph_node can
    /// return every overload instead of guessing one.
    fn find_symbol_matches(&self, cg: &CodeGraph, symbol: &str) -> Result<Vec<Node>> {
        let is_qualified = symbol.contains('.') || symbol.contains('/') || symbol.contains("::");

        // For a bare name, enumerate EVERY exact-name definition via the
        // direct index (not FTS, which caps + ranks).
        if !is_qualified {
            let exact = cg.get_nodes_by_name(symbol)?;
            if !exact.is_empty() {
                let mut sorted = exact;
                sorted.sort_by_key(|n| {
                    if is_generated_file(&n.file_path) {
                        1
                    } else {
                        0
                    }
                });
                return Ok(sorted);
            }
            // No exact match — use the single top fuzzy result.
            let fuzzy = cg.search_nodes(
                symbol,
                Some(&SearchOptions {
                    limit: Some(10),
                    ..Default::default()
                }),
            )?;
            return Ok(fuzzy.into_iter().take(1).map(|r| r.node).collect());
        }

        // Qualified lookup: FTS + matches_symbol.
        let limit = 50;
        let mut results = cg.search_nodes(
            symbol,
            Some(&SearchOptions {
                limit: Some(limit),
                ..Default::default()
            }),
        )?;

        // FTS strips colons — re-search by the bare last part.
        if results.is_empty() {
            let tail = last_qualifier_part(symbol);
            if !tail.is_empty() && tail != symbol {
                results = cg.search_nodes(
                    &tail,
                    Some(&SearchOptions {
                        limit: Some(limit),
                        ..Default::default()
                    }),
                )?;
            }
        }

        if results.is_empty() {
            return Ok(Vec::new());
        }

        let exact_matches: Vec<&SearchResult> = results
            .iter()
            .filter(|r| self.matches_symbol(&r.node, symbol))
            .collect();
        if exact_matches.is_empty() {
            // A qualified lookup must not fall back to a fuzzy file hit (#173).
            return Ok(Vec::new());
        }

        // Down-rank generated files.
        let mut ranked: Vec<Node> = exact_matches.into_iter().map(|r| r.node.clone()).collect();
        ranked.sort_by_key(|n| {
            if is_generated_file(&n.file_path) {
                1
            } else {
                0
            }
        });
        Ok(ranked)
    }

    /// Find ALL symbols matching a name. Used by callers/callees/impact to
    /// aggregate results across all matching symbols.
    fn find_all_symbols(&self, cg: &CodeGraph, symbol: &str) -> Result<SymbolMatches> {
        let mut results = cg.search_nodes(
            symbol,
            Some(&SearchOptions {
                limit: Some(50),
                ..Default::default()
            }),
        )?;

        // Mirror the fallback for qualified queries.
        if results.is_empty()
            && (symbol.contains('.') || symbol.contains('/') || symbol.contains("::"))
        {
            let tail = last_qualifier_part(symbol);
            if !tail.is_empty() && tail != symbol {
                results = cg.search_nodes(
                    &tail,
                    Some(&SearchOptions {
                        limit: Some(50),
                        ..Default::default()
                    }),
                )?;
            }
        }

        if results.is_empty() {
            return Ok(SymbolMatches {
                nodes: Vec::new(),
                note: String::new(),
            });
        }

        let exact_matches: Vec<&SearchResult> = results
            .iter()
            .filter(|r| self.matches_symbol(&r.node, symbol))
            .collect();

        if exact_matches.len() <= 1 {
            let node = exact_matches
                .first()
                .map(|r| r.node.clone())
                .unwrap_or_else(|| results[0].node.clone());
            return Ok(SymbolMatches {
                nodes: vec![node],
                note: String::new(),
            });
        }

        // Same generated-file down-rank as find_symbol_matches.
        let mut ranked: Vec<Node> = exact_matches.into_iter().map(|r| r.node.clone()).collect();
        ranked.sort_by_key(|n| {
            if is_generated_file(&n.file_path) {
                1
            } else {
                0
            }
        });

        let locations: Vec<String> = ranked
            .iter()
            .map(|n| format!("{} at {}:{}", n.kind.as_str(), n.file_path, n.start_line))
            .collect();
        let note = format!(
            "\n\n> **Note:** Aggregated results across {} symbols named \"{}\": {}",
            ranked.len(),
            symbol,
            locations.join(", ")
        );
        Ok(SymbolMatches {
            nodes: ranked,
            note,
        })
    }

    /// Truncate output if it exceeds the maximum length.
    fn truncate_output(&self, text: &str) -> String {
        if text.len() <= MAX_OUTPUT_LENGTH {
            return text.to_string();
        }
        let truncated = &text[..floor_char_boundary(text, MAX_OUTPUT_LENGTH)];
        let last_newline = truncated.rfind('\n');
        let cut_point = match last_newline {
            Some(pos) if (pos as f64) > MAX_OUTPUT_LENGTH as f64 * 0.8 => pos,
            _ => truncated.len(),
        };
        format!("{}\n\n... (output truncated)", &truncated[..cut_point])
    }

    // =========================================================================
    // Formatting helpers (compact by default to reduce context usage)
    // =========================================================================

    fn format_search_results(&self, results: &[SearchResult]) -> String {
        let mut lines: Vec<String> = vec![
            format!("## Search Results ({} found)", results.len()),
            String::new(),
        ];

        for result in results {
            let node = &result.node;
            let location = if node.start_line > 0 {
                format!(":{}", node.start_line)
            } else {
                String::new()
            };
            // Compact format: one line per result with key info
            lines.push(format!("### {} ({})", node.name, node.kind.as_str()));
            lines.push(format!("{}{}", node.file_path, location));
            if let Some(sig) = &node.signature {
                if !sig.is_empty() {
                    lines.push(format!("`{sig}`"));
                }
            }
            lines.push(String::new());
        }

        lines.join("\n")
    }

    fn format_node_list(&self, nodes: &[Node], title: &str) -> String {
        let mut lines: Vec<String> = vec![
            format!("## {} ({} found)", title, nodes.len()),
            String::new(),
        ];

        for node in nodes {
            let location = if node.start_line > 0 {
                format!(":{}", node.start_line)
            } else {
                String::new()
            };
            // Compact: just name, kind, location
            lines.push(format!(
                "- {} ({}) - {}{}",
                node.name,
                node.kind.as_str(),
                node.file_path,
                location
            ));
        }

        lines.join("\n")
    }

    fn format_impact(&self, symbol: &str, nodes: &OrderedNodeMap) -> String {
        let node_count = nodes.len();

        // Compact format: just list affected symbols grouped by file
        let mut lines: Vec<String> = vec![
            format!("## Impact: \"{symbol}\" affects {node_count} symbols"),
            String::new(),
        ];

        // Group by file
        let mut file_order: Vec<String> = Vec::new();
        let mut by_file: HashMap<String, Vec<&Node>> = HashMap::new();
        for node in nodes.values() {
            if !by_file.contains_key(&node.file_path) {
                file_order.push(node.file_path.clone());
            }
            by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node);
        }

        for file in &file_order {
            lines.push(format!("**{file}:**"));
            let node_list = by_file[file]
                .iter()
                .map(|n| format!("{}:{}", n.name, n.start_line))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(node_list);
            lines.push(String::new());
        }

        lines.join("\n")
    }

    /// Build a compact structural outline of a container symbol from its
    /// indexed children. Returns "" when the container has no indexed
    /// children.
    fn build_container_outline(&self, cg: &CodeGraph, node: &Node) -> Result<String> {
        let mut children: Vec<Node> = cg
            .get_children(&node.id)?
            .into_iter()
            .filter(|c| c.kind != NodeKind::Import && c.kind != NodeKind::Export)
            .collect();
        children.sort_by_key(|c| c.start_line);
        if children.is_empty() {
            return Ok(String::new());
        }

        let mut lines: Vec<String> =
            vec![format!("**Members ({}):**", children.len()), String::new()];
        for c in &children {
            let loc = if c.start_line > 0 {
                format!(":{}", c.start_line)
            } else {
                String::new()
            };
            let sig = c
                .signature
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| format!(" — `{s}`"))
                .unwrap_or_default();
            lines.push(format!("- {} ({}){}{}", c.name, c.kind.as_str(), loc, sig));
        }
        Ok(lines.join("\n"))
    }

    fn format_node_details(
        &self,
        node: &Node,
        code: Option<&str>,
        outline: Option<&str>,
    ) -> String {
        let location = if node.start_line > 0 {
            format!(":{}", node.start_line)
        } else {
            String::new()
        };
        let mut lines: Vec<String> = vec![
            format!("## {} ({})", node.name, node.kind.as_str()),
            String::new(),
            format!("**Location:** {}{}", node.file_path, location),
        ];

        if let Some(sig) = &node.signature {
            if !sig.is_empty() {
                lines.push(format!("**Signature:** `{sig}`"));
            }
        }

        // Only include docstring if it's short and useful
        if let Some(doc) = &node.docstring {
            if !doc.is_empty() && doc.chars().count() < 200 {
                lines.push(String::new());
                lines.push(doc.clone());
            }
        }

        if let Some(outline) = outline {
            lines.push(String::new());
            lines.push(outline.to_string());
            lines.push(String::new());
            lines.push(format!(
                "> Structural outline only. Read `{}` or call codegraph_node on a specific member for its body.",
                node.file_path
            ));
        } else if let Some(code) = code {
            // Line-numbered (cat -n style) so the agent can cite/edit exact
            // lines without re-Reading the file for them.
            let numbered = if node.start_line > 0 {
                number_source_lines(code, node.start_line as usize)
            } else {
                code.to_string()
            };
            lines.push(String::new());
            lines.push(format!("```{}", node.language.as_str()));
            lines.push(numbered);
            lines.push("```".to_string());
        }

        lines.join("\n")
    }

    fn text_result(&self, text: &str) -> ToolResult {
        ToolResult {
            content: vec![ToolContent {
                content_type: "text".into(),
                text: text.to_string(),
            }],
            is_error: None,
        }
    }

    fn error_result(&self, message: &str) -> ToolResult {
        ToolResult {
            content: vec![ToolContent {
                content_type: "text".into(),
                text: format!("Error: {message}"),
            }],
            is_error: Some(true),
        }
    }
}

struct SymbolMatches {
    nodes: Vec<Node>,
    note: String,
}

/// Convert glob pattern to regex (TS `globToRegex` parity; unanchored test).
fn glob_to_regex(pattern: &str) -> Result<Regex> {
    // Escape special regex chars except * and ?
    static ESCAPE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.+^${}()|\[\]\\]").unwrap());
    let escaped = ESCAPE_RE.replace_all(pattern, r"\$0").to_string();
    let escaped = escaped.replace("**", "{{GLOBSTAR}}");
    let escaped = escaped.replace('*', "[^/]*");
    let escaped = escaped.replace('?', "[^/]");
    let escaped = escaped.replace("{{GLOBSTAR}}", ".*");
    Regex::new(&escaped).map_err(|e| CodeGraphError::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explore_budget_tiers_match_ts() {
        assert_eq!(get_explore_budget(0), 1);
        assert_eq!(get_explore_budget(499), 1);
        assert_eq!(get_explore_budget(500), 2);
        assert_eq!(get_explore_budget(4999), 2);
        assert_eq!(get_explore_budget(5000), 3);
        assert_eq!(get_explore_budget(14999), 3);
        assert_eq!(get_explore_budget(15000), 4);
        assert_eq!(get_explore_budget(24999), 4);
        assert_eq!(get_explore_budget(25000), 5);
        assert_eq!(get_explore_budget(u64::MAX), 5);
    }

    #[test]
    fn output_budget_max_chars_per_file_is_monotonic_across_tiers() {
        // The invariant that motivated the doc: a larger tier must never get a
        // smaller max_chars_per_file than a smaller tier.
        let tiers = [0u64, 149, 150, 499, 500, 4999, 5000, 14999, 15000, 30000];
        let mut prev = 0usize;
        for t in tiers {
            let b = get_explore_output_budget(t);
            assert!(
                b.max_chars_per_file >= prev,
                "max_chars_per_file regressed at tier {t}: {} < {prev}",
                b.max_chars_per_file
            );
            prev = b.max_chars_per_file;
        }
    }

    #[test]
    fn output_budget_tier_values_are_digit_for_digit() {
        let t0 = get_explore_output_budget(100);
        assert_eq!(
            (
                t0.max_output_chars,
                t0.default_max_files,
                t0.max_chars_per_file,
                t0.gap_threshold
            ),
            (13000, 4, 3800, 7)
        );
        assert!(t0.exclude_low_value_files);
        let t1 = get_explore_output_budget(300);
        assert_eq!(
            (
                t1.max_output_chars,
                t1.default_max_files,
                t1.max_chars_per_file,
                t1.gap_threshold
            ),
            (18000, 5, 3800, 8)
        );
        let t2 = get_explore_output_budget(1000);
        assert_eq!(
            (
                t2.max_output_chars,
                t2.default_max_files,
                t2.max_chars_per_file,
                t2.gap_threshold
            ),
            (24000, 8, 6500, 12)
        );
        let t3 = get_explore_output_budget(10000);
        assert_eq!(
            (
                t3.max_output_chars,
                t3.default_max_files,
                t3.max_chars_per_file,
                t3.gap_threshold
            ),
            (24000, 8, 7000, 15)
        );
        let t4 = get_explore_output_budget(30000);
        assert_eq!(t3.max_output_chars, t4.max_output_chars);
        assert_eq!(t4.max_chars_per_file, 7000);
        assert!(!t4.exclude_low_value_files);
    }

    #[test]
    fn glob_to_regex_matches_like_ts() {
        let re = glob_to_regex("*.tsx").unwrap();
        assert!(re.is_match("src/App.tsx"));
        assert!(!re.is_match("src/App.ts"));
        let re = glob_to_regex("**/*.test.ts").unwrap();
        assert!(re.is_match("src/deep/x.test.ts"));
        let re = glob_to_regex("src/*.ts").unwrap();
        assert!(re.is_match("src/a.ts"));
        assert!(!re.is_match("src/sub/a.ts"));
    }

    #[test]
    fn locale_string_groups_thousands() {
        assert_eq!(to_locale_string(0), "0");
        assert_eq!(to_locale_string(999), "999");
        assert_eq!(to_locale_string(1000), "1,000");
        assert_eq!(to_locale_string(1234567), "1,234,567");
    }

    #[test]
    fn last_qualifier_part_handles_separators() {
        assert_eq!(last_qualifier_part("a.b.c"), "c");
        assert_eq!(last_qualifier_part("a::b::c"), "c");
        assert_eq!(last_qualifier_part("a/b"), "b");
        assert_eq!(last_qualifier_part("plain"), "plain");
    }

    #[test]
    fn number_source_lines_is_cat_n_style() {
        assert_eq!(number_source_lines("a\nb", 5), "5\ta\n6\tb");
    }

    #[test]
    fn symbol_token_extraction_strips_extensions_and_dedupes() {
        let toks =
            extract_symbol_tokens("AuthService loginUser session-manager Create.cs loginUser");
        assert!(toks.contains(&"AuthService".to_string()));
        assert!(toks.contains(&"loginUser".to_string()));
        assert!(toks.contains(&"Create".to_string()));
        // hyphenated term fails the identifier regex
        assert!(!toks.iter().any(|t| t.contains('-')));
        // deduped
        assert_eq!(toks.iter().filter(|t| *t == "loginUser").count(), 1);
    }

    #[test]
    fn tool_definition_json_is_wire_compatible_with_ts() {
        // codegraph_search serialized: camelCase inputSchema, properties in TS
        // literal order, per-property keys in (type, description, enum?,
        // default?) order, required present.
        let defs = tools();
        assert_eq!(defs.len(), 8);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "codegraph_search",
                "codegraph_callers",
                "codegraph_callees",
                "codegraph_impact",
                "codegraph_node",
                "codegraph_explore",
                "codegraph_status",
                "codegraph_files"
            ]
        );
        let json = serde_json::to_string(&defs[0]).unwrap();
        // Top-level key order + camelCase inputSchema.
        let name_i = json.find("\"name\"").unwrap();
        let desc_i = json.find("\"description\"").unwrap();
        let schema_i = json.find("\"inputSchema\"").unwrap();
        assert!(name_i < desc_i && desc_i < schema_i);
        // Property order: query, kind, limit, projectPath.
        let q = json.find("\"query\"").unwrap();
        let k = json.find("\"kind\"").unwrap();
        let l = json.find("\"limit\"").unwrap();
        let p = json.find("\"projectPath\"").unwrap();
        assert!(q < k && k < l && l < p);
        // kind carries its enum; limit its default.
        assert!(json.contains("\"enum\":[\"function\",\"method\",\"class\",\"interface\",\"type\",\"variable\",\"route\",\"component\"]"));
        assert!(json.contains("\"default\":10"));
        assert!(json.contains("\"required\":[\"query\"]"));
        // status has no `required` key at all (TS omits it).
        let status = serde_json::to_value(&defs[6]).unwrap();
        assert!(status["inputSchema"].get("required").is_none());
    }

    #[test]
    fn tool_result_json_omits_is_error_on_success() {
        let ok = ToolResult {
            content: vec![ToolContent {
                content_type: "text".into(),
                text: "hi".into(),
            }],
            is_error: None,
        };
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"content":[{"type":"text","text":"hi"}]}"#
        );
        let err = ToolResult {
            content: vec![ToolContent {
                content_type: "text".into(),
                text: "Error: x".into(),
            }],
            is_error: Some(true),
        };
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            r#"{"content":[{"type":"text","text":"Error: x"}],"isError":true}"#
        );
    }
}
