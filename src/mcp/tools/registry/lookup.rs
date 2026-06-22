//! Symbol lookup tool schemas.

use serde_json::{Map, Value};

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
            prop("string", "Name of the symbol to get details for"),
        );
        props.insert(
            "includeCode".into(),
            prop_default(
                "boolean",
                "Include full source code (default: false to minimize context)",
                Value::from(false),
            ),
        );
        props.insert(
            "file".into(),
            prop(
                "string",
                "Optional: disambiguate an overloaded name to the definition in this file (path or basename, e.g. \"harness.rs\").",
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
            description: "SECONDARY (after codegraph_explore): get ONE symbol in full — its location, signature, callers/callees trail, and verbatim body (includeCode=true). When the name is AMBIGUOUS (an overloaded method, or the same method name on different types), it returns EVERY matching definition's full body in a single call — so you never need to Read a file to find the specific overload you want. For a heavily-overloaded name, pass `file` (and/or `line`) to pin the exact definition — e.g. the `file:line` a trail or another tool already showed you. Reach for this when explore trimmed a body you need. Use codegraph_explore for several related symbols or the full flow.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["symbol".into()]),
            },
            annotations: read_only_annotations(),
        });
    }
}
