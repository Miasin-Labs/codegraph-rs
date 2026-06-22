//! Xref support for graph MCP tools.

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::num_or;
use super::super::schema::ToolResult;
use crate::error::Result;
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
            return Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")));
        }

        let mut out = String::new();
        for node in &matches.nodes {
            out.push_str(&format!(
                "\n{} {} — {}:{}\n",
                node.kind.as_str(),
                node.name,
                node.file_path,
                node.start_line
            ));
            let incoming = cg.get_incoming_edges(&node.id)?;
            if incoming.is_empty() {
                out.push_str("  (no incoming references)\n");
                continue;
            }
            let mut by_kind: std::collections::BTreeMap<&str, Vec<String>> =
                std::collections::BTreeMap::new();
            for e in &incoming {
                let loc = match cg.get_node(&e.source)? {
                    Some(s) => format!(
                        "{} {} — {}:{}",
                        s.kind.as_str(),
                        s.name,
                        s.file_path,
                        e.line.unwrap_or(s.start_line)
                    ),
                    None => e.source.clone(),
                };
                by_kind.entry(e.kind.as_str()).or_default().push(loc);
            }
            for (kind, refs) in &by_kind {
                out.push_str(&format!("  {} ({}):\n", kind, refs.len()));
                for r in refs.iter().take(max_refs) {
                    out.push_str(&format!("    {r}\n"));
                }
                if refs.len() > max_refs {
                    out.push_str(&format!("    … +{} more\n", refs.len() - max_refs));
                }
            }
        }
        out.push_str(&matches.note);
        Ok(self.text_result(&self.truncate_output(&out)))
    }
}
