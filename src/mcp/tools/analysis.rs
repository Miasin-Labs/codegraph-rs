//! Analysis-backed MCP tools: answers the CLI's `codegraph analyze` already
//! computes, surfaced to agents that only speak MCP.

use serde_json::{Map, Value};

use super::context::ToolHandler;
use super::format::num_or;
use super::schema::ToolResult;
use crate::analysis_bridge::{BridgeOptions, build_analysis_graph_cached_with_options};
use crate::analyze::CoChangeReport;
use crate::db::{DatabaseConnection, QueryBuilder, get_database_path};
use crate::error::{CodeGraphError, Result};
use crate::utils::clamp;

impl ToolHandler {
    /// codegraph_history — temporal coupling from git history: which code
    /// changes together with a symbol (or across the repo). Mined sessions
    /// shelled out 15,907 `git log`/`blame`/`diff` calls for this by hand.
    pub(in crate::mcp::tools) fn handle_history(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let root = cg.get_project_root().to_path_buf();
        let min_support = clamp(num_or(args, "minSupport", 2.0), 1.0, 1000.0) as u32;
        let max_commits = clamp(num_or(args, "maxCommits", 500.0), 1.0, 5000.0) as usize;
        let top = clamp(num_or(args, "limit", 15.0), 1.0, 100.0) as usize;
        let symbol = args
            .get("symbol")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let conn = DatabaseConnection::open(get_database_path(&root))
            .map_err(|e| CodeGraphError::other(format!("history: open db: {e}")))?;
        let db = conn
            .get_db()
            .map_err(|e| CodeGraphError::other(format!("history: db handle: {e}")))?;
        let queries = QueryBuilder::new(db);
        let cached = build_analysis_graph_cached_with_options(
            &queries,
            &root,
            true,
            &BridgeOptions::default(),
        )
        .map_err(|e| CodeGraphError::other(format!("history: bridge graph: {e}")))?;

        let seed = match symbol {
            Some(name) => {
                let matches = self.find_all_symbols(&cg, name)?;
                let seed = matches
                    .nodes
                    .iter()
                    .find_map(|node| cached.result.id_map.get(&node.id).cloned());
                if seed.is_none() {
                    return Ok(self.text_result(&format!(
                        "Symbol \"{name}\" not found in the analysis graph{}",
                        matches.note
                    )));
                }
                seed
            }
            None => None,
        };

        let report = crate::analyze::co_change_report(
            &cached.result.graph,
            &root,
            seed.as_ref(),
            min_support,
            max_commits,
            top,
        );
        let text = render_history(&report, symbol);
        self.structured_result(&self.truncate_output(&text), &report)
    }
}

fn render_history(report: &CoChangeReport, symbol: Option<&str>) -> String {
    let scope = symbol
        .map(|name| format!(" with `{name}`"))
        .unwrap_or_default();
    if report.commits_analyzed == 0 {
        return format!("No git history available{scope}. {}", report.note);
    }
    if report.pairs.is_empty() {
        return format!(
            "No code changed together{scope} in at least {} of the last {} commits.",
            report.min_support, report.commits_analyzed
        );
    }
    let mut lines = vec![format!(
        "Changes together{scope} — {} pair{} (of {} cross-file) over {} commits, min support {}:",
        report.pairs.len(),
        if report.pairs.len() == 1 { "" } else { "s" },
        report.cross_file_pair_count,
        report.commits_analyzed,
        report.min_support
    )];
    lines.push(String::new());
    for pair in &report.pairs {
        lines.push(format!(
            "- `{}` ({}:{}) ⇄ `{}` ({}:{}) — together {}×, confidence {:.2}",
            pair.a.name,
            pair.a.file,
            pair.a.line,
            pair.b.name,
            pair.b.file,
            pair.b.line,
            pair.times_changed_together,
            pair.confidence
        ));
    }
    if report.truncated {
        lines.push(String::new());
        lines.push("(truncated — raise `limit` for more pairs)".into());
    }
    lines.join("\n")
}
