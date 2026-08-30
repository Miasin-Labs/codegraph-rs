//! Shared CodeGraph engine owned by one dedicated thread and exposed through a cloneable handle.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tokio::runtime::Handle;

use crate::codegraph::CodeGraph;
use crate::mcp::tools::ToolHandler;

mod handle;
mod logging;
mod project;

pub use handle::EngineHandle;
pub use logging::{LogBroadcaster, LogSubscription, logging_level_rank};
pub use project::parse_debounce_env;

#[derive(Clone, Copy)]
pub struct MCPEngineOptions {
    pub watch: bool,
}

impl Default for MCPEngineOptions {
    fn default() -> Self {
        MCPEngineOptions { watch: true }
    }
}

pub struct MCPEngine {
    cg: RefCell<Option<Rc<CodeGraph>>>,
    tool_handler: ToolHandler,
    project_path: RefCell<Option<String>>,
    watcher_started: Cell<bool>,
    opts: MCPEngineOptions,
    closed: Cell<bool>,
    logs: LogBroadcaster,
    runtime: Handle,
}

impl MCPEngine {
    pub fn new(opts: MCPEngineOptions) -> MCPEngine {
        MCPEngine::with_log_broadcaster(opts, LogBroadcaster::default())
    }

    pub fn with_log_broadcaster(opts: MCPEngineOptions, logs: LogBroadcaster) -> MCPEngine {
        Self::with_runtime(opts, logs, Handle::current())
    }

    fn with_runtime(opts: MCPEngineOptions, logs: LogBroadcaster, runtime: Handle) -> MCPEngine {
        MCPEngine {
            cg: RefCell::new(None),
            tool_handler: ToolHandler::new(None),
            project_path: RefCell::new(None),
            watcher_started: Cell::new(false),
            opts,
            closed: Cell::new(false),
            logs,
            runtime,
        }
    }

    pub fn set_project_path_hint(&self, project_path: &str) {
        *self.project_path.borrow_mut() = Some(project_path.to_string());
        self.tool_handler.set_default_project_hint(project_path);
    }

    pub fn get_project_path(&self) -> Option<String> {
        self.project_path.borrow().clone()
    }

    pub fn get_tool_handler(&self) -> &ToolHandler {
        &self.tool_handler
    }

    pub fn has_default_code_graph(&self) -> bool {
        self.tool_handler.has_default_code_graph()
    }

    pub fn stop(&self) {
        if self.closed.get() {
            return;
        }
        self.closed.set(true);
        self.tool_handler.close_all();
        if let Some(codegraph) = self.cg.borrow_mut().take() {
            codegraph.close();
        }
    }
}
