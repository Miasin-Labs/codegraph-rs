//! Ordered JSON schema builders for MCP tool definitions.

use serde_json::{Map, Value};

use super::super::schema::ToolAnnotations;

/// The annotation set every CodeGraph tool shares: all tools are pure
/// reads over the local index — read-only, non-destructive, idempotent,
/// closed-world.
pub(in crate::mcp::tools::registry) fn read_only_annotations() -> Option<ToolAnnotations> {
    Some(ToolAnnotations {
        read_only_hint: Some(true),
        destructive_hint: Some(false),
        idempotent_hint: Some(true),
        open_world_hint: Some(false),
    })
}

/// Build a `{ type, description }` property schema map (ordered keys:
/// type, description, enum?, default? — matching the TS literal order).
pub(in crate::mcp::tools::registry) fn prop_map(
    prop_type: &str,
    description: &str,
) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("type".into(), Value::String(prop_type.into()));
    m.insert("description".into(), Value::String(description.into()));
    m
}

pub(in crate::mcp::tools::registry) fn prop(prop_type: &str, description: &str) -> Value {
    Value::Object(prop_map(prop_type, description))
}

pub(in crate::mcp::tools::registry) fn prop_enum_map(
    prop_type: &str,
    description: &str,
    enum_values: &[&str],
) -> Map<String, Value> {
    let mut m = prop_map(prop_type, description);
    m.insert(
        "enum".into(),
        Value::Array(
            enum_values
                .iter()
                .map(|e| Value::String((*e).into()))
                .collect(),
        ),
    );
    m
}

pub(in crate::mcp::tools::registry) fn prop_enum(
    prop_type: &str,
    description: &str,
    enum_values: &[&str],
) -> Value {
    Value::Object(prop_enum_map(prop_type, description, enum_values))
}

pub(in crate::mcp::tools::registry) fn prop_default(
    prop_type: &str,
    description: &str,
    default: Value,
) -> Value {
    let mut m = prop_map(prop_type, description);
    m.insert("default".into(), default);
    Value::Object(m)
}

pub(in crate::mcp::tools::registry) fn prop_enum_default(
    prop_type: &str,
    description: &str,
    enum_values: &[&str],
    default: Value,
) -> Value {
    let mut m = prop_enum_map(prop_type, description, enum_values);
    m.insert("default".into(), default);
    Value::Object(m)
}

/// Common projectPath property for cross-project queries.
pub(in crate::mcp::tools::registry) fn project_path_property() -> Value {
    prop(
        "string",
        "Path to a different project with .codegraph/ initialized. If omitted, uses current project. Use this to query other codebases.",
    )
}
