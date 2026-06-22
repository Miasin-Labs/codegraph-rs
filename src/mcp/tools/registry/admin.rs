//! Administrative tool schemas.

use serde_json::{Map, Value};

use super::super::schema::{InputSchema, ToolDefinition};
use super::schema_builder::{
    project_path_property,
    prop,
    prop_default,
    prop_enum_default,
    read_only_annotations,
};

pub(in crate::mcp::tools::registry) fn push_status_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_status
    {
        let mut props = Map::new();
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_status".into(),
            description: "Index health check (files / nodes / edges). Skip unless debugging."
                .into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: None,
            },
            annotations: read_only_annotations(),
        });
    }
}

pub(in crate::mcp::tools::registry) fn push_files_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_files
    {
        let mut props = Map::new();
        props.insert(
            "path".into(),
            prop(
                "string",
                "Filter to files under this directory path (e.g., \"src/components\"). Returns all files if not specified.",
            ),
        );
        props.insert(
            "pattern".into(),
            prop(
                "string",
                "Filter files matching this glob pattern (e.g., \"*.tsx\", \"**/*.test.ts\")",
            ),
        );
        props.insert(
            "format".into(),
            prop_enum_default(
                "string",
                "Output format: \"tree\" (hierarchical, default), \"flat\" (simple list), \"grouped\" (by language)",
                &["tree", "flat", "grouped"],
                Value::from("tree"),
            ),
        );
        props.insert(
            "includeMetadata".into(),
            prop_default(
                "boolean",
                "Include file metadata like language and symbol count (default: true)",
                Value::from(true),
            ),
        );
        props.insert(
            "maxDepth".into(),
            prop(
                "number",
                "Maximum directory depth to show (default: unlimited)",
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_files".into(),
            description: "Indexed file tree with language + symbol counts. Faster than Glob for project layout.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: None,
            },
            annotations: read_only_annotations(),
        });
    }
}
