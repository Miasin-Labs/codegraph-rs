//! The short, marker-fenced CodeGraph guidance written to agent
//! instruction files.
//!
//! The MCP `initialize` response remains the detailed source of truth for
//! the main agent. This smaller block exists for Task-tool subagents and
//! non-MCP harnesses, which receive the instructions file but not the MCP
//! server instructions. Keep it intentionally compact.

/// Markers used by the marker-based section write/removal.
pub const CODEGRAPH_SECTION_START: &str = "<!-- CODEGRAPH_START -->";
pub const CODEGRAPH_SECTION_END: &str = "<!-- CODEGRAPH_END -->";

/// The complete block written to CLAUDE.md (and reusable by other agents).
///
/// The wording is conditional because a global install applies this file to
/// projects that may not have a `.codegraph/` index.
pub const CODEGRAPH_INSTRUCTIONS_BLOCK: &str = r#"<!-- CODEGRAPH_START -->
## CodeGraph

In repositories indexed by CodeGraph (a `.codegraph/` directory exists at the repo root), reach for it BEFORE grep/find or reading files when you need to understand or locate code:

- **MCP tool** (when available): `codegraph_explore` answers most code questions in one call — the relevant symbols' verbatim source plus the call paths between them, including dynamic-dispatch hops grep can't follow. Name a file or symbol in the query to read its current line-numbered source. If it's listed but deferred, load it by name via tool search.
- **Shell** (always works): `codegraph explore "<symbol names or question>"` prints the same output.

If there is no `.codegraph/` directory, skip CodeGraph entirely — indexing is the user's decision.
<!-- CODEGRAPH_END -->"#;
