//! Hermes Agent target.
//!
//! Hermes reads MCP servers from `$HERMES_HOME/config.yaml` under the
//! top-level `mcp_servers` key, and exposes discovered MCP tools through
//! dynamic toolsets named `mcp-<server>`. We add:
//!
//! ```yaml
//! mcp_servers.codegraph -> `codegraph serve --mcp`
//! platform_toolsets.cli -> `mcp-codegraph`
//! ```
//!
//! The second entry matters because Hermes CLI profiles often enable an
//! explicit `platform_toolsets.cli` list. Without `mcp-codegraph` in that
//! list, the MCP server can be configured and connected but its tools may
//! still be filtered out of normal CLI sessions.

use std::path::{Path, PathBuf};
use std::{env, fs};

use regex::Regex;

use super::shared::{atomic_write_file_sync, cwd, home_dir};
use super::types::{
    AgentTarget,
    DetectionResult,
    FileAction,
    FileWrite,
    InstallOptions,
    Location,
    TargetId,
    WriteResult,
};

#[derive(Debug, Clone, Copy)]
struct LineRange {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
struct ListChildBlock {
    start: usize,
    end: usize,
    item_indent: String,
}

pub struct HermesTarget;

impl AgentTarget for HermesTarget {
    fn id(&self) -> TargetId {
        TargetId::Hermes
    }

    fn display_name(&self) -> &'static str {
        "Hermes Agent"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some("https://hermes-agent.nousresearch.com")
    }

    fn supports_location(&self, loc: Location) -> bool {
        loc == Location::Global
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        if loc != Location::Global {
            return DetectionResult {
                installed: false,
                already_configured: false,
                config_path: None,
            };
        }
        let file = config_path();
        let content = read_text(&file);
        let installed = hermes_home().exists() || file.exists();
        DetectionResult {
            installed,
            already_configured: has_codegraph_mcp_server(&content),
            config_path: Some(file),
        }
    }

    fn install(&self, loc: Location, _opts: &InstallOptions) -> WriteResult {
        if loc != Location::Global {
            return WriteResult {
                files: Vec::new(),
                notes: vec![
                    "Hermes Agent uses $HERMES_HOME/config.yaml; re-run with --location=global."
                        .to_string(),
                ],
            };
        }
        WriteResult {
            files: vec![write_hermes_config()],
            notes: vec!["Start a new Hermes session for MCP changes to take effect.".to_string()],
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        if loc != Location::Global {
            return WriteResult::default();
        }
        let file = config_path();
        if !file.exists() {
            return WriteResult {
                files: vec![FileWrite {
                    path: file,
                    action: FileAction::NotFound,
                }],
                notes: Vec::new(),
            };
        }

        let before = read_text(&file);
        let after = remove_codegraph_toolset(&remove_codegraph_mcp_server(&before));
        if after == before {
            return WriteResult {
                files: vec![FileWrite {
                    path: file,
                    action: FileAction::NotFound,
                }],
                notes: Vec::new(),
            };
        }
        let _ = atomic_write_file_sync(&file, &ensure_trailing_newline(&after));
        WriteResult {
            files: vec![FileWrite {
                path: file,
                action: FileAction::Removed,
            }],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        if loc != Location::Global {
            return "# Hermes Agent uses $HERMES_HOME/config.yaml; use --location=global.\n"
                .to_string();
        }
        [
            format!("# Add to {}", config_path().display()),
            "".to_string(),
            render_codegraph_mcp_block().join("\n"),
            "".to_string(),
            "platform_toolsets:".to_string(),
            "  cli:".to_string(),
            "    - hermes-cli".to_string(),
            "    - mcp-codegraph".to_string(),
            "".to_string(),
        ]
        .join("\n")
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        if loc == Location::Global {
            vec![config_path()]
        } else {
            Vec::new()
        }
    }
}

fn hermes_home() -> PathBuf {
    match env::var("HERMES_HOME") {
        Ok(v) if !v.is_empty() => {
            // `path.resolve` parity: absolutize a relative HERMES_HOME
            // against the current working directory.
            let p = PathBuf::from(&v);
            if p.is_absolute() { p } else { cwd().join(p) }
        }
        _ => home_dir().join(".hermes"),
    }
}

fn config_path() -> PathBuf {
    hermes_home().join("config.yaml")
}

fn read_text(file: &Path) -> String {
    fs::read_to_string(file).unwrap_or_default()
}

fn write_hermes_config() -> FileWrite {
    let file = config_path();
    let existed = file.exists();
    let before = read_text(&file);
    let after_mcp = upsert_codegraph_mcp_server(&before);
    let after = upsert_codegraph_toolset(&after_mcp);

    if after == before {
        return FileWrite {
            path: file,
            action: FileAction::Unchanged,
        };
    }
    let _ = atomic_write_file_sync(&file, &ensure_trailing_newline(&after));
    FileWrite {
        path: file,
        action: if existed {
            FileAction::Updated
        } else {
            FileAction::Created
        },
    }
}

fn ensure_trailing_newline(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

fn split_lines(content: &str) -> Vec<String> {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|s| s.to_string())
        .collect()
}

fn join_lines(mut lines: Vec<String>) -> String {
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    format!("{}\n", lines.join("\n"))
}

fn top_level_range(lines: &[String], key: &str) -> Option<LineRange> {
    let needle = format!("{key}:");
    let start = lines.iter().position(|line| line.trim() == needle)?;
    let top_key_re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_-]*:\s*(?:#.*)?$").unwrap();
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if top_key_re.is_match(line) {
            end = i;
            break;
        }
    }
    Some(LineRange { start, end })
}

// Index loops mirror the TS line-scanner's bounded `for (let i = ...)` scans;
// the iterator+skip/take rewrite obscures the parent.start/parent.end window.
#[allow(clippy::needless_range_loop)]
fn child_range(lines: &[String], parent: LineRange, child: &str) -> Option<LineRange> {
    let start_pattern = Regex::new(&format!(r"^  {}:\s*(?:#.*)?$", regex::escape(child))).unwrap();
    let mut start = None;
    for i in (parent.start + 1)..parent.end {
        if start_pattern.is_match(&lines[i]) {
            start = Some(i);
            break;
        }
    }
    let start = start?;

    let sibling_re = Regex::new(r"^  \S").unwrap();
    let mut end = parent.end;
    for i in (start + 1)..parent.end {
        let line = &lines[i];
        if line.trim().is_empty() {
            continue;
        }
        if sibling_re.is_match(line) {
            end = i;
            break;
        }
    }
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    Some(LineRange { start, end })
}

/// Block-range for a 2-space-indented child whose value is a YAML block list.
///
/// Unlike `child_range`, this handles PyYAML's default `default_flow_style=False`
/// serialization, where list items sit at the SAME indent as the parent key:
///
/// ```yaml
///     cli:
///     - hermes-cli       # indent 2 — belongs to cli, not a sibling
///     - browser
/// ```
///
/// `child_range`'s `^  \S` heuristic mistakes that first `  - hermes-cli` line
/// for the next sibling key and truncates the block, causing inserts to land
/// before the existing items at a different indent (issue #456). This helper
/// recognizes a `  - ` line as part of the block instead, and reports back
/// the actual indent used by existing items so the inserter matches it.
// Index loops mirror the TS line-scanner (see `child_range` above).
#[allow(clippy::needless_range_loop)]
fn list_child_block(lines: &[String], parent: LineRange, child: &str) -> Option<ListChildBlock> {
    let start_pattern = Regex::new(&format!(r"^  {}:\s*(?:#.*)?$", regex::escape(child))).unwrap();
    let mut start = None;
    for i in (parent.start + 1)..parent.end {
        if start_pattern.is_match(&lines[i]) {
            start = Some(i);
            break;
        }
    }
    let start = start?;

    let two_space_item_re = Regex::new(r"^  - ").unwrap();
    let mut end = parent.end;
    for i in (start + 1)..parent.end {
        let line = &lines[i];
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent >= 4 {
            continue;
        }
        if indent == 2 && two_space_item_re.is_match(line) {
            continue;
        }
        end = i;
        break;
    }
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }

    let item_re = Regex::new(r"^( +)- ").unwrap();
    let mut item_indent = "    ".to_string();
    for line in lines.iter().take(end).skip(start + 1) {
        if let Some(caps) = item_re.captures(line) {
            item_indent = caps[1].to_string();
            break;
        }
    }
    Some(ListChildBlock {
        start,
        end,
        item_indent,
    })
}

fn render_codegraph_mcp_child() -> Vec<String> {
    [
        "  codegraph:",
        "    command: codegraph",
        "    args:",
        "      - serve",
        "      - --mcp",
        "    timeout: 120",
        "    connect_timeout: 60",
        "    enabled: true",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn render_codegraph_mcp_block() -> Vec<String> {
    let mut block = vec!["mcp_servers:".to_string()];
    block.extend(render_codegraph_mcp_child());
    block
}

fn has_codegraph_mcp_server(content: &str) -> bool {
    let lines = split_lines(content);
    match top_level_range(&lines, "mcp_servers") {
        Some(parent) => child_range(&lines, parent, "codegraph").is_some(),
        None => false,
    }
}

fn upsert_codegraph_mcp_server(content: &str) -> String {
    let mut lines = split_lines(content);
    let parent = top_level_range(&lines, "mcp_servers");
    let child = parent.and_then(|p| child_range(&lines, p, "codegraph"));
    let replacement = render_codegraph_mcp_child();

    let parent = match parent {
        None => {
            if lines.last().map(|l| l.is_empty()).unwrap_or(false) {
                lines.pop();
            }
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.extend(render_codegraph_mcp_block());
            return join_lines(lines);
        }
        Some(p) => p,
    };

    if let Some(child) = child {
        let existing = &lines[child.start..child.end];
        if existing == replacement.as_slice() {
            return join_lines(lines);
        }
        lines.splice(child.start..child.end, replacement);
        return join_lines(lines);
    }

    lines.splice(parent.end..parent.end, replacement);
    join_lines(lines)
}

fn remove_codegraph_mcp_server(content: &str) -> String {
    let mut lines = split_lines(content);
    let parent = top_level_range(&lines, "mcp_servers");
    let child = parent.and_then(|p| child_range(&lines, p, "codegraph"));
    let child = match child {
        None => return content.to_string(),
        Some(c) => c,
    };
    lines.splice(child.start..child.end, std::iter::empty::<String>());
    join_lines(lines)
}

fn upsert_codegraph_toolset(content: &str) -> String {
    let mut lines = split_lines(content);
    let parent = top_level_range(&lines, "platform_toolsets");
    let cli = parent.and_then(|p| list_child_block(&lines, p, "cli"));

    let parent = match parent {
        None => {
            if lines.last().map(|l| l.is_empty()).unwrap_or(false) {
                lines.pop();
            }
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.extend(
                [
                    "platform_toolsets:",
                    "  cli:",
                    "    - hermes-cli",
                    "    - mcp-codegraph",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
            return join_lines(lines);
        }
        Some(p) => p,
    };

    let cli = match cli {
        None => {
            lines.splice(
                parent.end..parent.end,
                ["  cli:", "    - hermes-cli", "    - mcp-codegraph"]
                    .iter()
                    .map(|s| s.to_string()),
            );
            return join_lines(lines);
        }
        Some(c) => c,
    };

    let has_entry = lines[(cli.start + 1)..cli.end]
        .iter()
        .any(|line| line.trim() == "- mcp-codegraph");
    if has_entry {
        return join_lines(lines);
    }

    lines.insert(cli.end, format!("{}- mcp-codegraph", cli.item_indent));
    join_lines(lines)
}

fn remove_codegraph_toolset(content: &str) -> String {
    let lines = split_lines(content);
    let parent = top_level_range(&lines, "platform_toolsets");
    let cli = parent.and_then(|p| list_child_block(&lines, p, "cli"));
    let cli = match cli {
        None => return content.to_string(),
        Some(c) => c,
    };

    let has_entry = lines[(cli.start + 1)..cli.end]
        .iter()
        .any(|line| line.trim() == "- mcp-codegraph");
    if !has_entry {
        return content.to_string();
    }

    let next: Vec<String> = lines
        .into_iter()
        .enumerate()
        .filter(|(idx, line)| {
            if *idx <= cli.start || *idx >= cli.end {
                return true;
            }
            line.trim() != "- mcp-codegraph"
        })
        .map(|(_, line)| line)
        .collect();
    join_lines(next)
}

pub static HERMES_TARGET: HermesTarget = HermesTarget;
