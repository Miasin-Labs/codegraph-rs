//! Tool handler state and per-call context.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::codegraph::CodeGraph;
use crate::sync::worktree::WorktreeIndexMismatch;

pub struct ToolHandler {
    /// The default CodeGraph instance (None until a project is opened).
    pub(in crate::mcp::tools) cg: RefCell<Option<Rc<CodeGraph>>>,
    /// Cache of opened CodeGraph instances for cross-project queries.
    pub(in crate::mcp::tools::context) project_cache: RefCell<HashMap<String, Rc<CodeGraph>>>,
    /// The directory the server last searched for a default project.
    pub(in crate::mcp::tools::context) default_project_hint: RefCell<Option<String>>,
    /// Per-start-path cache of the git worktree/index mismatch (issue #155).
    pub(in crate::mcp::tools::context) worktree_mismatch_cache:
        RefCell<HashMap<String, Option<WorktreeIndexMismatch>>>,
    /// Gate the MCP engine pokes after `open()` so the first tool call blocks
    /// on the post-open filesystem reconcile (catch-up sync). The TS gate is a
    /// Promise; here it is a one-shot closure run (and cleared) on the next
    /// `execute()`. Failures inside the closure are the engine's to log.
    pub(in crate::mcp::tools::context) catch_up_gate: RefCell<Option<Box<dyn FnOnce()>>>,
    /// EXCEEDS TS: per-call context (progress emitter + cooperative cancel
    /// flag) the engine sets around each `execute()` — see [`CallContext`].
    pub(in crate::mcp::tools::context) call_context: Rc<CallContext>,
}

/// Progress callback the session plumbs through the engine when a `tools/call`
/// carried a `_meta.progressToken` (rmcp `ProgressNotificationParam` fields:
/// progress, total?, message?). Never installed unsolicited.
pub type ProgressEmitter = Arc<dyn Fn(f64, Option<f64>, Option<&str>) + Send + Sync>;

/// EXCEEDS TS: per-call execution context — set by the engine thread for the
/// duration of one `ToolHandler::execute()`. The catch-up gate closure (built
/// before any call arrives) reads the *current* call's progress emitter and
/// cancel flag through this shared cell, mirroring rmcp's
/// `RequestContext { ct, peer, .. }` made available to handlers.
#[derive(Default)]
pub struct CallContext {
    progress: RefCell<Option<ProgressEmitter>>,
    cancel: RefCell<Option<Arc<AtomicBool>>>,
}

impl CallContext {
    /// Install the per-call progress emitter / cancel flag (engine thread).
    pub fn set(&self, progress: Option<ProgressEmitter>, cancel: Option<Arc<AtomicBool>>) {
        *self.progress.borrow_mut() = progress;
        *self.cancel.borrow_mut() = cancel;
    }

    /// Clear after the call completes.
    pub fn clear(&self) {
        *self.progress.borrow_mut() = None;
        *self.cancel.borrow_mut() = None;
    }

    /// Emit one `notifications/progress` if (and only if) the current call
    /// asked for progress.
    pub fn emit_progress(&self, progress: f64, total: Option<f64>, message: Option<&str>) {
        if let Some(emit) = self.progress.borrow().as_ref() {
            emit(progress, total, message);
        }
    }

    /// Whether the current call was cancelled via `notifications/cancelled`.
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
    }
}

impl ToolHandler {
    pub fn new(cg: Option<Rc<CodeGraph>>) -> ToolHandler {
        ToolHandler {
            cg: RefCell::new(cg),
            project_cache: RefCell::new(HashMap::new()),
            default_project_hint: RefCell::new(None),
            worktree_mismatch_cache: RefCell::new(HashMap::new()),
            catch_up_gate: RefCell::new(None),
            call_context: Rc::new(CallContext::default()),
        }
    }

    /// Shared per-call context — the engine sets/clears it around `execute()`
    /// and shares it with the catch-up gate closure.
    pub fn call_context(&self) -> Rc<CallContext> {
        Rc::clone(&self.call_context)
    }

    /// Update the default CodeGraph instance (e.g. after lazy initialization).
    pub fn set_default_code_graph(&self, cg: Rc<CodeGraph>) {
        *self.cg.borrow_mut() = Some(cg);
    }

    /// Engine-only: register the catch-up sync gate so the next `execute()`
    /// call runs it before serving. Cleared on first use.
    pub fn set_catch_up_gate(&self, gate: Option<Box<dyn FnOnce()>>) {
        *self.catch_up_gate.borrow_mut() = gate;
    }

    /// Record the directory the server tried to resolve the default project
    /// from. Used only to make the "no default project" error actionable.
    pub fn set_default_project_hint(&self, searched_path: impl Into<String>) {
        *self.default_project_hint.borrow_mut() = Some(searched_path.into());
    }

    /// Whether a default CodeGraph instance is available.
    pub fn has_default_code_graph(&self) -> bool {
        self.cg.borrow().is_some()
    }
}
