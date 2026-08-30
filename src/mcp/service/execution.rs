use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rmcp::model::{CallToolRequestParams, CallToolResponse};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};
use serde_json::Value;

use super::CodeGraphService;
use super::wire::{convert_result, progress_channel, run_blocking};
use crate::mcp::explore_session::{SESSION_ARG, dedup_enabled};

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
        let _explore_guard = if name == "codegraph_explore" {
            Some(self.inner.explore_gate.lock().await)
        } else {
            None
        };
        let project_root = if name == "codegraph_explore" {
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
                    let view = self
                        .inner
                        .explore_session
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .view_for(std::path::Path::new(root));
                    args.insert(
                        SESSION_ARG.to_string(),
                        serde_json::to_value(view)
                            .map_err(|error| McpError::internal_error(error.to_string(), None))?,
                    );
                }
            }
        }
        let engine = self.inner.engine.clone();
        let result = run_blocking(move || {
            engine.execute_with_context(&name, arguments, progress, Some(cancelled))
        })
        .await;
        cancel_task.abort();
        if let Some(task) = progress_task {
            let _ = task.await;
        }
        let result = result?;
        if let Some(root) = project_root.as_deref() {
            self.inner
                .explore_session
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .record(std::path::Path::new(root), &result);
        }
        let result = convert_result(result)?;
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
