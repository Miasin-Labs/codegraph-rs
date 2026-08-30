//! MCP (Model Context Protocol) server module — port of `src/mcp/`.
//!
//! `server.rs` is the port of TS `src/mcp/index.ts` (the `MCPServer` runtime
//! mode picker); the re-exports below mirror that file's `export` block.

pub mod daemon;
pub mod daemon_paths;
pub mod daemon_registry;
pub mod engine;
pub(crate) mod explore_session;
pub mod proxy;
pub mod server;
pub mod server_instructions;
pub mod service;
pub mod startup;
pub mod tools;
pub mod version;

// Surface a few daemon-mode bits for tests + diagnostics.
#[cfg(unix)]
pub use daemon::Daemon;
pub use server::MCPServer;
pub use tools::{ToolHandler, tools};
pub use version::CODEGRAPH_PACKAGE_VERSION;
