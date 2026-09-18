//! Notices a result carries when it may not match the code on disk.
//!
//! These only record the notice (`_meta.notices`); the MCP projection
//! (`ToolResult::into_mcp_projection`) is what puts them in front of the model
//! — in the payload's `notices`, or as leading `⚠️` lines of a text result.
//! The human text the CLI prints is left alone.

use std::rc::Rc;

use super::super::format::resolve_path;
use super::super::schema::{NoticeKind, ToolNotice, ToolNoticeFile, ToolResult};
use super::ToolHandler;
use crate::sync::PendingFile;
use crate::sync::worktree::{WorktreeIndexMismatch, worktree_mismatch_notice};

/// What a `stale_index` notice tells the model, for files the watcher saw
/// change and files whose bytes no longer match the index alike: source
/// tools re-read a drifted file from disk, so any source shown is current.
const STALE_INDEX_MESSAGE: &str = "These files changed after the last index sync, so index \
                                   data about them (symbols, line numbers, call edges) may be \
                                   out of date. Any source shown for them is current; Read \
                                   them for anything else.";

const STALE_EXTRACTION_MESSAGE: &str = "This index was built by an older version of the \
                                        extractor: files unchanged since keep its output, so \
                                        symbols and call edges may be missing or wrong until \
                                        the user runs `codegraph index`.";

impl ToolHandler {
    pub(in crate::mcp::tools::context) fn with_auto_sync_notice(
        &self,
        result: ToolResult,
    ) -> ToolResult {
        if result.is_error == Some(true) {
            return result;
        }
        let Some(reason) = self.auto_sync_disabled_reason() else {
            return result;
        };
        result.with_notice(auto_sync_disabled_notice(reason))
    }

    pub(in crate::mcp::tools::context) fn with_worktree_notice(
        &self,
        result: ToolResult,
        project_path: Option<&str>,
    ) -> ToolResult {
        if result.is_error == Some(true) {
            return result;
        }
        let Some(mismatch) = self.worktree_mismatch_for(project_path) else {
            return result;
        };
        result.with_notice(worktree_notice(&mismatch))
    }

    /// Flag a result from an index an older extractor built: its edges are
    /// what that extractor produced until the user re-indexes.
    pub(in crate::mcp::tools::context) fn with_extraction_notice(
        &self,
        result: ToolResult,
        project_path: Option<&str>,
    ) -> ToolResult {
        if result.is_error == Some(true) {
            return result;
        }
        let Ok(cg) = self.get_code_graph(project_path) else {
            return result;
        };
        if !cg.is_index_stale().unwrap_or(false) {
            return result;
        }
        result.with_notice(stale_extraction_notice())
    }

    /// Annotate a successful read-tool result with per-file staleness (#403).
    pub(in crate::mcp::tools::context) fn with_staleness_notice(
        &self,
        result: ToolResult,
        project_path: Option<&str>,
    ) -> ToolResult {
        if result.is_error == Some(true) {
            return result;
        }

        let Ok(mut cg) = self.get_code_graph(project_path) else {
            return result; // no default project — leave as is
        };

        // Cross-project `projectPath` calls open a cached CodeGraph WITHOUT a
        // watcher. When that path is actually the default project, prefer the
        // default instance so the staleness signal still fires.
        if let Some(default_cg) = &*self.cg.borrow() {
            if !Rc::ptr_eq(default_cg, &cg)
                && resolve_path(default_cg.get_project_root())
                    == resolve_path(cg.get_project_root())
            {
                cg = Rc::clone(default_cg);
            }
        }

        let pending = cg.get_pending_files();
        if pending.is_empty() {
            return result;
        }

        // Files this result mentions first: they are the ones it may have
        // got wrong, and the list the model sees is capped.
        let text = result.text();
        let (in_response, elsewhere): (Vec<PendingFile>, Vec<PendingFile>) =
            pending.into_iter().partition(|p| text.contains(&p.path));
        let notice_files = in_response
            .iter()
            .chain(elsewhere.iter())
            .map(stale_notice_file)
            .collect::<Vec<_>>();
        result.with_notice(ToolNotice {
            kind: NoticeKind::StaleIndex,
            severity: "warning".into(),
            message: STALE_INDEX_MESSAGE.into(),
            files: notice_files,
            data: None,
        })
    }
}

pub(in crate::mcp::tools) fn auto_sync_disabled_message(reason: &str) -> String {
    format!(
        "CodeGraph auto-sync is DISABLED ({reason}): live file watching stopped, so the index is frozen and any file edited since then is stale here. Read files directly to confirm current content before relying on it."
    )
}

pub(in crate::mcp::tools) fn auto_sync_disabled_notice(reason: String) -> ToolNotice {
    ToolNotice {
        kind: NoticeKind::AutoSyncDisabled,
        severity: "warning".into(),
        message: auto_sync_disabled_message(&reason),
        files: Vec::new(),
        data: Some(serde_json::json!({ "reason": reason })),
    }
}

pub(in crate::mcp::tools) fn worktree_notice(mismatch: &WorktreeIndexMismatch) -> ToolNotice {
    ToolNotice {
        kind: NoticeKind::WorktreeMismatch,
        severity: "warning".into(),
        message: worktree_mismatch_notice(mismatch),
        files: Vec::new(),
        data: Some(serde_json::to_value(mismatch).unwrap_or_default()),
    }
}

pub(in crate::mcp::tools) fn stale_extraction_notice() -> ToolNotice {
    ToolNotice {
        kind: NoticeKind::StaleExtraction,
        severity: "warning".into(),
        message: STALE_EXTRACTION_MESSAGE.into(),
        files: Vec::new(),
        data: None,
    }
}

pub(in crate::mcp::tools) fn stale_slice_notice(paths: &[String]) -> ToolNotice {
    ToolNotice {
        kind: NoticeKind::StaleIndex,
        severity: "warning".into(),
        message: STALE_INDEX_MESSAGE.into(),
        files: paths
            .iter()
            .map(|path| ToolNoticeFile {
                path: path.clone(),
                age_ms: 0,
                status: "changed on disk".into(),
            })
            .collect(),
        data: Some(serde_json::json!({ "indexedSlicesWithheld": true })),
    }
}

fn stale_notice_file(pending: &PendingFile) -> ToolNoticeFile {
    let age_ms = (super::super::format::now_ms() - pending.last_seen_ms).max(0);
    ToolNoticeFile {
        path: pending.path.clone(),
        age_ms,
        status: if pending.indexing {
            "indexing in progress".into()
        } else {
            "pending sync".into()
        },
    }
}
