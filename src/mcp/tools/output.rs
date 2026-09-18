//! Structured payloads of the MCP tools and the output schemas that declare
//! them. Every payload is what goes on the wire as compact JSON (see
//! `ToolResult::into_mcp_projection`), so fields that repeat the request or
//! each other are left out, and every payload is bounded by the MCP output
//! budget with an explicit `truncated` flag rather than silently cut.
//!
//! Every success schema also declares `notices` (see [`notices`]): the
//! projection adds it to a payload only when something makes the result
//! suspect, so a normal result never carries it.

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::format::{json_len, rows_within_budget};
use crate::types::{GraphStats, SearchResult};

mod notices;
mod rows;
use notices::notices_schema;
pub(in crate::mcp::tools) use notices::{attach_notices, notice_banner, notice_outputs};
pub(in crate::mcp::tools) use rows::{SymbolRef, SymbolRow};
use rows::{symbol_ref_schema, symbol_row_properties, symbol_row_required};

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

// =============================================================================
// codegraph_search

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct SearchHitOutput {
    #[serde(flatten)]
    pub symbol: SymbolRow,
    /// Which of a batch query's names this hit answered. Absent for a single
    /// query, where it would repeat the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_query: Option<String>,
}

impl From<&SearchResult> for SearchHitOutput {
    fn from(result: &SearchResult) -> Self {
        Self {
            symbol: SymbolRow::new(&result.node),
            matched_query: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct SearchOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    /// Ranked best first; the order is the score.
    pub results: Vec<SearchHitOutput>,
    /// Names of a batch query that matched nothing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched: Vec<String>,
    /// Trailing rows were dropped to stay within the output budget.
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

impl SearchOutput {
    pub fn new(results: &[SearchResult]) -> Self {
        Self {
            schema_version: 2,
            kind: "search",
            results: results.iter().map(SearchHitOutput::from).collect(),
            unmatched: Vec::new(),
            truncated: false,
        }
    }

    /// One call, several names — agents otherwise emulate this with a regex
    /// alternation through grep (82% of the greps in the mined sessions).
    pub fn new_batch(hits: Vec<(String, SearchResult)>, unmatched: Vec<String>) -> Self {
        Self {
            schema_version: 2,
            kind: "search",
            results: hits
                .iter()
                .map(|(matched, result)| SearchHitOutput {
                    matched_query: Some(matched.clone()),
                    ..SearchHitOutput::from(result)
                })
                .collect(),
            unmatched,
            truncated: false,
        }
    }

    /// Keep the leading (best-ranked) rows that fit `budget`.
    pub fn fit_to(&mut self, budget: usize) {
        let rows = std::mem::take(&mut self.results);
        self.truncated = true;
        let fixed = json_len(self);
        let keep = rows_within_budget(budget, fixed, rows.iter().map(json_len));
        self.truncated = keep < rows.len();
        self.results = rows;
        self.results.truncate(keep);
    }
}

// =============================================================================
// codegraph_node — symbol mode

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NodeDetailOutput {
    #[serde(flatten)]
    pub symbol: SymbolRow,
    /// Verbatim source of the definition (not line-numbered; it starts at
    /// `line`, or at `codeStartLine` when that is present).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// First line of `code`, when it is not `line` (a file that changed on
    /// disk since it was indexed is shown whole, from line 1) or the budget
    /// cut `code` short.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_start_line: Option<usize>,
    /// Last line of `code` when the output budget cut it short.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_end_line: Option<usize>,
    /// The output budget cut `code` short (see `codeEndLine`) or withheld it.
    #[serde(skip_serializing_if = "is_false")]
    pub code_truncated: bool,
    /// The definition's lines were already sent in this session and the file
    /// is unchanged on disk, so `code` is not repeated.
    #[serde(skip_serializing_if = "is_false")]
    pub already_sent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub callers: Vec<SymbolRef>,
    #[serde(skip_serializing_if = "is_zero")]
    pub callers_omitted: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub callees: Vec<SymbolRef>,
    #[serde(skip_serializing_if = "is_zero")]
    pub callees_omitted: usize,
}

impl NodeDetailOutput {
    pub fn bare(symbol: SymbolRow) -> Self {
        Self {
            symbol,
            code: None,
            code_start_line: None,
            code_end_line: None,
            code_truncated: false,
            already_sent: false,
            outline: None,
            callers: Vec::new(),
            callers_omitted: 0,
            callees: Vec::new(),
            callees_omitted: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NodeOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    /// Definitions that matched, including any not returned in `matches`.
    pub match_count: usize,
    /// Some matches, or their code, were left out (relevance cap or budget).
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
    pub matches: Vec<NodeDetailOutput>,
}

impl NodeOutput {
    pub fn new(match_count: usize, truncated: bool, matches: Vec<NodeDetailOutput>) -> Self {
        Self {
            schema_version: 2,
            kind: "node",
            match_count,
            truncated,
            matches,
        }
    }
}

/// Bound a symbol-mode `node` payload (single or batch) to `budget`: first
/// withhold code and outlines from the trailing matches, then cut the first
/// match's code at a line boundary, then drop trailing matches. Code is only
/// ever cut between lines, never inside one, and every cut is flagged.
pub(in crate::mcp::tools) fn fit_node_payload(payload: &mut Value, budget: usize) {
    let count = payload["matches"].as_array().map_or(0, Vec::len);
    for index in (1..count).rev() {
        if json_len(payload) <= budget {
            return;
        }
        if withhold_code(&mut payload["matches"][index]) {
            payload["truncated"] = Value::Bool(true);
        }
    }
    if json_len(payload) <= budget || count == 0 {
        return;
    }
    if clip_code(payload, budget) {
        payload["truncated"] = Value::Bool(true);
    }
    while json_len(payload) > budget && payload["matches"].as_array().is_some_and(|m| m.len() > 1) {
        if let Some(matches) = payload["matches"].as_array_mut() {
            matches.pop();
        }
        payload["truncated"] = Value::Bool(true);
    }
}

/// Drop a match's code and outline, flagging it. Returns whether anything
/// was dropped.
fn withhold_code(detail: &mut Value) -> bool {
    let Some(detail) = detail.as_object_mut() else {
        return false;
    };
    let had_code = detail.remove("code").is_some();
    detail.remove("codeStartLine");
    detail.remove("codeEndLine");
    let had_outline = detail.remove("outline").is_some();
    if had_code || had_outline {
        detail.insert("codeTruncated".into(), Value::Bool(true));
    }
    had_code || had_outline
}

/// Keep as many whole leading lines of the first match's code as fit next to
/// the rest of the payload. Returns whether the match changed.
fn clip_code(payload: &mut Value, budget: usize) -> bool {
    let detail = &payload["matches"][0];
    let Some(code) = detail["code"].as_str().map(str::to_string) else {
        return withhold_code(&mut payload["matches"][0]);
    };
    let start = detail["codeStartLine"]
        .as_u64()
        .or_else(|| detail["line"].as_u64())
        .map_or(1, |line| line as usize);
    withhold_code(&mut payload["matches"][0]);
    // Room left for the code string plus the two line-range fields.
    let spare = budget.saturating_sub(json_len(payload) + 64);
    let mut kept = Vec::new();
    let mut used = 0usize;
    for line in code.split('\n') {
        // Escaped length of the line without its quotes, plus the `\n` joint.
        let cost = json_len(line).saturating_sub(2) + 2;
        if used + cost > spare {
            break;
        }
        used += cost;
        kept.push(line);
    }
    if !kept.is_empty() {
        let detail = &mut payload["matches"][0];
        detail["code"] = Value::String(kept.join("\n"));
        detail["codeStartLine"] = Value::from(start);
        detail["codeEndLine"] = Value::from(start + kept.len() - 1);
    }
    true
}

// =============================================================================
// codegraph_node — file view

/// One page of an indexed file, or its outline.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NodeFileOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub path: String,
    pub symbol_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<usize>,
    /// The window this reply covers (`source`, or `alreadySent`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    /// Verbatim lines `startLine..=endLine`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The window was already sent in this session and the file is unchanged
    /// on disk, so `source` is not repeated.
    #[serde(skip_serializing_if = "is_false")]
    pub already_sent: bool,
    /// The `offset` asked for, when it was past the end of the file and the
    /// window was moved back to the file's last lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_offset: Option<usize>,
    /// The output budget ended the window before `limit` lines; continue
    /// from `endLine + 1`.
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// Definitions that begin before the window and reach into it — what
    /// the first lines shown are part of.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub enclosing: Vec<SymbolRow>,
    /// The file's outline (`symbolsOnly`, or the keys of a data file).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<SymbolRow>,
    #[serde(skip_serializing_if = "is_false")]
    pub symbols_truncated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependents: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub dependents_omitted: usize,
    #[serde(skip_serializing_if = "is_false")]
    pub values_withheld: bool,
}

impl NodeFileOutput {
    pub fn new(path: String, symbol_count: usize) -> Self {
        Self {
            schema_version: 2,
            kind: "file",
            path,
            symbol_count,
            total_lines: None,
            start_line: None,
            end_line: None,
            source: None,
            already_sent: false,
            requested_offset: None,
            truncated: false,
            enclosing: Vec::new(),
            symbols: Vec::new(),
            symbols_truncated: false,
            dependents: Vec::new(),
            dependents_omitted: 0,
            values_withheld: false,
        }
    }

    /// Keep the leading outline rows that fit `budget`.
    pub fn fit_symbols_to(&mut self, budget: usize) {
        let rows = std::mem::take(&mut self.symbols);
        let was_truncated = self.symbols_truncated;
        self.symbols_truncated = true;
        let fixed = json_len(self) + r#","symbols":[]"#.len();
        let keep = rows_within_budget(budget, fixed, rows.iter().map(json_len));
        self.symbols_truncated = was_truncated || keep < rows.len();
        self.symbols = rows;
        self.symbols.truncate(keep);
    }
}

// =============================================================================
// codegraph_files

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct FileGroupOutput {
    pub language: String,
    pub count: usize,
}

/// One directory of a `files` listing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct FileDirOutput {
    /// Project-relative directory (`.` for the project root).
    pub path: String,
    /// Files directly in this directory: name → indexed symbol count.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub files: Map<String, Value>,
    /// Subdirectories deeper than `maxDepth`, collapsed: name → file count.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub dirs: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct FilesOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    /// Files matching `path`/`pattern`, whether listed or collapsed.
    pub total: usize,
    /// Depth below `path` that the listing stops at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
    /// `maxDepth` was chosen by the server so the listing fits the budget.
    #[serde(skip_serializing_if = "is_false")]
    pub auto_depth: bool,
    pub groups: Vec<FileGroupOutput>,
    pub dirs: Vec<FileDirOutput>,
    /// More entries follow; pass `nextCursor` as `cursor` for the next page.
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl FilesOutput {
    pub fn empty() -> Self {
        Self {
            schema_version: 2,
            kind: "files",
            total: 0,
            max_depth: None,
            auto_depth: false,
            groups: Vec::new(),
            dirs: Vec::new(),
            truncated: false,
            next_cursor: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct CountOutput {
    pub name: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct PendingSyncOutput {
    pub path: String,
    pub age_ms: i64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct StatusOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub files_indexed: u64,
    pub total_nodes: u64,
    pub total_edges: u64,
    pub database_size_bytes: u64,
    pub backend: String,
    pub journal_mode: String,
    pub nodes_by_kind: Vec<CountOutput>,
    pub files_by_language: Vec<CountOutput>,
    pub pending_sync: Vec<PendingSyncOutput>,
    pub auto_sync: AutoSyncOutput,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct AutoSyncOutput {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl StatusOutput {
    pub fn from_stats(
        stats: &GraphStats,
        backend: String,
        journal_mode: String,
        pending_sync: Vec<PendingSyncOutput>,
        auto_sync_disabled_reason: Option<String>,
    ) -> Self {
        Self {
            schema_version: 1,
            kind: "status",
            files_indexed: stats.file_count,
            total_nodes: stats.node_count,
            total_edges: stats.edge_count,
            database_size_bytes: stats.db_size_bytes,
            backend,
            journal_mode,
            nodes_by_kind: sorted_counts(&stats.nodes_by_kind),
            files_by_language: sorted_counts(&stats.files_by_language),
            pending_sync,
            auto_sync: AutoSyncOutput {
                enabled: auto_sync_disabled_reason.is_none(),
                reason: auto_sync_disabled_reason,
            },
        }
    }
}

fn sorted_counts(map: &std::collections::HashMap<String, u64>) -> Vec<CountOutput> {
    let mut counts: Vec<CountOutput> = map
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(name, count)| CountOutput {
            name: name.clone(),
            count: *count,
        })
        .collect();
    counts.sort_by(|a, b| a.name.cmp(&b.name));
    counts
}

pub(in crate::mcp::tools) fn search_output_schema() -> Value {
    let mut hit = symbol_row_properties(true);
    hit.insert("matchedQuery".into(), json!({ "type": "string" }));
    hit.insert("project".into(), json!({ "type": "string" }));
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "search" },
            "notices": notices_schema(),
            "results": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": Value::Object(hit),
                "required": symbol_row_required(true)
            }},
            "unmatched": { "type": "array", "items": { "type": "string" } },
            "failedProjects": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "project": { "type": "string" },
                    "message": { "type": "string" }
                },
                "required": ["project", "message"]
            }},
            "truncated": { "type": "boolean" }
        },
        "required": ["schemaVersion", "kind", "results"]
    }))
}

pub(in crate::mcp::tools) fn node_output_schema() -> Value {
    json!({
        "type": "object",
        "oneOf": [node_symbol_output_schema(), node_file_output_schema(), error_output_schema()]
    })
}

fn node_symbol_output_schema() -> Value {
    let mut detail = symbol_row_properties(true);
    for (name, schema) in [
        ("code", json!({ "type": "string" })),
        ("codeStartLine", json!({ "type": "integer" })),
        ("codeEndLine", json!({ "type": "integer" })),
        ("codeTruncated", json!({ "type": "boolean" })),
        ("alreadySent", json!({ "type": "boolean" })),
        ("outline", json!({ "type": "string" })),
        (
            "callers",
            json!({ "type": "array", "items": symbol_ref_schema() }),
        ),
        ("callersOmitted", json!({ "type": "integer" })),
        (
            "callees",
            json!({ "type": "array", "items": symbol_ref_schema() }),
        ),
        ("calleesOmitted", json!({ "type": "integer" })),
    ] {
        detail.insert(name.into(), schema);
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "node" },
            "notices": notices_schema(),
            "matchCount": { "type": "integer" },
            "truncated": { "type": "boolean" },
            "matches": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": Value::Object(detail),
                "required": symbol_row_required(true)
            }}
        },
        "required": ["schemaVersion", "kind", "matchCount", "matches"]
    })
}

fn node_file_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "file" },
            "notices": notices_schema(),
            "path": { "type": "string" },
            "symbolCount": { "type": "integer" },
            "totalLines": { "type": "integer" },
            "startLine": { "type": "integer" },
            "endLine": { "type": "integer" },
            "source": { "type": "string" },
            "alreadySent": { "type": "boolean" },
            "requestedOffset": { "type": "integer" },
            "truncated": { "type": "boolean" },
            "enclosing": { "type": "array", "items": rows::symbol_row_schema(false) },
            "symbols": { "type": "array", "items": rows::symbol_row_schema(false) },
            "symbolsTruncated": { "type": "boolean" },
            "dependents": { "type": "array", "items": { "type": "string" } },
            "dependentsOmitted": { "type": "integer" },
            "valuesWithheld": { "type": "boolean" }
        },
        "required": ["schemaVersion", "kind", "path", "symbolCount"]
    })
}

pub(in crate::mcp::tools) fn files_output_schema() -> Value {
    let counts = json!({ "type": "object", "additionalProperties": { "type": "integer" } });
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "files" },
            "notices": notices_schema(),
            "total": { "type": "integer" },
            "maxDepth": { "type": "integer" },
            "autoDepth": { "type": "boolean" },
            "groups": { "type": "array", "items": count_name_schema("language") },
            "dirs": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": { "type": "string" },
                    "files": counts,
                    "dirs": counts
                },
                "required": ["path"]
            }},
            "truncated": { "type": "boolean" },
            "nextCursor": { "type": "string" }
        },
        "required": ["schemaVersion", "kind", "total", "groups", "dirs"]
    }))
}

pub(in crate::mcp::tools) fn status_output_schema() -> Value {
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "status" },
            "notices": notices_schema(),
            "filesIndexed": { "type": "integer" },
            "totalNodes": { "type": "integer" },
            "totalEdges": { "type": "integer" },
            "databaseSizeBytes": { "type": "integer" },
            "backend": { "type": "string" },
            "journalMode": { "type": "string" },
            "nodesByKind": { "type": "array", "items": count_name_schema("name") },
            "filesByLanguage": { "type": "array", "items": count_name_schema("name") },
            "pendingSync": { "type": "array", "items": pending_sync_schema() }
            ,"autoSync": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "enabled": { "type": "boolean" },
                    "reason": { "type": "string" }
                },
                "required": ["enabled"]
            }
        },
        "required": ["schemaVersion", "kind", "filesIndexed", "totalNodes", "totalEdges", "databaseSizeBytes", "backend", "journalMode", "nodesByKind", "filesByLanguage", "pendingSync", "autoSync"]
    }))
}

pub(in crate::mcp::tools) fn explore_output_schema() -> Value {
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "explore" },
            "notices": notices_schema(),
            "query": { "type": "string" },
            "totalSymbols": { "type": "integer" },
            "totalFiles": { "type": "integer" },
            "filesIncluded": { "type": "integer" },
            "sourceFiles": { "type": "array", "items": source_file_schema() },
            "backReferences": { "type": "array", "items": back_reference_schema() },
            "relationships": { "type": "array", "items": relationship_schema() },
            "additionalFiles": { "type": "array", "items": additional_file_schema() },
            "literalMatches": { "type": "array", "items": literal_file_match_schema() },
            "trimmed": { "type": "boolean" },
            "filesOmitted": { "type": "integer" },
            "omissions": { "type": "array", "items": omission_schema() },
            "continuation": continuation_schema()
        },
        "required": ["schemaVersion", "kind", "query", "totalSymbols", "totalFiles", "filesIncluded", "sourceFiles", "relationships", "additionalFiles", "literalMatches", "trimmed", "filesOmitted", "omissions", "continuation"]
    }))
}

fn back_reference_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string" },
            "ranges": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "start": { "type": "integer" },
                    "end": { "type": "integer" }
                },
                "required": ["start", "end"]
            }},
            "symbols": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["path", "ranges", "symbols"]
    })
}

pub(in crate::mcp::tools) fn success_or_error(success: Value) -> Value {
    // The MCP spec requires a tool's `outputSchema` root to be an object schema
    // (`"type": "object"`); Claude Code rejects the whole tools/list otherwise
    // ("expected object at outputSchema.type"). Both branches are objects, so
    // declaring the root `type: object` alongside the discriminated `oneOf`
    // keeps the success/error union while satisfying the validator.
    json!({ "type": "object", "oneOf": [success, error_output_schema()] })
}

fn error_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "error" },
            "error": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "code": { "type": "string" },
                    "category": { "type": "string" },
                    "message": { "type": "string" },
                    "retryable": { "type": "boolean" },
                    "field": { "type": "string" },
                    "expected": { "type": "string" },
                    "receivedKind": { "type": "string" },
                    "hint": { "type": "string" }
                },
                "required": ["code", "category", "message", "retryable"]
            }
        },
        "required": ["schemaVersion", "kind", "error"]
    })
}

fn count_name_schema(name_field: &str) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(name_field.to_string(), json!({ "type": "string" }));
    properties.insert("count".to_string(), json!({ "type": "integer" }));
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": Value::Object(properties),
        "required": [name_field, "count"]
    })
}

fn pending_sync_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string" },
            "ageMs": { "type": "integer" },
            "status": { "type": "string" }
        },
        "required": ["path", "ageMs", "status"]
    })
}

fn source_file_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string" },
            "language": { "type": "string" },
            "chunks": { "type": "array", "items": source_chunk_schema() },
            "sourceTruncated": { "type": "boolean" }
        },
        "required": ["path", "language", "chunks", "sourceTruncated"]
    })
}

fn relationship_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kind": { "type": "string" },
            "source": { "type": "string" },
            "target": { "type": "string" }
        },
        "required": ["kind", "source", "target"]
    })
}

fn additional_file_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string" },
            "symbols": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["path", "symbols"]
    })
}

fn literal_file_match_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "filePath": { "type": "string" },
            "language": { "type": "string" },
            "lines": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "lineNumber": { "type": "integer" },
                        "text": { "type": "string" },
                        "terms": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["lineNumber", "text", "terms"]
                }
            }
        },
        "required": ["filePath", "language", "lines"]
    })
}

fn source_chunk_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "startLine": { "type": "integer" },
            "endLine": { "type": "integer" },
            "mode": { "enum": ["whole", "excerpt", "body", "signature"] },
            "symbols": { "type": "array", "items": { "type": "string" } },
            "source": { "type": "string" },
            "unicodeHazards": { "type": "array", "items": unicode_hazard_schema() }
        },
        "required": ["startLine", "endLine", "mode", "symbols", "source", "unicodeHazards"]
    })
}

fn unicode_hazard_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "codepoint": { "type": "integer" },
            "line": { "type": "integer" },
            "column": { "type": "integer" },
            "category": { "enum": ["bidi_control", "zero_width", "private_use", "noncharacter", "control_char"] }
        },
        "required": ["codepoint", "line", "column", "category"]
    })
}

fn omission_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string" },
            "reason": { "enum": ["max_files", "budget", "unavailable", "no_source", "stale_index"] },
            "symbols": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["path", "reason", "symbols"]
    })
}

fn continuation_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "suggestedQueries": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["suggestedQueries"]
    })
}
