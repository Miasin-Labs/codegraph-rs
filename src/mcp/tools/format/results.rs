//! Generic MCP result construction and output capping.

use serde::Serialize;
use serde_json::{Value, json};

use super::super::context::ToolHandler;
use super::super::schema::{ToolContent, ToolError, ToolResult};
use super::{floor_char_boundary, output_char_cap};
use crate::error::Result;

impl ToolHandler {
    pub(in crate::mcp::tools) fn truncate_output(&self, text: &str) -> String {
        let Some(cap) = output_char_cap() else {
            return text.to_string();
        };
        if text.len() <= cap {
            return text.to_string();
        }
        let truncated = &text[..floor_char_boundary(text, cap)];
        let last_newline = truncated.rfind('\n');
        let cut_point = match last_newline {
            Some(pos) if (pos as f64) > cap as f64 * 0.8 => pos,
            _ => truncated.len(),
        };
        format!("{}\n\n... (output truncated)", &truncated[..cut_point])
    }

    // =========================================================================
    // Formatting helpers (compact by default to reduce context usage)

    pub(in crate::mcp::tools) fn text_result(&self, text: &str) -> ToolResult {
        ToolResult {
            content: vec![ToolContent {
                content_type: "text".into(),
                text: text.to_string(),
            }],
            structured_content: None,
            meta: None,
            is_error: None,
        }
    }

    pub(in crate::mcp::tools) fn structured_result<T: Serialize>(
        &self,
        text: &str,
        payload: &T,
    ) -> Result<ToolResult> {
        Ok(ToolResult {
            content: vec![ToolContent {
                content_type: "text".into(),
                text: text.to_string(),
            }],
            structured_content: Some(cap_structured_content(serde_json::to_value(payload)?)),
            meta: None,
            is_error: None,
        })
    }

    pub(in crate::mcp::tools) fn error_result(&self, message: &str) -> ToolResult {
        self.error_result_detail(ToolError {
            code: "tool_error".into(),
            category: "execution".into(),
            message: message.to_string(),
            retryable: false,
            field: None,
            expected: None,
            received_kind: None,
            hint: None,
        })
    }

    pub(in crate::mcp::tools) fn validation_error_result(
        &self,
        field: &str,
        message: &str,
        expected: &str,
        received_kind: Option<&str>,
    ) -> ToolResult {
        self.error_result_detail(ToolError {
            code: "invalid_argument".into(),
            category: "validation".into(),
            message: message.to_string(),
            retryable: false,
            field: Some(field.to_string()),
            expected: Some(expected.to_string()),
            received_kind: received_kind.map(str::to_string),
            hint: None,
        })
    }

    fn error_result_detail(&self, error: ToolError) -> ToolResult {
        let text = error.message.clone();
        ToolResult {
            content: vec![ToolContent {
                content_type: "text".into(),
                text,
            }],
            structured_content: Some(cap_structured_content(json!({
                "schemaVersion": 1,
                "kind": "error",
                "error": error,
            }))),
            meta: None,
            is_error: Some(true),
        }
    }
}

pub(in crate::mcp::tools) fn cap_structured_content(mut value: Value) -> Value {
    let Some(cap) = output_char_cap() else {
        return value;
    };
    cap_value(&mut value, 2048, 100);
    if serde_json::to_string(&value).map(|s| s.len()).unwrap_or(0) > cap {
        cap_value(&mut value, 256, 25);
    }
    if serde_json::to_string(&value).map(|s| s.len()).unwrap_or(0) > cap {
        cap_value(&mut value, 0, 10);
    }
    value
}

fn cap_value(value: &mut Value, max_string_chars: usize, max_array_len: usize) {
    match value {
        Value::String(s) => {
            if s.chars().count() > max_string_chars {
                *s = if max_string_chars == 0 {
                    "[truncated]".to_string()
                } else {
                    let mut out: String = s.chars().take(max_string_chars).collect();
                    out.push_str("... [truncated]");
                    out
                };
            }
        }
        Value::Array(items) => {
            if items.len() > max_array_len {
                items.truncate(max_array_len);
            }
            for item in items {
                cap_value(item, max_string_chars, max_array_len);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                let field_limit = if matches!(key.as_str(), "body" | "code" | "outline") {
                    max_string_chars.min(512)
                } else {
                    max_string_chars
                };
                cap_value(item, field_limit, max_array_len);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
