//! Schema for the cross-session memory tool (opt-in via CODEGRAPH_MCP_TOOLS).

use serde_json::{Map, Value};

use super::super::recall::recall_output_schema;
use super::super::schema::{InputSchema, ToolDefinition};
use super::schema_builder::{project_path_property, prop, prop_default, read_only_annotations};

pub(in crate::mcp::tools::registry) fn push_recall_tool(out: &mut Vec<ToolDefinition>) {
    let mut props = Map::new();
    props.insert(
        "about".into(),
        prop(
            "string",
            "What to recall: a repo-relative file or path prefix (`src/mcp/tools/`), \
             `symbol:NAME`, `failures`, `cochange` or `cochange:PATH`, or `last` (default).",
        ),
    );
    props.insert(
        "since".into(),
        prop(
            "string",
            "Only activity newer than this: `12h`, `7d`, `2w`.",
        ),
    );
    props.insert(
        "limit".into(),
        prop_default(
            "number",
            "Episodes (or rows) to return (default: 3)",
            Value::from(3),
        ),
    );
    props.insert(
        "related".into(),
        prop_default(
            "boolean",
            "Also ask the projects linked to this one (path dependencies either way, clones of \
             its remote); their matches come back grouped by project. For a path: their \
             sessions that touched it.",
            Value::from(false),
        ),
    );
    props.insert("projectPath".into(), project_path_property());
    out.push(ToolDefinition {
        name: "codegraph_recall".into(),
        description: "What earlier agent sessions in this repository already did: the episodes \
            that read or edited a path (and whether those files changed since), where a symbol \
            was found, recent build/test failures and whether a later run fixed them, and files \
            edited together; with `related`, the same for linked projects. Call it before \
            re-exploring an area another session worked on. At most 2 KB."
            .into(),
        input_schema: InputSchema {
            schema_type: "object".into(),
            properties: props,
            required: None,
        },
        output_schema: Some(recall_output_schema()),
        annotations: read_only_annotations(),
    });
}
