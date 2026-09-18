use serde::Serialize;
use serde_json::{Value, json};

use super::format::output_char_cap;
use crate::types::{GraphStats, Node, SearchResult};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NodeSummary {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub language: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl From<&Node> for NodeSummary {
    fn from(node: &Node) -> Self {
        Self {
            id: node.id.clone(),
            kind: node.kind.as_str().to_string(),
            name: node.name.clone(),
            qualified_name: node.qualified_name.clone(),
            file_path: node.file_path.clone(),
            language: node.language.as_str().to_string(),
            start_line: node.start_line,
            end_line: node.end_line,
            signature: node.signature.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct SearchHitOutput {
    pub node: NodeSummary,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlights: Option<Vec<String>>,
    /// Which of a batch query's names this hit answered. Absent for a single
    /// query, where it would repeat `query`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_query: Option<String>,
}

impl From<&SearchResult> for SearchHitOutput {
    fn from(result: &SearchResult) -> Self {
        Self {
            node: NodeSummary::from(&result.node),
            score: result.score,
            highlights: result.highlights.clone(),
            matched_query: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct SearchOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub query: String,
    /// Present when the caller passed several names in one call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_kind: Option<String>,
    pub limit: usize,
    pub total: usize,
    pub results: Vec<SearchHitOutput>,
}

impl SearchOutput {
    pub fn new(
        query: String,
        filter_kind: Option<String>,
        limit: usize,
        results: &[SearchResult],
    ) -> Self {
        Self {
            schema_version: 1,
            kind: "search",
            query,
            queries: None,
            filter_kind,
            limit,
            total: results.len(),
            results: results.iter().map(SearchHitOutput::from).collect(),
        }
    }

    /// One call, several names — agents otherwise emulate this with a regex
    /// alternation through grep (82% of the greps in the mined sessions).
    pub fn new_batch(
        queries: Vec<String>,
        filter_kind: Option<String>,
        limit: usize,
        hits: Vec<(String, SearchResult)>,
    ) -> Self {
        Self {
            schema_version: 1,
            kind: "search",
            query: queries.join(", "),
            queries: Some(queries),
            filter_kind,
            limit,
            total: hits.len(),
            results: hits
                .iter()
                .map(|(matched, result)| SearchHitOutput {
                    matched_query: Some(matched.clone()),
                    ..SearchHitOutput::from(result)
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NodeDetailOutput {
    pub node: NodeSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline: Option<String>,
    pub callers: Vec<NodeSummary>,
    pub callees: Vec<NodeSummary>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NodeOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub query: String,
    pub include_code: bool,
    pub match_count: usize,
    pub returned_full_count: usize,
    pub truncated: bool,
    pub matches: Vec<NodeDetailOutput>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(in crate::mcp::tools) enum NodeSuccessOutput {
    Symbol(NodeOutput),
    File(NodeFileOutput),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NodeFileSourceChunkOutput {
    pub start_line: usize,
    pub end_line: usize,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NodeFileSymbolOutput {
    pub kind: String,
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl From<&Node> for NodeFileSymbolOutput {
    fn from(node: &Node) -> Self {
        Self {
            kind: node.kind.as_str().to_string(),
            name: node.name.clone(),
            start_line: node.start_line,
            end_line: node.end_line,
            signature: node.signature.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::mcp::tools) struct NodeFileMetadataOutput {
    pub path: String,
    pub language: String,
    pub symbol_count: usize,
    pub symbols: Vec<NodeFileSymbolOutput>,
    pub symbols_truncated: bool,
    pub dependents: Vec<String>,
    pub dependents_omitted: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NodeFileRangeOutput {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone)]
pub(in crate::mcp::tools) enum NodeFileContent {
    Source {
        chunks: Vec<NodeFileSourceChunkOutput>,
        source_truncated: bool,
        total_lines: usize,
        offset: usize,
        limit: usize,
    },
    /// The requested window was already sent in this session and the file is
    /// unchanged on disk, so the source is replaced by a back-reference.
    AlreadySent {
        ranges: Vec<NodeFileRangeOutput>,
        total_lines: usize,
        offset: usize,
        limit: usize,
    },
    SymbolsOnly,
    ValuesWithheld,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NodeFileOutput {
    schema_version: u32,
    kind: &'static str,
    path: String,
    language: String,
    symbol_count: usize,
    symbols: Vec<NodeFileSymbolOutput>,
    symbols_truncated: bool,
    dependents: Vec<String>,
    dependents_omitted: usize,
    source_chunks: Vec<NodeFileSourceChunkOutput>,
    source_truncated: bool,
    values_withheld: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    already_sent: Vec<NodeFileRangeOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_lines: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

impl NodeFileOutput {
    pub fn new(metadata: NodeFileMetadataOutput, content: NodeFileContent) -> Self {
        let mut already_sent = Vec::new();
        let (source_chunks, source_truncated, values_withheld, total_lines, offset, limit) =
            match content {
                NodeFileContent::Source {
                    chunks,
                    source_truncated,
                    total_lines,
                    offset,
                    limit,
                } => (
                    chunks,
                    source_truncated,
                    false,
                    Some(total_lines),
                    Some(offset),
                    Some(limit),
                ),
                NodeFileContent::AlreadySent {
                    ranges,
                    total_lines,
                    offset,
                    limit,
                } => {
                    already_sent = ranges;
                    (
                        Vec::new(),
                        false,
                        false,
                        Some(total_lines),
                        Some(offset),
                        Some(limit),
                    )
                }
                NodeFileContent::SymbolsOnly => (Vec::new(), false, false, None, None, None),
                NodeFileContent::ValuesWithheld => (Vec::new(), false, true, None, None, None),
            };
        Self {
            schema_version: 1,
            kind: "file",
            path: metadata.path,
            language: metadata.language,
            symbol_count: metadata.symbol_count,
            symbols: metadata.symbols,
            symbols_truncated: metadata.symbols_truncated,
            dependents: metadata.dependents,
            dependents_omitted: metadata.dependents_omitted,
            source_chunks,
            source_truncated,
            values_withheld,
            already_sent,
            total_lines,
            offset,
            limit,
        }
    }

    /// Enforce the opt-in structured-output cap without mutating metadata or
    /// pretending a shortened source chunk is complete. Whole chunks are
    /// withheld and `sourceTruncated` records the loss.
    pub fn cap_source_to_output_limit(&mut self) -> bool {
        let Some(cap) = output_char_cap() else {
            return true;
        };
        let serialized_len = |value: &Self| {
            serde_json::to_string(value)
                .map(|serialized| serialized.len())
                .unwrap_or(usize::MAX)
        };
        if serialized_len(self) <= cap {
            return true;
        }
        while serialized_len(self) > cap && !self.source_chunks.is_empty() {
            let largest = self
                .source_chunks
                .iter()
                .enumerate()
                .max_by_key(|(_, chunk)| chunk.source.len())
                .map(|(index, _)| index)
                .unwrap_or(0);
            self.source_chunks.remove(largest);
            self.source_truncated = true;
        }
        serialized_len(self) <= cap
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct FileOutput {
    pub path: String,
    pub language: String,
    pub node_count: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct FileGroupOutput {
    pub language: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct FilesOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    pub format: String,
    pub total: usize,
    pub files: Vec<FileOutput>,
    pub groups: Vec<FileGroupOutput>,
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
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "search" },
            "query": { "type": "string" },
            "queries": { "type": "array", "items": { "type": "string" } },
            "filterKind": { "type": "string" },
            "limit": { "type": "integer" },
            "total": { "type": "integer" },
            "results": { "type": "array", "items": search_hit_schema() }
        },
        "required": ["schemaVersion", "kind", "query", "limit", "total", "results"]
    }))
}

pub(in crate::mcp::tools) fn node_output_schema() -> Value {
    json!({
        "type": "object",
        "oneOf": [node_symbol_output_schema(), node_file_output_schema(), error_output_schema()]
    })
}

fn node_symbol_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "node" },
            "query": { "type": "string" },
            "includeCode": { "type": "boolean" },
            "matchCount": { "type": "integer" },
            "returnedFullCount": { "type": "integer" },
            "truncated": { "type": "boolean" },
            "matches": { "type": "array", "items": node_detail_schema() }
        },
        "required": ["schemaVersion", "kind", "query", "includeCode", "matchCount", "returnedFullCount", "truncated", "matches"]
    })
}

fn node_file_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "file" },
            "path": { "type": "string" },
            "language": { "type": "string" },
            "symbolCount": { "type": "integer" },
            "symbols": { "type": "array", "items": node_file_symbol_schema() },
            "symbolsTruncated": { "type": "boolean" },
            "dependents": { "type": "array", "items": { "type": "string" } },
            "dependentsOmitted": { "type": "integer" },
            "sourceChunks": { "type": "array", "items": node_file_source_chunk_schema() },
            "sourceTruncated": { "type": "boolean" },
            "valuesWithheld": { "type": "boolean" },
            "alreadySent": { "type": "array", "items": node_file_range_schema() },
            "totalLines": { "type": "integer" },
            "offset": { "type": "integer" },
            "limit": { "type": "integer" }
        },
        "required": ["schemaVersion", "kind", "path", "language", "symbolCount", "symbols", "symbolsTruncated", "dependents", "dependentsOmitted", "sourceChunks", "sourceTruncated", "valuesWithheld"]
    })
}

fn node_file_range_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "startLine": { "type": "integer" },
            "endLine": { "type": "integer" }
        },
        "required": ["startLine", "endLine"]
    })
}

fn node_file_source_chunk_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "startLine": { "type": "integer" },
            "endLine": { "type": "integer" },
            "source": { "type": "string" }
        },
        "required": ["startLine", "endLine", "source"]
    })
}

fn node_file_symbol_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kind": { "type": "string" },
            "name": { "type": "string" },
            "startLine": { "type": "integer" },
            "endLine": { "type": "integer" },
            "signature": { "type": "string" }
        },
        "required": ["kind", "name", "startLine", "endLine"]
    })
}

pub(in crate::mcp::tools) fn files_output_schema() -> Value {
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "files" },
            "pathFilter": { "type": "string" },
            "pattern": { "type": "string" },
            "format": { "type": "string" },
            "total": { "type": "integer" },
            "files": { "type": "array", "items": file_schema() },
            "groups": { "type": "array", "items": count_name_schema("language") }
        },
        "required": ["schemaVersion", "kind", "format", "total", "files", "groups"]
    }))
}

pub(in crate::mcp::tools) fn status_output_schema() -> Value {
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "status" },
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

fn success_or_error(success: Value) -> Value {
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

fn node_summary_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": { "type": "string" },
            "kind": { "type": "string" },
            "name": { "type": "string" },
            "qualifiedName": { "type": "string" },
            "filePath": { "type": "string" },
            "language": { "type": "string" },
            "startLine": { "type": "integer" },
            "endLine": { "type": "integer" },
            "signature": { "type": "string" }
        },
        "required": ["id", "kind", "name", "qualifiedName", "filePath", "language", "startLine", "endLine"]
    })
}

fn search_hit_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "node": node_summary_schema(),
            "score": { "type": "number" },
            "highlights": { "type": "array", "items": { "type": "string" } },
            "matchedQuery": { "type": "string" }
        },
        "required": ["node", "score"]
    })
}

fn node_detail_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "node": node_summary_schema(),
            "code": { "type": "string" },
            "outline": { "type": "string" },
            "callers": { "type": "array", "items": node_summary_schema() },
            "callees": { "type": "array", "items": node_summary_schema() }
        },
        "required": ["node", "callers", "callees"]
    })
}

fn file_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string" },
            "language": { "type": "string" },
            "nodeCount": { "type": "integer" }
        },
        "required": ["path", "language", "nodeCount"]
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
            "reason": { "enum": ["max_files", "budget", "unavailable", "no_source"] },
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
