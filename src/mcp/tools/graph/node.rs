//! `codegraph_node` symbol mode: one definition (or every overload of a
//! name) with its source and caller/callee trail. File mode lives in
//! `node_file`.

use std::collections::HashSet;
use std::fs;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::context::notices::stale_slice_notice;
use super::super::format::{is_container_node_kind, mcp_output_budget, number_source_lines};
use super::super::output::{NodeDetailOutput, NodeOutput, SymbolRef, SymbolRow, fit_node_payload};
use super::super::schema::ToolResult;
use super::federated::graph_arg;
use super::node_file::FileViewRequest;
use super::node_foreign::attach_external;
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::extraction::is_value_sensitive_language;
use crate::federation::GraphSet;
use crate::mcp::explore_session::{ProjectState, SESSION_ARG, range_already_sent};
use crate::types::{Node, NodeRef};
use crate::utils::resolve_existing_path_within_root_real;

/// Callers/callees listed per definition; the rest are counted.
pub(super) const TRAIL_CAP: usize = 12;

/// One rendered definition: its text section, its structured detail, and the
/// file it came from when that file changed on disk since indexing.
struct RenderedDetail {
    text: String,
    detail: NodeDetailOutput,
    stale_file: Option<String>,
}

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_node(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        if let Some(names) = super::node_batch::batch_symbols(args) {
            return self.handle_node_batch(args, names);
        }
        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        // The session ledger the MCP service injects: what this conversation
        // already holds, so source it has seen is not sent again.
        let prior = args
            .get(SESSION_ARG)
            .and_then(|value| serde_json::from_value::<ProjectState>(value.clone()).ok());
        // Default to false to minimize context usage
        let include_code = args.get("includeCode") == Some(&Value::Bool(true));
        let file_hint: Option<String> = args
            .get("file")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let line_hint: Option<f64> = args
            .get("line")
            .and_then(|v| v.as_f64())
            .filter(|&l| l > 0.0);
        let positive = |key: &str| {
            args.get(key)
                .and_then(Value::as_f64)
                .filter(|value| *value > 0.0)
                .map(|value| value.floor() as usize)
        };
        let symbol_raw = args
            .get("symbol")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();

        if symbol_raw.is_empty() {
            if let Some(file) = file_hint.as_deref() {
                return self.handle_file_view(
                    &cg,
                    FileViewRequest {
                        file,
                        offset: positive("offset"),
                        limit: positive("limit"),
                        symbols_only: args.get("symbolsOnly") == Some(&Value::Bool(true)),
                        prior: prior.as_ref(),
                    },
                );
            }
        }

        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };

        let graph = graph_arg(args);
        let fed = self.federation();
        let mut matches = match graph {
            Some(_) => Vec::new(),
            None => self.find_symbol_matches(&cg, &symbol)?,
        };
        if matches.is_empty() {
            // A symbol of a dependency or a linked project, read from there.
            if let Some(fed) = &fed {
                let foreign = self.foreign_symbols(fed, &cg, &symbol, graph.as_deref());
                if !foreign.is_empty() {
                    return self.foreign_node_result(
                        fed,
                        &cg,
                        &foreign,
                        include_code,
                        prior.as_ref(),
                    );
                }
            }
            return self.node_result(
                &format!("Symbol not found: `{symbol}`"),
                NodeOutput::new(0, false, Vec::new()),
                &[],
            );
        }

        // Disambiguate a heavily-overloaded name to a specific definition the
        // caller pinned by file/line. Only narrows (never empties).
        if matches.len() > 1 && (file_hint.is_some() || line_hint.is_some()) {
            matches = narrow_matches(matches, file_hint.as_deref(), line_hint);
        }

        // Single definition — the common case.
        if matches.len() == 1 {
            let rendered = self.render_node_detail(
                &cg,
                &matches[0],
                include_code,
                prior.as_ref(),
                fed.as_ref(),
            )?;
            let stale: Vec<String> = rendered.stale_file.into_iter().collect();
            return self.node_result(
                &self.truncate_output(&rendered.text),
                NodeOutput::new(1, false, vec![rendered.detail]),
                &stale,
            );
        }

        // Multiple definitions share this name — return them ALL.
        let header = format!("{} definitions named `{}`", matches.len(), symbol);
        if !include_code {
            let mut out = vec![
                header,
                String::new(),
                "Re-query with `includeCode: true` to get bodies.".to_string(),
                String::new(),
            ];
            out.extend(matches.iter().map(definition_line));
            let details = matches
                .iter()
                .map(|node| NodeDetailOutput::bare(SymbolRow::new(node)))
                .collect();
            return self.node_result(
                &self.truncate_output(&out.join("\n")),
                NodeOutput::new(matches.len(), false, details),
                &[],
            );
        }

        // Render every definition in full up to a RELEVANCE cap (how many
        // overloads are plausibly useful); the output budget then decides how
        // much of their code fits.
        const HARD_CAP: usize = 16;
        const LIST_CAP: usize = 20;
        let mut sections: Vec<String> = Vec::new();
        let mut details: Vec<NodeDetailOutput> = Vec::new();
        let mut stale_files: Vec<String> = Vec::new();
        for node in matches.iter().take(HARD_CAP) {
            let rendered =
                self.render_node_detail(&cg, node, true, prior.as_ref(), fed.as_ref())?;
            sections.push(rendered.text);
            details.push(rendered.detail);
            stale_files.extend(rendered.stale_file);
        }
        let listed = &matches[details.len()..];

        let mut out: Vec<String> = vec![
            header,
            format!(
                "Returning {} in full{}.",
                sections.len(),
                if listed.is_empty() {
                    String::new()
                } else {
                    format!("; {} more listed below", listed.len())
                }
            ),
            String::new(),
            sections.join("\n\n---\n\n"),
        ];
        if !listed.is_empty() {
            out.push(String::new());
            out.push("Other definitions".to_string());
            out.extend(listed.iter().take(LIST_CAP).map(definition_line));
            if listed.len() > LIST_CAP {
                out.push(format!("- … +{} more", listed.len() - LIST_CAP));
            }
        }
        stale_files.sort();
        stale_files.dedup();
        self.node_result(
            &self.truncate_output(&out.join("\n")),
            NodeOutput::new(matches.len(), !listed.is_empty(), details),
            &stale_files,
        )
    }

    /// A symbol-mode result bounded to the MCP output budget.
    pub(super) fn node_result(
        &self,
        text: &str,
        output: NodeOutput,
        stale_files: &[String],
    ) -> Result<ToolResult> {
        let mut payload = serde_json::to_value(output)?;
        fit_node_payload(&mut payload, mcp_output_budget());
        let mut result = self.structured_result(text, &payload)?;
        if !stale_files.is_empty() {
            result = result.with_notice(stale_slice_notice(stale_files));
        }
        Ok(result)
    }

    fn render_node_detail(
        &self,
        cg: &CodeGraph,
        node: &Node,
        include_code: bool,
        prior: Option<&ProjectState>,
        fed: Option<&GraphSet>,
    ) -> Result<RenderedDetail> {
        let stale_source = current_source_if_stale(cg, &node.file_path);
        if let Some(source) = stale_source {
            return self.render_stale_node_detail(cg, node, include_code, source, prior, fed);
        }
        let mut code: Option<String> = None;
        let mut outline: Option<String> = None;
        if include_code {
            // For container symbols, return a structural outline instead.
            if is_container_node_kind(node.kind) {
                let o = self.build_container_outline(cg, node)?;
                if !o.is_empty() {
                    outline = Some(o);
                }
            }
            if outline.is_none() {
                code = cg.get_code(&node.id)?;
            }
        }
        let start = node.start_line.max(1) as usize;
        let already_sent = code
            .as_deref()
            .is_some_and(|code| was_sent(cg, prior, &node.file_path, start, code));
        if already_sent {
            code = None;
        }
        let (callers, callees) = self.trail_refs(cg, node)?;
        let mut text = self.format_node_details(node, code.as_deref(), outline.as_deref());
        if already_sent {
            text.push_str(&already_sent_note(&node.file_path));
        }
        text.push_str(&self.format_trail_refs(node, &callers, &callees));
        let mut detail = NodeDetailOutput::bare(SymbolRow::new(node));
        detail.code = code;
        detail.already_sent = already_sent;
        detail.outline = outline;
        attach_trail(&mut detail, &callers, &callees);
        if let Some(fed) = fed {
            text.push_str(&attach_external(fed, cg, node, &mut detail)?);
        }
        Ok(RenderedDetail {
            text,
            detail,
            stale_file: None,
        })
    }

    fn render_stale_node_detail(
        &self,
        cg: &CodeGraph,
        node: &Node,
        include_code: bool,
        source: String,
        prior: Option<&ProjectState>,
        fed: Option<&GraphSet>,
    ) -> Result<RenderedDetail> {
        const MAX_LINES: usize = 300;
        const MAX_CHARS: usize = 12_000;
        let source = source.trim_end_matches('\n');
        let embed = include_code
            && !is_value_sensitive_language(node.language)
            && source.len() <= MAX_CHARS
            && source.lines().count() <= MAX_LINES;
        let already_sent = embed && was_sent(cg, prior, &node.file_path, 1, source);
        let code = (embed && !already_sent).then(|| source.to_string());
        let (callers, callees) = self.trail_refs(cg, node)?;
        let mut lines = vec![
            format!("## {} ({})", node.name, node.kind.as_str()),
            String::new(),
            format!(
                "**Location:** {}:{} — ⚠ as of the last index sync; the file changed on disk after it was last indexed, so this line may be shifted",
                node.file_path, node.start_line
            ),
        ];
        if let Some(signature) = node.signature.as_deref() {
            lines.push(format!("**Signature:** `{signature}`"));
        }
        lines.push(String::new());
        if let Some(current) = code.as_deref() {
            lines.extend([
                format!(
                    "> ⚠ `{}` changed on disk after it was last indexed. Showing the file's full CURRENT source instead:",
                    node.file_path
                ),
                String::new(),
                format!("```{}", node.language.as_str()),
                number_source_lines(current, 1),
                "```".to_string(),
            ]);
        } else if already_sent {
            lines.push(already_sent_note(&node.file_path));
        } else {
            lines.push(format!(
                "> ⚠ `{}` changed on disk after it was last indexed — its body is omitted rather than risk showing a different symbol's code. Call codegraph_node with `file: \"{}\"` for current content.",
                node.file_path, node.file_path
            ));
        }
        lines.push(self.format_trail_refs(node, &callers, &callees));
        let mut detail = NodeDetailOutput::bare(SymbolRow::new(node));
        // The current file is shown whole, from line 1.
        detail.code_start_line = code.as_ref().filter(|_| node.start_line != 1).map(|_| 1);
        detail.code = code;
        detail.already_sent = already_sent;
        attach_trail(&mut detail, &callers, &callees);
        if let Some(fed) = fed {
            lines.push(attach_external(fed, cg, node, &mut detail)?);
        }
        Ok(RenderedDetail {
            text: lines.join("\n"),
            detail,
            stale_file: Some(node.file_path.clone()),
        })
    }

    fn trail_refs(&self, cg: &CodeGraph, node: &Node) -> Result<(Vec<NodeRef>, Vec<NodeRef>)> {
        let collect = |edges: Vec<NodeRef>| -> Vec<NodeRef> {
            let mut seen: HashSet<String> = HashSet::new();
            seen.insert(node.id.clone());
            let mut out = Vec::new();
            for e in edges {
                if seen.insert(e.node.id.clone()) {
                    out.push(e);
                }
            }
            out
        };
        let callers = collect(cg.get_callers(&node.id, None)?);
        let callees = collect(cg.get_callees(&node.id, None)?);
        Ok((callers, callees))
    }

    fn format_trail_refs(&self, node: &Node, callers: &[NodeRef], callees: &[NodeRef]) -> String {
        if callees.is_empty() && callers.is_empty() {
            return String::new();
        }
        let fmt = |e: &NodeRef| -> String {
            let base = format!(
                "{} ({}:{})",
                e.node.name, e.node.file_path, e.node.start_line
            );
            match self.synth_edge_note(Some(&e.edge)) {
                Some(synth) => format!("{} [{}]", base, synth.compact),
                None => base,
            }
        };
        let list = |refs: &[NodeRef]| -> String {
            format!(
                "{}{}",
                refs.iter()
                    .take(TRAIL_CAP)
                    .map(&fmt)
                    .collect::<Vec<_>>()
                    .join(", "),
                if refs.len() > TRAIL_CAP {
                    format!(", +{} more", refs.len() - TRAIL_CAP)
                } else {
                    String::new()
                }
            )
        };
        let mut lines: Vec<String> = vec![String::new(), format!("Trail for `{}`", node.name)];
        if !callees.is_empty() {
            lines.push(format!("Calls: {}", list(callees)));
        }
        if !callers.is_empty() {
            lines.push(format!("Called by: {}", list(callers)));
        }
        lines.join("\n")
    }
}

/// Keep the definitions the caller pinned by `file` and/or `line`; never
/// narrows to nothing.
fn narrow_matches(
    matches: Vec<Node>,
    file_hint: Option<&str>,
    line_hint: Option<f64>,
) -> Vec<Node> {
    let norm = |p: &str| p.replace('\\', "/").to_lowercase();
    let mut narrowed = matches.clone();
    if let Some(fh) = file_hint {
        let fh = norm(fh);
        let by_file: Vec<Node> = narrowed
            .iter()
            .filter(|n| {
                let np = norm(&n.file_path);
                np.ends_with(&fh) || np.contains(&fh)
            })
            .cloned()
            .collect();
        if !by_file.is_empty() {
            narrowed = by_file;
        }
    }
    if let Some(lh) = line_hint {
        if narrowed.len() > 1 {
            let containing: Vec<Node> = narrowed
                .iter()
                .filter(|n| (n.start_line as f64) <= lh && (n.end_line as f64) >= lh)
                .cloned()
                .collect();
            narrowed = if !containing.is_empty() {
                containing
            } else {
                let mut sorted = narrowed.clone();
                sorted.sort_by(|a, b| {
                    let da = (a.start_line as f64 - lh).abs();
                    let db = (b.start_line as f64 - lh).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                });
                sorted.truncate(1);
                sorted
            };
        }
    }
    if narrowed.is_empty() {
        matches
    } else {
        narrowed
    }
}

fn definition_line(node: &Node) -> String {
    format!(
        "- `{}` ({}) — {}:{}",
        node.name,
        node.kind.as_str(),
        node.file_path,
        node.start_line
    )
}

/// Whether `code`, starting at `start` in `file`, went out earlier in this
/// session and the file is unchanged since.
pub(super) fn was_sent(
    cg: &CodeGraph,
    prior: Option<&ProjectState>,
    file: &str,
    start: usize,
    code: &str,
) -> bool {
    let Some(prior) = prior else {
        return false;
    };
    if code.is_empty() {
        return false;
    }
    let end = start + code.split('\n').count() - 1;
    range_already_sent(prior, cg.get_project_root(), file, start, end)
}

pub(super) fn already_sent_note(file: &str) -> String {
    format!(
        "\n\n> The source of this definition was already sent earlier in this conversation and \
         `{file}` is unchanged on disk since; it is not repeated."
    )
}

fn attach_trail(detail: &mut NodeDetailOutput, callers: &[NodeRef], callees: &[NodeRef]) {
    let refs = |list: &[NodeRef]| -> Vec<SymbolRef> {
        list.iter()
            .take(TRAIL_CAP)
            .map(|item| SymbolRef::from(&item.node))
            .collect()
    };
    detail.callers = refs(callers);
    detail.callers_omitted = callers.len().saturating_sub(TRAIL_CAP);
    detail.callees = refs(callees);
    detail.callees_omitted = callees.len().saturating_sub(TRAIL_CAP);
}

fn current_source_if_stale(cg: &CodeGraph, file_path: &str) -> Option<String> {
    let indexed = cg.get_file(file_path).ok().flatten()?;
    let real_path = resolve_existing_path_within_root_real(cg.get_project_root(), file_path)?;
    let source = fs::read_to_string(real_path).ok()?;
    (crate::extraction::hash_content(&source) != indexed.content_hash).then_some(source)
}
