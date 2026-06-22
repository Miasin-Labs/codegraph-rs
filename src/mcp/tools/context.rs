//! MCP handler state, project context, and dispatch.

mod dispatch;
mod notices;
mod project;
mod state;
mod tools;
mod validation;

pub use state::{CallContext, ProgressEmitter, ToolHandler};
