//! `codegraph_node` file view: one page of an indexed file, or its outline.
//!
//! A page is bounded three ways: the caller's `limit` (default 240 lines), a
//! 14K-character window, and the MCP output budget left after the rest of
//! the reply. A window already sent in this session (same lines, file
//! unchanged on disk) comes back as `alreadySent` instead of the source.

use std::fs;

use super::super::context::ToolHandler;
use super::super::format::{json_len, mcp_output_budget};
use super::super::output::{NodeFileOutput, SymbolRow};
use super::super::schema::ToolResult;
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::extraction::is_value_sensitive_language;
use crate::mcp::explore_session::{ProjectState, range_already_sent};
use crate::types::{Node, NodeKind};
use crate::utils::resolve_existing_path_within_root_real;

const DEFAULT_LIMIT: usize = 240;
const CHAR_BUDGET: usize = 14_000;
const OUTLINE_CAP: usize = 200;
const DEPENDENTS_SHOWN: usize = 8;
const ENCLOSING_CAP: usize = 4;
/// Payload keys a source page adds besides the source itself
/// (`startLine`, `endLine`, `source`, `truncated`, `requestedOffset`).
const PAGE_OVERHEAD: usize = 112;

/// What the caller asked of the file view.
pub(super) struct FileViewRequest<'a> {
    pub file: &'a str,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub symbols_only: bool,
    pub prior: Option<&'a ProjectState>,
}

/// The lines of a page that fit, and why the page ends where it does.
struct Window {
    start_line: usize,
    end_line: usize,
    budget_cut: bool,
}

impl ToolHandler {
    pub(super) fn handle_file_view(
        &self,
        cg: &CodeGraph,
        request: FileViewRequest<'_>,
    ) -> Result<ToolResult> {
        let file_arg = request.file;
        let resolved = match resolve_indexed_file(cg, file_arg)? {
            Resolution::Found(file) => file,
            Resolution::Ambiguous(candidates) => {
                let mut out = vec![
                    format!(
                        "\"{file_arg}\" matches {} indexed files - pass a longer path:",
                        candidates.len()
                    ),
                    String::new(),
                ];
                out.extend(candidates.iter().take(25).map(|path| format!("- {path}")));
                return Ok(self.validation_error_result(
                    "file",
                    &out.join("\n"),
                    "an unambiguous indexed file path",
                    Some("ambiguous string"),
                ));
            }
            Resolution::Missing => {
                return Ok(self.validation_error_result(
                    "file",
                    &format!("No indexed file matches \"{file_arg}\"."),
                    "an indexed file path",
                    Some("unknown string"),
                ));
            }
            Resolution::NoIndex => {
                return Ok(self.error_result("No files indexed. Run `codegraph index` first."));
            }
        };

        let file_path = resolved.path.as_str();
        let mut nodes = cg.get_nodes_in_file(file_path)?;
        nodes.retain(|node| {
            !matches!(
                node.kind,
                NodeKind::File | NodeKind::Import | NodeKind::Export
            )
        });
        nodes.sort_by_key(|node| node.start_line);
        let dependents = cg.get_file_dependents(file_path)?;
        let dep_summary = dependents_summary(&dependents);
        let budget = mcp_output_budget();
        let mut payload = NodeFileOutput::new(file_path.to_string(), nodes.len());
        let attach_dependents = |payload: &mut NodeFileOutput| {
            payload.dependents = dependents.iter().take(DEPENDENTS_SHOWN).cloned().collect();
            payload.dependents_omitted = dependents.len().saturating_sub(DEPENDENTS_SHOWN);
        };

        if request.symbols_only {
            attach_dependents(&mut payload);
            attach_outline(&mut payload, &nodes, budget);
            let mut out = vec![
                format!(
                    "**{file_path}** - {} symbol{}, {dep_summary}",
                    nodes.len(),
                    plural(nodes.len())
                ),
                String::new(),
            ];
            if nodes.is_empty() {
                out.push("_No indexed symbols in this file._".to_string());
            } else {
                out.extend(symbol_map("**Symbols**", &nodes));
            }
            out.push(String::new());
            out.push(
                "> Drop `symbolsOnly` (or pass `offset`/`limit`) to read the source, like Read."
                    .to_string(),
            );
            return self.structured_result(&self.truncate_output(&out.join("\n")), &payload);
        }

        if is_value_sensitive_language(resolved.language) {
            attach_dependents(&mut payload);
            attach_outline(&mut payload, &nodes, budget);
            payload.values_withheld = true;
            let mut out = vec![
                format!("**{file_path}** - configuration/data file, {dep_summary}"),
                String::new(),
            ];
            if !nodes.is_empty() {
                out.extend(symbol_map("**Keys (values withheld for safety)**", &nodes));
            }
            out.push(String::new());
            out.push(
                "> Values may be secrets, so codegraph indexes keys only. Read the file directly if you need a value."
                    .to_string(),
            );
            return self.structured_result(&self.truncate_output(&out.join("\n")), &payload);
        }

        let Some(real_path) =
            resolve_existing_path_within_root_real(cg.get_project_root(), file_path)
        else {
            return Ok(self.error_result(&format!(
                "Indexed file `{file_path}` is missing or no longer resolves safely within the project root."
            )));
        };
        let content = match fs::read_to_string(&real_path) {
            Ok(content) => content,
            Err(error) => {
                return Ok(self.error_result(&format!(
                    "Could not read indexed file `{file_path}`: {error}"
                )));
            }
        };

        let file_lines = content.split('\n').collect::<Vec<_>>();
        let total = file_lines.len();
        let max_lines = request.limit.unwrap_or(DEFAULT_LIMIT).max(1);
        let requested = request.offset.unwrap_or(1).max(1);
        // A file that shrank since the caller last saw it: show its end
        // rather than fail the call.
        let offset = if requested > total {
            payload.requested_offset = Some(requested);
            total.saturating_sub(max_lines - 1).max(1)
        } else {
            requested
        };
        payload.total_lines = Some(total);
        if offset == 1 {
            attach_dependents(&mut payload);
        }
        payload.enclosing = nodes
            .iter()
            .filter(|node| {
                (node.start_line as usize) < offset && (node.end_line as usize) >= offset
            })
            .take(ENCLOSING_CAP)
            .map(SymbolRow::in_file)
            .collect();

        let source_budget = budget
            .saturating_sub(json_len(&payload) + PAGE_OVERHEAD)
            .min(CHAR_BUDGET);
        let window = fit_window(&file_lines, offset, max_lines, source_budget);
        payload.truncated = window.budget_cut;
        let header = format!(
            "**{file_path}** - {total} lines, {} symbol{} | {dep_summary}",
            nodes.len(),
            plural(nodes.len())
        );
        let clamp_note = payload.requested_offset.map(|requested| {
            format!(
                "> offset {requested} is past the end of {file_path} ({total} line{}); showing its last lines instead.",
                plural(total)
            )
        });

        let Some(window) = window.filter_nonempty() else {
            let mut out = vec![header];
            out.extend(clamp_note);
            out.push(format!(
                "> Line {offset} alone is longer than this reply's output budget; read the file directly for it."
            ));
            return self.structured_result(&out.join("\n\n"), &payload);
        };
        payload.start_line = Some(window.start_line);
        payload.end_line = Some(window.end_line);
        let range = format!("{}-{}", window.start_line, window.end_line);

        // Already sent, byte-identical, in this session: send the ledger entry
        // instead of the source. Symbols and dependents still ride along, so the
        // reply stays useful without repeating the file.
        if request.prior.is_some_and(|prior| {
            range_already_sent(
                prior,
                cg.get_project_root(),
                file_path,
                window.start_line,
                window.end_line,
            )
        }) {
            payload.already_sent = true;
            let mut out = vec![format!(
                "**{file_path}** - lines {range} of {total} were already sent \
                 earlier in this conversation and the file is unchanged on disk; the source \
                 is not repeated.\n\nPass a different `offset`/`limit` for unseen lines, or \
                 `codegraph_node <symbol>` for one symbol in full.",
            )];
            out.extend(clamp_note);
            return self.structured_result(&out.join("\n\n"), &payload);
        }

        let lines = &file_lines[window.start_line - 1..window.end_line];
        payload.source = Some(lines.join("\n"));
        let mut out = vec![header];
        out.extend(clamp_note);
        out.push(String::new());
        out.extend(
            lines
                .iter()
                .enumerate()
                .map(|(index, line)| format!("{}\t{line}", window.start_line + index)),
        );
        let complete = window.start_line == 1 && window.end_line >= total;
        if !complete {
            out.push(String::new());
            out.push(format!(
                "(lines {range} of {total} - pass `offset`/`limit` for another range, or `codegraph_node <symbol>` for one symbol in full)"
            ));
        }
        self.structured_result(&out.join("\n"), &payload)
    }
}

impl Window {
    fn filter_nonempty(self) -> Option<Self> {
        (self.end_line >= self.start_line).then_some(self)
    }
}

/// The longest run of whole lines from `offset` that stays within
/// `max_lines` and `char_budget` (measured as the JSON-escaped source).
fn fit_window(lines: &[&str], offset: usize, max_lines: usize, char_budget: usize) -> Window {
    let mut used = 0usize;
    let mut end_line = offset - 1;
    let mut budget_cut = false;
    while end_line < lines.len() && end_line + 1 - offset < max_lines {
        // Escaped length without the quotes, plus the `\n` joining lines.
        let cost = json_len(lines[end_line]).saturating_sub(2) + 2;
        if used + cost > char_budget {
            budget_cut = true;
            break;
        }
        used += cost;
        end_line += 1;
    }
    Window {
        start_line: offset,
        end_line,
        budget_cut,
    }
}

fn attach_outline(payload: &mut NodeFileOutput, nodes: &[Node], budget: usize) {
    payload.symbols = nodes
        .iter()
        .take(OUTLINE_CAP)
        .map(SymbolRow::in_file)
        .collect();
    payload.symbols_truncated = nodes.len() > OUTLINE_CAP;
    payload.fit_symbols_to(budget);
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn dependents_summary(dependents: &[String]) -> String {
    if dependents.is_empty() {
        return "no other indexed file depends on it".to_string();
    }
    let omitted = dependents.len().saturating_sub(DEPENDENTS_SHOWN);
    format!(
        "used by {} file{}: {}{}",
        dependents.len(),
        plural(dependents.len()),
        dependents
            .iter()
            .take(DEPENDENTS_SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        if omitted == 0 {
            String::new()
        } else {
            format!(", +{omitted} more")
        }
    )
}

fn symbol_map(heading: &str, nodes: &[Node]) -> Vec<String> {
    let mut lines = vec![heading.to_string()];
    for node in nodes.iter().take(OUTLINE_CAP) {
        let signature = node
            .signature
            .as_deref()
            .map(|value| {
                format!(
                    " {}",
                    value.split_whitespace().collect::<Vec<_>>().join(" ")
                )
            })
            .unwrap_or_default();
        lines.push(format!(
            "- `{}` ({}){} - :{}",
            node.name,
            node.kind.as_str(),
            signature,
            node.start_line
        ));
    }
    if nodes.len() > OUTLINE_CAP {
        lines.push(format!("- ... +{} more", nodes.len() - OUTLINE_CAP));
    }
    lines
}

enum Resolution {
    Found(crate::types::FileRecord),
    Ambiguous(Vec<String>),
    Missing,
    NoIndex,
}

/// Match `file_arg` against the indexed paths: exact, then a unique path
/// suffix, then a unique substring (case-insensitive).
fn resolve_indexed_file(cg: &CodeGraph, file_arg: &str) -> Result<Resolution> {
    fn normalize(path: &str) -> String {
        path.replace('\\', "/")
            .trim_start_matches("./")
            .trim_matches('/')
            .to_lowercase()
    }

    let wanted = normalize(file_arg);
    let all_files = cg.get_files()?;
    if all_files.is_empty() {
        return Ok(Resolution::NoIndex);
    }
    if let Some(file) = all_files
        .iter()
        .find(|file| file.path.to_lowercase() == wanted)
    {
        return Ok(Resolution::Found(file.clone()));
    }
    let suffix = format!("/{wanted}");
    let mut candidates: Vec<&crate::types::FileRecord> = all_files
        .iter()
        .filter(|file| file.path.to_lowercase().ends_with(&suffix))
        .collect();
    if candidates.is_empty() {
        candidates = all_files
            .iter()
            .filter(|file| file.path.to_lowercase().contains(&wanted))
            .collect();
    }
    Ok(match candidates.as_slice() {
        [] => Resolution::Missing,
        [only] => Resolution::Found((*only).clone()),
        many => Resolution::Ambiguous(many.iter().map(|file| file.path.clone()).collect()),
    })
}
