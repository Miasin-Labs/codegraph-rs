use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mcp::tools::ToolResult;
use crate::utils::sha256_hex;

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

    pub(crate) fn record(&mut self, project_root: &Path, result: &ToolResult) {
        let Some(payload) = result.structured_content.as_ref() else {
            return;
        };
        // Every tool that emits source participates: `explore` (many files) and
        // the `node` file view (one file, paged). Agents re-fetch the same
        // ranges constantly, so the ledger has to span tools, not just explore.
        if !matches!(
            payload.get("kind").and_then(Value::as_str),
            Some("explore") | Some("file")
        ) {
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
            files: emissions(project_root, payload),
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
    Some(format!("{}:{}", bytes.len(), &sha256_hex(&bytes)[..16]))
}

/// The `node` file view emits one file as `path` + `sourceChunks`.
fn file_view_emission(root: &Path, payload: &Value) -> Option<FileEmission> {
    let path = payload["path"].as_str()?.to_string();
    let chunks = payload["sourceChunks"].as_array()?;
    let ranges = chunks
        .iter()
        .filter_map(|chunk| {
            Some(LineRange {
                start: usize::try_from(chunk["startLine"].as_u64()?).ok()?,
                end: usize::try_from(chunk["endLine"].as_u64()?).ok()?,
            })
        })
        .collect();
    let (ranges, _) = coalesce(ranges);
    if ranges.is_empty() {
        return None;
    }
    let bytes = chunks
        .iter()
        .filter_map(|chunk| chunk["source"].as_str())
        .map(str::len)
        .sum();
    Some(FileEmission {
        fingerprint: file_fingerprint(root, &path),
        path,
        ranges,
        bytes,
    })
}

fn emissions(root: &Path, payload: &Value) -> Vec<FileEmission> {
    if payload.get("kind").and_then(Value::as_str) == Some("file") {
        return file_view_emission(root, payload).into_iter().collect();
    }
    let mut files = payload["sourceFiles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| {
            let path = file["path"].as_str()?.to_string();
            let ranges = file["chunks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|chunk| {
                    Some(LineRange {
                        start: usize::try_from(chunk["startLine"].as_u64()?).ok()?,
                        end: usize::try_from(chunk["endLine"].as_u64()?).ok()?,
                    })
                })
                .collect();
            let (ranges, _) = coalesce(ranges);
            let bytes = file["chunks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|chunk| chunk["source"].as_str())
                .map(str::len)
                .sum();
            Some(FileEmission {
                fingerprint: file_fingerprint(root, &path),
                path,
                ranges,
                bytes,
            })
        })
        .collect::<Vec<_>>();
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
