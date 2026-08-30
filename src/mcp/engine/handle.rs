use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value;
use tokio::runtime::Handle;

use super::{LogBroadcaster, LogSubscription, MCPEngine, MCPEngineOptions};
use crate::mcp::tools::{ProgressEmitter, ToolContent, ToolDefinition, ToolResult};

#[cfg(test)]
mod tests;

fn fatal_engine_worker_failure(reason: &str) -> ! {
    let _ = writeln!(
        std::io::stderr().lock(),
        "[CodeGraph MCP] Engine worker failed unexpectedly ({reason}); aborting."
    );
    std::process::exit(1);
}

type FatalCallback = Arc<dyn Fn(&str) + Send + Sync + 'static>;

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
        progress: Option<ProgressEmitter>,
        cancel: Option<Arc<AtomicBool>>,
        reply: Sender<ToolResult>,
    },
    Stop(Sender<()>),
    #[cfg(test)]
    PanicForTest,
}

#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<EngineCommand>,
    logs: LogBroadcaster,
}

impl EngineHandle {
    pub fn spawn(opts: MCPEngineOptions) -> EngineHandle {
        Self::spawn_on(opts, Handle::current())
    }

    fn spawn_on(opts: MCPEngineOptions, runtime: Handle) -> EngineHandle {
        Self::spawn_on_with_fatal(
            opts,
            runtime,
            Arc::new(|reason| {
                fatal_engine_worker_failure(reason);
            }),
        )
    }

    fn spawn_on_with_fatal(
        opts: MCPEngineOptions,
        runtime: Handle,
        fatal: FatalCallback,
    ) -> EngineHandle {
        let (tx, rx) = crossbeam_channel::unbounded::<EngineCommand>();
        let logs = LogBroadcaster::default();
        let engine_logs = logs.clone();
        let worker = match std::thread::Builder::new()
            .name("codegraph-mcp-engine".to_string())
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let engine = MCPEngine::with_runtime(opts, engine_logs, runtime);
                for command in rx {
                    match command {
                        EngineCommand::SetProjectPathHint(path) => {
                            engine.set_project_path_hint(&path);
                        }
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
                        EngineCommand::Execute {
                            name,
                            args,
                            progress,
                            cancel,
                            reply,
                        } => {
                            let context = engine.get_tool_handler().call_context();
                            context.set(progress, cancel);
                            let result = engine.get_tool_handler().execute(&name, &args);
                            context.clear();
                            let _ = reply.send(result);
                        }
                        EngineCommand::Stop(reply) => {
                            engine.stop();
                            let _ = reply.send(());
                            return;
                        }
                        #[cfg(test)]
                        EngineCommand::PanicForTest => panic!("injected engine worker panic"),
                    }
                }
                engine.stop();
            }) {
            Ok(worker) => worker,
            Err(error) => {
                fatal(&format!("failed to start worker: {error}"));
                return EngineHandle { tx, logs };
            }
        };
        let supervisor_fatal = Arc::clone(&fatal);
        if let Err(error) = std::thread::Builder::new()
            .name("codegraph-mcp-engine-supervisor".to_string())
            .spawn(move || {
                if worker.join().is_err() {
                    supervisor_fatal("worker thread panicked");
                }
            })
        {
            fatal(&format!("failed to start supervisor: {error}"));
        }
        EngineHandle { tx, logs }
    }

    pub fn register_log_subscriber(&self, subscription: Arc<LogSubscription>) {
        self.logs.subscribe(subscription);
    }

    pub fn set_project_path_hint(&self, project_path: &str) {
        let _ = self
            .tx
            .send(EngineCommand::SetProjectPathHint(project_path.to_string()));
    }

    pub fn ensure_initialized_async(&self, search_from: &str) -> Receiver<()> {
        let (done, receiver) = crossbeam_channel::bounded(1);
        let _ = self.tx.send(EngineCommand::EnsureInitialized {
            search_from: search_from.to_string(),
            done,
        });
        receiver
    }

    pub fn ensure_initialized(&self, search_from: &str) {
        let _ = self.ensure_initialized_async(search_from).recv();
    }

    pub fn retry_initialize_sync(&self, search_from: &str) {
        let (done, receiver) = crossbeam_channel::bounded(1);
        let _ = self.tx.send(EngineCommand::RetryInitializeSync {
            search_from: search_from.to_string(),
            done,
        });
        let _ = receiver.recv();
    }

    pub fn has_default_code_graph(&self) -> bool {
        let (reply, receiver) = crossbeam_channel::bounded(1);
        if self
            .tx
            .send(EngineCommand::HasDefaultCodeGraph(reply))
            .is_err()
        {
            return false;
        }
        receiver.recv().unwrap_or(false)
    }

    pub fn get_project_path(&self) -> Option<String> {
        let (reply, receiver) = crossbeam_channel::bounded(1);
        if self.tx.send(EngineCommand::GetProjectPath(reply)).is_err() {
            return None;
        }
        receiver.recv().unwrap_or(None)
    }

    pub fn get_tools(&self) -> Vec<ToolDefinition> {
        let (reply, receiver) = crossbeam_channel::bounded(1);
        if self.tx.send(EngineCommand::GetTools(reply)).is_err() {
            return crate::mcp::tools::tools();
        }
        receiver
            .recv()
            .unwrap_or_else(|_| crate::mcp::tools::tools())
    }

    pub fn execute(&self, name: &str, args: Value) -> ToolResult {
        self.execute_with_context(name, args, None, None)
    }

    pub fn execute_with_context(
        &self,
        name: &str,
        args: Value,
        progress: Option<ProgressEmitter>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> ToolResult {
        let (reply, receiver) = crossbeam_channel::bounded(1);
        let sent = self
            .tx
            .send(EngineCommand::Execute {
                name: name.to_string(),
                args,
                progress,
                cancel,
                reply,
            })
            .is_ok();
        if sent {
            if let Ok(result) = receiver.recv() {
                return result;
            }
        }
        ToolResult {
            content: vec![ToolContent {
                content_type: "text".to_string(),
                text: "Tool execution failed: engine stopped".to_string(),
            }],
            structured_content: None,
            meta: None,
            is_error: Some(true),
        }
    }

    pub fn stop(&self) {
        let (reply, receiver) = crossbeam_channel::bounded(1);
        if self.tx.send(EngineCommand::Stop(reply)).is_ok() {
            let _ = receiver.recv();
        }
    }

    #[cfg(test)]
    fn panic_for_test(&self) {
        let _ = self.tx.send(EngineCommand::PanicForTest);
    }
}
