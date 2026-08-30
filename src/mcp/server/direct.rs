use super::MCPServer;
use crate::error::Result;
use crate::mcp::engine::{EngineHandle, MCPEngineOptions};
use crate::mcp::service::CodeGraphService;

pub(super) async fn start(server: &MCPServer, reason: &str) -> Result<()> {
    if !reason.is_empty()
        && std::env::var("CODEGRAPH_MCP_DEBUG").is_ok_and(|value| !value.is_empty())
    {
        eprintln!("[CodeGraph MCP] Direct mode: {reason}.");
    }

    let engine = EngineHandle::spawn(MCPEngineOptions::default());
    let service = CodeGraphService::new(engine.clone(), server.project_path.clone())
        .map_err(|error| crate::error::CodeGraphError::other(error.to_string()))?;

    if let Some(path) = &server.project_path {
        let _ = engine.ensure_initialized_async(path);
    }
    *server
        .engine
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(engine.clone());
    start_parent_watchdog(engine.clone());

    let (input, output) = rmcp::transport::stdio();
    let timeout_engine = engine.clone();
    let input = crate::mcp::startup::StartupRead::new(
        input,
        crate::mcp::startup::arm_process_timeout(move || {
            timeout_engine.stop();
            std::process::exit(0);
        }),
    );
    let running = rmcp::serve_server(service, (input, output))
        .await
        .map_err(|error| crate::error::CodeGraphError::other(error.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|error| crate::error::CodeGraphError::other(error.to_string()))?;
    engine.stop();
    Ok(())
}

#[cfg(not(unix))]
fn start_parent_watchdog(_engine: EngineHandle) {}

#[cfg(unix)]
fn start_parent_watchdog(engine: EngineHandle) {
    let poll_ms =
        crate::mcp::proxy::parse_poll_ms(std::env::var("CODEGRAPH_PPID_POLL_MS").ok().as_deref());
    if poll_ms == 0 {
        return;
    }
    let host_ppid = crate::mcp::proxy::parse_host_ppid(
        std::env::var(crate::mcp::daemon_paths::HOST_PPID_ENV)
            .ok()
            .as_deref(),
    );
    // SAFETY: getppid has no preconditions and does not dereference memory.
    let original_ppid = unsafe { libc::getppid() } as u32;
    crate::mcp::proxy::spawn_ppid_watchdog_with(poll_ms, original_ppid, host_ppid, move |reason| {
        eprintln!("[CodeGraph MCP] Parent process exited ({reason}); shutting down.");
        engine.stop();
        std::process::exit(0);
    });
}
