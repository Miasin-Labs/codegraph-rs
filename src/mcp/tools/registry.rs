//! MCP tool registry and JSON schemas.

mod admin;
mod analysis;
mod catalog;
mod explore;
mod filters;
mod lookup;
mod navigation;
mod schema_builder;

pub use catalog::tools;
pub use filters::get_static_tools;
pub(in crate::mcp::tools) use filters::{short_tool_name, tool_allowlist};
