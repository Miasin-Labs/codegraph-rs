//! Administrative MCP tools for status and indexed file listings.

mod files;
mod glob;
mod status;
mod tree;

pub(in crate::mcp::tools) use glob::glob_to_regex;
