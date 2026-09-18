//! codegraph_recall — cross-session memory: what earlier agent sessions in
//! this repository already explored, looked up, broke and fixed (see
//! [`crate::history::memory`]).
//!
//! Read-only and bounded: the history store is opened read-only, every
//! query is indexed under a hard deadline, and the answer is at most
//! [`crate::history::memory::RECALL_BUDGET`] bytes. A stale store is refreshed by a detached
//! `codegraph history ingest --incremental`, never inline.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::context::ToolHandler;
use super::format::num_or;
use super::output::{notices_schema, success_or_error};
use super::schema::ToolResult;
use crate::error::{CodeGraphError, Result};
use crate::history::background::{history_enabled, is_stale, spawn_background_ingest};
use crate::history::memory::{About, RecallRequest, last_ingest_ms, recall_at};
use crate::history::{default_history_path, now_ms, parse_duration_ms, repo_root_of};
use crate::utils::clamp;

/// Hard deadline of one recall (its queries are interrupted past it).
const RECALL_DEADLINE: Duration = Duration::from_millis(250);

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_recall(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        if !history_enabled() {
            return Ok(self.error_result("Agent history is disabled (CODEGRAPH_HISTORY=0)."));
        }
        let project_path = args.get("projectPath").and_then(Value::as_str);
        let root: PathBuf = match self.get_code_graph(project_path) {
            Ok(cg) => cg.get_project_root().to_path_buf(),
            Err(e) => match project_path.map(Path::new).filter(|p| p.is_dir()) {
                Some(dir) => dir.to_path_buf(),
                None => return Err(e),
            },
        };
        let since_ms = match args.get("since").and_then(Value::as_str) {
            Some(s) => match parse_duration_ms(s) {
                Some(d) => Some(now_ms() - d),
                None => {
                    return Ok(self.validation_error_result(
                        "since",
                        &format!("invalid since `{s}`"),
                        "a duration like 12h, 7d or 2w",
                        Some("string"),
                    ));
                }
            },
            None => None,
        };
        let limit = clamp(num_or(args, "limit", 3.0), 1.0, 20.0) as usize;
        let mut about = About::parse(args.get("about").and_then(Value::as_str).unwrap_or("last"));
        // A codegraph project nested in the repository names paths relative
        // to itself; the memory keys them relative to the repository.
        if let Some(repo) = repo_root_of(&root) {
            if let Ok(base) = root.strip_prefix(&repo) {
                about = about.under(&base.to_string_lossy());
            }
        }
        let db = default_history_path();
        let request = RecallRequest {
            about,
            since_ms,
            limit,
        };
        let report = recall_at(&db, &root, &request, RECALL_DEADLINE)
            .map_err(|e| CodeGraphError::other(format!("recall: {e}")))?;
        if is_stale(last_ingest_ms(&db), now_ms()) {
            let _ = spawn_background_ingest(&db);
        }
        self.structured_result(&report.render_text(), &report)
    }
}

/// The declared shape of a recall answer (every field the payload can carry).
pub(in crate::mcp::tools) fn recall_output_schema() -> Value {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    let file = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string" },
            "op": { "enum": ["edit", "read", "search"] },
            "n": { "type": "integer" },
            "unchanged": { "type": "boolean" }
        },
        "required": ["path", "op", "n"]
    });
    let episode = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "ago": { "type": "string" },
            "source": { "type": "string" },
            "calls": { "type": "integer" },
            "outcome": { "type": "string" },
            "files": { "type": "array", "items": file },
            "moreFiles": { "type": "integer" }
        },
        "required": ["ago", "source", "calls", "outcome", "files"]
    });
    let symbol = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": { "type": "string" },
            "lookups": { "type": "integer" },
            "episodes": { "type": "integer" },
            "found": strings,
            "ago": { "type": "string" }
        },
        "required": ["name", "lookups", "episodes", "found", "ago"]
    });
    let failure = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "ago": { "type": "string" },
            "kind": { "type": "string" },
            "command": { "type": "string" },
            "codes": strings,
            "repeats": { "type": "integer" },
            "fixedLater": { "type": "boolean" }
        },
        "required": ["ago", "kind", "command", "repeats", "fixedLater"]
    });
    let pair = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "a": { "type": "string" },
            "b": { "type": "string" },
            "episodes": { "type": "integer" },
            "commits": { "type": "integer" }
        },
        "required": ["a", "b", "episodes", "commits"]
    });
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "recall" },
            "notices": notices_schema(),
            "about": { "type": "string" },
            "episodes": { "type": "array", "items": episode },
            "symbols": { "type": "array", "items": symbol },
            "failures": { "type": "array", "items": failure },
            "cochange": { "type": "array", "items": pair },
            "truncated": { "type": "boolean" },
            "note": { "type": "string" }
        },
        "required": ["schemaVersion", "kind", "about"]
    }))
}
