//! Node support for graph MCP tools.

use std::collections::HashSet;
use std::fs;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::is_container_node_kind;
use super::super::output::{NodeDetailOutput, NodeOutput, NodeSummary};
use super::super::schema::ToolResult;
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::types::{Language, Node, NodeKind};
use crate::utils::validate_path_within_root;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_node(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
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
        let offset = args
            .get("offset")
            .and_then(Value::as_f64)
            .filter(|value| *value > 0.0)
            .map(|value| value.floor() as usize);
        let limit = args
            .get("limit")
            .and_then(Value::as_f64)
            .filter(|value| *value > 0.0)
            .map(|value| value.floor() as usize);
        let symbols_only = args.get("symbolsOnly") == Some(&Value::Bool(true));
        let symbol_raw = args
            .get("symbol")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();

        if symbol_raw.is_empty() {
            if let Some(file) = file_hint.as_deref() {
                return self.handle_file_view(&cg, file, offset, limit, symbols_only);
            }
        }

        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };

        let mut matches = self.find_symbol_matches(&cg, &symbol)?;
        if matches.is_empty() {
            let output = NodeOutput {
                schema_version: 1,
                kind: "node",
                query: symbol.clone(),
                include_code,
                match_count: 0,
                returned_full_count: 0,
                truncated: false,
                matches: Vec::new(),
            };
            return self.structured_result(&format!("Symbol not found: `{symbol}`"), &output);
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
            let (section, detail) = self.render_node_detail(&cg, &matches[0], include_code)?;
            let output = NodeOutput {
                schema_version: 1,
                kind: "node",
                query: symbol,
                include_code,
                match_count: 1,
                returned_full_count: 1,
                truncated: false,
                matches: vec![detail],
            };
            return self.structured_result(&self.truncate_output(&section), &output);
        }

        // Multiple definitions share this name — return them ALL.
        let header = format!("{} definitions named `{}`", matches.len(), symbol);
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
                "Re-query with `includeCode: true` to get bodies.".to_string(),
                String::new(),
            ];
            out.extend(list);
            let output = NodeOutput {
                schema_version: 1,
                kind: "node",
                query: symbol,
                include_code,
                match_count: matches.len(),
                returned_full_count: 0,
                truncated: false,
                matches: matches.iter().map(empty_node_detail).collect(),
            };
            return self.structured_result(&self.truncate_output(&out.join("\n")), &output);
        }

        // Render every definition in full up to a RELEVANCE cap (how many
        // overloads are plausibly useful), not a size budget — size policy
        // belongs to the host.
        const HARD_CAP: usize = 16;
        let mut rendered: Vec<String> = Vec::new();
        let mut details: Vec<NodeDetailOutput> = Vec::new();
        let mut listed: Vec<Node> = Vec::new();
        for n in &matches {
            if rendered.len() >= HARD_CAP {
                listed.push(n.clone());
                continue;
            }
            let (section, detail) = self.render_node_detail(&cg, n, true)?;
            rendered.push(section);
            details.push(detail);
        }

        let mut out: Vec<String> = vec![
            header,
            format!(
                "Returning {} in full{}.",
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
            out.push("Other definitions".to_string());
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
            let _ = basename;
        }
        let output = NodeOutput {
            schema_version: 1,
            kind: "node",
            query: symbol,
            include_code,
            match_count: matches.len(),
            returned_full_count: details.len(),
            truncated: !listed.is_empty(),
            matches: details,
        };
        self.structured_result(&self.truncate_output(&out.join("\n")), &output)
    }

    fn handle_file_view(
        &self,
        cg: &CodeGraph,
        file_arg: &str,
        offset: Option<usize>,
        limit: Option<usize>,
        symbols_only: bool,
    ) -> Result<ToolResult> {
        fn normalize(path: &str) -> String {
            path.replace('\\', "/")
                .trim_start_matches("./")
                .trim_matches('/')
                .to_lowercase()
        }

        let wanted = normalize(file_arg);
        let all_files = cg.get_files()?;
        if all_files.is_empty() {
            return Ok(self.text_result("No files indexed. Run `codegraph index` first."));
        }

        let mut resolved = all_files
            .iter()
            .find(|file| file.path.to_lowercase() == wanted);
        let mut candidates = Vec::new();
        if resolved.is_none() {
            let suffix = format!("/{wanted}");
            candidates = all_files
                .iter()
                .filter(|file| file.path.to_lowercase().ends_with(&suffix))
                .collect();
            if candidates.len() == 1 {
                resolved = candidates.first().copied();
            }
        }
        if resolved.is_none() && candidates.is_empty() {
            candidates = all_files
                .iter()
                .filter(|file| file.path.to_lowercase().contains(&wanted))
                .collect();
            if candidates.len() == 1 {
                resolved = candidates.first().copied();
            }
        }
        if resolved.is_none() && candidates.len() > 1 {
            let mut out = vec![
                format!(
                    "\"{file_arg}\" matches {} indexed files - pass a longer path:",
                    candidates.len()
                ),
                String::new(),
            ];
            out.extend(
                candidates
                    .iter()
                    .take(25)
                    .map(|candidate| format!("- {}", candidate.path)),
            );
            return Ok(self.text_result(&out.join("\n")));
        }
        let Some(resolved) = resolved else {
            return Ok(self.text_result(&format!(
                "No indexed file matches \"{file_arg}\". Codegraph indexes source files; configs/docs it doesn't parse won't appear - read those directly."
            )));
        };

        let file_path = &resolved.path;
        let mut nodes = cg.get_nodes_in_file(file_path)?;
        nodes.retain(|node| {
            !matches!(
                node.kind,
                NodeKind::File | NodeKind::Import | NodeKind::Export
            )
        });
        nodes.sort_by_key(|node| node.start_line);
        let dependents = cg.get_file_dependents(file_path)?;
        let dep_summary = if dependents.is_empty() {
            "no other indexed file depends on it".to_string()
        } else {
            let shown = dependents.iter().take(8).cloned().collect::<Vec<_>>();
            let overflow = dependents.len().saturating_sub(shown.len());
            format!(
                "used by {} file{}: {}{}",
                dependents.len(),
                if dependents.len() == 1 { "" } else { "s" },
                shown.join(", "),
                if overflow == 0 {
                    String::new()
                } else {
                    format!(", +{overflow} more")
                }
            )
        };

        let symbol_map = |heading: &str| {
            let mut lines = vec![heading.to_string()];
            for node in nodes.iter().take(200) {
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
            if nodes.len() > 200 {
                lines.push(format!("- ... +{} more", nodes.len() - 200));
            }
            lines
        };

        if symbols_only {
            let mut out = vec![
                format!(
                    "**{file_path}** - {} symbol{}, {dep_summary}",
                    nodes.len(),
                    if nodes.len() == 1 { "" } else { "s" }
                ),
                String::new(),
            ];
            if nodes.is_empty() {
                out.push("_No indexed symbols in this file._".to_string());
            } else {
                out.extend(symbol_map("**Symbols**"));
            }
            out.push(String::new());
            out.push(
                "> Drop `symbolsOnly` (or pass `offset`/`limit`) to read the source, like Read."
                    .to_string(),
            );
            return Ok(self.text_result(&self.truncate_output(&out.join("\n"))));
        }

        if matches!(resolved.language, Language::Yaml | Language::Properties) {
            let mut out = vec![
                format!("**{file_path}** - configuration/data file, {dep_summary}"),
                String::new(),
            ];
            if !nodes.is_empty() {
                out.extend(symbol_map("**Keys (values withheld for safety)**"));
            }
            out.push(String::new());
            out.push(
                "> Values may be secrets, so codegraph indexes keys only. Read the file directly if you need a value."
                    .to_string(),
            );
            return Ok(self.text_result(&self.truncate_output(&out.join("\n"))));
        }

        let content = validate_path_within_root(cg.get_project_root(), file_path)
            .and_then(|path| fs::read_to_string(path).ok());
        let Some(content) = content else {
            let mut out = vec![
                format!(
                    "**{file_path}** - could not read from disk (it may have moved since indexing). {dep_summary}"
                ),
                String::new(),
            ];
            if !nodes.is_empty() {
                out.extend(symbol_map("**Symbols**"));
            }
            out.push(String::new());
            out.push(format!(
                "> Read `{file_path}` directly for its current content."
            ));
            return Ok(self.text_result(&self.truncate_output(&out.join("\n"))));
        };

        const DEFAULT_LIMIT: usize = 2_000;
        const CHAR_BUDGET: usize = 38_000;
        let file_lines = content.split('\n').collect::<Vec<_>>();
        let total = file_lines.len();
        let offset = offset.unwrap_or(1).max(1);
        if offset > total {
            return Ok(self.text_result(&format!(
                "**{file_path}** has {total} line{} - offset {offset} is past the end. {dep_summary}",
                if total == 1 { "" } else { "s" }
            )));
        }
        let max_lines = limit.unwrap_or(DEFAULT_LIMIT).max(1);
        let start = offset - 1;
        let header = format!(
            "**{file_path}** - {total} lines, {} symbol{} | {dep_summary}",
            nodes.len(),
            if nodes.len() == 1 { "" } else { "s" }
        );
        let mut numbered = Vec::new();
        let mut used = header.len() + 8;
        let mut index = start;
        while index < total && numbered.len() < max_lines {
            let line = format!("{}\t{}", index + 1, file_lines[index]);
            if used + line.len() + 1 > CHAR_BUDGET && !numbered.is_empty() {
                break;
            }
            used += line.len() + 1;
            numbered.push(line);
            index += 1;
        }
        let shown_end = start + numbered.len();
        let complete = offset == 1 && shown_end >= total;
        let mut out = vec![header, String::new()];
        out.extend(numbered);
        if !complete {
            out.push(String::new());
            out.push(format!(
                "(lines {offset}-{shown_end} of {total} - pass `offset`/`limit` for another range, or `codegraph_node <symbol>` for one symbol in full)"
            ));
        }
        Ok(self.text_result(&out.join("\n")))
    }

    fn render_node_detail(
        &self,
        cg: &CodeGraph,
        node: &Node,
        include_code: bool,
    ) -> Result<(String, NodeDetailOutput)> {
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
        let (callers, callees) = self.trail_refs(cg, node)?;
        let text = format!(
            "{}{}",
            self.format_node_details(node, code.as_deref(), outline.as_deref()),
            self.format_trail_refs(node, &callers, &callees)
        );
        let detail = NodeDetailOutput {
            node: NodeSummary::from(node),
            code,
            outline,
            callers: callers.iter().map(|r| NodeSummary::from(&r.node)).collect(),
            callees: callees.iter().map(|r| NodeSummary::from(&r.node)).collect(),
        };
        Ok((text, detail))
    }

    fn trail_refs(
        &self,
        cg: &CodeGraph,
        node: &Node,
    ) -> Result<(Vec<crate::types::NodeRef>, Vec<crate::types::NodeRef>)> {
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
        let callers = collect(cg.get_callers(&node.id, None)?);
        let callees = collect(cg.get_callees(&node.id, None)?);
        Ok((callers, callees))
    }

    fn format_trail_refs(
        &self,
        node: &Node,
        callers: &[crate::types::NodeRef],
        callees: &[crate::types::NodeRef],
    ) -> String {
        const TRAIL_CAP: usize = 12;
        if callees.is_empty() && callers.is_empty() {
            return String::new();
        }
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
        let mut lines: Vec<String> = vec![String::new(), format!("Trail for `{}`", node.name)];
        if !callees.is_empty() {
            lines.push(format!(
                "Calls: {}{}",
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
                "Called by: {}{}",
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
        lines.join("\n")
    }

    // =========================================================================
    // codegraph_status
}

fn empty_node_detail(node: &Node) -> NodeDetailOutput {
    NodeDetailOutput {
        node: NodeSummary::from(node),
        code: None,
        outline: None,
        callers: Vec::new(),
        callees: Vec::new(),
    }
}
