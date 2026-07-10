//! MCP (Model Context Protocol) server module — port of `src/mcp/`.
//!
//! `server.rs` is the port of TS `src/mcp/index.ts` (the `MCPServer` runtime
//! mode picker); the re-exports below mirror that file's `export` block.

pub mod daemon;
pub mod daemon_paths;
pub mod daemon_registry;
pub mod engine;
pub mod proxy;
pub mod server;
pub mod server_instructions;
pub mod session;
pub mod tools;
pub mod transport;
pub mod version;

// Surface a few daemon-mode bits for tests + diagnostics.
#[cfg(unix)]
pub use daemon::Daemon;
pub use server::MCPServer;
pub use tools::{ToolHandler, tools};
// Export for use in CLI (mirrors TS `export { StdioTransport } from './transport'`).
pub use transport::StdioTransport;
pub use version::CODEGRAPH_PACKAGE_VERSION;
