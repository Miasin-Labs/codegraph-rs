//! Administrative tool schemas.

use serde_json::Map;

use super::super::output::{files_output_schema, status_output_schema};
use super::super::schema::{InputSchema, ToolDefinition};
use super::schema_builder::{project_path_property, prop, read_only_annotations};

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
            output_schema: Some(status_output_schema()),
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
            "maxDepth".into(),
            prop(
                "number",
                "Directory levels to list below `path` (1 = its direct entries). Deeper \
                 directories are collapsed to their file counts. Default: the deepest level \
                 that fits one reply.",
            ),
        );
        props.insert(
            "cursor".into(),
            prop(
                "string",
                "Next page of a listing: the `nextCursor` a previous call returned, passed with \
                 the same `path` and `pattern`.",
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_files".into(),
            description: "Indexed files grouped by directory, with symbol counts per file and \
                language totals. Faster than Glob for project layout; narrow with `path`, \
                `pattern`, or `maxDepth`, and page with `cursor`."
                .into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: None,
            },
            output_schema: Some(files_output_schema()),
            annotations: read_only_annotations(),
        });
    }
}
