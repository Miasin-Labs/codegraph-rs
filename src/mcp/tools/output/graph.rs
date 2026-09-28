//! Payloads of the call-graph tools: `codegraph_callers`, `codegraph_callees`
//! and `codegraph_impact`, with what they found in other graphs.
//!
//! Rows are the shared ones ([`SymbolRef`], [`ExternalRef`]); a blast radius,
//! which can list hundreds of symbols, groups them by file so a path is sent
//! once. Cross-project sections carry what the text does: each project read
//! with its rows, the projects read that have none, and the projects that
//! could not be read with the reason.

use serde::Serialize;
use serde_json::{Value, json};

use super::{
    ExternalRef,
    SymbolRef,
    external_ref_schema,
    is_false,
    is_zero,
    notices_schema,
    success_or_error,
    symbol_ref_schema,
};
use crate::federation::render::availability_note;
use crate::federation::{Caller, CrossCallers, CrossImpact, Followed, SkippedProject};
use crate::mcp::tools::format::json_len;
use crate::types::Node;

impl From<&Followed> for ExternalRef {
    fn from(followed: &Followed) -> Self {
        Self {
            name: followed.edge.target_qualified_name.clone(),
            kind: followed.edge.target_kind.as_str(),
            graph: followed.label.clone(),
            file: followed.file().to_string(),
            line: followed.line(),
            unavailable: availability_note(followed),
        }
    }
}

/// A definition of another graph a query was answered in: its qualified
/// name, and where it is in that graph.
pub(in crate::mcp::tools) fn foreign_ref(label: &str, node: &Node) -> ExternalRef {
    ExternalRef {
        name: node.qualified_name.clone(),
        kind: node.kind.as_str(),
        graph: label.to_string(),
        file: node.file_path.clone(),
        line: Some(node.start_line),
        unavailable: None,
    }
}

/// A symbol in a file group.
#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct LineRef {
    pub name: String,
    pub kind: &'static str,
    pub line: u32,
}

/// The symbols of one file, in the order they were found.
#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct FileSymbols {
    pub file: String,
    pub symbols: Vec<LineRef>,
}

/// `nodes` grouped by file, files in first-seen order.
pub(in crate::mcp::tools) fn group_by_file<'a>(
    nodes: impl IntoIterator<Item = &'a Node>,
) -> Vec<FileSymbols> {
    let mut groups: Vec<FileSymbols> = Vec::new();
    let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for node in nodes {
        let at = *index.entry(node.file_path.as_str()).or_insert_with(|| {
            groups.push(FileSymbols {
                file: node.file_path.clone(),
                symbols: Vec::new(),
            });
            groups.len() - 1
        });
        groups[at].symbols.push(LineRef {
            name: node.name.clone(),
            kind: node.kind.as_str(),
            line: node.start_line,
        });
    }
    groups
}

/// A caller or callee; `target` is the definition it reaches, given only when
/// the name matched several (`sync` on two types): without it the rows of
/// unrelated same-named definitions read as one list. `dispatch` says the
/// hop is not a static call: `dynamic` (a call on `dyn Trait`), `generic`
/// (on a `T: Trait`), `implementation` (a trait method to an
/// implementation of it, which such calls may run).
#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct CallRow {
    #[serde(flatten)]
    pub symbol: SymbolRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<&'static str>,
}

/// How the edge `edge` dispatches, for [`CallRow::dispatch`]: `None` for a
/// static call.
pub(in crate::mcp::tools) fn dispatch_label(edge: &crate::types::Edge) -> Option<&'static str> {
    let metadata = edge.metadata.as_ref()?;
    match metadata.get("dispatch").and_then(serde_json::Value::as_str) {
        Some("dynamic") => Some("dynamic"),
        Some("generic") => Some("generic"),
        _ => (metadata
            .get("synthesizedBy")
            .and_then(serde_json::Value::as_str)
            == Some("interface-impl"))
        .then_some("implementation"),
    }
}

/// A caller in another project; `target` names which of several items it
/// reaches (absent when there is one).
#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct ProjectCallerRow {
    pub name: String,
    pub kind: &'static str,
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

fn caller_rows(callers: &[Caller], several_targets: bool) -> Vec<ProjectCallerRow> {
    callers
        .iter()
        .map(|caller| ProjectCallerRow {
            name: caller.node.name.clone(),
            kind: caller.node.kind.as_str(),
            file: caller.node.file_path.clone(),
            line: caller.node.start_line,
            target: several_targets.then(|| caller.target.clone()),
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct SkippedProjectOutput {
    pub project: String,
    pub reason: &'static str,
}

fn skipped(projects: &[SkippedProject]) -> Vec<SkippedProjectOutput> {
    projects
        .iter()
        .map(|entry| SkippedProjectOutput {
            project: entry.project.name.clone(),
            reason: entry.reason.describe(),
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct ProjectCallersOutput {
    pub project: String,
    pub callers: Vec<ProjectCallerRow>,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
    /// The project's external pass is incomplete: it may call more.
    #[serde(skip_serializing_if = "is_false")]
    pub partial: bool,
}

/// Callers of the same code in the projects that use it.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct CrossCallersOutput {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_projects: Vec<ProjectCallersOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub projects_without_callers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_projects: Vec<SkippedProjectOutput>,
}

impl From<&CrossCallers> for CrossCallersOutput {
    fn from(cross: &CrossCallers) -> Self {
        let targets: std::collections::HashSet<&str> = cross
            .groups
            .iter()
            .flat_map(|group| group.callers.iter().map(|caller| caller.target.as_str()))
            .collect();
        Self {
            other_projects: cross
                .groups
                .iter()
                .map(|group| ProjectCallersOutput {
                    project: group.project.name.clone(),
                    callers: caller_rows(&group.callers, targets.len() > 1),
                    omitted: group.omitted,
                    partial: group.partial,
                })
                .collect(),
            projects_without_callers: cross
                .without_callers
                .iter()
                .map(|project| project.name.clone())
                .collect(),
            skipped_projects: skipped(&cross.skipped),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct ProjectImpactOutput {
    pub project: String,
    /// Its symbols that reference the changed code directly.
    pub entries: Vec<ProjectCallerRow>,
    /// Its symbols that depend on those, by file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub affected: Vec<FileSymbols>,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
    #[serde(skip_serializing_if = "is_false")]
    pub partial: bool,
}

/// The blast radius of the same code in the projects that use it.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct CrossImpactOutput {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_projects: Vec<ProjectImpactOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unaffected_projects: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_projects: Vec<SkippedProjectOutput>,
}

impl From<&CrossImpact> for CrossImpactOutput {
    fn from(cross: &CrossImpact) -> Self {
        Self {
            other_projects: cross
                .groups
                .iter()
                .map(|group| ProjectImpactOutput {
                    project: group.project.name.clone(),
                    entries: caller_rows(&group.entries, false),
                    affected: group_by_file(&group.affected),
                    omitted: group.omitted,
                    partial: group.partial,
                })
                .collect(),
            unaffected_projects: cross
                .unaffected
                .iter()
                .map(|project| project.name.clone())
                .collect(),
            skipped_projects: skipped(&cross.skipped),
        }
    }
}

// =============================================================================
// codegraph_callers / codegraph_callees

/// Callers or callees of a symbol that lives in another graph (a
/// dependency, a linked project): the rows are in that graph, `file`
/// relative to its root.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct ForeignCallsOutput {
    pub symbol: ExternalRef,
    pub results: Vec<SymbolRef>,
    #[serde(skip_serializing_if = "is_zero")]
    pub results_omitted: usize,
    #[serde(flatten)]
    pub cross: CrossCallersOutput,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct CallsOutput {
    pub schema_version: u32,
    /// `callers` or `callees`.
    pub kind: &'static str,
    /// How many there are in this project, listed or not. Stated, not left
    /// to be counted: in a blind reading test a small model miscounted a
    /// 20-row list that the markdown's "(20 found)" header had spelled out.
    pub count: usize,
    /// In this project.
    pub results: Vec<CallRow>,
    #[serde(skip_serializing_if = "is_zero")]
    pub results_omitted: usize,
    /// The definitions the name matched, when there were several (each
    /// result's `target` says which one it belongs to).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<SymbolRef>,
    /// Callees in other graphs (dependencies, linked projects).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub external: Vec<ExternalRef>,
    #[serde(skip_serializing_if = "is_zero")]
    pub external_omitted: usize,
    #[serde(flatten)]
    pub cross: CrossCallersOutput,
    /// The name is not this project's: answered in the graphs it names.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<ForeignCallsOutput>,
    #[serde(skip_serializing_if = "is_false")]
    pub not_found: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

impl CallsOutput {
    pub fn new(kind: &'static str) -> Self {
        Self {
            schema_version: 1,
            kind,
            count: 0,
            results: Vec::new(),
            results_omitted: 0,
            matches: Vec::new(),
            external: Vec::new(),
            external_omitted: 0,
            cross: CrossCallersOutput::default(),
            foreign: Vec::new(),
            not_found: false,
            truncated: false,
        }
    }

    pub fn not_found(kind: &'static str) -> Self {
        Self {
            not_found: true,
            ..Self::new(kind)
        }
    }
}

// =============================================================================
// codegraph_impact

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct ForeignImpactOutput {
    pub symbol: ExternalRef,
    pub count: usize,
    pub files: Vec<FileSymbols>,
    #[serde(flatten)]
    pub cross: CrossImpactOutput,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct ImpactOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    /// Symbols affected in this project (the changed ones included).
    pub count: usize,
    pub files: Vec<FileSymbols>,
    /// Symbols the budget left out of `files` (`count` includes them).
    #[serde(skip_serializing_if = "is_zero")]
    pub symbols_omitted: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<SymbolRef>,
    #[serde(flatten)]
    pub cross: CrossImpactOutput,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<ForeignImpactOutput>,
    #[serde(skip_serializing_if = "is_false")]
    pub not_found: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

impl ImpactOutput {
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            kind: "impact",
            count: 0,
            files: Vec::new(),
            symbols_omitted: 0,
            matches: Vec::new(),
            cross: CrossImpactOutput::default(),
            foreign: Vec::new(),
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
}

// =============================================================================
// Budget

/// One array [`fitted`] may shorten, and the count that must still add up
/// after it does: what the payload leaves out is always counted, whether the
/// tool's own limit or the budget cut it.
#[derive(Debug, Clone, Copy)]
pub(in crate::mcp::tools) struct Trim {
    array: &'static str,
    /// The field that counts left-out items (created if absent).
    omitted: Option<&'static str>,
    /// For a list of groups (`files: [{file, tests: [...]}]`): the inner
    /// array whose rows a dropped group takes with it.
    rows: Option<&'static str>,
}

impl Trim {
    /// An array whose items another field already totals (or that has no
    /// count): only `truncated` records the cut.
    pub const fn plain(array: &'static str) -> Self {
        Self {
            array,
            omitted: None,
            rows: None,
        }
    }

    /// A flat list: each dropped item adds one to `omitted`.
    pub const fn counted(array: &'static str, omitted: &'static str) -> Self {
        Self {
            array,
            omitted: Some(omitted),
            rows: None,
        }
    }

    /// A list of groups: each dropped group adds its `rows` to `omitted`.
    pub const fn groups(array: &'static str, rows: &'static str, omitted: &'static str) -> Self {
        Self {
            array,
            omitted: Some(omitted),
            rows: Some(rows),
        }
    }
}

/// Serialize `payload` and bound it to `budget` by dropping trailing items of
/// its top-level arrays, trimming them in the order given (least important
/// first), adding what each drop leaves out to its count, and flagging
/// `truncated`. Items are never cut inside.
pub(in crate::mcp::tools) fn fitted<T: Serialize>(
    payload: &T,
    budget: usize,
    trims: &[Trim],
) -> Value {
    let mut value = serde_json::to_value(payload).unwrap_or(Value::Null);
    if json_len(&value) <= budget {
        return value;
    }
    value["truncated"] = Value::Bool(true);
    for trim in trims {
        while json_len(&value) > budget {
            let Some(items) = value.get_mut(trim.array).and_then(Value::as_array_mut) else {
                break;
            };
            let Some(dropped) = items.pop() else {
                break;
            };
            if let Some(omitted) = trim.omitted {
                let rows = match trim.rows {
                    Some(inner) => dropped[inner].as_array().map_or(0, Vec::len),
                    None => 1,
                };
                let before = value[omitted].as_u64().unwrap_or(0);
                value[omitted] = Value::from(before + rows as u64);
            }
        }
    }
    value
}

// =============================================================================
// Schemas

fn line_ref_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": { "type": "string" },
            "kind": { "type": "string" },
            "line": { "type": "integer" }
        },
        "required": ["name", "kind", "line"]
    })
}

pub(in crate::mcp::tools) fn file_symbols_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "file": { "type": "string" },
            "symbols": { "type": "array", "items": line_ref_schema() }
        },
        "required": ["file", "symbols"]
    })
}

fn project_caller_row_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": { "type": "string" },
            "kind": { "type": "string" },
            "file": { "type": "string" },
            "line": { "type": "integer" },
            "target": { "type": "string" }
        },
        "required": ["name", "kind", "file", "line"]
    })
}

fn skipped_projects_schema() -> Value {
    json!({ "type": "array", "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "project": { "type": "string" },
            "reason": { "type": "string" }
        },
        "required": ["project", "reason"]
    }})
}

fn string_array() -> Value {
    json!({ "type": "array", "items": { "type": "string" } })
}

fn cross_callers_properties() -> serde_json::Map<String, Value> {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "otherProjects".into(),
        json!({ "type": "array", "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "project": { "type": "string" },
                "callers": { "type": "array", "items": project_caller_row_schema() },
                "omitted": { "type": "integer" },
                "partial": { "type": "boolean" }
            },
            "required": ["project", "callers"]
        }}),
    );
    properties.insert("projectsWithoutCallers".into(), string_array());
    properties.insert("skippedProjects".into(), skipped_projects_schema());
    properties
}

fn cross_impact_properties() -> serde_json::Map<String, Value> {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "otherProjects".into(),
        json!({ "type": "array", "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "project": { "type": "string" },
                "entries": { "type": "array", "items": project_caller_row_schema() },
                "affected": { "type": "array", "items": file_symbols_schema() },
                "omitted": { "type": "integer" },
                "partial": { "type": "boolean" }
            },
            "required": ["project", "entries"]
        }}),
    );
    properties.insert("unaffectedProjects".into(), string_array());
    properties.insert("skippedProjects".into(), skipped_projects_schema());
    properties
}

/// Output schema of `codegraph_callers` (`kind: "callers"`) or
/// `codegraph_callees` (`kind: "callees"`).
pub(in crate::mcp::tools) fn calls_output_schema(kind: &str) -> Value {
    let refs = json!({ "type": "array", "items": symbol_ref_schema() });
    let mut foreign = cross_callers_properties();
    foreign.insert("symbol".into(), external_ref_schema());
    foreign.insert("results".into(), refs.clone());
    foreign.insert("resultsOmitted".into(), json!({ "type": "integer" }));

    let mut call_row = symbol_ref_schema();
    call_row["properties"]["target"] = json!({ "type": "string" });
    call_row["properties"]["dispatch"] =
        json!({ "enum": ["dynamic", "generic", "implementation"] });
    let mut properties = cross_callers_properties();
    for (name, schema) in [
        ("schemaVersion", json!({ "type": "integer" })),
        ("kind", json!({ "const": kind })),
        ("notices", notices_schema()),
        ("count", json!({ "type": "integer" })),
        ("results", json!({ "type": "array", "items": call_row })),
        ("resultsOmitted", json!({ "type": "integer" })),
        ("matches", refs),
        (
            "external",
            json!({ "type": "array", "items": external_ref_schema() }),
        ),
        ("externalOmitted", json!({ "type": "integer" })),
        (
            "foreign",
            json!({ "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": Value::Object(foreign),
                "required": ["symbol", "results"]
            }}),
        ),
        ("notFound", json!({ "type": "boolean" })),
        ("truncated", json!({ "type": "boolean" })),
    ] {
        properties.insert(name.into(), schema);
    }
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": Value::Object(properties),
        "required": ["schemaVersion", "kind", "count", "results"]
    }))
}

pub(in crate::mcp::tools) fn impact_output_schema() -> Value {
    let files = json!({ "type": "array", "items": file_symbols_schema() });
    let mut foreign = cross_impact_properties();
    foreign.insert("symbol".into(), external_ref_schema());
    foreign.insert("count".into(), json!({ "type": "integer" }));
    foreign.insert("files".into(), files.clone());

    let mut properties = cross_impact_properties();
    for (name, schema) in [
        ("schemaVersion", json!({ "type": "integer" })),
        ("kind", json!({ "const": "impact" })),
        ("notices", notices_schema()),
        ("count", json!({ "type": "integer" })),
        ("files", files),
        ("symbolsOmitted", json!({ "type": "integer" })),
        (
            "matches",
            json!({ "type": "array", "items": symbol_ref_schema() }),
        ),
        (
            "foreign",
            json!({ "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": Value::Object(foreign),
                "required": ["symbol", "count", "files"]
            }}),
        ),
        ("notFound", json!({ "type": "boolean" })),
        ("truncated", json!({ "type": "boolean" })),
    ] {
        properties.insert(name.into(), schema);
    }
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": Value::Object(properties),
        "required": ["schemaVersion", "kind", "count", "files"]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Language, NodeKind};

    fn node(name: &str, file: &str, line: u32) -> Node {
        Node::new(
            format!("function:{name}"),
            NodeKind::Function,
            name,
            name,
            file,
            Language::Rust,
            line,
            line + 1,
        )
    }

    #[test]
    fn groups_keep_first_seen_file_order() {
        let nodes = [
            node("a", "x.rs", 1),
            node("b", "y.rs", 2),
            node("c", "x.rs", 3),
        ];
        let groups = serde_json::to_value(group_by_file(&nodes)).unwrap();
        assert_eq!(
            groups,
            json!([
                { "file": "x.rs", "symbols": [
                    { "name": "a", "kind": "function", "line": 1 },
                    { "name": "c", "kind": "function", "line": 3 }
                ]},
                { "file": "y.rs", "symbols": [{ "name": "b", "kind": "function", "line": 2 }]}
            ])
        );
    }

    #[test]
    fn empty_sections_are_left_out() {
        let value = serde_json::to_value(CallsOutput::new("callers")).unwrap();
        assert_eq!(
            value,
            json!({ "schemaVersion": 1, "kind": "callers", "count": 0, "results": [] })
        );
        let value = serde_json::to_value(ImpactOutput::not_found()).unwrap();
        assert_eq!(
            value,
            json!({ "schemaVersion": 1, "kind": "impact", "count": 0, "files": [], "notFound": true })
        );
    }

    #[test]
    fn dropped_file_groups_count_their_symbols() {
        let nodes: Vec<Node> = (0..300)
            .map(|i| node(&format!("affected_{i}"), &format!("src/f{}.rs", i / 3), i))
            .collect();
        let mut output = ImpactOutput::new();
        output.count = nodes.len();
        output.files = group_by_file(&nodes);
        let value = fitted(
            &output,
            2_000,
            &[Trim::groups("files", "symbols", "symbolsOmitted")],
        );
        assert!(json_len(&value) <= 2_000);
        assert_eq!(value["truncated"], true);
        let listed: u64 = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|group| group["symbols"].as_array().unwrap().len() as u64)
            .sum();
        assert!(listed > 0);
        assert_eq!(listed + value["symbolsOmitted"].as_u64().unwrap(), 300);
    }

    #[test]
    fn fitting_drops_trailing_items_and_flags_it() {
        let mut output = CallsOutput::new("callers");
        output.results = (0..200)
            .map(|i| CallRow {
                symbol: SymbolRef::from(&node(&format!("caller_{i}"), "src/lib.rs", i)),
                target: None,
                dispatch: None,
            })
            .collect();
        let value = fitted(
            &output,
            1_000,
            &[Trim::counted("results", "resultsOmitted")],
        );
        assert!(json_len(&value) <= 1_000);
        assert_eq!(value["truncated"], true);
        let kept = value["results"].as_array().unwrap();
        assert!(!kept.is_empty() && kept.len() < 200);
        assert_eq!(kept[0]["name"], "caller_0");
        // Every caller is either listed or counted.
        assert_eq!(
            kept.len() as u64 + value["resultsOmitted"].as_u64().unwrap(),
            200
        );
    }
}
