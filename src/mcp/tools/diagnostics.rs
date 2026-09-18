//! codegraph_diagnostics — compiler errors and warnings, placed on symbols.
//!
//! Runs the project's own checker (see [`crate::diagnostics`]) time-boxed:
//! a run that outlasts `wait` keeps going in the background and the next call
//! returns it. Each diagnostic is attributed to the innermost indexed symbol
//! around its line, so the answer reads "error in `parse_header`", not just
//! "error at src/http.rs:212".

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde_json::{Map, Value};

use super::context::ToolHandler;
use super::format::{is_callable_kind, now_ms, num_or};
use super::schema::ToolResult;
use crate::codegraph::CodeGraph;
use crate::diagnostics::{Checker, Diagnostic, DiagnosticsRun, RunStatus, Severity, check_or_poll};
use crate::error::Result;
use crate::types::{Node, NodeKind};
use crate::utils::clamp;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_diagnostics(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let project_path = args.get("projectPath").and_then(|v| v.as_str());
        let cg = self.get_code_graph(project_path)?;
        // The checker runs in the index's checkout and writes its output under
        // that checkout's `.codegraph/diagnostics/`. From a different checkout
        // (a worktree nested in the main one) it would build — and report — the
        // other checkout's code, so refuse instead.
        if let Some(mismatch) = self.worktree_mismatch_for(project_path) {
            return Ok(self.error_result(&format!(
                "codegraph_diagnostics runs the checker in the index's own checkout ({index}), \
                 but you are working in a different git checkout ({here}) — it would report \
                 {index}'s code, not yours. Run the build in {here} yourself, or run \
                 \"codegraph init {here}\" so this checkout has its own index.",
                index = mismatch.index_root.display(),
                here = mismatch.worktree_root.display()
            )));
        }
        let root = cg.get_project_root().to_path_buf();
        let checker = match Checker::for_project(&root, args.get("checker").and_then(Value::as_str))
        {
            Ok(checker) => checker,
            Err(message) => return Ok(self.error_result(&message)),
        };
        // Not `num_or`: its JS `||` semantics would turn `wait: 0` (just poll)
        // into the default.
        let wait = args.get("wait").and_then(Value::as_f64).unwrap_or(20.0);
        let wait = Duration::from_secs_f64(clamp(wait, 0.0, 55.0));
        let limit = clamp(num_or(args, "limit", 50.0), 1.0, 500.0) as usize;
        let errors_only = args.get("severity").and_then(Value::as_str) == Some("error");
        let file_filter = args
            .get("file")
            .and_then(Value::as_str)
            .map(|f| f.trim_start_matches("./").replace('\\', "/"));

        let stop = || self.call_context.is_cancelled();
        let run = match check_or_poll(&root, checker, wait, &stop) {
            Ok(run) => run,
            Err(message) => return Ok(self.error_result(&message)),
        };
        if run.status == RunStatus::Running {
            let elapsed = (now_ms() as u64).saturating_sub(run.started_ms) / 1000;
            return Ok(self.text_result(&format!(
                "`{}` is still running (started {elapsed}s ago) — it continues in the \
                 background. Call codegraph_diagnostics again for the result; don't start \
                 your own build meanwhile.",
                command_label(checker)
            )));
        }

        let shown: Vec<&Diagnostic> = run
            .diagnostics
            .iter()
            .filter(|d| !errors_only || d.severity == Severity::Error)
            .filter(|d| {
                file_filter
                    .as_deref()
                    .is_none_or(|f| d.file == f || d.file.starts_with(&format!("{f}/")))
            })
            .collect();
        Ok(self.text_result(&self.truncate_output(&render(&cg, &run, &shown, limit))))
    }
}

fn command_label(checker: Checker) -> &'static str {
    match checker {
        Checker::Check => "cargo check",
        Checker::Clippy => "cargo clippy",
        Checker::Tsc => "tsc --noEmit",
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn render(cg: &CodeGraph, run: &DiagnosticsRun, shown: &[&Diagnostic], limit: usize) -> String {
    let label = command_label(run.checker);
    let count = |severity| {
        run.diagnostics
            .iter()
            .filter(|d| d.severity == severity)
            .count()
    };
    let took = run
        .finished_ms
        .map(|done| {
            format!(
                " in {:.1}s",
                done.saturating_sub(run.started_ms) as f64 / 1000.0
            )
        })
        .unwrap_or_default();
    let mut lines = vec![format!(
        "`{label}` finished{took}: {}, {}.",
        plural(count(Severity::Error), "error"),
        plural(count(Severity::Warning), "warning")
    )];
    if let Some(failure) = &run.failure {
        lines.push(format!(
            "It failed without reporting a diagnostic (exit {}):\n```\n{failure}\n```",
            run.exit_code.map_or("?".to_string(), |c| c.to_string())
        ));
        return lines.join("\n");
    }
    if shown.is_empty() {
        if !run.diagnostics.is_empty() {
            lines.push("None match the `file`/`severity` filter.".into());
        }
        return lines.join("\n");
    }

    let mut by_file: BTreeMap<&str, Vec<&Diagnostic>> = BTreeMap::new();
    for d in shown.iter().take(limit) {
        by_file.entry(d.file.as_str()).or_default().push(d);
    }
    let mut symbols = SymbolIndex::new(cg);
    lines.push(String::new());
    for (file, diagnostics) in by_file {
        lines.push(format!("**{file}**"));
        for d in diagnostics {
            let severity = match d.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Note => "note",
            };
            let code = d
                .code
                .as_deref()
                .map(|c| format!("[{c}]"))
                .unwrap_or_default();
            let within = symbols
                .enclosing(file, d.line)
                .map(|name| format!(" in `{name}`"))
                .unwrap_or_default();
            lines.push(format!(
                "- {severity}{code} L{}:{}{within} — {}",
                d.line, d.column, d.message
            ));
        }
    }
    if shown.len() > limit {
        lines.push(format!("… {} more (raise `limit`)", shown.len() - limit));
    }
    lines.join("\n")
}

/// Innermost indexed symbol around a line, per file, loaded once per file.
struct SymbolIndex<'a> {
    cg: &'a CodeGraph,
    files: HashMap<String, Vec<Node>>,
}

impl<'a> SymbolIndex<'a> {
    fn new(cg: &'a CodeGraph) -> Self {
        Self {
            cg,
            files: HashMap::new(),
        }
    }

    fn enclosing(&mut self, file: &str, line: u32) -> Option<String> {
        let nodes = self.files.entry(file.to_string()).or_insert_with(|| {
            self.cg
                .get_nodes_in_file(file)
                .unwrap_or_default()
                .into_iter()
                .filter(|n| n.kind != NodeKind::File && n.kind != NodeKind::Import)
                .collect()
        });
        nodes
            .iter()
            .filter(|n| n.start_line <= line && line <= n.end_line)
            // Innermost first; at equal span prefer a callable over its container.
            .min_by_key(|n| (n.end_line - n.start_line, !is_callable_kind(n.kind)))
            .map(|n| {
                let qualified = n.qualified_name.as_str();
                qualified
                    .strip_prefix(file)
                    .and_then(|rest| rest.strip_prefix("::"))
                    .filter(|rest| !rest.is_empty())
                    .unwrap_or(if qualified.is_empty() {
                        &n.name
                    } else {
                        qualified
                    })
                    .to_string()
            })
    }
}
