//! The `codegraph_grep` payload, its output schema, and its human text.

use serde::Serialize;
use serde_json::{Value, json};

use super::super::format::json_len;
use super::super::output::{notices_schema, success_or_error};

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Hit lines of one file under the definition they sit in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct GrepHitGroup {
    /// Innermost indexed definition containing the lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// `N: text` per matching line (trimmed, cut to 160 characters), with
    /// context `N- text` lines verbatim; a bare `N` where the value is
    /// withheld (configuration files, whose values may be secrets).
    pub lines: Vec<String>,
}

/// One file with matches.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct GrepFileRow {
    pub file: String,
    /// Matching lines in the file (absent in `files` mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    /// Matching lines neither shown in `hits` nor listed in `alreadySent`.
    #[serde(skip_serializing_if = "is_zero")]
    pub more: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hits: Vec<GrepHitGroup>,
    /// Matching lines this session already received (from an earlier
    /// grep, explore, or node call) and the file is unchanged since: not
    /// repeated.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub already_sent: Vec<u32>,
}

/// Files with matches not listed on this page, per directory.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct GrepDirRow {
    pub dir: String,
    pub files: usize,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct GrepOutput {
    pub schema_version: u32,
    pub kind: &'static str,
    /// Files with matches, most relevant first.
    pub files: Vec<GrepFileRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<GrepDirRow>,
    /// Matching lines and files among the files this page searched; absent
    /// when `files` lists them all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_hits: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_files: Option<usize>,
    /// Present when this page searched only part of the files in scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub searched_files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_files: Option<usize>,
    /// Files in scope too large to search.
    #[serde(skip_serializing_if = "is_zero")]
    pub skipped_files: usize,
    #[serde(skip_serializing_if = "is_false")]
    pub values_withheld: bool,
    /// The time or byte budget ran out before every file in scope was
    /// searched; `nextCursor` continues.
    #[serde(skip_serializing_if = "is_false")]
    pub incomplete: bool,
    /// Files with matches were left for the next page (`dirs` says where).
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl GrepOutput {
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            kind: "grep",
            files: Vec::new(),
            dirs: Vec::new(),
            total_hits: None,
            total_files: None,
            searched_files: None,
            candidate_files: None,
            skipped_files: 0,
            values_withheld: false,
            incomplete: false,
            truncated: false,
            next_cursor: None,
        }
    }

    /// Drop trailing directory rows, then trailing file rows, until the
    /// payload fits `budget`. Returns how many file rows were dropped (they
    /// belong to the next page).
    pub fn fit_to(&mut self, budget: usize) -> usize {
        let mut dropped = 0;
        while json_len(self) > budget {
            if self.dirs.pop().is_some() {
                self.truncated = true;
            } else if self.files.len() > 1 {
                self.files.pop();
                dropped += 1;
                self.truncated = true;
            } else {
                break;
            }
        }
        dropped
    }

    /// The human-readable rendering (what the CLI and logs show): grouped
    /// like grep tools print, a path header then `N: text` lines.
    pub fn render(&self) -> String {
        let total_files = self.total_files.unwrap_or(self.files.len());
        let mut lines = vec![match self.total_hits {
            Some(hits) => format!("{hits} matching lines in {total_files} files"),
            None => format!("{total_files} files"),
        }];
        if let (Some(searched), Some(candidates)) = (self.searched_files, self.candidate_files) {
            lines[0].push_str(&format!(" (searched {searched} of {candidates} files)"));
        }
        for file in &self.files {
            match file.count {
                Some(count) => lines.push(format!("{} ({count})", file.file)),
                None => lines.push(file.file.clone()),
            }
            for group in &file.hits {
                if let Some(symbol) = &group.symbol {
                    lines.push(format!("  in {symbol}:"));
                }
                lines.extend(group.lines.iter().map(|line| format!("    {line}")));
            }
            if !file.already_sent.is_empty() {
                let listed: Vec<String> = file.already_sent.iter().map(u32::to_string).collect();
                lines.push(format!("  already sent: lines {}", listed.join(", ")));
            }
            if file.more > 0 {
                lines.push(format!("  … {} more", file.more));
            }
        }
        if !self.dirs.is_empty() {
            lines.push("More matches in:".into());
            for dir in &self.dirs {
                lines.push(format!(
                    "  {}/ ({} files, {})",
                    dir.dir, dir.files, dir.count
                ));
            }
        }
        if self.incomplete {
            lines.push("The search stopped at its time/byte budget before every file.".into());
        }
        if let Some(cursor) = &self.next_cursor {
            lines.push(format!("Next page: cursor \"{cursor}\""));
        }
        lines.join("\n")
    }
}

pub(in crate::mcp::tools) fn grep_output_schema() -> Value {
    let group = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "symbol": { "type": "string" },
            "lines": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["lines"]
    });
    success_or_error(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "grep" },
            "notices": notices_schema(),
            "files": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "file": { "type": "string" },
                    "count": { "type": "integer" },
                    "more": { "type": "integer" },
                    "hits": { "type": "array", "items": group },
                    "alreadySent": { "type": "array", "items": { "type": "integer" } }
                },
                "required": ["file"]
            }},
            "dirs": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "dir": { "type": "string" },
                    "files": { "type": "integer" },
                    "count": { "type": "integer" }
                },
                "required": ["dir", "files", "count"]
            }},
            "totalHits": { "type": "integer" },
            "totalFiles": { "type": "integer" },
            "searchedFiles": { "type": "integer" },
            "candidateFiles": { "type": "integer" },
            "skippedFiles": { "type": "integer" },
            "valuesWithheld": { "type": "boolean" },
            "incomplete": { "type": "boolean" },
            "truncated": { "type": "boolean" },
            "nextCursor": { "type": "string" }
        },
        "required": ["schemaVersion", "kind", "files"]
    }))
}
