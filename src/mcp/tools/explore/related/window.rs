//! Short source windows for the best related files.
//!
//! A window is the key symbol's first few lines — its signature and opening —
//! returned as an ordinary `sourceFiles` entry, so the session ledger records
//! it like any other explore source. The payload adds windows only while they
//! fit the output budget.

use std::path::Path;

use super::super::types::{SourceChunk, SourceChunkMode, StructuredSourceFile};
use super::RelatedPlan;
use crate::codegraph::CodeGraph;
use crate::mcp::explore_session::{ProjectState, range_already_sent};
use crate::utils::resolve_existing_path_within_root_real;

/// Related files that may carry a window.
pub(super) const WINDOW_FILES: usize = 4;
/// Serialized size budgeted per window when reserving room for them.
pub(super) const WINDOW_COST_ESTIMATE: usize = 450;
/// Lines per window, from the key symbol's first line.
const WINDOW_LINES: usize = 8;
/// Files larger than this are not read for a window (hashing a generated
/// megabyte to show eight lines is not worth the latency).
const MAX_WINDOW_FILE_BYTES: u64 = 512 * 1024;

/// Windows for the top related files, best first. A file is skipped when its
/// key symbol's lines already went out this session and are unchanged on disk
/// (the ledger's `alreadySent` rule), when it changed since the last index
/// sync (its indexed lines are no longer trustworthy), or when it can't be read.
pub(in crate::mcp::tools::explore) fn related_windows(
    cg: &CodeGraph,
    project_root: &Path,
    plan: &RelatedPlan,
    prior: Option<&ProjectState>,
) -> Vec<StructuredSourceFile> {
    plan.files
        .iter()
        .filter_map(|file| {
            let symbol = file.symbol.as_ref()?;
            let start = symbol.start_line.max(1);
            let end = symbol.end_line.max(start).min(start + WINDOW_LINES - 1);
            if prior.is_some_and(|prior| {
                range_already_sent(prior, project_root, &file.path, start, end)
            }) {
                return None;
            }
            let record = cg.get_file(&file.path).ok().flatten()?;
            if record.size > MAX_WINDOW_FILE_BYTES {
                return None;
            }
            let abs = resolve_existing_path_within_root_real(project_root, &file.path)?;
            let content = std::fs::read_to_string(abs).ok()?;
            if crate::extraction::hash_content(&content) != record.content_hash {
                return None;
            }
            let lines: Vec<&str> = content.split('\n').collect();
            let chunk = SourceChunk::from_lines(
                &lines,
                start as i64,
                end as i64,
                SourceChunkMode::Excerpt,
                vec![format!("{}({})", symbol.name, symbol.kind.as_str())],
            )?;
            Some(StructuredSourceFile {
                path: file.path.clone(),
                language: record.language.as_str().to_string(),
                chunks: vec![chunk],
                source_truncated: false,
            })
        })
        .take(WINDOW_FILES)
        .collect()
}
