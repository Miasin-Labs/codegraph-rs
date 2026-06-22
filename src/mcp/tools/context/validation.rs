//! MCP argument validation helpers.

use serde_json::Value;

use super::super::format::{MAX_INPUT_LENGTH, MAX_PATH_LENGTH};
use super::super::schema::ToolResult;
use super::ToolHandler;

impl ToolHandler {
    pub(in crate::mcp::tools) fn validate_string(
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
    pub(in crate::mcp::tools) fn validate_optional_path(
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
}
