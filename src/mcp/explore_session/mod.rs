use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mcp::tools::ToolResult;
use crate::utils::sha256_hex;

mod grep;
#[cfg(test)]
pub(crate) use grep::GrepSentFile;
pub(crate) use grep::{GREP_SESSION_ARG, GrepSent};

pub(crate) const SESSION_ARG: &str = "_cgExploreSession";
const MAX_PROJECTS: usize = 4;
const MAX_CALLS: usize = 8;
const MAX_FILES: usize = 24;
const MAX_RANGES: usize = 24;
const MAX_VIEW_CALLS: usize = 4;

pub(crate) fn dedup_enabled() -> bool {
    std::env::var("CODEGRAPH_EXPLORE_DEDUP")
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(true)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LineRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileEmission {
    pub path: String,
    pub ranges: Vec<LineRange>,
    pub bytes: usize,
    pub fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallRecord {
    pub index: u64,
    pub files: Vec<FileEmission>,
    pub response_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectState {
    pub project_root: String,
    pub call_count: u64,
    pub response_bytes: u64,
    pub calls: Vec<CallRecord>,
}

#[derive(Default)]
pub(crate) struct ExploreSessionState {
    projects: VecDeque<ProjectState>,
    /// `codegraph_grep` hits sent per project, kept out of `projects` (see
    /// [`grep`]), least recently used first.
    grep: VecDeque<(PathBuf, GrepSent)>,
}

impl ExploreSessionState {
    pub(crate) fn view_for(&self, project_root: &Path) -> ProjectState {
        let key = project_key(project_root);
        self.projects
            .iter()
            .find(|project| project_key(Path::new(&project.project_root)) == key)
            .map(|project| {
                let mut view = project.clone();
                view.calls = view.calls.into_iter().rev().take(MAX_VIEW_CALLS).collect();
                view.calls.reverse();
                view
            })
            .unwrap_or(ProjectState {
                project_root: project_root.to_string_lossy().into_owned(),
                call_count: 0,
                response_bytes: 0,
                calls: Vec::new(),
            })
    }

    /// The grep hits this session already received for `project_root`.
    pub(crate) fn grep_view_for(&self, project_root: &Path) -> GrepSent {
        let key = project_key(project_root);
        self.grep
            .iter()
            .find(|(root, _)| *root == key)
            .map(|(_, sent)| sent.clone())
            .unwrap_or_default()
    }

    fn record_grep(&mut self, project_root: &Path, payload: &Value) {
        let key = project_key(project_root);
        let mut sent = match self.grep.iter().position(|(root, _)| *root == key) {
            Some(index) => self
                .grep
                .remove(index)
                .map(|(_, sent)| sent)
                .unwrap_or_default(),
            None => GrepSent::default(),
        };
        sent.record(project_root, payload);
        self.grep.push_back((key, sent));
        while self.grep.len() > MAX_PROJECTS {
            self.grep.pop_front();
        }
    }

    /// Record the source a delivered result carried. Pass the result as it
    /// went on the wire (after the MCP projection), so the ledger never holds
    /// lines the client did not receive.
    pub(crate) fn record(&mut self, project_root: &Path, result: &ToolResult) {
        let Some(payload) = result.structured_content.as_ref() else {
            return;
        };
        // Every tool that emits source participates: `explore` (many files),
        // the `node` file view (one file, paged), and `node` symbol reads
        // (a definition's lines). Agents re-fetch the same ranges constantly,
        // so the ledger has to span tools, not just explore.
        let kind = payload.get("kind").and_then(Value::as_str);
        if kind == Some("grep") {
            self.record_grep(project_root, payload);
            return;
        }
        if !matches!(kind, Some("explore") | Some("file") | Some("node")) {
            return;
        }
        let files = emissions(project_root, payload);
        // A node call that sent no source (an outline, a back-reference, a
        // miss) holds nothing to dedup against; don't let it push a call
        // that did out of the bounded view.
        if files.is_empty() && kind != Some("explore") {
            return;
        }
        let key = project_key(project_root);
        let index = self
            .projects
            .iter()
            .position(|project| project_key(Path::new(&project.project_root)) == key);
        let mut project = index
            .and_then(|index| self.projects.remove(index))
            .unwrap_or(ProjectState {
                project_root: project_root.to_string_lossy().into_owned(),
                call_count: 0,
                response_bytes: 0,
                calls: Vec::new(),
            });
        project.call_count += 1;
        let response_bytes = serde_json::to_string(payload).map_or(0, |text| text.len()) as u64;
        project.response_bytes = project.response_bytes.saturating_add(response_bytes);
        project.calls.push(CallRecord {
            index: project.call_count,
            files,
            response_bytes,
        });
        if project.calls.len() > MAX_CALLS {
            project.calls.drain(..project.calls.len() - MAX_CALLS);
        }
        self.projects.push_back(project);
        while self.projects.len() > MAX_PROJECTS {
            self.projects.pop_front();
        }
    }
}

pub(crate) fn served_ranges(prior: &ProjectState, path: &str, fingerprint: &str) -> Vec<LineRange> {
    coalesce(
        prior
            .calls
            .iter()
            .flat_map(|call| &call.files)
            .filter(|file| file.path == path && file.fingerprint.as_deref() == Some(fingerprint))
            .flat_map(|file| file.ranges.clone())
            .collect(),
    )
    .0
}

pub(crate) fn file_fingerprint(root: &Path, relative: &str) -> Option<String> {
    let bytes = std::fs::read(root.join(relative)).ok()?;
    Some(content_fingerprint(&bytes))
}

/// The fingerprint [`file_fingerprint`] gives a file with these contents.
pub(crate) fn content_fingerprint(bytes: &[u8]) -> String {
    format!("{}:{}", bytes.len(), &sha256_hex(bytes)[..16])
}

/// Whether `path`'s lines `start..=end` went out earlier in this session
/// and the file is byte-identical on disk since.
pub(crate) fn range_already_sent(
    prior: &ProjectState,
    root: &Path,
    path: &str,
    start: usize,
    end: usize,
) -> bool {
    let Some(fingerprint) = file_fingerprint(root, path) else {
        return false;
    };
    served_ranges(prior, path, &fingerprint)
        .iter()
        .any(|range| range.start <= start && range.end >= end)
}

/// The line range a `source` string really covers when it claims to start at
/// `start` and end at `end`. `None` when the text does not span exactly those
/// lines — a string some later pass shortened must not be recorded as sent.
fn verbatim_range(start: u64, end: u64, source: &str) -> Option<LineRange> {
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    let lines = source.split('\n').count();
    (start >= 1 && end >= start && end - start + 1 == lines).then_some(LineRange { start, end })
}

/// Line ranges of one file gathered from `(start, end, source)` triples.
fn emission(root: &Path, path: &str, spans: &[(u64, u64, &str)]) -> Option<FileEmission> {
    let ranges = spans
        .iter()
        .filter_map(|(start, end, source)| verbatim_range(*start, *end, source))
        .collect();
    let (ranges, _) = coalesce(ranges);
    if ranges.is_empty() {
        return None;
    }
    Some(FileEmission {
        fingerprint: file_fingerprint(root, path),
        path: path.to_string(),
        ranges,
        bytes: spans.iter().map(|(_, _, source)| source.len()).sum(),
    })
}

/// The `node` file view emits one window of one file.
fn file_view_emission(root: &Path, payload: &Value) -> Option<FileEmission> {
    let span = (
        payload["startLine"].as_u64()?,
        payload["endLine"].as_u64()?,
        payload["source"].as_str()?,
    );
    emission(root, payload["path"].as_str()?, &[span])
}

/// `node` symbol reads emit each match's `code`, which starts at
/// `codeStartLine` when present and at the symbol's `line` otherwise.
fn symbol_emissions(root: &Path, payload: &Value) -> Vec<FileEmission> {
    let mut by_file: Vec<(&str, Vec<(u64, u64, &str)>)> = Vec::new();
    for detail in payload["matches"].as_array().into_iter().flatten() {
        let (Some(file), Some(code)) = (detail["file"].as_str(), detail["code"].as_str()) else {
            continue;
        };
        let Some(start) = detail["codeStartLine"]
            .as_u64()
            .or_else(|| detail["line"].as_u64())
        else {
            continue;
        };
        let end = start + code.split('\n').count() as u64 - 1;
        match by_file.iter_mut().find(|(path, _)| *path == file) {
            Some((_, spans)) => spans.push((start, end, code)),
            None => by_file.push((file, vec![(start, end, code)])),
        }
    }
    by_file
        .iter()
        .filter_map(|(path, spans)| emission(root, path, spans))
        .collect()
}

fn emissions(root: &Path, payload: &Value) -> Vec<FileEmission> {
    let mut files = match payload.get("kind").and_then(Value::as_str) {
        Some("file") => file_view_emission(root, payload).into_iter().collect(),
        Some("node") => symbol_emissions(root, payload),
        _ => payload["sourceFiles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|file| {
                let spans = file["chunks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|chunk| {
                        Some((
                            chunk["startLine"].as_u64()?,
                            chunk["endLine"].as_u64()?,
                            chunk["source"].as_str()?,
                        ))
                    })
                    .collect::<Vec<_>>();
                emission(root, file["path"].as_str()?, &spans)
            })
            .collect::<Vec<_>>(),
    };
    files.sort_by_key(|file| std::cmp::Reverse(file.bytes));
    files.truncate(MAX_FILES);
    files
}

fn coalesce(mut ranges: Vec<LineRange>) -> (Vec<LineRange>, bool) {
    ranges.retain(|range| range.start >= 1 && range.end >= range.start);
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<LineRange> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end.saturating_add(1) => {
                last.end = last.end.max(range.end);
            }
            Some(_) | None => merged.push(range),
        }
    }
    let truncated = merged.len() > MAX_RANGES;
    if truncated {
        merged.sort_by_key(|range| std::cmp::Reverse(range.end - range.start));
        merged.truncate(MAX_RANGES);
        merged.sort_by_key(|range| range.start);
    }
    (merged, truncated)
}

fn project_key(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

#[cfg(test)]
mod tests;
