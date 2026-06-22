//! Glob pattern conversion for codegraph_files.

use std::sync::LazyLock;

use regex::Regex;

use crate::error::{CodeGraphError, Result};

pub(in crate::mcp::tools) fn glob_to_regex(pattern: &str) -> Result<Regex> {
    // Escape special regex chars except * and ?
    static ESCAPE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.+^${}()|\[\]\\]").unwrap());
    let escaped = ESCAPE_RE.replace_all(pattern, r"\$0").to_string();
    let escaped = escaped.replace("**", "{{GLOBSTAR}}");
    let escaped = escaped.replace('*', "[^/]*");
    let escaped = escaped.replace('?', "[^/]");
    let escaped = escaped.replace("{{GLOBSTAR}}", ".*");
    Regex::new(&escaped).map_err(|e| CodeGraphError::other(e.to_string()))
}
