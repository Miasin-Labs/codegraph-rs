//! MCP handler state, project context, and dispatch.

mod dispatch;
pub(in crate::mcp::tools) mod notices;
mod project;
mod state;
mod tools;
mod validation;

pub use state::{CallContext, ProgressEmitter, ToolHandler};
