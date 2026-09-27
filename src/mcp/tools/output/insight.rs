//! Payloads of the insight tools: `codegraph_tests` (tests reaching a
//! symbol), `codegraph_history` (code that changes together) and
//! `codegraph_diagnostics` (compiler errors placed on symbols).
//!
//! Each mirrors what its text lists: tests and diagnostics are grouped by
//! file so a path is sent once, and a `limit` cut is an `…Omitted`/`omitted`
//! count, never a silent drop.

use serde::Serialize;
use serde_json::{Value, json};

use super::{SymbolRef, is_false, is_zero, notices_schema, success_or_error, symbol_ref_schema};
use crate::analyze::CoChangeReport;

// =============================================================================
// codegraph_tests

/// A test function or method in a file group.
#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct TestRow {
    pub name: String,
    pub line: u32,
}

/// The tests of one file, by line.
#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct TestFile {
    pub file: String,
    pub tests: Vec<TestRow>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct TestsOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    /// Tests reaching the symbol, listed or not.
    pub count: usize,
    /// Caller hops followed back to a test.
    pub depth: u32,
    pub files: Vec<TestFile>,
    /// Tests past `limit`.
    #[serde(skip_serializing_if = "is_zero")]
    pub tests_omitted: usize,
    /// The definitions the name matched, when there were several (the
    /// tests are theirs together).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<SymbolRef>,
    #[serde(skip_serializing_if = "is_false")]
    pub not_found: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

impl TestsOutput {
    pub fn new(depth: u32) -> Self {
        Self {
            schema_version: 1,
            kind: "tests",
            count: 0,
            depth,
            files: Vec::new(),
            tests_omitted: 0,
            matches: Vec::new(),
            not_found: false,
            truncated: false,
        }
    }

    pub fn not_found(depth: u32) -> Self {
        Self {
            not_found: true,
            ..Self::new(depth)
        }
    }
}

pub(in crate::mcp::tools) fn tests_output_schema() -> Value {
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "tests" },
            "notices": notices_schema(),
            "count": { "type": "integer" },
            "depth": { "type": "integer" },
            "files": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "file": { "type": "string" },
                    "tests": { "type": "array", "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "name": { "type": "string" },
                            "line": { "type": "integer" }
                        },
                        "required": ["name", "line"]
                    }}
                },
                "required": ["file", "tests"]
            }},
            "testsOmitted": { "type": "integer" },
            "matches": { "type": "array", "items": symbol_ref_schema() },
            "notFound": { "type": "boolean" },
            "truncated": { "type": "boolean" }
        },
        "required": ["schemaVersion", "kind", "count", "depth", "files"]
    }))
}

// =============================================================================
// codegraph_history

/// The co-change report, or — when the symbol is not in the analysis graph —
/// only `notFound`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct HistoryOutput<'a> {
    pub schema_version: u32,
    pub kind: &'static str,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub report: Option<&'a CoChangeReport>,
    #[serde(skip_serializing_if = "is_false")]
    pub not_found: bool,
}

impl<'a> HistoryOutput<'a> {
    pub fn new(report: &'a CoChangeReport) -> Self {
        Self {
            schema_version: 1,
            kind: "history",
            report: Some(report),
            not_found: false,
        }
    }

    pub fn not_found() -> Self {
        Self {
            schema_version: 1,
            kind: "history",
            report: None,
            not_found: true,
        }
    }
}

fn analysis_symbol_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": { "type": "string" },
            "qualifiedName": { "type": "string" },
            "kind": { "type": "string" },
            "file": { "type": "string" },
            "line": { "type": "integer" }
        },
        "required": ["name", "qualifiedName", "kind", "file", "line"]
    })
}

pub(in crate::mcp::tools) fn history_output_schema() -> Value {
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "history" },
            "notices": notices_schema(),
            "seed": analysis_symbol_schema(),
            "commitsAnalyzed": { "type": "integer" },
            "maxCommits": { "type": "integer" },
            "minSupport": { "type": "integer" },
            "crossFilePairCount": { "type": "integer" },
            "sameFilePairCount": { "type": "integer" },
            "truncated": { "type": "boolean" },
            "pairs": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "a": analysis_symbol_schema(),
                    "b": analysis_symbol_schema(),
                    "timesChangedTogether": { "type": "integer" },
                    "totalChangesA": { "type": "integer" },
                    "totalChangesB": { "type": "integer" },
                    "confidence": { "type": "number" }
                },
                "required": [
                    "a", "b", "timesChangedTogether", "totalChangesA", "totalChangesB",
                    "confidence"
                ]
            }},
            "note": { "type": "string" },
            "notFound": { "type": "boolean" }
        },
        "required": ["schemaVersion", "kind"]
    }))
}

// =============================================================================
// codegraph_diagnostics

/// One compiler diagnostic; `symbol` is the innermost indexed symbol around
/// its line.
#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct DiagnosticRow {
    pub severity: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub line: u32,
    pub column: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::mcp::tools) struct DiagnosticFile {
    pub file: String,
    pub diagnostics: Vec<DiagnosticRow>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct DiagnosticsOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    /// `check`, `clippy` or `tsc`.
    pub checker: &'static str,
    /// `complete`, or `running` (still going in the background).
    pub status: &'static str,
    /// How long the run took (complete) or has been going (running).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Errors in the whole run, before filters (absent while running).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<usize>,
    /// Warnings in the whole run, before filters (absent while running).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<usize>,
    /// The checker's stderr tail when it failed without a diagnostic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub files: Vec<DiagnosticFile>,
    /// Diagnostics the `file`/`severity` filter left out.
    #[serde(skip_serializing_if = "is_zero")]
    pub filtered_out: usize,
    /// Matching diagnostics past `limit`.
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

impl DiagnosticsOutput {
    pub fn new(checker: &'static str, status: &'static str) -> Self {
        Self {
            schema_version: 1,
            kind: "diagnostics",
            checker,
            status,
            elapsed_ms: None,
            exit_code: None,
            errors: None,
            warnings: None,
            failure: None,
            files: Vec::new(),
            filtered_out: 0,
            omitted: 0,
            truncated: false,
        }
    }
}

pub(in crate::mcp::tools) fn diagnostics_output_schema() -> Value {
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "diagnostics" },
            "notices": notices_schema(),
            "checker": { "enum": ["check", "clippy", "tsc"] },
            "status": { "enum": ["complete", "running"] },
            "elapsedMs": { "type": "integer" },
            "exitCode": { "type": "integer" },
            "errors": { "type": "integer" },
            "warnings": { "type": "integer" },
            "failure": { "type": "string" },
            "files": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "file": { "type": "string" },
                    "diagnostics": { "type": "array", "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "severity": { "enum": ["error", "warning", "note"] },
                            "code": { "type": "string" },
                            "line": { "type": "integer" },
                            "column": { "type": "integer" },
                            "symbol": { "type": "string" },
                            "message": { "type": "string" }
                        },
                        "required": ["severity", "line", "column", "message"]
                    }}
                },
                "required": ["file", "diagnostics"]
            }},
            "filteredOut": { "type": "integer" },
            "omitted": { "type": "integer" },
            "truncated": { "type": "boolean" }
        },
        "required": ["schemaVersion", "kind", "checker", "status", "files"]
    }))
}

#[cfg(test)]
mod tests {
    use super::super::{Trim, fitted};
    use super::*;
    use crate::analyze::{CoChangePairSummary, SymbolRef as AnalysisSymbol};

    /// The JSON-Schema subset the output schemas use: oneOf, enum, const,
    /// type, properties, required, additionalProperties:false, items.
    fn matches(schema: &Value, value: &Value) -> bool {
        if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            if !branches.iter().any(|branch| matches(branch, value)) {
                return false;
            }
        }
        if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
            if !allowed.contains(value) {
                return false;
            }
        }
        if schema
            .get("const")
            .is_some_and(|constant| constant != value)
        {
            return false;
        }
        if let Some(ty) = schema.get("type").and_then(Value::as_str) {
            let ok = match ty {
                "object" => value.is_object(),
                "array" => value.is_array(),
                "string" => value.is_string(),
                "integer" => value.is_i64() || value.is_u64(),
                "number" => value.is_number(),
                "boolean" => value.is_boolean(),
                _ => true,
            };
            if !ok {
                return false;
            }
        }
        if let Some(object) = value.as_object() {
            let properties = schema.get("properties").and_then(Value::as_object);
            let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
            if closed
                && object
                    .keys()
                    .any(|key| properties.is_none_or(|p| !p.contains_key(key)))
            {
                return false;
            }
            let required = schema.get("required").and_then(Value::as_array);
            if required.is_some_and(|required| {
                required
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|key| !object.contains_key(key))
            }) {
                return false;
            }
            if let Some(properties) = properties {
                for (key, child) in object {
                    if properties.get(key).is_some_and(|sub| !matches(sub, child)) {
                        return false;
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

    fn assert_valid(schema: &Value, value: &Value) {
        assert!(matches(schema, value), "payload fails its schema: {value}");
    }

    fn analysis_symbol(name: &str, file: &str) -> AnalysisSymbol {
        AnalysisSymbol {
            name: name.into(),
            qualified_name: format!("{file}::{name}"),
            kind: "function".into(),
            file: file.into(),
            line: 3,
        }
    }

    #[test]
    fn the_checker_rejects_undeclared_fields() {
        let schema = tests_output_schema();
        let mut value = serde_json::to_value(TestsOutput::new(4)).unwrap();
        assert_valid(&schema, &value);
        value["surprise"] = json!(1);
        assert!(!matches(&schema, &value));
    }

    #[test]
    fn tests_payloads_match_their_schema() {
        let schema = tests_output_schema();
        let mut output = TestsOutput::new(4);
        output.count = 3;
        output.files = vec![TestFile {
            file: "tests/auth.rs".into(),
            tests: vec![
                TestRow {
                    name: "parses".into(),
                    line: 10,
                },
                TestRow {
                    name: "trims".into(),
                    line: 20,
                },
            ],
        }];
        output.tests_omitted = 1;
        output.matches = vec![
            SymbolRef {
                name: "parse".into(),
                kind: "function",
                file: "src/a.rs".into(),
                line: 1,
            },
            SymbolRef {
                name: "parse".into(),
                kind: "method",
                file: "src/b.rs".into(),
                line: 2,
            },
        ];
        let value = fitted(
            &output,
            24_000,
            &[Trim::groups("files", "tests", "testsOmitted")],
        );
        assert_valid(&schema, &value);
        assert_eq!(value["testsOmitted"], 1);
        assert_valid(
            &schema,
            &serde_json::to_value(TestsOutput::not_found(4)).unwrap(),
        );
        let cut = fitted(
            &output,
            60,
            &[Trim::groups("files", "tests", "testsOmitted")],
        );
        assert_eq!(cut["truncated"], true);
        assert_valid(&schema, &cut);
    }

    #[test]
    fn history_payloads_match_their_schema() {
        let schema = history_output_schema();
        let report = CoChangeReport {
            seed: Some(analysis_symbol("parse", "src/a.rs")),
            commits_analyzed: 12,
            max_commits: 500,
            min_support: 2,
            cross_file_pair_count: 1,
            same_file_pair_count: 4,
            truncated: false,
            pairs: vec![CoChangePairSummary {
                a: analysis_symbol("parse", "src/a.rs"),
                b: analysis_symbol("login", "src/b.rs"),
                times_changed_together: 3,
                total_changes_a: 4,
                total_changes_b: 6,
                confidence: 0.5,
            }],
            note: String::new(),
        };
        let value = fitted(
            &HistoryOutput::new(&report),
            24_000,
            &[Trim::plain("pairs")],
        );
        assert_valid(&schema, &value);
        assert_eq!(value["kind"], "history");
        assert_eq!(value["commitsAnalyzed"], 12);
        assert_eq!(value["pairs"][0]["confidence"], 0.5);
        let missing = serde_json::to_value(HistoryOutput::not_found()).unwrap();
        assert_eq!(
            missing,
            json!({ "schemaVersion": 1, "kind": "history", "notFound": true })
        );
        assert_valid(&schema, &missing);
    }

    #[test]
    fn diagnostics_payloads_match_their_schema() {
        let schema = diagnostics_output_schema();
        let mut output = DiagnosticsOutput::new("check", "complete");
        output.elapsed_ms = Some(1_200);
        output.exit_code = Some(101);
        output.errors = Some(1);
        output.warnings = Some(1);
        output.files = vec![DiagnosticFile {
            file: "src/lib.rs".into(),
            diagnostics: vec![
                DiagnosticRow {
                    severity: "error",
                    code: Some("E0308".into()),
                    line: 6,
                    column: 21,
                    symbol: Some("broken".into()),
                    message: "mismatched types".into(),
                },
                DiagnosticRow {
                    severity: "warning",
                    code: None,
                    line: 1,
                    column: 1,
                    symbol: None,
                    message: "unused".into(),
                },
            ],
        }];
        output.filtered_out = 2;
        output.omitted = 3;
        assert_valid(
            &schema,
            &fitted(
                &output,
                24_000,
                &[Trim::groups("files", "tests", "testsOmitted")],
            ),
        );

        let mut running = DiagnosticsOutput::new("clippy", "running");
        running.elapsed_ms = Some(4_000);
        let value = serde_json::to_value(&running).unwrap();
        assert_valid(&schema, &value);
        assert_eq!(
            value,
            json!({
                "schemaVersion": 1, "kind": "diagnostics", "checker": "clippy",
                "status": "running", "elapsedMs": 4000, "files": []
            })
        );

        let mut failed = DiagnosticsOutput::new("check", "complete");
        failed.errors = Some(0);
        failed.warnings = Some(0);
        failed.failure = Some("error: no matching package".into());
        assert_valid(&schema, &serde_json::to_value(&failed).unwrap());
    }
}
