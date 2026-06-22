//! Architecture, xref, and path tool schemas.

use serde_json::{Map, Value};

use super::super::schema::{InputSchema, ToolDefinition};
use super::schema_builder::{project_path_property, prop, prop_default, read_only_annotations};

pub(in crate::mcp::tools::registry) fn push_arch_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_arch
    {
        let mut props = Map::new();
        props.insert(
            "path".into(),
            prop(
                "string",
                "Subsystem path/prefix to map, e.g. \"src/ast_deobfuscate\" (omit for whole project)",
            ),
        );
        props.insert(
            "maxSymbolsPerFile".into(),
            prop_default(
                "number",
                "Max definitions listed per file (default: 12)",
                Value::from(12),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_arch".into(),
            description: "Architecture overview of a subsystem: its modules with their key definitions (functions/types) plus the external module dependencies in and out. Use INSTEAD of grep+read to map an area's shape.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: None,
            },
            annotations: read_only_annotations(),
        });
    }
}

pub(in crate::mcp::tools::registry) fn push_xref_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_xref
    {
        let mut props = Map::new();
        props.insert(
            "symbol".into(),
            prop(
                "string",
                "Symbol to cross-reference (find all references to)",
            ),
        );
        props.insert(
            "maxRefs".into(),
            prop_default(
                "number",
                "Max references listed per reference kind (default: 50)",
                Value::from(50),
            ),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_xref".into(),
            description: "All incoming references to a symbol in one shot (callers, reads/writes, type refs, impls), grouped by reference kind. IDA-style xref — use instead of separate callers/impact calls.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["symbol".into()]),
            },
            annotations: read_only_annotations(),
        });
    }
}

pub(in crate::mcp::tools::registry) fn push_paths_tool(out: &mut Vec<ToolDefinition>) {
    // codegraph_paths
    {
        let mut props = Map::new();
        props.insert(
            "from".into(),
            prop(
                "string",
                "Source symbol (e.g. an entrypoint or tainted input)",
            ),
        );
        props.insert(
            "to".into(),
            prop("string", "Sink symbol (e.g. a dangerous function)"),
        );
        props.insert("projectPath".into(), project_path_property());
        out.push(ToolDefinition {
            name: "codegraph_paths".into(),
            description: "Find a call/reference path from a source symbol to a sink symbol — the reachability chain that connects them. Use to prove 'does X reach Y' for taint and impact reasoning.".into(),
            input_schema: InputSchema {
                schema_type: "object".into(),
                properties: props,
                required: Some(vec!["from".into(), "to".into()]),
            },
            annotations: read_only_annotations(),
        });
    }
}
