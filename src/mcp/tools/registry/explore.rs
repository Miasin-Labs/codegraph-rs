//! Deep exploration tool schema.

use serde_json::Map;

use super::super::output::explore_output_schema;
use super::super::schema::{InputSchema, ToolDefinition};
use super::schema_builder::{project_path_property, prop, read_only_annotations};

pub(in crate::mcp::tools::registry) fn push_explore_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_explore
    {
        let mut props = Map::new();
        props.insert(
            "query".into(),
            prop(
                "string",
                "Symbol names, file names, or short code terms to explore (e.g., \"AuthService loginUser session-manager\", \"GraphTraverser BFS impact traversal.ts\"). For a flow question, name the symbols spanning the flow (e.g. \"mutateElement renderScene\"). A natural-language question works too — no prior codegraph_search needed.",
            ),
        );
        props.insert(
            "maxFiles".into(),
            prop(
                "number",
                "Maximum number of ranked files whose source is included. It does not change symbol or literal discovery. The default adapts to project size (4-8).",
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
