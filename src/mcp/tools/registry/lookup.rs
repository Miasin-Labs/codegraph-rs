//! Symbol lookup tool schemas.

use serde_json::{Map, Value};

use super::super::output::{node_output_schema, search_output_schema};
use super::super::schema::{InputSchema, ToolDefinition};
use super::schema_builder::{
    project_path_property,
    prop,
    prop_default,
    prop_enum,
    read_only_annotations,
};

pub(in crate::mcp::tools::registry) fn push_search_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_search
    {
        let mut props = Map::new();
        props.insert(
            "query".into(),
            prop(
                "string",
                "Symbol name or partial name (e.g., \"auth\", \"signIn\", \"UserService\")",
            ),
        );
        props.insert(
            "kind".into(),
            prop_enum(
                "string",
                "Filter by node kind",
                &[
                    "function",
                    "method",
                    "class",
                    "interface",
                    "type",
                    "variable",
                    "route",
                    "component",
                ],
            ),
        );
        props.insert(
            "limit".into(),
            prop_default("number", "Maximum results (default: 10)", Value::from(10)),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_search".into(),
            description: "Quick symbol search by name. Returns locations only (no code). Use codegraph_explore instead to get the actual source / understand an area in one call.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["query".into()]),
            },
            output_schema: Some(search_output_schema()),
            annotations: read_only_annotations(),
        });
    }
}
pub(in crate::mcp::tools::registry) fn push_callee_tools(out: &mut Vec<ToolDefinition>) {
    // codegraph_callers
    {
        let mut props = Map::new();
        props.insert(
            "symbol".into(),
            prop(
                "string",
                "Name of the function, method, or class to find callers for",
            ),
        );
        props.insert(
            "limit".into(),
            prop_default(
                "number",
                "Maximum number of callers to return (default: 20)",
                Value::from(20),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_callers".into(),
            description:
                "List functions that call <symbol>. For the full flow, use codegraph_explore."
                    .into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["symbol".into()]),
            },
            output_schema: None,
            annotations: read_only_annotations(),
        });
    }

    // codegraph_callees
    {
        let mut props = Map::new();
        props.insert(
            "symbol".into(),
            prop(
                "string",
                "Name of the function, method, or class to find callees for",
            ),
        );
        props.insert(
            "limit".into(),
            prop_default(
                "number",
                "Maximum number of callees to return (default: 20)",
                Value::from(20),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_callees".into(),
            description:
                "List functions that <symbol> calls. For the full flow, use codegraph_explore."
                    .into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["symbol".into()]),
            },
            output_schema: None,
            annotations: read_only_annotations(),
        });
    }
}
pub(in crate::mcp::tools::registry) fn push_impact_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_impact
    {
        let mut props = Map::new();
        props.insert(
            "symbol".into(),
            prop("string", "Name of the symbol to analyze impact for"),
        );
        props.insert(
            "depth".into(),
            prop_default(
                "number",
                "How many levels of dependencies to traverse (default: 2)",
                Value::from(2),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_impact".into(),
            description: "List symbols affected by changing <symbol>. Use before a refactor."
                .into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["symbol".into()]),
            },
            output_schema: None,
            annotations: read_only_annotations(),
        });
    }
}
pub(in crate::mcp::tools::registry) fn push_node_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_node
    {
        let mut props = Map::new();
        props.insert(
            "symbol".into(),
            prop(
                "string",
                "Name of the symbol to read (symbol mode). Omit it and pass `file` alone to read a whole file.",
            ),
        );
        props.insert(
            "includeCode".into(),
            prop_default(
                "boolean",
                "Symbol mode: include full source code (default: false). Ignored in file mode.",
                Value::from(false),
            ),
        );
        props.insert(
            "file".into(),
            prop(
                "string",
                "A file path or basename. Pass alone to read the file, or with `symbol` to disambiguate an overloaded definition.",
            ),
        );
        props.insert(
            "offset".into(),
            prop("number", "File mode: 1-based line to start reading from."),
        );
        props.insert(
            "limit".into(),
            prop("number", "File mode: maximum number of lines to return."),
        );
        props.insert(
            "symbolsOnly".into(),
            prop_default(
                "boolean",
                "File mode: return only the symbol map and dependents.",
                Value::from(false),
            ),
        );
        props.insert(
            "line".into(),
            prop(
                "number",
                "Optional: disambiguate to the definition at/around this line (use with the file:line a trail showed you).",
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_node".into(),
            description: "Two modes: read an indexed file with line numbers and dependents by passing `file`, or inspect one symbol with source and its caller/callee trail by passing `symbol`. Use codegraph_explore for several related symbols or the full flow.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(Vec::new()),
            },
            output_schema: Some(node_output_schema()),
            annotations: read_only_annotations(),
        });
    }
}
