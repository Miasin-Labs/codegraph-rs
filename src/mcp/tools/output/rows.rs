//! Compact symbol rows shared by the search, node, and file-view payloads.
//!
//! A row carries what an agent acts on — a name, a kind, and a place it can
//! pass straight back as `codegraph_node { symbol: name, file, line }` — and
//! nothing it can derive: no internal node id (no tool takes one), no
//! language (the file extension says it), no qualified name that only
//! repeats `name` (its useful part, the enclosing type or module, is
//! `container`), and no relevance score (the row order is the ranking).

use serde::Serialize;
use serde_json::{Value, json};

use crate::types::{Node, NodeKind};

/// Longest signature a row carries; longer ones end in `…`.
const MAX_SIGNATURE_CHARS: usize = 240;

/// One definition.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct SymbolRow {
    pub name: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Absent where the payload is about one file already (`node` file view).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub line: u32,
    pub end_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl SymbolRow {
    pub fn new(node: &Node) -> Self {
        Self {
            file: Some(node.file_path.clone()),
            ..Self::in_file(node)
        }
    }

    /// A row for a payload that already names the file.
    pub fn in_file(node: &Node) -> Self {
        Self {
            name: node.name.clone(),
            kind: node.kind.as_str(),
            container: container(node),
            file: None,
            line: node.start_line,
            end_line: node.end_line,
            signature: node
                .signature
                .as_deref()
                .and_then(|signature| compact_signature(signature, MAX_SIGNATURE_CHARS)),
        }
    }
}

/// Where a caller or callee is — the shortest row that still locates it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct SymbolRef {
    pub name: String,
    pub kind: &'static str,
    pub file: String,
    pub line: u32,
}

impl From<&Node> for SymbolRef {
    fn from(node: &Node) -> Self {
        Self {
            name: node.name.clone(),
            kind: node.kind.as_str(),
            file: node.file_path.clone(),
            line: node.start_line,
        }
    }
}

/// The part of a qualified name that is not the symbol itself — the type,
/// module, or namespace around it. `None` when the qualified name only
/// repeats the name or restates the file path.
fn container(node: &Node) -> Option<String> {
    if node.kind == NodeKind::File {
        return None;
    }
    let prefix = node.qualified_name.strip_suffix(node.name.as_str())?;
    let prefix = prefix.trim_end_matches([':', '.', '/', '\\', '#', ' ']);
    let prefix = prefix
        .strip_prefix(node.file_path.as_str())
        .map_or(prefix, |rest| rest.trim_start_matches([':', '.', '/', '#']));
    let restates_path =
        prefix == node.file_path || node.file_path.starts_with(&format!("{prefix}/"));
    (!prefix.is_empty() && !restates_path).then(|| prefix.to_string())
}

/// A signature on one line: whitespace runs collapsed, the padding a
/// multi-line parameter list leaves inside its brackets dropped, and
/// signatures longer than `max_chars` clipped (ending in `…`).
pub(in crate::mcp::tools) fn compact_signature(
    signature: &str,
    max_chars: usize,
) -> Option<String> {
    let collapsed = signature.split_whitespace().collect::<Vec<_>>().join(" ");
    let collapsed = collapsed
        .replace("( ", "(")
        .replace(", )", ")")
        .replace(" )", ")");
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= max_chars {
        return Some(collapsed);
    }
    let mut clipped: String = collapsed.chars().take(max_chars).collect();
    clipped.push('…');
    Some(clipped)
}

/// JSON schema for [`SymbolRow`]; `with_file` says whether rows carry `file`.
pub(in crate::mcp::tools) fn symbol_row_properties(
    with_file: bool,
) -> serde_json::Map<String, Value> {
    let mut properties = serde_json::Map::new();
    properties.insert("name".into(), json!({ "type": "string" }));
    properties.insert("kind".into(), json!({ "type": "string" }));
    properties.insert("container".into(), json!({ "type": "string" }));
    if with_file {
        properties.insert("file".into(), json!({ "type": "string" }));
    }
    properties.insert("line".into(), json!({ "type": "integer" }));
    properties.insert("endLine".into(), json!({ "type": "integer" }));
    properties.insert("signature".into(), json!({ "type": "string" }));
    properties
}

pub(in crate::mcp::tools) fn symbol_row_required(with_file: bool) -> Vec<&'static str> {
    if with_file {
        vec!["name", "kind", "file", "line", "endLine"]
    } else {
        vec!["name", "kind", "line", "endLine"]
    }
}

pub(in crate::mcp::tools) fn symbol_row_schema(with_file: bool) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": Value::Object(symbol_row_properties(with_file)),
        "required": symbol_row_required(with_file)
    })
}

pub(in crate::mcp::tools) fn symbol_ref_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": { "type": "string" },
            "kind": { "type": "string" },
            "file": { "type": "string" },
            "line": { "type": "integer" }
        },
        "required": ["name", "kind", "file", "line"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Language;

    fn node(name: &str, qualified_name: &str, file_path: &str, signature: Option<&str>) -> Node {
        let mut node = Node::new(
            "method:0e0a2cefc3eb7b0dc6ba040b03a8cc65",
            NodeKind::Method,
            name,
            qualified_name,
            file_path,
            Language::Rust,
            38,
            95,
        );
        node.signature = signature.map(str::to_string);
        node
    }

    #[test]
    fn row_keeps_the_container_and_drops_what_repeats() {
        let row = SymbolRow::new(&node(
            "handle_search",
            "ToolHandler::handle_search",
            "src/mcp/tools/graph/search.rs",
            Some(
                "(\n        &self,\n        args: &Map<String, Value>,\n    ) -> Result<ToolResult>",
            ),
        ));
        let value = serde_json::to_value(&row).unwrap();
        assert_eq!(
            value,
            json!({
                "name": "handle_search",
                "kind": "method",
                "container": "ToolHandler",
                "file": "src/mcp/tools/graph/search.rs",
                "line": 38,
                "endLine": 95,
                "signature": "(&self, args: &Map<String, Value>) -> Result<ToolResult>"
            })
        );
    }

    #[test]
    fn container_is_absent_when_the_qualified_name_adds_nothing() {
        for (name, qualified) in [
            ("run", "run"),
            ("lib.rs", "src/lib.rs"),
            ("helper", "src/util.ts::helper"),
            ("GET /users", "src/routes.rs::route:/users"),
        ] {
            let file = if name == "lib.rs" {
                "src/lib.rs"
            } else {
                "src/util.ts"
            };
            let row = SymbolRow::new(&node(name, qualified, file, None));
            assert_eq!(row.container, None, "{name} / {qualified}");
        }
        let row = SymbolRow::new(&node(
            "calculateTotal",
            "src/utils.ts::MathHelper.calculateTotal",
            "src/utils.ts",
            None,
        ));
        assert_eq!(row.container.as_deref(), Some("MathHelper"));
    }

    #[test]
    fn long_signatures_are_clipped() {
        let long = format!("fn f({})", "a: u8, ".repeat(80));
        let row = SymbolRow::new(&node("f", "f", "src/f.rs", Some(&long)));
        let signature = row.signature.unwrap();
        assert_eq!(signature.chars().count(), MAX_SIGNATURE_CHARS + 1);
        assert!(signature.ends_with('…'));
    }
}
