//! `codegraph_grep` — text search over the indexed files.
//!
//! The searches agents run with grep/rg that are not symbol lookups (log and
//! error messages, string literals, partial names, regexes, markers) answered
//! from the files the index holds, read fresh from disk: grouped by file,
//! ranked, each hit placed on its enclosing symbol, bounded by the output
//! budget, a wall-clock deadline, and a byte budget, and paged by a cursor.
//! Hits the session already received are listed by line, not re-sent.

mod cursor;
mod handler;
mod ledger;
mod matcher;
mod open;
mod output;
mod page;
mod rank;
mod render;
mod scan;
mod scope;
mod symbols;

pub(in crate::mcp::tools) use output::grep_output_schema;
