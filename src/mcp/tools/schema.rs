//! MCP tool and result wire schema.

use serde::Serialize;
use serde_json::{Map, Value};

use super::format::{mcp_output_budget, truncate_text};
use super::output::{attach_notices, notice_banner, notice_outputs};

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

/// What a notice warns about: a reason the result may not match the code on
/// disk. Serialized `snake_case`, in `_meta.notices` and in the payload's
/// `notices` (see [`ToolResult::into_mcp_projection`]). Declared in the order
/// the payload lists them: whole-index conditions before per-file ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// The index belongs to a different git worktree than the one in use.
    WorktreeMismatch,
    /// Live file watching stopped, so the whole index is frozen.
    AutoSyncDisabled,
    /// The index was built by an older extractor; edges may be missing.
    StaleExtraction,
    /// The listed files changed after the last index sync.
    StaleIndex,
}

impl NoticeKind {
    pub const ALL: [NoticeKind; 4] = [
        NoticeKind::WorktreeMismatch,
        NoticeKind::AutoSyncDisabled,
        NoticeKind::StaleExtraction,
        NoticeKind::StaleIndex,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NoticeKind::WorktreeMismatch => "worktree_mismatch",
            NoticeKind::AutoSyncDisabled => "auto_sync_disabled",
            NoticeKind::StaleExtraction => "stale_extraction",
            NoticeKind::StaleIndex => "stale_index",
        }
    }
}

impl PartialEq<&str> for NoticeKind {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolNotice {
    pub kind: NoticeKind,
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

    /// Converts this result into what goes on the MCP wire.
    ///
    /// A tool with an output schema returns `structuredContent`, and its one
    /// text block is that payload serialized as compact JSON — what the spec
    /// asks for, and the only copy most hosts show the model. The payload is
    /// already shaped to the output budget by the tool, so the text is bounded
    /// too. A tool without an output schema has no structured payload: its own
    /// text goes out as-is (never wrapped in a JSON envelope, which would only
    /// escape it), cut to the output budget.
    ///
    /// Notices (files changed since the last sync, a frozen index, a foreign
    /// worktree, an old extractor) reach the model either way: a structured
    /// payload carries them as `notices`, next to `kind`, so they are in
    /// `structuredContent` and in the JSON text; a text result starts with one
    /// `⚠️` line per notice. Hosts rarely show `_meta` to the model, so
    /// `_meta.notices` (kept, in full detail) is never the only copy. A result
    /// without notices goes out unchanged.
    ///
    /// # Errors
    /// Returns an error if the structured value cannot be serialized.
    pub fn into_mcp_projection(self) -> serde_json::Result<Self> {
        let Self {
            content,
            structured_content,
            meta,
            is_error,
        } = self;
        let notices = match &meta {
            Some(meta) if is_error != Some(true) => notice_outputs(&meta.notices),
            _ => Vec::new(),
        };
        let (structured_content, text) = match structured_content {
            Some(mut structured) => {
                attach_notices(&mut structured, &notices)?;
                let text = serde_json::to_string(&structured)?;
                (Some(structured), text)
            }
            None => {
                let body = content
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let text = match notice_banner(&notices) {
                    Some(banner) => format!("{banner}\n\n{body}"),
                    None => body,
                };
                (None, truncate_text(&text, mcp_output_budget()))
            }
        };

        Ok(Self {
            content: vec![ToolContent {
                content_type: "text".into(),
                text,
            }],
            structured_content,
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
