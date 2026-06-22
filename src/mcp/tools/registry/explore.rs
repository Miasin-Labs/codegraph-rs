//! Deep exploration tool schema.

use serde_json::{Map, Value};

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
            description: "PRIMARY TOOL — call FIRST for almost any question: how does X work, architecture, a bug, where/what is X, or surveying an area. Returns the verbatim source of the relevant symbols grouped by file in ONE capped call (Read-equivalent — do NOT re-open shown files). Query can be a natural-language question OR a bag of symbol/file names. Usually the ONLY call you need — answers without further search/node/Read/Grep.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["query".into()]),
            },
            annotations: read_only_annotations(),
        });
    }
}
