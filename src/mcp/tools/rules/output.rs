//! Payloads of `codegraph_rules`, one per action, and the output schema
//! that declares every field they carry.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use super::super::output::{notices_schema, successes_or_error};
use crate::analyze::rules::score::{RuleScore, UnitCounts};
use crate::analyze::rules::{ScoreReport, SkeletonCheck, VariantCall, VariantNode};

fn is_false(value: &bool) -> bool {
    !*value
}

/// One example's outcome.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckExample {
    /// `bad[0]`, `good[1]`.
    pub example: String,
    pub passed: bool,
    /// The engine's explanation of a failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// One rule's outcome.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckRule {
    pub id: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<CheckExample>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub passed: usize,
    pub failed: usize,
    pub rules: Vec<CheckRule>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// A place backing a finding (`file` only when not the finding's).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EvidenceRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub line: u32,
    pub note: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FindingRow {
    pub rule: String,
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RunOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    /// Rules run.
    pub rules: usize,
    pub files_scanned: usize,
    /// Findings in all.
    pub total: usize,
    pub by_rule: BTreeMap<String, usize>,
    pub findings: Vec<FindingRow>,
    /// Findings not listed (past `limit` or the output budget).
    pub omitted: usize,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// The enclosing function's window (`source` omitted when this session
/// already holds it unchanged).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FunctionOut {
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub already_sent: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VariantFinding {
    pub rule: String,
    pub line: u32,
    pub message: String,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VariantOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub file: String,
    pub line: u32,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionOut>,
    pub node: VariantNode,
    pub skeleton: String,
    pub skeleton_check: SkeletonCheck,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<VariantCall>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<VariantFinding>,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SaveOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub id: String,
    /// Project-relative path written.
    pub path: String,
    pub replaced: bool,
    /// Saved rules the project now has.
    pub saved: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ScoreOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    pub corpus: String,
    pub scoring: &'static str,
    pub units: UnitCounts,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub failures: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_rate: Option<f64>,
    pub positives: usize,
    pub rules: Vec<RuleScore>,
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

impl ScoreOutput {
    pub fn from_report(report: &ScoreReport) -> Self {
        Self {
            schema_version: 1,
            kind: "ruleScore",
            corpus: report.corpus.clone(),
            scoring: report.scoring,
            units: report.units.clone(),
            failures: report.failures.clone(),
            base_rate: report.base_rate,
            positives: report.positives,
            rules: report.rules.clone(),
            complete: report.complete,
            next_cursor: report.next_cursor.clone(),
            elapsed_ms: report.elapsed_ms,
            truncated: false,
        }
    }
}

/// A success branch: `kind` const, `notices`, and `properties`.
fn branch(kind: &str, properties: Value, required: &[&str]) -> Value {
    let mut props = serde_json::Map::new();
    props.insert("schemaVersion".into(), json!({ "type": "integer" }));
    props.insert("kind".into(), json!({ "const": kind }));
    props.insert("notices".into(), notices_schema());
    if let Value::Object(more) = properties {
        props.extend(more);
    }
    let mut all = vec!["schemaVersion", "kind"];
    all.extend_from_slice(required);
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": props,
        "required": all
    })
}

/// The declared shape of every `codegraph_rules` answer.
pub(in crate::mcp::tools) fn rules_output_schema() -> Value {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    let evidence = json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "file": { "type": "string" },
                "line": { "type": "integer" },
                "note": { "type": "string" }
            },
            "required": ["line", "note"]
        }
    });
    let check = branch(
        "ruleCheck",
        json!({
            "passed": { "type": "integer" },
            "failed": { "type": "integer" },
            "rules": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": { "type": "string" },
                        "passed": { "type": "boolean" },
                        "line": { "type": "integer" },
                        "error": { "type": "string" },
                        "examples": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "example": { "type": "string" },
                                    "passed": { "type": "boolean" },
                                    "why": { "type": "string" }
                                },
                                "required": ["example", "passed"]
                            }
                        }
                    },
                    "required": ["id", "passed"]
                }
            },
            "errors": strings.clone()
        }),
        &["passed", "failed", "rules"],
    );
    let run = branch(
        "ruleRun",
        json!({
            "rules": { "type": "integer" },
            "filesScanned": { "type": "integer" },
            "total": { "type": "integer" },
            "byRule": { "type": "object", "additionalProperties": { "type": "integer" } },
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "rule": { "type": "string" },
                        "file": { "type": "string" },
                        "line": { "type": "integer" },
                        "function": { "type": "string" },
                        "message": { "type": "string" },
                        "evidence": evidence.clone()
                    },
                    "required": ["rule", "file", "line", "message"]
                }
            },
            "omitted": { "type": "integer" },
            "truncated": { "type": "boolean" }
        }),
        &[
            "rules",
            "filesScanned",
            "total",
            "byRule",
            "findings",
            "omitted",
        ],
    );
    let variant = branch(
        "ruleVariant",
        json!({
            "file": { "type": "string" },
            "line": { "type": "integer" },
            "language": { "type": "string" },
            "function": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string" },
                    "startLine": { "type": "integer" },
                    "endLine": { "type": "integer" },
                    "source": { "type": "string" },
                    "alreadySent": { "type": "boolean" }
                },
                "required": ["name", "startLine", "endLine"]
            },
            "node": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string" },
                    "startLine": { "type": "integer" },
                    "endLine": { "type": "integer" },
                    "tree": { "type": "string" }
                },
                "required": ["kind", "startLine", "endLine", "tree"]
            },
            "skeleton": { "type": "string" },
            "skeletonCheck": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "passed": { "type": "boolean" },
                    "failures": strings.clone()
                },
                "required": ["passed"]
            },
            "calls": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "line": { "type": "integer" },
                        "callee": { "type": "string" },
                        "resolvesTo": strings.clone()
                    },
                    "required": ["line", "callee"]
                }
            },
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "rule": { "type": "string" },
                        "line": { "type": "integer" },
                        "message": { "type": "string" },
                        "confidence": { "type": "number" },
                        "evidence": evidence
                    },
                    "required": ["rule", "line", "message", "confidence"]
                }
            },
            "truncated": { "type": "boolean" }
        }),
        &[
            "file",
            "line",
            "language",
            "node",
            "skeleton",
            "skeletonCheck",
        ],
    );
    let save = branch(
        "ruleSave",
        json!({
            "id": { "type": "string" },
            "path": { "type": "string" },
            "replaced": { "type": "boolean" },
            "saved": { "type": "integer" }
        }),
        &["id", "path", "replaced", "saved"],
    );
    let count = json!({ "type": "integer" });
    let score = branch(
        "ruleScore",
        json!({
            "corpus": { "type": "string" },
            "scoring": { "enum": ["labeled", "differential"] },
            "units": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "total": count.clone(),
                    "done": count.clone(),
                    "indexed": count.clone(),
                    "failed": count.clone(),
                    "pending": count.clone()
                },
                "required": ["total", "done", "indexed", "failed", "pending"]
            },
            "failures": { "type": "object", "additionalProperties": { "type": "string" } },
            "baseRate": { "type": "number" },
            "positives": count.clone(),
            "rules": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": { "type": "string" },
                        "findings": count.clone(),
                        "tp": count.clone(),
                        "fp": count.clone(),
                        "unlabeled": count.clone(),
                        "notDiscriminating": count.clone(),
                        "offFix": count.clone(),
                        "background": count.clone(),
                        "precision": { "type": "number" },
                        "recall": { "type": "number" },
                        "verdict": { "enum": ["keep", "discard"] },
                        "reason": { "type": "string" }
                    },
                    "required": ["id", "findings", "tp", "verdict", "reason"]
                }
            },
            "complete": { "type": "boolean" },
            "nextCursor": { "type": "string" },
            "elapsedMs": count,
            "truncated": { "type": "boolean" }
        }),
        &[
            "corpus",
            "scoring",
            "units",
            "positives",
            "rules",
            "complete",
            "elapsedMs",
        ],
    );
    successes_or_error(vec![check, run, variant, save, score])
}
