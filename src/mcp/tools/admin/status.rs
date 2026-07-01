//! codegraph_status handler.

use std::rc::Rc;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::{now_ms, resolve_path};
use super::super::output::{PendingSyncOutput, StatusOutput};
use super::super::schema::{ToolNotice, ToolResult};
use crate::error::Result;
use crate::sync::worktree::worktree_mismatch_warning;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_status(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let project_path = args.get("projectPath").and_then(|v| v.as_str());
        let mut cg = self.get_code_graph(project_path)?;
        // Same trick as with_staleness_notice — prefer the default instance
        // when an explicit projectPath resolves to the same project.
        if let Some(default_cg) = &*self.cg.borrow() {
            if !Rc::ptr_eq(default_cg, &cg)
                && resolve_path(default_cg.get_project_root())
                    == resolve_path(cg.get_project_root())
            {
                cg = Rc::clone(default_cg);
            }
        }
        let stats = cg.get_stats()?;

        let mismatch = self.worktree_mismatch_for(project_path);

        let mut lines: Vec<String> = vec!["CodeGraph status".to_string(), String::new()];
        if let Some(m) = &mismatch {
            lines.push(format!(
                "Notice: {}",
                worktree_mismatch_warning(m).replace('\n', "\n> ")
            ));
            lines.push(String::new());
        }
        lines.push(format!("Files indexed: {}", stats.file_count));
        lines.push(format!("Total nodes: {}", stats.node_count));
        lines.push(format!("Total edges: {}", stats.edge_count));
        lines.push(format!(
            "Database size: {:.2} MB",
            stats.db_size_bytes as f64 / 1024.0 / 1024.0
        ));
        let backend = cg.get_backend().as_str().to_string();
        lines.push(format!(
            "Backend: {} (rusqlite bundled SQLite, FTS5)",
            backend
        ));

        let journal_mode = cg.get_journal_mode()?;
        if journal_mode == "wal" {
            lines.push("Journal mode: wal".to_string());
        } else {
            let mode = if journal_mode.is_empty() {
                "unknown".to_string()
            } else {
                journal_mode.clone()
            };
            lines.push(format!(
                "Journal mode: {mode} (WAL not active; reads can block on concurrent writes)"
            ));
        }

        lines.push(String::new());
        lines.push("Nodes by kind:".to_string());
        let mut kinds: Vec<(&String, &u64)> = stats.nodes_by_kind.iter().collect();
        kinds.sort_by(|a, b| a.0.cmp(b.0));
        for (kind, count) in kinds {
            if *count > 0 {
                lines.push(format!("- {kind}: {count}"));
            }
        }

        lines.push(String::new());
        lines.push("Languages:".to_string());
        let mut langs: Vec<(&String, &u64)> = stats.files_by_language.iter().collect();
        langs.sort_by(|a, b| a.0.cmp(b.0));
        for (lang, count) in langs {
            if *count > 0 {
                lines.push(format!("- {lang}: {count}"));
            }
        }

        let pending = cg.get_pending_files();
        let mut pending_sync = Vec::new();
        if !pending.is_empty() {
            lines.push(String::new());
            lines.push("Pending sync:".to_string());
            let now = now_ms();
            for p in &pending {
                let age_ms = (now - p.last_seen_ms).max(0);
                let label = if p.indexing {
                    "indexing in progress"
                } else {
                    "pending sync"
                };
                lines.push(format!("- {} (edited {}ms ago, {})", p.path, age_ms, label));
                pending_sync.push(PendingSyncOutput {
                    path: p.path.clone(),
                    age_ms,
                    status: label.to_string(),
                });
            }
        }

        let output = StatusOutput::from_stats(&stats, backend, journal_mode, pending_sync);
        let mut result = self.structured_result(&lines.join("\n"), &output)?;
        if let Some(m) = mismatch {
            result = result.with_notice(ToolNotice {
                kind: "worktree_mismatch".into(),
                severity: "warning".into(),
                message: worktree_mismatch_warning(&m),
                files: Vec::new(),
                data: Some(serde_json::to_value(m)?),
            });
        }
        Ok(result)
    }

    // =========================================================================
    // codegraph_files
}
