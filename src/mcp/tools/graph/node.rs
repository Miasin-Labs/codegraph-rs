//! Node support for graph MCP tools.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::is_container_node_kind;
use super::super::schema::ToolResult;
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::types::Node;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_node(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
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

        let mut matches = self.find_symbol_matches(&cg, &symbol)?;
        if matches.is_empty() {
            return Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")));
        }

        // Disambiguate a heavily-overloaded name to a specific definition the
        // caller pinned by file/line. Only narrows (never empties).
        if matches.len() > 1 && (file_hint.is_some() || line_hint.is_some()) {
            let norm = |p: &str| p.replace('\\', "/").to_lowercase();
            let mut narrowed = matches.clone();
            if let Some(fh) = &file_hint {
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
            if !narrowed.is_empty() {
                matches = narrowed;
            }
        }

        // Single definition — the common case.
        if matches.len() == 1 {
            let section = self.render_node_section(&cg, &matches[0], include_code)?;
            return Ok(self.text_result(&self.truncate_output(&section)));
        }

        // Multiple definitions share this name — return them ALL.
        let header = format!("**{} definitions named \"{}\"**", matches.len(), symbol);
        if !include_code {
            let list: Vec<String> = matches
                .iter()
                .map(|n| {
                    format!(
                        "- `{}` ({}) — {}:{}",
                        n.name,
                        n.kind.as_str(),
                        n.file_path,
                        n.start_line
                    )
                })
                .collect();
            let mut out = vec![
                header,
                String::new(),
                "Re-query with `includeCode: true` to get every body in one call — no need to pick one first."
                    .to_string(),
                String::new(),
            ];
            out.extend(list);
            return Ok(self.text_result(&self.truncate_output(&out.join("\n"))));
        }

        // Render every definition in full up to a RELEVANCE cap (how many
        // overloads are plausibly useful), not a size budget — size policy
        // belongs to the host.
        const HARD_CAP: usize = 16;
        let mut rendered: Vec<String> = Vec::new();
        let mut listed: Vec<Node> = Vec::new();
        for n in &matches {
            if rendered.len() >= HARD_CAP {
                listed.push(n.clone());
                continue;
            }
            rendered.push(self.render_node_section(&cg, n, true)?);
        }

        let mut out: Vec<String> = vec![
            header,
            format!(
                "Returning {} in full{} — pick the one you need (no Read required).",
                rendered.len(),
                if !listed.is_empty() {
                    format!("; {} more listed below", listed.len())
                } else {
                    String::new()
                }
            ),
            String::new(),
            rendered.join("\n\n---\n\n"),
        ];
        if !listed.is_empty() {
            const LIST_CAP: usize = 20;
            out.push(String::new());
            out.push("### Other definitions".to_string());
            for n in listed.iter().take(LIST_CAP) {
                out.push(format!(
                    "- `{}` ({}) — {}:{}",
                    n.name,
                    n.kind.as_str(),
                    n.file_path,
                    n.start_line
                ));
            }
            if listed.len() > LIST_CAP {
                out.push(format!("- … +{} more", listed.len() - LIST_CAP));
            }
            let basename = listed[0]
                .file_path
                .split('/')
                .next_back()
                .unwrap_or(&listed[0].file_path);
            out.push(String::new());
            out.push(format!(
                "> Need one of these in full? Call codegraph_node again with `file` (e.g. `\"{basename}\"`) or `line` — do NOT Read it."
            ));
        }
        Ok(self.text_result(&self.truncate_output(&out.join("\n"))))
    }

    /// Render one symbol: details + (optional) body/outline + its trail.
    fn render_node_section(
        &self,
        cg: &CodeGraph,
        node: &Node,
        include_code: bool,
    ) -> Result<String> {
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
        Ok(format!(
            "{}{}",
            self.format_node_details(node, code.as_deref(), outline.as_deref()),
            self.format_trail(cg, node)?
        ))
    }

    /// Build the "trail" for a symbol: direct callees and callers with
    /// file:line.
    fn format_trail(&self, cg: &CodeGraph, node: &Node) -> Result<String> {
        const TRAIL_CAP: usize = 12;
        let fmt = |e: &crate::types::NodeRef| -> String {
            let base = format!(
                "{} ({}:{})",
                e.node.name, e.node.file_path, e.node.start_line
            );
            match self.synth_edge_note(Some(&e.edge)) {
                Some(synth) => format!("{} [{}]", base, synth.compact),
                None => base,
            }
        };
        let collect = |edges: Vec<crate::types::NodeRef>| -> Vec<crate::types::NodeRef> {
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
        let callees = collect(cg.get_callees(&node.id, None)?);
        let callers = collect(cg.get_callers(&node.id, None)?);
        if callees.is_empty() && callers.is_empty() {
            return Ok(String::new());
        }
        let mut lines: Vec<String> = vec![
            String::new(),
            "### Trail — codegraph_node any of these to follow it (no Read needed)".to_string(),
        ];
        if !callees.is_empty() {
            lines.push(format!(
                "**Calls →** {}{}",
                callees
                    .iter()
                    .take(TRAIL_CAP)
                    .map(&fmt)
                    .collect::<Vec<_>>()
                    .join(", "),
                if callees.len() > TRAIL_CAP {
                    format!(", +{} more", callees.len() - TRAIL_CAP)
                } else {
                    String::new()
                }
            ));
        }
        if !callers.is_empty() {
            lines.push(format!(
                "**Called by ←** {}{}",
                callers
                    .iter()
                    .take(TRAIL_CAP)
                    .map(&fmt)
                    .collect::<Vec<_>>()
                    .join(", "),
                if callers.len() > TRAIL_CAP {
                    format!(", +{} more", callers.len() - TRAIL_CAP)
                } else {
                    String::new()
                }
            ));
        }
        Ok(lines.join("\n"))
    }

    // =========================================================================
    // codegraph_status
}
