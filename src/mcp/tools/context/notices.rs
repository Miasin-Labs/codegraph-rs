//! Worktree and stale-index response notices.

use std::rc::Rc;

use super::super::format::{format_stale_banner, format_stale_footer, resolve_path};
use super::super::schema::{ToolContent, ToolResult};
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

        let notice = worktree_mismatch_notice(&mismatch);
        let mut content = result.content;
        if let Some(first) = content.first_mut() {
            if first.content_type == "text" {
                first.text = format!("{}\n\n{}", notice, first.text);
            }
        }
        ToolResult {
            content,
            is_error: result.is_error,
        }
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

        let Some(first) = result.content.first() else {
            return result;
        };
        if first.content_type != "text" {
            return result;
        }

        let text = first.text.clone();
        let mut in_response: Vec<PendingFile> = Vec::new();
        let mut elsewhere: Vec<PendingFile> = Vec::new();
        for p in pending {
            if text.contains(&p.path) {
                in_response.push(p);
            } else {
                elsewhere.push(p);
            }
        }

        let banner = if in_response.is_empty() {
            String::new()
        } else {
            format_stale_banner(&in_response)
        };
        let footer = if elsewhere.is_empty() {
            String::new()
        } else {
            format_stale_footer(&elsewhere)
        };
        if banner.is_empty() && footer.is_empty() {
            return result;
        }

        let composed = [banner, text, footer]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut content = result.content;
        content[0] = ToolContent {
            content_type: "text".into(),
            text: composed,
        };
        ToolResult {
            content,
            is_error: result.is_error,
        }
    }
}
