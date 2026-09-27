//! Ground truth: the rows of a corpus's `ground_truth.jsonl`, indexed by
//! file — `tools/bugbench/gt.py`, row for row.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

pub(crate) const BAD: &str = "bad";
pub(crate) const GOOD: &str = "good";

/// One labeled row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GtRow {
    /// Index in the loaded rows.
    pub id: usize,
    pub file: String,
    pub label: String,
    /// `function`, `line` or `file`.
    pub granularity: String,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub function: Option<String>,
    pub cwe: Option<String>,
    pub category: Option<String>,
    pub flaw_lines: Vec<i64>,
}

impl GtRow {
    /// The class a row is scored under: its CWE, else its category, else
    /// `?` (an empty string counts as absent, as in Python).
    pub fn klass(&self) -> &str {
        [&self.cwe, &self.category]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty())
            .map_or("?", String::as_str)
    }

    /// Does a finding at `line` fall on this row? Function rows: inside the
    /// function. Line rows: within `slack` lines of the span. File rows (or
    /// rows without lines): anywhere in the file.
    pub fn covers(&self, line: i64, slack: i64) -> bool {
        let Some(start) = self.line_start else {
            return true;
        };
        if self.granularity == "file" {
            return true;
        }
        let end = self.line_end.unwrap_or(start);
        let pad = if self.granularity == "line" { slack } else { 0 };
        start - pad <= line && line <= end + pad
    }
}

impl GtRow {
    /// Does [`GtRow::covers`] hold for some line of `start..=end`?
    pub fn overlaps(&self, start: i64, end: i64, slack: i64) -> bool {
        let Some(row_start) = self.line_start else {
            return true;
        };
        if self.granularity == "file" {
            return true;
        }
        let row_end = self.line_end.unwrap_or(row_start);
        let pad = if self.granularity == "line" { slack } else { 0 };
        row_start - pad <= end && start <= row_end + pad
    }
}

/// Python truthiness of a JSON value (`raw.get(k) or …`).
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python `str(value)` for the scalar values the rows carry.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        other => other.to_string(),
    }
}

fn int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

fn string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        other => Some(py_str(other)),
    }
}

/// The rows of `path` whose `file` passes `keep` (all when `None`).
pub(crate) fn load_rows(
    path: &Path,
    keep: Option<&dyn Fn(&str) -> bool>,
) -> Result<Vec<GtRow>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_rows(&text, keep).map_err(|e| format!("{}: {e}", path.display()))
}

/// [`load_rows`] over the text of a `ground_truth.jsonl`.
pub(crate) fn parse_rows(
    text: &str,
    keep: Option<&dyn Fn(&str) -> bool>,
) -> Result<Vec<GtRow>, String> {
    let mut rows = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let raw: Value =
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", number + 1))?;
        let Some(file) = raw.get("file").map(py_str) else {
            return Err(format!("line {}: no `file`", number + 1));
        };
        if keep.is_some_and(|keep| !keep(&file)) {
            continue;
        }
        let granularity = if truthy(raw.get("granularity")) {
            py_str(&raw["granularity"])
        } else if truthy(raw.get("line_start")) {
            "line".to_string()
        } else {
            "file".to_string()
        };
        let flaw_lines = raw
            .get("flaw_lines")
            .and_then(Value::as_array)
            .map(|lines| lines.iter().filter_map(|v| int(Some(v))).collect())
            .unwrap_or_default();
        rows.push(GtRow {
            id: rows.len(),
            file,
            label: raw.get("label").map(py_str).unwrap_or_default(),
            granularity,
            line_start: int(raw.get("line_start")),
            line_end: int(raw.get("line_end")),
            function: string(raw.get("function")),
            cwe: string(raw.get("cwe")),
            category: string(raw.get("category")),
            flaw_lines,
        });
    }
    Ok(rows)
}

/// Rows grouped by file for point lookups.
pub(crate) struct FileIndex<'a> {
    by_file: HashMap<&'a str, Vec<&'a GtRow>>,
}

impl<'a> FileIndex<'a> {
    pub fn new(rows: &'a [GtRow]) -> Self {
        let mut by_file: HashMap<&str, Vec<&GtRow>> = HashMap::new();
        for row in rows {
            by_file.entry(row.file.as_str()).or_default().push(row);
        }
        Self { by_file }
    }

    /// The rows of `file` covering `line`, with a label in `labels` (any
    /// label when `None`), in row order.
    pub fn at(&self, file: &str, line: i64, slack: i64, labels: Option<&[&str]>) -> Vec<&'a GtRow> {
        self.by_file
            .get(file)
            .into_iter()
            .flatten()
            .filter(|row| labels.is_none_or(|labels| labels.contains(&row.label.as_str())))
            .filter(|row| row.covers(line, slack))
            .copied()
            .collect()
    }
}
