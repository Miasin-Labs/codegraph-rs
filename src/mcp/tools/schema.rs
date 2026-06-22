//! MCP tool and result wire schema.

use serde::Serialize;
use serde_json::{Map, Value};

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
