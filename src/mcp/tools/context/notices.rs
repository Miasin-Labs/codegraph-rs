//! Worktree and stale-index response notices.

use std::rc::Rc;

use super::super::format::resolve_path;
use super::super::schema::{ToolNotice, ToolNoticeFile, ToolResult};
use super::ToolHandler;
use crate::sync::PendingFile;
use crate::sync::worktree::worktree_mismatch_notice;

impl ToolHandler {
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

        let notice_text = worktree_mismatch_notice(&mismatch);
        result.with_notice(ToolNotice {
            kind: "worktree_mismatch".into(),
            severity: "warning".into(),
            message: notice_text,
            files: Vec::new(),
            data: Some(serde_json::to_value(mismatch).unwrap_or_default()),
        })
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

        let mut in_response: Vec<PendingFile> = Vec::new();
        let mut elsewhere: Vec<PendingFile> = Vec::new();
        let text = result.text();
        for p in pending {
            if text.contains(&p.path) {
                in_response.push(p);
            } else {
                elsewhere.push(p);
            }
        }

        let notice_files = in_response
            .iter()
            .chain(elsewhere.iter())
            .map(stale_notice_file)
            .collect::<Vec<_>>();
        if notice_files.is_empty() {
            return result;
        }
        result.with_notice(ToolNotice {
            kind: "stale_index".into(),
            severity: "warning".into(),
            message: "Some indexed files are pending sync".into(),
            files: notice_files,
            data: None,
        })
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
