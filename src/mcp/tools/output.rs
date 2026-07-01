use serde::Serialize;
use serde_json::{Value, json};

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
}

impl From<&SearchResult> for SearchHitOutput {
    fn from(result: &SearchResult) -> Self {
        Self {
            node: NodeSummary::from(&result.node),
            score: result.score,
            highlights: result.highlights.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct SearchOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub query: String,
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
            filter_kind,
            limit,
            total: results.len(),
            results: results.iter().map(SearchHitOutput::from).collect(),
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
}

impl StatusOutput {
    pub fn from_stats(
        stats: &GraphStats,
        backend: String,
        journal_mode: String,
        pending_sync: Vec<PendingSyncOutput>,
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
            "filterKind": { "type": "string" },
            "limit": { "type": "integer" },
            "total": { "type": "integer" },
            "results": { "type": "array", "items": search_hit_schema() }
        },
        "required": ["schemaVersion", "kind", "query", "limit", "total", "results"]
    }))
}

pub(in crate::mcp::tools) fn node_output_schema() -> Value {
    success_or_error(json!({
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
    }))
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
        },
        "required": ["schemaVersion", "kind", "filesIndexed", "totalNodes", "totalEdges", "databaseSizeBytes", "backend", "journalMode", "nodesByKind", "filesByLanguage", "pendingSync"]
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
            "relationships": { "type": "array", "items": relationship_schema() },
            "additionalFiles": { "type": "array", "items": additional_file_schema() },
            "literalMatches": { "type": "array", "items": literal_file_match_schema() },
            "trimmed": { "type": "boolean" }
        },
        "required": ["schemaVersion", "kind", "query", "totalSymbols", "totalFiles", "filesIncluded", "sourceFiles", "relationships", "additionalFiles", "literalMatches", "trimmed"]
    }))
}

fn success_or_error(success: Value) -> Value {
    json!({ "oneOf": [success, error_output_schema()] })
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
            "highlights": { "type": "array", "items": { "type": "string" } }
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
            "header": { "type": "string" },
            "body": { "type": "string" }
        },
        "required": ["path", "language", "header", "body"]
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
