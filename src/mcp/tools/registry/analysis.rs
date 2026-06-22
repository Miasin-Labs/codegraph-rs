//! Analysis and verification tool schemas.

use serde_json::{Map, Value};

use super::super::schema::{InputSchema, ToolDefinition};
use super::schema_builder::{
    project_path_property,
    prop,
    prop_default,
    prop_enum,
    prop_map,
    read_only_annotations,
};

pub(in crate::mcp::tools::registry) fn push_vuln_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_vuln
    {
        let mut props = Map::new();
        props.insert(
            "minConfidence".into(),
            prop_default(
                "number",
                "Drop findings below this confidence, 0.0-1.0 (default: 0.5)",
                Value::from(0.5),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_vuln".into(),
            description: "Scan the project for likely vulnerabilities and bug-class anomalies (missing-guard / BAC / IDOR, unsanitized taint flows, concurrency lints), each with a confidence score. Inferred from corpus consistency, taint seeds, and concurrency analysis — no config needed.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: None,
            },
            annotations: read_only_annotations(),
        });
    }
}

pub(in crate::mcp::tools::registry) fn push_verify_roles_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_verify_roles
    {
        let mut props = Map::new();
        // `roles`: array of {symbol, role, confidence?, rationale?} objects.
        let mut item_props = Map::new();
        item_props.insert(
            "symbol".into(),
            prop(
                "string",
                "Function name to classify (resolved in the graph)",
            ),
        );
        item_props.insert(
            "role".into(),
            prop_enum(
                "string",
                "The role you believe this symbol plays",
                &["sink", "guard", "source", "sanitizer"],
            ),
        );
        item_props.insert(
            "confidence".into(),
            prop(
                "number",
                "Your confidence in this role, 0.0–1.0 (default 0.6)",
            ),
        );
        item_props.insert(
            "rationale".into(),
            prop("string", "Why you assigned this role (free text)"),
        );
        let mut item_schema = Map::new();
        item_schema.insert("type".into(), Value::from("object"));
        item_schema.insert("properties".into(), Value::Object(item_props));
        item_schema.insert(
            "required".into(),
            Value::Array(vec![Value::from("symbol"), Value::from("role")]),
        );
        let mut roles_schema = prop_map(
            "array",
            "Model-proposed roles to verify against the call graph. Each is \
             {symbol, role, confidence?, rationale?}.",
        );
        roles_schema.insert("items".into(), Value::Object(item_schema));
        props.insert("roles".into(), Value::Object(roles_schema));
        props.insert(
            "minCallers".into(),
            prop_default(
                "number",
                "A proposed sink must have at least this many distinct callers to be \
                 believable (default: 4)",
                Value::from(4),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_verify_roles".into(),
            description: "Verify model-proposed predicate roles (sinks/guards/sanitizers/sources) against the call graph, then emit only the missing-guard findings the graph corroborates — the 'model proposes, graph proves' boundary. Hallucinated sinks/guards are dropped before any finding is produced; survivors are tagged with the `llm` inference origin.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["roles".into()]),
            },
            annotations: read_only_annotations(),
        });
    }
}
