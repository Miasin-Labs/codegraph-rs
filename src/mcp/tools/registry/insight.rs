//! Schemas for the history and test-reach tools.

use serde_json::{Map, Value};

use super::super::schema::{InputSchema, ToolDefinition};
use super::schema_builder::{project_path_property, prop, prop_default, read_only_annotations};

pub(in crate::mcp::tools::registry) fn push_history_tool(out: &mut Vec<ToolDefinition>) {
    let mut props = Map::new();
    props.insert(
        "symbol".into(),
        prop(
            "string",
            "Symbol to center the history on. Omit for the most tightly coupled pairs in the whole repo.",
        ),
    );
    props.insert(
        "minSupport".into(),
        prop_default(
            "number",
            "Minimum commits a pair must share (default: 2)",
            Value::from(2),
        ),
    );
    props.insert(
        "maxCommits".into(),
        prop_default(
            "number",
            "How many recent commits to mine (default: 500)",
            Value::from(500),
        ),
    );
    props.insert(
        "limit".into(),
        prop_default("number", "Maximum pairs (default: 15)", Value::from(15)),
    );
    props.insert("projectPath".into(), project_path_property());
    out.push(ToolDefinition {
        name: "codegraph_history".into(),
        description: "What changes together with a symbol, mined from git history (temporal \
            coupling). Use before editing to find the other code that historically moves with it, \
            instead of running git log / git blame by hand."
            .into(),
        input_schema: InputSchema {
            schema_type: "object".into(),
            properties: props,
            required: None,
        },
        output_schema: None,
        annotations: read_only_annotations(),
    });
}

pub(in crate::mcp::tools::registry) fn push_tests_tool(out: &mut Vec<ToolDefinition>) {
    let mut props = Map::new();
    props.insert(
        "symbol".into(),
        prop("string", "Symbol whose covering tests you want"),
    );
    props.insert(
        "depth".into(),
        prop_default(
            "number",
            "How many caller hops to follow back to a test (default: 4)",
            Value::from(4),
        ),
    );
    props.insert(
        "limit".into(),
        prop_default(
            "number",
            "Maximum tests listed (default: 40)",
            Value::from(40),
        ),
    );
    props.insert("projectPath".into(), project_path_property());
    out.push(ToolDefinition {
        name: "codegraph_tests".into(),
        description: "Which tests exercise a symbol, found by walking its callers back to test \
            code. Use it to pick the tests to run after a change, or to spot untested code — no \
            coverage file needed."
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
