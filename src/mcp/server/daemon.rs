use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{MCPServer, resolve_daemon_root};
use crate::error::Result;
use crate::mcp::daemon::{
    AcquireResult,
    Daemon,
    DaemonOptions,
    DaemonSessionFactory,
    clear_stale_daemon_lock,
    try_acquire_daemon_lock,
};
use crate::mcp::engine::{EngineHandle, MCPEngineOptions};
use crate::mcp::service::CodeGraphService;

const TAKEOVER_MAX_RETRIES: u32 = 5;
const TAKEOVER_RETRY_DELAY_MS: u64 = 100;

pub(super) fn start(server: &MCPServer) -> Result<()> {
    let root = resolve_daemon_root(server.project_path.as_deref())
        .or_else(|| server.project_path.as_deref().map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));

    for _attempt in 0..TAKEOVER_MAX_RETRIES {
        match try_acquire_daemon_lock(&root)? {
            AcquireResult::Acquired { .. } => {
                let factory = Arc::new(EngineSessionFactory {
                    engine: EngineHandle::spawn(MCPEngineOptions::default()),
                    runtime: tokio::runtime::Handle::current(),
                });
                let daemon = Daemon::new(&root, factory, DaemonOptions::default());
                daemon.start()?;
                daemon.wait();
                std::process::exit(0);
            }
            AcquireResult::Taken { existing, pid_path } => {
                if existing.as_ref().is_some_and(|info| {
                    u32::try_from(info.pid).is_ok_and(crate::utils::is_process_alive)
                }) {
                    std::process::exit(0);
                }
                clear_stale_daemon_lock(&pid_path, existing.as_ref().map(|info| info.pid));
                std::thread::sleep(std::time::Duration::from_millis(TAKEOVER_RETRY_DELAY_MS));
            }
        }
    }
    std::process::exit(0);
}

struct EngineSessionFactory {
    engine: EngineHandle,
    runtime: tokio::runtime::Handle,
}

impl DaemonSessionFactory for EngineSessionFactory {
    fn warm_up(&self, project_root: &Path) {
        self.engine
            .ensure_initialized(&project_root.to_string_lossy());
    }

    fn serve_connection(&self, stream: std::os::unix::net::UnixStream, project_root: &Path) {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        let Ok(service) = CodeGraphService::new(
            self.engine.clone(),
            Some(project_root.to_string_lossy().to_string()),
        ) else {
            return;
        };
        let _ = self.runtime.block_on(async move {
            let stream = tokio::net::UnixStream::from_std(stream)?;
            let running = rmcp::serve_server(service, stream).await?;
            running.waiting().await?;
            anyhow::Ok(())
        });
    }

    fn stop_engine(&self) {
        self.engine.stop();
    }
}
