//! Payloads of the navigation tools: `codegraph_arch`, `codegraph_xref` and
//! `codegraph_paths`.
//!
//! Each mirrors what the tool's text shows, as rows: an architecture map's
//! files with their key definitions and its boundary modules, a symbol's
//! incoming references grouped per definition and edge kind, and the chain
//! of a found path.

use serde::Serialize;
use serde_json::{Value, json};

use super::{SymbolRef, is_false, is_zero, notices_schema, success_or_error, symbol_ref_schema};
use crate::mcp::tools::format::json_len;
use crate::types::Node;

// =============================================================================
// codegraph_arch

/// A key definition of a file.
#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct ArchSymbol {
    pub name: String,
    pub kind: &'static str,
    pub line: u32,
    #[serde(skip_serializing_if = "is_false")]
    pub exported: bool,
}

impl From<&Node> for ArchSymbol {
    fn from(node: &Node) -> Self {
        Self {
            name: node.name.clone(),
            kind: node.kind.as_str(),
            line: node.start_line,
            exported: node.is_exported == Some(true),
        }
    }
}

/// A file of the mapped area: its language, how many symbols it has, and
/// its key definitions in line order.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct ArchFile {
    pub file: String,
    pub language: &'static str,
    pub node_count: u32,
    pub symbols: Vec<ArchSymbol>,
    #[serde(skip_serializing_if = "is_zero")]
    pub symbols_omitted: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct ArchOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    /// Files in the area.
    pub file_count: usize,
    /// Key definitions (functions, methods, types) in those files.
    pub definition_count: usize,
    pub files: Vec<ArchFile>,
    /// Files outside the area that its files depend on.
    pub depends_on: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub depends_on_omitted: usize,
    /// Files outside the area that depend on its files.
    pub depended_on_by: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub depended_on_by_omitted: usize,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

impl ArchOutput {
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            kind: "arch",
            file_count: 0,
            definition_count: 0,
            files: Vec::new(),
            depends_on: Vec::new(),
            depends_on_omitted: 0,
            depended_on_by: Vec::new(),
            depended_on_by_omitted: 0,
            truncated: false,
        }
    }
}

// =============================================================================
// codegraph_xref

/// The references of one edge kind into a definition. `omitted` counts the
/// ones not listed: past `maxRefs`, cut by the output budget, or whose
/// source symbol is no longer in the index.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct XrefGroup {
    pub edge_kind: &'static str,
    pub references: Vec<SymbolRef>,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
}

/// One definition the name matched, with its incoming references by kind
/// (none listed: nothing references it).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct XrefDefinition {
    pub name: String,
    pub kind: &'static str,
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_kind: Vec<XrefGroup>,
}

impl From<&Node> for XrefDefinition {
    fn from(node: &Node) -> Self {
        Self {
            name: node.name.clone(),
            kind: node.kind.as_str(),
            file: node.file_path.clone(),
            line: node.start_line,
            by_kind: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct XrefOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub definitions: Vec<XrefDefinition>,
    #[serde(skip_serializing_if = "is_false")]
    pub not_found: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

impl XrefOutput {
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            kind: "xref",
            definitions: Vec::new(),
            not_found: false,
            truncated: false,
        }
    }

    pub fn not_found() -> Self {
        Self {
            not_found: true,
            ..Self::new()
        }
    }

    /// Bound the payload to `budget` by dropping references from the end
    /// (the last definition's last group first), counting each in its
    /// group's `omitted`, then whole definitions. Groups stay, so every kind
    /// still says how many references it had.
    pub fn fit_to(&mut self, budget: usize) {
        let mut size = json_len(&*self);
        if size <= budget {
            return;
        }
        self.truncated = true;
        size += ",\"truncated\":true".len();
        // `"omitted":N,` appearing on a group, with room for its digits.
        const OMITTED_FIELD: usize = ",\"omitted\":".len() + 6;
        while size > budget {
            let popped = self.definitions.iter_mut().rev().find_map(|definition| {
                definition.by_kind.iter_mut().rev().find_map(|group| {
                    let reference = group.references.pop()?;
                    let grew = if group.omitted == 0 { OMITTED_FIELD } else { 0 };
                    group.omitted += 1;
                    Some((json_len(&reference) + 1).saturating_sub(grew))
                })
            });
            match popped {
                Some(freed) => size = size.saturating_sub(freed.max(1)),
                None => {
                    if self.definitions.pop().is_none() {
                        break;
                    }
                    size = json_len(&*self);
                }
            }
            if size <= budget {
                // The estimate is conservative per step; confirm it.
                size = json_len(&*self);
            }
        }
    }
}

// =============================================================================
// codegraph_paths

/// A symbol on a path; `via` is the edge kind that leads into it (absent on
/// the first).
#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct PathStepOutput {
    pub name: String,
    pub kind: &'static str,
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<&'static str>,
}

/// How many definitions of each name were tried when no path was found.
#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct PathsSearched {
    pub sources: usize,
    pub sinks: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct PathsOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub found: bool,
    pub steps: Vec<PathStepOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub searched: Option<PathsSearched>,
    #[serde(skip_serializing_if = "is_false")]
    pub source_not_found: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub sink_not_found: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

impl PathsOutput {
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            kind: "paths",
            found: false,
            steps: Vec::new(),
            searched: None,
            source_not_found: false,
            sink_not_found: false,
            truncated: false,
        }
    }
}

// =============================================================================
// Schemas

fn string_array() -> Value {
    json!({ "type": "array", "items": { "type": "string" } })
}

pub(in crate::mcp::tools) fn arch_output_schema() -> Value {
    let symbol = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": { "type": "string" },
            "kind": { "type": "string" },
            "line": { "type": "integer" },
            "exported": { "type": "boolean" }
        },
        "required": ["name", "kind", "line"]
    });
    let file = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "file": { "type": "string" },
            "language": { "type": "string" },
            "nodeCount": { "type": "integer" },
            "symbols": { "type": "array", "items": symbol },
            "symbolsOmitted": { "type": "integer" }
        },
        "required": ["file", "language", "nodeCount", "symbols"]
    });
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "arch" },
            "notices": notices_schema(),
            "fileCount": { "type": "integer" },
            "definitionCount": { "type": "integer" },
            "files": { "type": "array", "items": file },
            "dependsOn": string_array(),
            "dependsOnOmitted": { "type": "integer" },
            "dependedOnBy": string_array(),
            "dependedOnByOmitted": { "type": "integer" },
            "truncated": { "type": "boolean" }
        },
        "required": [
            "schemaVersion", "kind", "fileCount", "definitionCount", "files",
            "dependsOn", "dependedOnBy"
        ]
    }))
}

pub(in crate::mcp::tools) fn xref_output_schema() -> Value {
    let group = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "edgeKind": { "type": "string" },
            "references": { "type": "array", "items": symbol_ref_schema() },
            "omitted": { "type": "integer" }
        },
        "required": ["edgeKind", "references"]
    });
    let definition = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": { "type": "string" },
            "kind": { "type": "string" },
            "file": { "type": "string" },
            "line": { "type": "integer" },
            "byKind": { "type": "array", "items": group }
        },
        "required": ["name", "kind", "file", "line"]
    });
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "xref" },
            "notices": notices_schema(),
            "definitions": { "type": "array", "items": definition },
            "notFound": { "type": "boolean" },
            "truncated": { "type": "boolean" }
        },
        "required": ["schemaVersion", "kind", "definitions"]
    }))
}

pub(in crate::mcp::tools) fn paths_output_schema() -> Value {
    let step = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": { "type": "string" },
            "kind": { "type": "string" },
            "file": { "type": "string" },
            "line": { "type": "integer" },
            "via": { "type": "string" }
        },
        "required": ["name", "kind", "file", "line"]
    });
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "paths" },
            "notices": notices_schema(),
            "found": { "type": "boolean" },
            "steps": { "type": "array", "items": step },
            "searched": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "sources": { "type": "integer" },
                    "sinks": { "type": "integer" }
                },
                "required": ["sources", "sinks"]
            },
            "sourceNotFound": { "type": "boolean" },
            "sinkNotFound": { "type": "boolean" },
            "truncated": { "type": "boolean" }
        },
        "required": ["schemaVersion", "kind", "found", "steps"]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Language, NodeKind};

    /// The JSON-Schema subset the output schemas use: oneOf, const, type,
    /// properties, required, additionalProperties: false, items.
    fn matches(schema: &Value, value: &Value) -> bool {
        if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            if !branches.iter().any(|branch| matches(branch, value)) {
                return false;
            }
        }
        if let Some(constant) = schema.get("const") {
            if constant != value {
                return false;
            }
        }
        if let Some(ty) = schema.get("type").and_then(Value::as_str) {
            let ok = match ty {
                "object" => value.is_object(),
                "array" => value.is_array(),
                "string" => value.is_string(),
                "integer" => value.is_i64() || value.is_u64(),
                "boolean" => value.is_boolean(),
                _ => true,
            };
            if !ok {
                return false;
            }
        }
        if let Some(object) = value.as_object() {
            let properties = schema.get("properties").and_then(Value::as_object);
            if schema.get("additionalProperties") == Some(&Value::Bool(false))
                && properties.is_some_and(|props| object.keys().any(|key| !props.contains_key(key)))
            {
                return false;
            }
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                if required
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|key| !object.contains_key(key))
                {
                    return false;
                }
            }
            if let Some(props) = properties {
                for (key, child) in object {
                    if let Some(sub) = props.get(key) {
                        if !matches(sub, child) {
                            return false;
                        }
                    }
                }
            }
        }
        if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
            if array.iter().any(|item| !matches(items, item)) {
                return false;
            }
        }
        true
    }

    fn node(name: &str, file: &str, line: u32) -> Node {
        let mut node = Node::new(
            format!("function:{name}"),
            NodeKind::Function,
            name,
            name,
            file,
            Language::Rust,
            line,
            line + 1,
        );
        node.is_exported = Some(true);
        node
    }

    fn arch_payload() -> ArchOutput {
        let mut output = ArchOutput::new();
        output.file_count = 1;
        output.definition_count = 14;
        output.files.push(ArchFile {
            file: "src/core/util.rs".into(),
            language: "rust",
            node_count: 20,
            symbols: vec![ArchSymbol::from(&node("helper", "src/core/util.rs", 3))],
            symbols_omitted: 13,
        });
        output.depends_on = vec!["src/types.rs".into()];
        output.depended_on_by = vec!["src/app.rs".into()];
        output.depended_on_by_omitted = 2;
        output
    }

    fn xref_payload(references: u32) -> XrefOutput {
        let mut definition = XrefDefinition::from(&node("target", "src/lib.rs", 1));
        definition.by_kind.push(XrefGroup {
            edge_kind: "calls",
            references: (0..references)
                .map(|i| SymbolRef::from(&node(&format!("caller_{i}"), "src/lib.rs", i + 2)))
                .collect(),
            omitted: 0,
        });
        let mut output = XrefOutput::new();
        output.definitions = vec![
            definition,
            XrefDefinition::from(&node("target", "src/b.rs", 9)),
        ];
        output
    }

    fn paths_payload() -> PathsOutput {
        let mut output = PathsOutput::new();
        output.found = true;
        output.steps = vec![
            PathStepOutput {
                name: "source".into(),
                kind: "function",
                file: "src/lib.rs".into(),
                line: 3,
                via: None,
            },
            PathStepOutput {
                name: "sink".into(),
                kind: "function",
                file: "src/lib.rs".into(),
                line: 1,
                via: Some("calls"),
            },
        ];
        output
    }

    fn assert_valid(schema: &Value, payload: &Value) {
        assert!(
            matches(schema, payload),
            "{payload} does not match its schema"
        );
    }

    #[test]
    fn payloads_match_their_schemas() {
        let arch = serde_json::to_value(arch_payload()).unwrap();
        assert_eq!(arch["files"][0]["symbols"][0]["exported"], true);
        assert_eq!(arch["dependedOnByOmitted"], 2);
        assert!(arch.get("dependsOnOmitted").is_none());
        assert_valid(&arch_output_schema(), &arch);
        assert_valid(
            &arch_output_schema(),
            &serde_json::to_value(ArchOutput::new()).unwrap(),
        );

        let xref = serde_json::to_value(xref_payload(3)).unwrap();
        assert_eq!(xref["definitions"][0]["byKind"][0]["edgeKind"], "calls");
        assert!(xref["definitions"][1].get("byKind").is_none());
        assert_valid(&xref_output_schema(), &xref);
        assert_valid(
            &xref_output_schema(),
            &serde_json::to_value(XrefOutput::not_found()).unwrap(),
        );

        let paths = serde_json::to_value(paths_payload()).unwrap();
        assert!(paths["steps"][0].get("via").is_none());
        assert_eq!(paths["steps"][1]["via"], "calls");
        assert_valid(&paths_output_schema(), &paths);
        let mut missing = PathsOutput::new();
        missing.searched = Some(PathsSearched {
            sources: 2,
            sinks: 1,
        });
        assert_valid(
            &paths_output_schema(),
            &serde_json::to_value(missing).unwrap(),
        );
    }

    #[test]
    fn schemas_reject_undeclared_fields() {
        let mut arch = serde_json::to_value(arch_payload()).unwrap();
        arch["files"][0]["extra"] = json!(1);
        assert!(!matches(&arch_output_schema(), &arch));
        let mut paths = serde_json::to_value(paths_payload()).unwrap();
        paths["hops"] = json!(1);
        assert!(!matches(&paths_output_schema(), &paths));
    }

    #[test]
    fn xref_fitting_counts_what_it_drops() {
        let mut output = xref_payload(200);
        output.fit_to(1_500);
        let value = serde_json::to_value(&output).unwrap();
        assert!(json_len(&value) <= 1_500, "{}", json_len(&value));
        assert_eq!(value["truncated"], true);
        let group = &value["definitions"][0]["byKind"][0];
        let listed = group["references"].as_array().unwrap().len();
        assert!(listed > 0 && listed < 200);
        assert_eq!(listed + group["omitted"].as_u64().unwrap() as usize, 200);
        assert_valid(&xref_output_schema(), &value);
    }
}
