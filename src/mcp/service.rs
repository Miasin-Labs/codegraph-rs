//! rmcp adapter for CodeGraph's shared MCP engine.

#![allow(deprecated, reason = "MCP 2025 logging and roots compatibility")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use rmcp::model::{LoggingMessageNotificationParam, Tool};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};
use serde_json::Value;

use crate::mcp::engine::{EngineHandle, LogSubscription};
use crate::mcp::explore_session::ExploreSessionState;

mod execution;
mod handler;
mod root;
mod wire;

use root::file_uri_to_path;
use wire::{convert_tools, rmcp_logging_level, run_blocking};

const ROOTS_LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// One rmcp server connection backed by a shared CodeGraph engine.
#[derive(Clone)]
pub struct CodeGraphService {
    inner: Arc<ServiceState>,
}

struct ServiceState {
    engine: EngineHandle,
    explicit_project_path: Option<String>,
    tools: RwLock<Vec<Tool>>,
    roots_epoch: AtomicU64,
    resolved_epoch: AtomicU64,
    roots_gate: tokio::sync::Mutex<()>,
    last_target: Mutex<Option<String>>,
    last_listed_tools: Mutex<Option<String>>,
    log_subscription: Mutex<Option<Arc<LogSubscription>>>,
    explore_session: Mutex<ExploreSessionState>,
    explore_gate: tokio::sync::Mutex<()>,
}

impl CodeGraphService {
    /// Create one connection-local rmcp service over a shared engine.
    pub fn new(
        engine: EngineHandle,
        explicit_project_path: Option<String>,
    ) -> Result<Self, serde_json::Error> {
        let tools = convert_tools(crate::mcp::tools::tools())?;
        Ok(Self {
            inner: Arc::new(ServiceState {
                engine,
                explicit_project_path,
                tools: RwLock::new(tools),
                roots_epoch: AtomicU64::new(1),
                resolved_epoch: AtomicU64::new(0),
                roots_gate: tokio::sync::Mutex::new(()),
                last_target: Mutex::new(None),
                last_listed_tools: Mutex::new(None),
                log_subscription: Mutex::new(None),
                explore_session: Mutex::new(ExploreSessionState::default()),
                explore_gate: tokio::sync::Mutex::new(()),
            }),
        })
    }

    async fn ensure_project(&self, context: &RequestContext<RoleServer>) -> Result<(), McpError> {
        if self.has_project().await? {
            return Ok(());
        }
        let epoch = self.inner.roots_epoch.load(Ordering::Acquire);
        if self.inner.resolved_epoch.load(Ordering::Acquire) == epoch {
            let target = self
                .inner
                .last_target
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .or_else(|| self.inner.explicit_project_path.clone())
                .unwrap_or_else(process_cwd);
            self.retry_project(target).await?;
            return Ok(());
        }

        let _guard = self.inner.roots_gate.lock().await;
        if self.has_project().await? {
            return Ok(());
        }
        if self.inner.resolved_epoch.load(Ordering::Acquire) == epoch {
            return Ok(());
        }

        let target = match &self.inner.explicit_project_path {
            Some(path) => path.clone(),
            None if context
                .client_capabilities()
                .is_some_and(|capabilities| capabilities.roots.is_some()) =>
            {
                self.root_from_client(context).await
            }
            None => process_cwd(),
        };
        *self
            .inner
            .last_target
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(target.clone());
        self.retry_project(target).await?;
        self.inner.resolved_epoch.store(epoch, Ordering::Release);
        Ok(())
    }

    async fn has_project(&self) -> Result<bool, McpError> {
        let engine = self.inner.engine.clone();
        run_blocking(move || engine.has_default_code_graph()).await
    }

    async fn retry_project(&self, target: String) -> Result<(), McpError> {
        let engine = self.inner.engine.clone();
        run_blocking(move || engine.retry_initialize_sync(&target)).await
    }

    async fn root_from_client(&self, context: &RequestContext<RoleServer>) -> String {
        let roots = tokio::time::timeout(ROOTS_LIST_TIMEOUT, context.peer.list_roots()).await;
        if let Ok(Ok(result)) = roots {
            if let Some(root) = result.roots.first() {
                if let Some(path) = file_uri_to_path(&root.uri) {
                    return path.to_string_lossy().to_string();
                }
            }
        }
        process_cwd()
    }

    async fn refresh_tools(&self) -> Result<(Vec<Tool>, bool), McpError> {
        let engine = self.inner.engine.clone();
        let definitions = run_blocking(move || engine.get_tools()).await?;
        let signature = serde_json::to_string(&definitions)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        let tools = convert_tools(definitions)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        *self
            .inner
            .tools
            .write()
            .unwrap_or_else(|error| error.into_inner()) = tools.clone();
        let mut listed = self
            .inner
            .last_listed_tools
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let changed = listed
            .as_ref()
            .is_some_and(|previous| previous != &signature);
        *listed = Some(signature);
        Ok((tools, changed))
    }

    fn ensure_log_subscription(&self, peer: &rmcp::service::Peer<RoleServer>) {
        let mut slot = self
            .inner
            .log_subscription
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if slot.is_some() {
            return;
        }
        let peer = peer.clone();
        let runtime = tokio::runtime::Handle::current();
        let subscription = LogSubscription::with_emitter(move |level, message| {
            let notification = LoggingMessageNotificationParam::new(
                rmcp_logging_level(level),
                Value::String(message.to_string()),
            )
            .with_logger("codegraph");
            let peer = peer.clone();
            runtime.spawn(async move {
                let _ = peer.notify_logging_message(notification).await;
            });
            true
        });
        self.inner
            .engine
            .register_log_subscriber(Arc::clone(&subscription));
        *slot = Some(subscription);
    }
}

fn process_cwd() -> String {
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("/"))
        .to_string_lossy()
        .to_string()
}
