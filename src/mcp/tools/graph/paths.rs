//! Paths support for graph MCP tools.

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::schema::ToolResult;
use crate::error::Result;
use crate::types::{EdgeKind, Node};

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_paths(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let from = match self.validate_string(args.get("from"), "from") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };
        let to = match self.validate_string(args.get("to"), "to") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };
        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;

        let from_m = self.find_all_symbols(&cg, &from)?;
        if from_m.nodes.is_empty() {
            return Ok(self.text_result(&format!("Source symbol \"{from}\" not found")));
        }
        let to_m = self.find_all_symbols(&cg, &to)?;
        if to_m.nodes.is_empty() {
            return Ok(self.text_result(&format!("Sink symbol \"{to}\" not found")));
        }

        let edge_kinds = [
            EdgeKind::Calls,
            EdgeKind::References,
            EdgeKind::Instantiates,
        ];
        // Bound the pairwise search so a common name can't explode it.
        let froms: Vec<&Node> = from_m.nodes.iter().take(5).collect();
        let tos: Vec<&Node> = to_m.nodes.iter().take(5).collect();
        for fnode in &froms {
            for tnode in &tos {
                if fnode.id == tnode.id {
                    continue;
                }
                let Some(path) = cg.find_path(&fnode.id, &tnode.id, Some(&edge_kinds))? else {
                    continue;
                };
                if path.is_empty() {
                    continue;
                }
                let mut s = format!(
                    "Path from {} to {} ({} hops):\n\n",
                    from,
                    to,
                    path.len().saturating_sub(1)
                );
                for (i, step) in path.iter().enumerate() {
                    if i == 0 {
                        s.push_str(&format!(
                            "{} {} — {}:{}\n",
                            step.node.kind.as_str(),
                            step.node.name,
                            step.node.file_path,
                            step.node.start_line
                        ));
                    } else {
                        let via = step.edge.as_ref().map_or("?", |e| e.kind.as_str());
                        s.push_str(&format!(
                            "  └─{via}→ {} {} — {}:{}\n",
                            step.node.kind.as_str(),
                            step.node.name,
                            step.node.file_path,
                            step.node.start_line
                        ));
                    }
                }
                return Ok(self.text_result(&self.truncate_output(&s)));
            }
        }
        Ok(self.text_result(&format!(
            "No path found from \"{from}\" to \"{to}\" over calls/references (searched {}×{} symbol matches). They may be unreachable, or connected only via dynamic dispatch.",
            froms.len(),
            tos.len()
        )))
    }
}
