//! Deep exploration tool schema.

use serde_json::{Map, Value};

use super::super::output::explore_output_schema;
use super::super::schema::{InputSchema, ToolDefinition};
use super::schema_builder::{project_path_property, prop, prop_default, read_only_annotations};

pub(in crate::mcp::tools::registry) fn push_explore_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_explore
    {
        let mut props = Map::new();
        props.insert(
            "query".into(),
            prop(
                "string",
                "Symbol names, file names, or short code terms to explore (e.g., \"AuthService loginUser session-manager\", \"GraphTraverser BFS impact traversal.ts\"). Use codegraph_search first to find relevant names.",
            ),
        );
        props.insert(
            "maxFiles".into(),
            prop_default(
                "number",
                "Maximum number of files to include source code from (default: 12)",
                Value::from(12),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_explore".into(),
            description: "Primary context tool for codebase questions. Structured output (schema v2) returns exact source chunks with line ranges, mode, symbols, and verbatim source, plus explicit omissions with reasons, stateless continuation hints, and suspicious-Unicode findings; the CLI/text projection stays human-readable.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["query".into()]),
            },
            output_schema: Some(explore_output_schema()),
            annotations: read_only_annotations(),
        });
    }
}
