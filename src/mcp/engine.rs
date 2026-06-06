//! MCP shared engine — the heavyweight, *shared* state for an MCP server:
//! the project's [`CodeGraph`] instance, file watcher, and the
//! [`ToolHandler`] cache for cross-project queries.
//!
//! One engine, many sessions:
//! - direct mode (single stdio session) instantiates one engine + one session;
//! - daemon mode instantiates one engine and a new session per socket
//!   connection. Every session reads from the same SQLite WAL and the same
//!   inotify watch set — that's the entire point of issue #411.
//!
//! Port of `src/mcp/engine.ts`.
//!
//! ## Threading model (Rust deviation, behavior-identical)
//!
//! `CodeGraph`/`ToolHandler` are `!Send` (Rc/RefCell-backed), but sessions run
//! on transport dispatcher threads (and daemon connections each get a
//! thread). The TS design shares one engine across N sessions on the single
//! JS event loop; the Rust analog is [`EngineHandle`] — a `Send + Sync`
//! handle over a dedicated *engine thread* that owns the [`MCPEngine`] and
//! processes commands serially (exactly one engine, exactly one execution at
//! a time, like the event loop). `ensureInitialized`'s background-promise
//! semantics map to [`EngineHandle::ensure_initialized_async`].

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value;

use crate::codegraph::{CodeGraph, IndexOptions, OpenOptions};
use crate::directory::find_nearest_codegraph_root;
use crate::mcp::tools::{ToolDefinition, ToolHandler, ToolResult};
use crate::sync::{WatchOptions, WatchProbe, watch_disabled_reason};

/// Options for [`MCPEngine`].
#[derive(Clone, Copy)]
pub struct MCPEngineOptions {
    /// Whether to start the file watcher when initializing. Daemon and direct
    /// modes both want this true; tests may set it false to keep the engine
    /// cheap. Honors [`watch_disabled_reason`] regardless.
    pub watch: bool,
}

impl Default for MCPEngineOptions {
    fn default() -> Self {
        MCPEngineOptions { watch: true }
    }
}

/// Shared MCP engine. Single-threaded (`!Send`) — drive it through
/// [`EngineHandle`] when sessions live on other threads.
pub struct MCPEngine {
    cg: RefCell<Option<Rc<CodeGraph>>>,
    tool_handler: ToolHandler,
    /// Project root we resolved to. None until `ensure_initialized` succeeds
    /// (or None forever if no `.codegraph/` ever turned up — that's a valid
    /// state for the engine, since cross-project queries still work).
    project_path: RefCell<Option<String>>,
    watcher_started: Cell<bool>,
    opts: MCPEngineOptions,
    closed: Cell<bool>,
}

impl MCPEngine {
    pub fn new(opts: MCPEngineOptions) -> MCPEngine {
        MCPEngine {
            cg: RefCell::new(None),
            tool_handler: ToolHandler::new(None),
            project_path: RefCell::new(None),
            watcher_started: Cell::new(false),
            opts,
            closed: Cell::new(false),
        }
    }

    /// Convenience for `MCPServer` compatibility: pre-seed an explicit
    /// project path (from the `--path` CLI flag) without yet opening it. This
    /// keeps construction cheap; the actual open happens on the first
    /// `ensure_initialized` call.
    pub fn set_project_path_hint(&self, project_path: &str) {
        *self.project_path.borrow_mut() = Some(project_path.to_string());
        self.tool_handler.set_default_project_hint(project_path);
    }

    /// Project root that the engine resolved on first init (None if none).
    pub fn get_project_path(&self) -> Option<String> {
        self.project_path.borrow().clone()
    }

    /// Shared ToolHandler — sessions delegate tool dispatch through this.
    pub fn get_tool_handler(&self) -> &ToolHandler {
        &self.tool_handler
    }

    /// Whether the default project's CodeGraph is open.
    pub fn has_default_code_graph(&self) -> bool {
        self.tool_handler.has_default_code_graph()
    }

    /// Walk up from `search_from` to find the nearest `.codegraph/` and open
    /// it. Idempotent: callers after success are no-ops. (The TS in-flight
    /// promise sharing collapses to serialized calls on the engine thread.)
    ///
    /// The original `MCPServer.tryInitializeDefault` carried the same
    /// retry-on-subsequent-tool-call semantics; we preserve them by NOT
    /// erroring when the search misses (just leaves `cg` None so the next
    /// call can retry).
    pub fn ensure_initialized(&self, search_from: &str) {
        if self.closed.get() {
            return;
        }
        if self.tool_handler.has_default_code_graph() {
            return;
        }
        self.do_initialize(search_from);
    }

    /// Synchronous last-resort init used by the per-session retry loop when
    /// the background `ensure_initialized` already finished (or failed) and
    /// we need to pick up a project that appeared *after* the engine started.
    pub fn retry_initialize_sync(&self, search_from: &str) {
        if self.closed.get() {
            return;
        }
        if self.tool_handler.has_default_code_graph() {
            return;
        }
        self.tool_handler.set_default_project_hint(search_from);
        let resolved_root = match find_nearest_codegraph_root(Path::new(search_from)) {
            Some(root) => root,
            None => return,
        };
        // Close any previously failed instance to avoid leaking resources.
        if let Some(prev) = self.cg.borrow_mut().take() {
            prev.close();
        }
        match CodeGraph::open_sync(&resolved_root) {
            Ok(cg) => {
                let cg = Rc::new(cg);
                *self.cg.borrow_mut() = Some(Rc::clone(&cg));
                *self.project_path.borrow_mut() = Some(resolved_root.to_string_lossy().to_string());
                self.tool_handler.set_default_code_graph(Rc::clone(&cg));
                self.start_watching();
                self.catch_up_sync();
            }
            Err(_) => {
                // Still failing — caller will try again on the next tool call.
            }
        }
    }

    /// Close everything. Used on graceful daemon shutdown (SIGTERM/idle
    /// timeout) and on direct-mode stop. Idempotent.
    pub fn stop(&self) {
        if self.closed.get() {
            return;
        }
        self.closed.set(true);
        self.tool_handler.close_all();
        if let Some(cg) = self.cg.borrow_mut().take() {
            cg.close();
        }
    }

    fn do_initialize(&self, search_from: &str) {
        self.tool_handler.set_default_project_hint(search_from);

        let resolved_root = match find_nearest_codegraph_root(Path::new(search_from)) {
            Some(root) => root,
            None => {
                // No .codegraph/ above search_from. Sessions may still
                // discover one later via roots/list.
                *self.project_path.borrow_mut() = Some(search_from.to_string());
                return;
            }
        };

        *self.project_path.borrow_mut() = Some(resolved_root.to_string_lossy().to_string());
        match CodeGraph::open(&resolved_root, &OpenOptions::default()) {
            Ok(cg) => {
                let cg = Rc::new(cg);
                *self.cg.borrow_mut() = Some(Rc::clone(&cg));
                self.tool_handler.set_default_code_graph(Rc::clone(&cg));
                self.start_watching();
                self.catch_up_sync();
            }
            Err(err) => {
                eprintln!(
                    "[CodeGraph MCP] Failed to open project at {}: {}",
                    resolved_root.display(),
                    err
                );
            }
        }
    }

    /// Start file watching on the active CodeGraph instance. Idempotent — the
    /// watcher is per-engine, not per-session, which is why the daemon path
    /// collapses N inotify sets to one. The wording of the disabled-reason
    /// log exactly matches the prior in-tree implementation so log-driven
    /// dashboards keep working.
    fn start_watching(&self) {
        if self.cg.borrow().is_none() || self.watcher_started.get() || !self.opts.watch {
            return;
        }

        let root = self.project_path.borrow().clone().unwrap_or_else(|| {
            std::env::current_dir()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_default()
        });
        if let Some(disabled_reason) = watch_disabled_reason(&root, &WatchProbe::default()) {
            eprintln!(
                "[CodeGraph MCP] File watcher disabled — {disabled_reason}. The graph will not auto-update; run `codegraph sync` (or install the git sync hooks via `codegraph init`) to refresh."
            );
            self.watcher_started.set(true);
            return;
        }

        // Optional override for the debounce window via env var (issue #403).
        // Useful for workspaces with bursty writes (formatter-on-save chains,
        // large generated outputs) where the 2s default fires too often.
        // Clamped to [100ms, 60s]; out-of-range / non-numeric values fall
        // back to the FileWatcher default. We log the active value so it's
        // discoverable.
        let debounce_ms =
            parse_debounce_env(std::env::var("CODEGRAPH_WATCH_DEBOUNCE_MS").ok().as_deref());
        if let Some(ms) = debounce_ms {
            eprintln!(
                "[CodeGraph MCP] File watcher debounce: {ms}ms (CODEGRAPH_WATCH_DEBOUNCE_MS)"
            );
        }

        let started = self
            .cg
            .borrow()
            .as_ref()
            .map(|cg| {
                cg.watch(WatchOptions {
                    debounce_ms,
                    on_sync_complete: Some(Arc::new(|result| {
                        if result.files_changed > 0 {
                            eprintln!(
                                "[CodeGraph MCP] Auto-synced {} file(s) in {}ms",
                                result.files_changed, result.duration_ms
                            );
                        }
                    })),
                    on_sync_error: Some(Arc::new(|err| {
                        eprintln!("[CodeGraph MCP] Auto-sync error: {err}");
                    })),
                    inert_for_tests: false,
                })
            })
            .unwrap_or(false);

        self.watcher_started.set(true);
        if started {
            eprintln!("[CodeGraph MCP] File watcher active — graph will auto-sync on changes");
        } else {
            eprintln!(
                "[CodeGraph MCP] File watcher unavailable on this platform — run `codegraph sync` to refresh the graph after changes."
            );
        }
    }

    /// Reconcile the index with the current filesystem once, right after open
    /// — catches edits, adds, deletes, and `git pull`/`checkout` changes made
    /// while no watcher was running. The TS version fires `cg.sync()` in the
    /// background and pushes its promise into the ToolHandler as a one-shot
    /// gate; the sync Rust port pushes a one-shot *closure* that runs the
    /// sync, so the *first* tool call still blocks on the reconcile before
    /// serving (without this, a tool call that races past sync returns rows
    /// for files that no longer exist on disk — and the per-file staleness
    /// banner can't help because `get_pending_files()` is populated by the
    /// watcher, not by catch-up).
    fn catch_up_sync(&self) {
        let cg = match self.cg.borrow().as_ref() {
            Some(cg) => Rc::clone(cg),
            None => return,
        };
        let gate: Box<dyn FnOnce()> = Box::new(move || match cg.sync(&IndexOptions::default()) {
            Ok(result) => {
                let changed = result.files_added + result.files_modified + result.files_removed;
                if changed > 0 {
                    eprintln!("[CodeGraph MCP] Caught up {changed} file(s) changed since last run");
                }
            }
            Err(err) => {
                eprintln!("[CodeGraph MCP] Catch-up sync failed: {err}");
            }
        });
        self.tool_handler.set_catch_up_gate(Some(gate));
    }
}

/// Parse and clamp the CODEGRAPH_WATCH_DEBOUNCE_MS env override.
///
/// Issue #403: workspaces with bursty writes (formatter-on-save, multi-file
/// refactors) sometimes want a longer quiet window before sync. Returns
/// `None` for unset / empty / non-numeric / out-of-range values so the
/// FileWatcher default (2000ms) takes over — never panics.
///
/// Clamp range: 100ms (faster would mean a sync per keystroke) to 60s (longer
/// and the watcher feels broken). Out-of-range values are treated as "ignore
/// this misconfiguration" rather than capped, since silently capping a 0 or
/// a typoed value would mask a real config bug.
pub fn parse_debounce_env(raw: Option<&str>) -> Option<u64> {
    let raw = raw?;
    if raw.trim().is_empty() {
        return None;
    }
    // JS `Number(raw)` semantics for the relevant inputs: trims whitespace,
    // accepts scientific notation ('1e3' → 1000), 'Infinity'/'NaN' parse to
    // non-finite values (rejected below), non-numeric strings fail.
    let n: f64 = raw.trim().parse().ok()?;
    if !n.is_finite() || n.fract() != 0.0 {
        return None;
    }
    if n < 100.0 || n > 60000.0 {
        return None;
    }
    Some(n as u64)
}

// =============================================================================
// EngineHandle — Send/Sync seam over the engine thread
// =============================================================================

enum EngineCommand {
    SetProjectPathHint(String),
    EnsureInitialized {
        search_from: String,
        done: Sender<()>,
    },
    RetryInitializeSync {
        search_from: String,
        done: Sender<()>,
    },
    HasDefaultCodeGraph(Sender<bool>),
    GetProjectPath(Sender<Option<String>>),
    GetTools(Sender<Vec<ToolDefinition>>),
    Execute {
        name: String,
        args: Value,
        reply: Sender<ToolResult>,
    },
    Stop(Sender<()>),
}

/// `Send + Sync + Clone` handle over a dedicated engine thread that owns the
/// (`!Send`) [`MCPEngine`]. Commands are processed strictly serially — the
/// Rust analog of N sessions sharing one engine on the JS event loop.
#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<EngineCommand>,
}

impl EngineHandle {
    /// Spawn the engine thread and return a handle to it.
    pub fn spawn(opts: MCPEngineOptions) -> EngineHandle {
        let (tx, rx) = crossbeam_channel::unbounded::<EngineCommand>();
        let _ = std::thread::Builder::new()
            .name("codegraph-mcp-engine".to_string())
            .spawn(move || {
                let engine = MCPEngine::new(opts);
                for cmd in rx {
                    match cmd {
                        EngineCommand::SetProjectPathHint(p) => engine.set_project_path_hint(&p),
                        EngineCommand::EnsureInitialized { search_from, done } => {
                            engine.ensure_initialized(&search_from);
                            let _ = done.send(());
                        }
                        EngineCommand::RetryInitializeSync { search_from, done } => {
                            engine.retry_initialize_sync(&search_from);
                            let _ = done.send(());
                        }
                        EngineCommand::HasDefaultCodeGraph(reply) => {
                            let _ = reply.send(engine.has_default_code_graph());
                        }
                        EngineCommand::GetProjectPath(reply) => {
                            let _ = reply.send(engine.get_project_path());
                        }
                        EngineCommand::GetTools(reply) => {
                            let _ = reply.send(engine.get_tool_handler().get_tools());
                        }
                        EngineCommand::Execute { name, args, reply } => {
                            let _ = reply.send(engine.get_tool_handler().execute(&name, &args));
                        }
                        EngineCommand::Stop(reply) => {
                            engine.stop();
                            let _ = reply.send(());
                            return;
                        }
                    }
                }
                // All handles dropped — clean teardown.
                engine.stop();
            });
        EngineHandle { tx }
    }

    pub fn set_project_path_hint(&self, project_path: &str) {
        let _ = self
            .tx
            .send(EngineCommand::SetProjectPathHint(project_path.to_string()));
    }

    /// Kick off `ensure_initialized` without blocking (TS background
    /// `void engine.ensureInitialized(...)` / the session's `resolvePromise`).
    /// The returned receiver resolves when init finished (successfully or
    /// not).
    pub fn ensure_initialized_async(&self, search_from: &str) -> Receiver<()> {
        let (done, rx) = crossbeam_channel::bounded(1);
        let _ = self.tx.send(EngineCommand::EnsureInitialized {
            search_from: search_from.to_string(),
            done,
        });
        rx
    }

    /// Blocking `ensure_initialized` (TS `await engine.ensureInitialized(...)`).
    pub fn ensure_initialized(&self, search_from: &str) {
        let _ = self.ensure_initialized_async(search_from).recv();
    }

    pub fn retry_initialize_sync(&self, search_from: &str) {
        let (done, rx) = crossbeam_channel::bounded(1);
        let _ = self.tx.send(EngineCommand::RetryInitializeSync {
            search_from: search_from.to_string(),
            done,
        });
        let _ = rx.recv();
    }

    pub fn has_default_code_graph(&self) -> bool {
        let (reply, rx) = crossbeam_channel::bounded(1);
        if self
            .tx
            .send(EngineCommand::HasDefaultCodeGraph(reply))
            .is_err()
        {
            return false;
        }
        rx.recv().unwrap_or(false)
    }

    pub fn get_project_path(&self) -> Option<String> {
        let (reply, rx) = crossbeam_channel::bounded(1);
        if self.tx.send(EngineCommand::GetProjectPath(reply)).is_err() {
            return None;
        }
        rx.recv().unwrap_or(None)
    }

    pub fn get_tools(&self) -> Vec<ToolDefinition> {
        let (reply, rx) = crossbeam_channel::bounded(1);
        if self.tx.send(EngineCommand::GetTools(reply)).is_err() {
            return crate::mcp::tools::tools();
        }
        rx.recv().unwrap_or_else(|_| crate::mcp::tools::tools())
    }

    pub fn execute(&self, name: &str, args: Value) -> ToolResult {
        let (reply, rx) = crossbeam_channel::bounded(1);
        let send_ok = self
            .tx
            .send(EngineCommand::Execute {
                name: name.to_string(),
                args,
                reply,
            })
            .is_ok();
        if send_ok {
            if let Ok(result) = rx.recv() {
                return result;
            }
        }
        ToolResult {
            content: vec![crate::mcp::tools::ToolContent {
                content_type: "text".to_string(),
                text: "Tool execution failed: engine stopped".to_string(),
            }],
            is_error: Some(true),
        }
    }

    /// Stop the engine and its thread. Blocking; idempotent (subsequent calls
    /// are no-ops once the thread exited).
    pub fn stop(&self) {
        let (reply, rx) = crossbeam_channel::bounded(1);
        if self.tx.send(EngineCommand::Stop(reply)).is_ok() {
            let _ = rx.recv();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // __tests__/mcp-debounce-env.test.ts — parseDebounceEnv (issue #403).

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
        assert_eq!(parse_debounce_env(Some("50")), None); // below 100
        assert_eq!(parse_debounce_env(Some("99")), None);
        assert_eq!(parse_debounce_env(Some("60001")), None); // above 60s
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
        // Number('1e3') === 1000, Number.isInteger(1000) === true. Power users
        // who write debounce as 1e3 should not be surprised; the clamp still
        // applies.
        assert_eq!(parse_debounce_env(Some("1e3")), Some(1000));
    }
}
