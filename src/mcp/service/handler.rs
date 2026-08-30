use std::borrow::Cow;
use std::sync::atomic::Ordering;

use rmcp::model::{
    CallToolRequestParams,
    CallToolResponse,
    CustomRequest,
    CustomResult,
    Implementation,
    ListToolsResult,
    LoggingLevel,
    PaginatedRequestParams,
    ProtocolVersion,
    ServerCapabilities,
    ServerInfo,
    SetLevelRequestParams,
    Tool,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::Value;

use super::CodeGraphService;
use crate::mcp::engine::logging_level_rank;
use crate::mcp::server_instructions::{SERVER_INSTRUCTIONS, SERVER_INSTRUCTIONS_NO_ROOT_INDEX};
use crate::mcp::version::CODEGRAPH_PACKAGE_VERSION;
use crate::telemetry::Telemetry;

impl ServerHandler for CodeGraphService {
    fn get_info(&self) -> ServerInfo {
        let root = self
            .inner
            .explicit_project_path
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
            });
        let instructions = if crate::directory::is_initialized(&root) {
            SERVER_INSTRUCTIONS
        } else {
            SERVER_INSTRUCTIONS_NO_ROOT_INDEX
        };
        let capabilities = ServerCapabilities::builder()
            .enable_logging()
            .enable_tools()
            .enable_tool_list_changed()
            .build();
        ServerInfo::new(capabilities)
            .with_server_info(Implementation::new("codegraph", CODEGRAPH_PACKAGE_VERSION))
            .with_instructions(instructions)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::KNOWN_VERSIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.ensure_log_subscription(&context.peer);
        self.ensure_project(&context).await?;
        let (tools, _) = self.refresh_tools().await?;
        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.inner
            .tools
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let tool_name = request.name.clone();
        let client = context.client_info();
        let result = self.execute_tool(request, context).await;
        let ok = result.as_ref().is_ok_and(|response| match response {
            CallToolResponse::Complete(result) => result.is_error != Some(true),
            _ => true,
        });
        Telemetry::default().record_usage_with_client(
            "mcp_tool",
            &tool_name,
            ok,
            client.as_ref().map(|client| client.name.as_str()),
            client.as_ref().map(|client| client.version.as_str()),
        );
        result
    }

    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.ensure_log_subscription(&context.peer);
        if let Some(subscription) = self
            .inner
            .log_subscription
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            subscription
                .set_min_rank(logging_level_rank(logging_level_name(request.level)).unwrap_or(1));
        }
        Ok(())
    }

    async fn on_roots_list_changed(&self, _context: NotificationContext<RoleServer>) {
        self.inner.roots_epoch.fetch_add(1, Ordering::AcqRel);
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        if request.method == "logging/setLevel" {
            let level = request
                .params
                .as_ref()
                .and_then(|params| params.get("level"))
                .and_then(Value::as_str)
                .unwrap_or("<missing>");
            return Err(McpError::invalid_params(
                format!("Invalid logging level: {level}"),
                None,
            ));
        }
        if request.method == "tools/call" {
            let name = request
                .params
                .as_ref()
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty());
            let message = name.map_or_else(
                || "Missing tool name".to_string(),
                |name| format!("Unknown tool: {name}"),
            );
            return Err(McpError::invalid_params(message, None));
        }
        Err(McpError::new(
            rmcp::model::ErrorCode::METHOD_NOT_FOUND,
            format!("Method not found: {}", request.method),
            None,
        ))
    }
}

const fn logging_level_name(level: LoggingLevel) -> &'static str {
    match level {
        LoggingLevel::Debug => "debug",
        LoggingLevel::Info => "info",
        LoggingLevel::Notice => "notice",
        LoggingLevel::Warning => "warning",
        LoggingLevel::Error => "error",
        LoggingLevel::Critical => "critical",
        LoggingLevel::Alert => "alert",
        LoggingLevel::Emergency => "emergency",
    }
}
