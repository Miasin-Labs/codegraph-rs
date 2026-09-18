use std::sync::Arc;

use rmcp::model::{CallToolResult, LoggingLevel, ProgressNotificationParam, ProgressToken, Tool};
use rmcp::service::Peer;
use rmcp::{ErrorData as McpError, RoleServer};

use crate::mcp::tools::{ProgressEmitter, ToolDefinition, ToolResult};

pub(super) fn convert_tools(
    definitions: Vec<ToolDefinition>,
) -> Result<Vec<Tool>, serde_json::Error> {
    definitions
        .into_iter()
        .map(|definition| serde_json::from_value(serde_json::to_value(definition)?))
        .collect()
}

/// The wire form of a tool result (see `ToolResult::into_mcp_projection`).
pub(super) fn project_result(result: ToolResult) -> Result<ToolResult, McpError> {
    result
        .into_mcp_projection()
        .map_err(|error| McpError::internal_error(error.to_string(), None))
}

/// An already-projected result as the rmcp type.
pub(super) fn to_rmcp_result(projected: ToolResult) -> Result<CallToolResult, McpError> {
    serde_json::from_value(
        serde_json::to_value(projected)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?,
    )
    .map_err(|error| McpError::internal_error(error.to_string(), None))
}

pub(super) fn progress_channel(
    token: Option<ProgressToken>,
    peer: Peer<RoleServer>,
) -> (Option<ProgressEmitter>, Option<tokio::task::JoinHandle<()>>) {
    let Some(token) = token else {
        return (None, None);
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let _ = peer.notify_progress(message).await;
        }
    });
    let emitter = Arc::new(
        move |progress: f64, total: Option<f64>, message: Option<&str>| {
            let mut notification = ProgressNotificationParam::new(token.clone(), progress);
            notification.total = total;
            notification.message = message.map(str::to_string);
            let _ = tx.send(notification);
        },
    ) as ProgressEmitter;
    (Some(emitter), Some(task))
}

pub(super) async fn run_blocking<T, F>(work: F) -> Result<T, McpError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| McpError::internal_error(error.to_string(), None))
}

pub(super) fn rmcp_logging_level(level: &str) -> LoggingLevel {
    match level {
        "debug" => LoggingLevel::Debug,
        "notice" => LoggingLevel::Notice,
        "warning" => LoggingLevel::Warning,
        "error" => LoggingLevel::Error,
        "critical" => LoggingLevel::Critical,
        "alert" => LoggingLevel::Alert,
        "emergency" => LoggingLevel::Emergency,
        _ => LoggingLevel::Info,
    }
}
