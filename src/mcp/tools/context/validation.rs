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
                    Err(self.validation_error_result(
                        name,
                        &format!("{name} exceeds maximum length of {MAX_INPUT_LENGTH} characters"),
                        &format!("string with at most {MAX_INPUT_LENGTH} characters"),
                        Some(&format!("string with {len} characters")),
                    ))
                } else {
                    Ok(s.clone())
                }
            }
            other => Err(self.validation_error_result(
                name,
                &format!("{name} must be a non-empty string"),
                "non-empty string",
                other.map(value_kind),
            )),
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
                    Err(self.validation_error_result(
                        name,
                        &format!("{name} exceeds maximum length of {MAX_PATH_LENGTH} characters"),
                        &format!("string with at most {MAX_PATH_LENGTH} characters"),
                        Some(&format!("string with {len} characters")),
                    ))
                } else {
                    Ok(Some(s.clone()))
                }
            }
            Some(other) => Err(self.validation_error_result(
                name,
                &format!("{name} must be a string"),
                "string",
                Some(value_kind(other)),
            )),
        }
    }
}

fn value_kind(value: &Value) -> &str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
