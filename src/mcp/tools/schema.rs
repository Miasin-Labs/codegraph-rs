//! MCP tool and result wire schema.

use serde::Serialize;
use serde_json::{Map, Value, json};

/// MCP Tool definition. Serializes to the same JSON shape as the TS
/// `ToolDefinition` (camelCase `inputSchema`, ordered properties).
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: InputSchema,
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
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
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<ToolResultMeta>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMeta {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<ToolNotice>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolNotice {
    pub kind: String,
    pub severity: String,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ToolNoticeFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolNoticeFile {
    pub path: String,
    pub age_ms: i64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolError {
    pub code: String,
    pub category: String,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ToolResult {
    /// First text content (convenience for the server/tests).
    pub fn text(&self) -> &str {
        self.content.first().map(|c| c.text.as_str()).unwrap_or("")
    }

    /// Converts this result into the MCP structured-content compatibility shape.
    ///
    /// # Errors
    /// Returns an error if the canonical structured value cannot be serialized.
    pub fn into_mcp_projection(self) -> serde_json::Result<Self> {
        let Self {
            content,
            structured_content,
            meta,
            is_error,
        } = self;
        let structured_content = structured_content.unwrap_or_else(|| {
            json!({
                "schemaVersion": 1,
                "kind": "text",
                "text": content.first().map_or("", |item| item.text.as_str()),
            })
        });
        let text = serde_json::to_string(&structured_content)?;

        Ok(Self {
            content: vec![ToolContent {
                content_type: "text".into(),
                text,
            }],
            structured_content: Some(structured_content),
            meta,
            is_error,
        })
    }

    pub fn with_notice(mut self, notice: ToolNotice) -> Self {
        let mut meta = self.meta.take().unwrap_or_default();
        meta.notices.push(notice);
        self.meta = Some(meta);
        self
    }
}
