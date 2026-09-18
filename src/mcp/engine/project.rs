use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use super::MCPEngine;
use crate::codegraph::{CodeGraph, IndexOptions, OpenOptions};
use crate::directory::find_nearest_codegraph_root;
use crate::extraction::IndexProgress;
use crate::sync::worktree::{detect_worktree_index_mismatch, worktree_auto_sync_reason};
use crate::sync::{WatchOptions, WatchProbe, watch_disabled_reason};

impl MCPEngine {
    pub fn ensure_initialized(&self, search_from: &str) {
        if self.closed.get() || self.tool_handler.has_default_code_graph() {
            return;
        }
        self.do_initialize(search_from);
    }

    pub fn retry_initialize_sync(&self, search_from: &str) {
        if self.closed.get() || self.tool_handler.has_default_code_graph() {
            return;
        }
        self.tool_handler.set_default_project_hint(search_from);
        let Some(resolved_root) = find_nearest_codegraph_root(Path::new(search_from)) else {
            return;
        };
        if let Some(previous) = self.cg.borrow_mut().take() {
            previous.close();
        }
        if let Ok(codegraph) = self.runtime.block_on(CodeGraph::open_async(
            &resolved_root,
            &OpenOptions::default(),
        )) {
            self.activate_project(search_from, resolved_root, codegraph);
        }
    }

    fn do_initialize(&self, search_from: &str) {
        self.tool_handler.set_default_project_hint(search_from);
        let Some(resolved_root) = find_nearest_codegraph_root(Path::new(search_from)) else {
            *self.project_path.borrow_mut() = Some(search_from.to_string());
            return;
        };
        *self.project_path.borrow_mut() = Some(resolved_root.to_string_lossy().to_string());
        match self.runtime.block_on(CodeGraph::open_async(
            &resolved_root,
            &OpenOptions::default(),
        )) {
            Ok(codegraph) => self.activate_project(search_from, resolved_root, codegraph),
            Err(error) => self.logs.log(
                "error",
                &format!(
                    "Failed to open project at {}: {error}",
                    resolved_root.display()
                ),
            ),
        }
    }

    fn activate_project(&self, search_from: &str, root: std::path::PathBuf, codegraph: CodeGraph) {
        let codegraph = Rc::new(codegraph);
        *self.cg.borrow_mut() = Some(Rc::clone(&codegraph));
        *self.project_path.borrow_mut() = Some(root.to_string_lossy().to_string());
        self.tool_handler.set_default_code_graph(codegraph);
        // A session started in a different git checkout than the index (a
        // worktree nested in the main checkout) reads that index — tools carry
        // the worktree notice — but never writes it: no watcher, no catch-up
        // sync, either of which would rewrite the other checkout's index.
        if let Some(mismatch) = detect_worktree_index_mismatch(Path::new(search_from), &root) {
            let reason = worktree_auto_sync_reason(&mismatch);
            self.logs.log(
                "warning",
                &format!("File watcher and catch-up sync not started — {reason}."),
            );
            self.tool_handler.set_auto_sync_disabled(reason);
            return;
        }
        self.start_watching();
        self.catch_up_sync();
    }

    fn start_watching(&self) {
        if self.cg.borrow().is_none() || self.watcher_started.get() {
            return;
        }
        if !self.opts.watch {
            self.tool_handler
                .set_auto_sync_disabled("live file watching disabled by server configuration");
            self.watcher_started.set(true);
            return;
        }
        let root = self.project_path.borrow().clone().unwrap_or_else(|| {
            std::env::current_dir()
                .map(|directory| directory.to_string_lossy().to_string())
                .unwrap_or_default()
        });
        if let Some(reason) = watch_disabled_reason(&root, &WatchProbe::default()) {
            self.tool_handler.set_auto_sync_disabled(reason.clone());
            self.logs.log(
                "warning",
                &format!(
                    "File watcher disabled — {reason}. The graph will not auto-update; run `codegraph sync` (or install the git sync hooks via `codegraph init`) to refresh."
                ),
            );
            self.watcher_started.set(true);
            return;
        }

        let debounce_ms =
            parse_debounce_env(std::env::var("CODEGRAPH_WATCH_DEBOUNCE_MS").ok().as_deref());
        if let Some(milliseconds) = debounce_ms {
            self.logs.log(
                "debug",
                &format!("File watcher debounce: {milliseconds}ms (CODEGRAPH_WATCH_DEBOUNCE_MS)"),
            );
        }
        let sync_logs = self.logs.clone();
        let error_logs = self.logs.clone();
        let started = self.cg.borrow().as_ref().is_some_and(|codegraph| {
            codegraph.watch(WatchOptions {
                debounce_ms,
                on_sync_complete: Some(Arc::new(move |result| {
                    if result.files_changed > 0 {
                        sync_logs.log(
                            "info",
                            &format!(
                                "Auto-synced {} file(s) in {}ms",
                                result.files_changed, result.duration_ms
                            ),
                        );
                    }
                })),
                on_sync_error: Some(Arc::new(move |error| {
                    error_logs.log("error", &format!("Auto-sync error: {error}"));
                })),
                inert_for_tests: false,
            })
        });
        self.watcher_started.set(true);
        if started {
            self.logs.log(
                "info",
                "File watcher active — graph will auto-sync on changes",
            );
        } else {
            self.tool_handler.set_auto_sync_disabled(
                "file watcher setup failed or no watch set could be installed",
            );
            self.logs.log(
                "warning",
                "File watcher unavailable on this platform — run `codegraph sync` to refresh the graph after changes.",
            );
        }
    }

    fn catch_up_sync(&self) {
        let Some(codegraph) = self.cg.borrow().as_ref().map(Rc::clone) else {
            return;
        };
        let context = self.tool_handler.call_context();
        let logs = self.logs.clone();
        let runtime = self.runtime.clone();
        let gate: Box<dyn FnOnce()> = Box::new(move || {
            context.emit_progress(0.0, None, Some("Catching up index with filesystem changes"));
            let count = Cell::new(0.0f64);
            let last_emit = Cell::new(std::time::Instant::now());
            let on_progress = |progress: &IndexProgress| {
                count.set(count.get() + 1.0);
                if last_emit.get().elapsed() >= std::time::Duration::from_millis(100) {
                    last_emit.set(std::time::Instant::now());
                    context.emit_progress(
                        count.get(),
                        None,
                        Some(&format!(
                            "{}: {}/{}",
                            progress.phase.as_str(),
                            progress.current,
                            progress.total
                        )),
                    );
                }
            };
            let options = IndexOptions {
                on_progress: Some(&on_progress),
                ..IndexOptions::default()
            };
            match runtime.block_on(codegraph.sync(&options)) {
                Ok(result) => {
                    let changed = result.files_added + result.files_modified + result.files_removed;
                    if changed > 0 {
                        logs.log(
                            "info",
                            &format!("Caught up {changed} file(s) changed since last run"),
                        );
                    }
                }
                Err(error) => logs.log("error", &format!("Catch-up sync failed: {error}")),
            }
            let done = count.get() + 1.0;
            context.emit_progress(done, Some(done), Some("Catch-up sync complete"));
        });
        self.tool_handler.set_catch_up_gate(Some(gate));
    }
}

pub fn parse_debounce_env(raw: Option<&str>) -> Option<u64> {
    let raw = raw?;
    if raw.trim().is_empty() {
        return None;
    }
    let value: f64 = raw.trim().parse().ok()?;
    if !value.is_finite() || value.fract() != 0.0 || !(100.0..=60000.0).contains(&value) {
        return None;
    }
    Some(value as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_for_unset_or_empty_values() {
        assert_eq!(parse_debounce_env(None), None);
        assert_eq!(parse_debounce_env(Some("")), None);
        assert_eq!(parse_debounce_env(Some("   ")), None);
    }

    #[test]
    fn accepts_integer_values_inside_100_to_60000() {
        assert_eq!(parse_debounce_env(Some("100")), Some(100));
        assert_eq!(parse_debounce_env(Some("2000")), Some(2000));
        assert_eq!(parse_debounce_env(Some("5000")), Some(5000));
        assert_eq!(parse_debounce_env(Some("60000")), Some(60000));
    }

    #[test]
    fn rejects_out_of_range_values_returns_none_lets_default_win() {
        assert_eq!(parse_debounce_env(Some("0")), None);
        assert_eq!(parse_debounce_env(Some("50")), None);
        assert_eq!(parse_debounce_env(Some("99")), None);
        assert_eq!(parse_debounce_env(Some("60001")), None);
        assert_eq!(parse_debounce_env(Some("-500")), None);
    }

    #[test]
    fn rejects_non_integer_non_numeric_values() {
        assert_eq!(parse_debounce_env(Some("abc")), None);
        assert_eq!(parse_debounce_env(Some("500.5")), None);
        assert_eq!(parse_debounce_env(Some("NaN")), None);
        assert_eq!(parse_debounce_env(Some("Infinity")), None);
    }

    #[test]
    fn accepts_scientific_notation_that_resolves_to_an_in_range_integer() {
        assert_eq!(parse_debounce_env(Some("1e3")), Some(1000));
    }
}
