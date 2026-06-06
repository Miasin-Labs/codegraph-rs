//! Tiny TOML helpers — just enough to inject / replace / remove a
//! single dotted-key table block (`[mcp_servers.codegraph]`) inside an
//! existing `~/.codex/config.toml`. We deliberately do NOT try to be a
//! general TOML parser/serializer; that would mean pulling in a
//! dependency for ~6 lines of output.
//!
//! Strategy: treat the file as text. Find the `[mcp_servers.codegraph]`
//! header line, splice it (and the lines that follow it until the next
//! `[...]` header or EOF) in or out. Everything outside that block is
//! preserved verbatim, byte-for-byte.
//!
//! Limitations (acceptable for our narrow use):
//!   - Only handles top-level table headers; not array-of-tables or
//!     subtables nested inside `[mcp_servers]` itself (we always write
//!     the full dotted key `[mcp_servers.codegraph]`).
//!   - Doesn't validate sibling TOML — if the file is malformed
//!     elsewhere, our injection won't fix it but won't make it worse.
//!   - Quotes string values with double quotes; escapes `\` and `"`.

/// Value types supported by the narrow serializer (TS: `string | string[]`).
#[derive(Debug, Clone)]
pub enum TomlValue {
    String(String),
    Array(Vec<String>),
}

impl From<&str> for TomlValue {
    fn from(s: &str) -> Self {
        TomlValue::String(s.to_string())
    }
}

impl From<Vec<String>> for TomlValue {
    fn from(v: Vec<String>) -> Self {
        TomlValue::Array(v)
    }
}

impl From<Vec<&str>> for TomlValue {
    fn from(v: Vec<&str>) -> Self {
        TomlValue::Array(v.into_iter().map(|s| s.to_string()).collect())
    }
}

/// Serialize key/value pairs into the body lines of a TOML table.
/// Values supported: string, string[] — the codex MCP config only
/// needs these two. (Order of entries is preserved, mirroring the TS
/// `Object.entries` iteration of an object literal.)
pub fn serialize_toml_table_body(values: &[(&str, TomlValue)]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (key, value) in values {
        match value {
            TomlValue::String(s) => lines.push(format!("{key} = {}", quote_string(s))),
            TomlValue::Array(items) => {
                let parts = items
                    .iter()
                    .map(|v| quote_string(v))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!("{key} = [{parts}]"));
            }
        }
    }
    lines.join("\n")
}

fn quote_string(s: &str) -> String {
    // TOML basic strings: backslash and double-quote escapes; control
    // chars not expected in our payload (paths/args).
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Build a full table block: header line + body. Suitable for direct
/// insertion into a TOML file.
pub fn build_toml_table(header: &str, values: &[(&str, TomlValue)]) -> String {
    format!("[{header}]\n{}", serialize_toml_table_body(values))
}

/// Action returned by [`upsert_toml_table`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TomlUpsertAction {
    Inserted,
    Replaced,
    Unchanged,
}

/// Action returned by [`remove_toml_table`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TomlRemoveAction {
    Removed,
    NotFound,
}

/// Insert or replace a top-level dotted-key TOML table block in the
/// given file content. Preserves all other content verbatim.
///
/// Returns `Inserted` when the table was newly added, `Replaced`
/// when an existing one was rewritten, `Unchanged` when the
/// existing block already matches `block` byte-for-byte.
pub fn upsert_toml_table(
    file_content: &str,
    header: &str,
    block: &str,
) -> (String, TomlUpsertAction) {
    let header_line = format!("[{header}]");
    let header_idx = find_header_index(file_content, &header_line);

    let header_idx = match header_idx {
        None => {
            // Insert at end with separating blank line if there's existing content.
            let trimmed = file_content.trim_end();
            let sep = if !trimmed.is_empty() { "\n\n" } else { "" };
            return (
                format!("{trimmed}{sep}{block}\n"),
                TomlUpsertAction::Inserted,
            );
        }
        Some(i) => i,
    };

    // Find the end of this block: next `[...]` header (at line start) or EOF.
    let block_end = find_next_table_header(file_content, header_idx + header_line.len());
    let existing_block = file_content[header_idx..block_end].trim_end_matches('\n');

    if existing_block == block {
        return (file_content.to_string(), TomlUpsertAction::Unchanged);
    }

    let before = &file_content[..header_idx];
    let after = &file_content[block_end..];
    // Trim trailing blank lines from `before` (we'll re-add one) and
    // leading blank lines from `after` so the file shape stays clean.
    let before_clean = before.trim_end_matches('\n');
    let after_clean = after.trim_start_matches('\n');
    let sep_before = if !before_clean.is_empty() { "\n\n" } else { "" };
    let sep_after = if !after_clean.is_empty() {
        "\n\n"
    } else {
        "\n"
    };
    (
        format!("{before_clean}{sep_before}{block}{sep_after}{after_clean}"),
        TomlUpsertAction::Replaced,
    )
}

/// Remove a top-level dotted-key TOML table block. Returns the
/// possibly-empty new content + an action flag.
pub fn remove_toml_table(file_content: &str, header: &str) -> (String, TomlRemoveAction) {
    let header_line = format!("[{header}]");
    let header_idx = match find_header_index(file_content, &header_line) {
        None => return (file_content.to_string(), TomlRemoveAction::NotFound),
        Some(i) => i,
    };

    let block_end = find_next_table_header(file_content, header_idx + header_line.len());
    let before = file_content[..header_idx].trim_end_matches('\n');
    let after = file_content[block_end..].trim_start_matches('\n');
    let sep = if !before.is_empty() && !after.is_empty() {
        "\n\n"
    } else {
        ""
    };
    (format!("{before}{sep}{after}"), TomlRemoveAction::Removed)
}

/// Locate the byte index of a header line (`[foo.bar]`) when it
/// appears at the start of a line. Returns `None` if not found.
fn find_header_index(content: &str, header_line: &str) -> Option<usize> {
    // Search BOL or right after a newline.
    if content.starts_with(header_line) {
        return Some(0);
    }
    let needle = format!("\n{header_line}");
    content.find(&needle).map(|idx| idx + 1)
}

/// Find the byte index of the next top-level `[...]` table header
/// (excluding array-of-tables `[[...]]`) starting from `from`, or
/// return content length when none.
fn find_next_table_header(content: &str, from: usize) -> usize {
    // Look for "\n[" but skip "\n[[" (array of tables).
    let bytes = content.as_bytes();
    let mut i = from;
    while i < content.len() {
        let rel = match content[i..].find("\n[") {
            None => return content.len(),
            Some(r) => r,
        };
        let nl_idx = i + rel;
        if bytes.get(nl_idx + 2) == Some(&b'[') {
            // [[...]] — keep searching past it.
            i = nl_idx + 2;
            continue;
        }
        return nl_idx + 1;
    }
    content.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codegraph_block() -> String {
        build_toml_table(
            "mcp_servers.codegraph",
            &[
                ("command", "codegraph".into()),
                ("args", vec!["serve", "--mcp"].into()),
            ],
        )
    }

    #[test]
    fn builds_block_with_command_and_args() {
        let block = codegraph_block();
        assert!(block.contains("[mcp_servers.codegraph]"));
        assert!(block.contains("command = \"codegraph\""));
        assert!(block.contains("args = [\"serve\", \"--mcp\"]"));
    }

    #[test]
    fn upsert_inserts_into_empty_content() {
        let block = build_toml_table(
            "mcp_servers.codegraph",
            &[
                ("command", "codegraph".into()),
                ("args", vec!["serve"].into()),
            ],
        );
        let (content, action) = upsert_toml_table("", "mcp_servers.codegraph", &block);
        assert_eq!(action, TomlUpsertAction::Inserted);
        assert!(content.starts_with("[mcp_servers.codegraph]"));
    }

    #[test]
    fn upsert_is_idempotent() {
        let block = build_toml_table(
            "mcp_servers.codegraph",
            &[
                ("command", "codegraph".into()),
                ("args", vec!["serve"].into()),
            ],
        );
        let (first, _) = upsert_toml_table("", "mcp_servers.codegraph", &block);
        let (second, action) = upsert_toml_table(&first, "mcp_servers.codegraph", &block);
        assert_eq!(action, TomlUpsertAction::Unchanged);
        assert_eq!(second, first);
    }

    #[test]
    fn upsert_replaces_in_place_preserving_siblings() {
        let existing = [
            "[other_table]",
            "foo = \"bar\"",
            "",
            "[mcp_servers.codegraph]",
            "command = \"old-codegraph\"",
            "args = [\"old\"]",
            "",
            "[zzz]",
            "baz = \"qux\"",
            "",
        ]
        .join("\n");
        let new_block = codegraph_block();
        let (content, action) = upsert_toml_table(&existing, "mcp_servers.codegraph", &new_block);
        assert_eq!(action, TomlUpsertAction::Replaced);
        assert!(content.contains("[other_table]"));
        assert!(content.contains("foo = \"bar\""));
        assert!(content.contains("[zzz]"));
        assert!(content.contains("baz = \"qux\""));
        assert!(content.contains("command = \"codegraph\""));
        assert!(!content.contains("old-codegraph"));
    }

    #[test]
    fn remove_strips_block_preserving_siblings() {
        let existing = [
            "[other_table]",
            "foo = \"bar\"",
            "",
            "[mcp_servers.codegraph]",
            "command = \"codegraph\"",
            "args = [\"serve\"]",
        ]
        .join("\n");
        let (content, action) = remove_toml_table(&existing, "mcp_servers.codegraph");
        assert_eq!(action, TomlRemoveAction::Removed);
        assert!(content.contains("[other_table]"));
        assert!(content.contains("foo = \"bar\""));
        assert!(!content.contains("mcp_servers.codegraph"));
    }

    #[test]
    fn remove_missing_table_returns_not_found() {
        let existing = "[other]\nfoo = \"bar\"\n";
        let (content, action) = remove_toml_table(existing, "mcp_servers.codegraph");
        assert_eq!(action, TomlRemoveAction::NotFound);
        assert_eq!(content, existing);
    }

    #[test]
    fn upsert_preserves_array_of_tables_sibling() {
        let existing = ["[[foo]]", "name = \"a\"", "", "[[foo]]", "name = \"b\"", ""].join("\n");
        let block = build_toml_table(
            "mcp_servers.codegraph",
            &[
                ("command", "codegraph".into()),
                ("args", vec!["serve"].into()),
            ],
        );
        let (content, _) = upsert_toml_table(&existing, "mcp_servers.codegraph", &block);
        assert_eq!(content.matches("[[foo]]").count(), 2);
        assert!(content.contains("[mcp_servers.codegraph]"));
    }
}
