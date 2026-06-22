//! Generic MCP result construction and output capping.

use super::super::context::ToolHandler;
use super::super::schema::{ToolContent, ToolResult};
use super::{floor_char_boundary, output_char_cap};

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
            is_error: None,
        }
    }

    pub(in crate::mcp::tools) fn error_result(&self, message: &str) -> ToolResult {
        ToolResult {
            content: vec![ToolContent {
                content_type: "text".into(),
                text: format!("Error: {message}"),
            }],
            is_error: Some(true),
        }
    }
}
