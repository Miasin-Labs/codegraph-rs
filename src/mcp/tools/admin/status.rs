//! codegraph_status handler.

use std::rc::Rc;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::{now_ms, resolve_path};
use super::super::schema::ToolResult;
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

        let mut lines: Vec<String> = vec!["## CodeGraph Status".to_string(), String::new()];
        if let Some(m) = &mismatch {
            lines.push(format!(
                "> ⚠ {}",
                worktree_mismatch_warning(m).replace('\n', "\n> ")
            ));
            lines.push(String::new());
        }
        lines.push(format!("**Files indexed:** {}", stats.file_count));
        lines.push(format!("**Total nodes:** {}", stats.node_count));
        lines.push(format!("**Total edges:** {}", stats.edge_count));
        lines.push(format!(
            "**Database size:** {:.2} MB",
            stats.db_size_bytes as f64 / 1024.0 / 1024.0
        ));

        // Surface the active SQLite backend. The TS line names node:sqlite;
        // the Rust build embeds SQLite via rusqlite — report "native" per the
        // porting convention (PORTING.md rule 12) while keeping the line shape.
        lines.push(format!(
            "**Backend:** {} (rusqlite bundled SQLite) — full WAL + FTS5",
            cg.get_backend().as_str()
        ));

        // Effective journal mode.
        let journal_mode = cg.get_journal_mode()?;
        if journal_mode == "wal" {
            lines.push("**Journal mode:** wal (concurrent reads safe)".to_string());
        } else {
            let mode = if journal_mode.is_empty() {
                "unknown".to_string()
            } else {
                journal_mode
            };
            lines.push(format!(
                "**Journal mode:** ⚠ {mode} — WAL not active, so reads can block on a concurrent write (WAL appears unsupported on this filesystem)"
            ));
        }

        lines.push(String::new());
        lines.push("### Nodes by Kind:".to_string());

        // TS iterates Object.entries insertion order (the SQL GROUP BY order);
        // sort keys for determinism here.
        let mut kinds: Vec<(&String, &u64)> = stats.nodes_by_kind.iter().collect();
        kinds.sort_by(|a, b| a.0.cmp(b.0));
        for (kind, count) in kinds {
            if *count > 0 {
                lines.push(format!("- {kind}: {count}"));
            }
        }

        lines.push(String::new());
        lines.push("### Languages:".to_string());
        let mut langs: Vec<(&String, &u64)> = stats.files_by_language.iter().collect();
        langs.sort_by(|a, b| a.0.cmp(b.0));
        for (lang, count) in langs {
            if *count > 0 {
                lines.push(format!("- {lang}: {count}"));
            }
        }

        // Per-file freshness (#403).
        let pending = cg.get_pending_files();
        if !pending.is_empty() {
            lines.push(String::new());
            lines.push("### Pending sync:".to_string());
            let now = now_ms();
            for p in &pending {
                let age_ms = (now - p.last_seen_ms).max(0);
                let label = if p.indexing {
                    "indexing in progress"
                } else {
                    "pending sync"
                };
                lines.push(format!("- {} (edited {}ms ago, {})", p.path, age_ms, label));
            }
        }

        Ok(self.text_result(&lines.join("\n")))
    }

    // =========================================================================
    // codegraph_files
}
