use crate::support::*;

// ===========================================================================
// Installer targets — TOML serializer (Codex backbone)
// (Also covered as unit tests in src/installer/targets/toml.rs.)
// ===========================================================================

#[test]
fn toml_builds_codegraph_block() {
    let block = build_toml_table(
        "mcp_servers.codegraph",
        &[
            ("command", TomlValue::String("codegraph".to_string())),
            (
                "args",
                TomlValue::Array(vec!["serve".to_string(), "--mcp".to_string()]),
            ),
        ],
    );
    assert!(block.contains("[mcp_servers.codegraph]"));
    assert!(block.contains("command = \"codegraph\""));
    assert!(block.contains("args = [\"serve\", \"--mcp\"]"));
}

#[test]
fn toml_upsert_inserts_into_empty_content() {
    let block = build_toml_table(
        "mcp_servers.codegraph",
        &[
            ("command", TomlValue::String("codegraph".to_string())),
            ("args", TomlValue::Array(vec!["serve".to_string()])),
        ],
    );
    let (content, action) = upsert_toml_table("", "mcp_servers.codegraph", &block);
    assert_eq!(action, TomlUpsertAction::Inserted);
    assert!(content.starts_with("[mcp_servers.codegraph]"));
}

#[test]
fn toml_upsert_is_idempotent() {
    let block = build_toml_table(
        "mcp_servers.codegraph",
        &[
            ("command", TomlValue::String("codegraph".to_string())),
            ("args", TomlValue::Array(vec!["serve".to_string()])),
        ],
    );
    let (first, _) = upsert_toml_table("", "mcp_servers.codegraph", &block);
    let (second, action) = upsert_toml_table(&first, "mcp_servers.codegraph", &block);
    assert_eq!(action, TomlUpsertAction::Unchanged);
    assert_eq!(second, first);
}

#[test]
fn toml_upsert_replaces_existing_block_preserving_siblings() {
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
    let new_block = build_toml_table(
        "mcp_servers.codegraph",
        &[
            ("command", TomlValue::String("codegraph".to_string())),
            (
                "args",
                TomlValue::Array(vec!["serve".to_string(), "--mcp".to_string()]),
            ),
        ],
    );
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
fn toml_remove_strips_block_preserving_siblings() {
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
fn toml_remove_missing_table_returns_not_found() {
    let existing = "[other]\nfoo = \"bar\"\n";
    let (content, action) = remove_toml_table(existing, "mcp_servers.codegraph");
    assert_eq!(action, TomlRemoveAction::NotFound);
    assert_eq!(content, existing);
}

#[test]
fn toml_upsert_preserves_array_of_tables_sibling() {
    let existing = ["[[foo]]", "name = \"a\"", "", "[[foo]]", "name = \"b\"", ""].join("\n");
    let block = build_toml_table(
        "mcp_servers.codegraph",
        &[
            ("command", TomlValue::String("codegraph".to_string())),
            ("args", TomlValue::Array(vec!["serve".to_string()])),
        ],
    );
    let (content, _) = upsert_toml_table(&existing, "mcp_servers.codegraph", &block);
    assert_eq!(content.matches("[[foo]]").count(), 2);
    assert!(content.contains("[mcp_servers.codegraph]"));
}
