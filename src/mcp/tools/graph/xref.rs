//! Xref support for graph MCP tools.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::{mcp_output_budget, num_or};
use super::super::output::{SymbolRef, XrefDefinition, XrefGroup, XrefOutput};
use super::super::schema::ToolResult;
use crate::error::Result;
use crate::types::Node;
use crate::utils::clamp;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_xref(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };
        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let max_refs = clamp(num_or(args, "maxRefs", 50.0), 1.0, 500.0) as usize;

        let matches = self.find_all_symbols(&cg, &symbol)?;
        if matches.nodes.is_empty() {
            return self.structured_result(
                &format!("Symbol \"{symbol}\" not found in the codebase"),
                &XrefOutput::not_found(),
            );
        }

        let mut output = XrefOutput::new();
        let mut out = String::new();
        for node in &matches.nodes {
            out.push_str(&format!(
                "\n{} {} — {}:{}\n",
                node.kind.as_str(),
                node.name,
                node.file_path,
                node.start_line
            ));
            let mut definition = XrefDefinition::from(node);
            let incoming = cg.get_incoming_edges(&node.id)?;
            if incoming.is_empty() {
                out.push_str("  (no incoming references)\n");
                output.definitions.push(definition);
                continue;
            }
            // Per edge kind: each reference's source (None when it is no
            // longer in the index; the text then shows its id) and line.
            let mut by_kind: BTreeMap<&str, Vec<Reference>> = BTreeMap::new();
            for e in &incoming {
                let source = cg.get_node(&e.source)?;
                let line = source.as_ref().map(|s| e.line.unwrap_or(s.start_line));
                by_kind.entry(e.kind.as_str()).or_default().push(Reference {
                    id: &e.source,
                    source,
                    line: line.unwrap_or(0),
                });
            }
            for (kind, refs) in &by_kind {
                out.push_str(&format!("  {} ({}):\n", kind, refs.len()));
                for r in refs.iter().take(max_refs) {
                    out.push_str(&format!("    {}\n", r.render()));
                }
                if refs.len() > max_refs {
                    out.push_str(&format!("    … +{} more\n", refs.len() - max_refs));
                }
                let references: Vec<SymbolRef> = refs
                    .iter()
                    .take(max_refs)
                    .filter_map(Reference::row)
                    .collect();
                definition.by_kind.push(XrefGroup {
                    edge_kind: kind,
                    omitted: refs.len() - references.len(),
                    references,
                });
            }
            output.definitions.push(definition);
        }
        out.push_str(&matches.note);
        output.fit_to(mcp_output_budget());
        self.structured_result(&self.truncate_output(&out), &output)
    }
}

/// An incoming reference: where it comes from and the line it is on.
struct Reference<'a> {
    id: &'a str,
    source: Option<Node>,
    line: u32,
}

impl Reference<'_> {
    fn render(&self) -> String {
        match &self.source {
            Some(s) => format!(
                "{} {} — {}:{}",
                s.kind.as_str(),
                s.name,
                s.file_path,
                self.line
            ),
            None => self.id.to_string(),
        }
    }

    fn row(&self) -> Option<SymbolRef> {
        let source = self.source.as_ref()?;
        Some(SymbolRef {
            line: self.line,
            ..SymbolRef::from(source)
        })
    }
}
