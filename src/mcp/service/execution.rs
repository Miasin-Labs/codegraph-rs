use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rmcp::model::{CallToolRequestParams, CallToolResponse};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};
use serde_json::Value;

use super::CodeGraphService;
use super::wire::{progress_channel, project_result, run_blocking, to_rmcp_result};
use crate::mcp::explore_session::{GREP_SESSION_ARG, SESSION_ARG, dedup_enabled};

/// Tools whose results carry file source. They read the per-connection
/// session ledger (so lines this conversation already holds are not sent
/// again) and record what they sent into it. `grep` sends single-line
/// excerpts, which it records apart from the source ranges (see
/// `explore_session::grep`).
const LEDGER_TOOLS: &[&str] = &["codegraph_explore", "codegraph_node", "codegraph_grep"];

impl CodeGraphService {
    pub(super) async fn execute_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.ensure_log_subscription(&context.peer);
        self.ensure_project(&context).await?;
        if !crate::mcp::tools::tools()
            .iter()
            .any(|tool| tool.name == request.name)
        {
            return Err(McpError::invalid_params(
                format!("Unknown tool: {}", request.name),
                None,
            ));
        }
        let (progress, progress_task) =
            progress_channel(context.meta.get_progress_token(), context.peer.clone());
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_task = {
            let token = context.ct.clone();
            let cancelled = Arc::clone(&cancelled);
            tokio::spawn(async move {
                token.cancelled().await;
                cancelled.store(true, Ordering::SeqCst);
            })
        };
        let name = request.name.into_owned();
        let mut arguments = Value::Object(request.arguments.unwrap_or_default());
        // Explores are serialized per connection; other ledger tools run
        // concurrently (parallel calls in one turn each see the ledger as it
        // stood, which can only cost a duplicate, never a false `alreadySent`).
        let _explore_guard = if name == "codegraph_explore" {
            Some(self.inner.explore_gate.lock().await)
        } else {
            None
        };
        let project_root = if LEDGER_TOOLS.contains(&name.as_str()) {
            let requested = arguments
                .get("projectPath")
                .and_then(Value::as_str)
                .and_then(indexed_project_root);
            if requested.is_some() {
                requested
            } else {
                let engine = self.inner.engine.clone();
                run_blocking(move || engine.get_project_path()).await?
            }
        } else {
            None
        };
        if dedup_enabled() {
            if let Some(root) = project_root.as_deref() {
                if let Value::Object(args) = &mut arguments {
                    args.remove(SESSION_ARG);
                    args.remove(GREP_SESSION_ARG);
                    let (view, grep_view) = {
                        let session = self
                            .inner
                            .explore_session
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        let root = std::path::Path::new(root);
                        let grep_view =
                            (name == "codegraph_grep").then(|| session.grep_view_for(root));
                        (session.view_for(root), grep_view)
                    };
                    args.insert(
                        SESSION_ARG.to_string(),
                        serde_json::to_value(view)
                            .map_err(|error| McpError::internal_error(error.to_string(), None))?,
                    );
                    if let Some(grep_view) = grep_view {
                        args.insert(
                            GREP_SESSION_ARG.to_string(),
                            serde_json::to_value(grep_view).map_err(|error| {
                                McpError::internal_error(error.to_string(), None)
                            })?,
                        );
                    }
                }
            }
        }
        let engine = self.inner.engine.clone();
        let call_cancelled = Arc::clone(&cancelled);
        let result = run_blocking(move || {
            engine.execute_with_context(&name, arguments, progress, Some(call_cancelled))
        })
        .await;
        cancel_task.abort();
        if let Some(task) = progress_task {
            let _ = task.await;
        }
        // Record exactly what goes on the wire, and nothing from a call the
        // client cancelled (its response is never read).
        let projected = project_result(result?)?;
        if let Some(root) = project_root.as_deref() {
            if !cancelled.load(Ordering::SeqCst) {
                self.inner
                    .explore_session
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .record(std::path::Path::new(root), &projected);
            }
        }
        let result = to_rmcp_result(projected)?;
        let (_, changed) = self.refresh_tools().await?;
        if changed {
            let _ = context.peer.notify_tool_list_changed().await;
        }
        Ok(result.into())
    }
}

fn indexed_project_root(path: &str) -> Option<String> {
    let resolved = std::path::Path::new(path).canonicalize().ok()?;
    resolved
        .ancestors()
        .find(|ancestor| ancestor.join(".codegraph").is_dir())
        .map(|root| root.to_string_lossy().into_owned())
}
