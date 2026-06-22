//! Architecture support for graph MCP tools.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::num_or;
use super::super::schema::ToolResult;
use crate::error::Result;
use crate::types::{Node, NodeKind};
use crate::utils::clamp;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_arch(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let raw = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let prefix = raw.replace('\\', "/");
        let prefix = prefix
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string();
        let max_syms = clamp(num_or(args, "maxSymbolsPerFile", 12.0), 1.0, 100.0) as usize;

        let mut files: Vec<_> = cg
            .get_files()?
            .into_iter()
            .filter(|f| {
                if prefix.is_empty() {
                    return true;
                }
                let p = f.path.replace('\\', "/");
                p == prefix || p.starts_with(&format!("{prefix}/"))
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        if files.is_empty() {
            return Ok(self.text_result(&format!(
                "No indexed files under \"{}\". Try a different path or run `codegraph index`.",
                if prefix.is_empty() {
                    "<project root>"
                } else {
                    &prefix
                }
            )));
        }

        let in_scope: HashSet<String> = files.iter().map(|f| f.path.replace('\\', "/")).collect();
        let is_def = |k: &NodeKind| {
            matches!(
                k,
                NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Trait
                    | NodeKind::Class
                    | NodeKind::Interface
                    | NodeKind::TypeAlias
            )
        };

        let mut body = String::new();
        let mut total_defs = 0usize;
        let mut ext_deps: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut ext_dependents: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();

        for f in &files {
            let nodes = cg.get_nodes_in_file(&f.path)?;
            let mut defs: Vec<&Node> = nodes.iter().filter(|n| is_def(&n.kind)).collect();
            defs.sort_by_key(|n| n.start_line);
            total_defs += defs.len();

            body.push_str(&format!(
                "\n{} [{}] — {} symbols\n",
                f.path,
                f.language.as_str(),
                f.node_count
            ));
            for n in defs.iter().take(max_syms) {
                let exported = if n.is_exported == Some(true) {
                    " (pub)"
                } else {
                    ""
                };
                body.push_str(&format!(
                    "  {} {}{}  :{}\n",
                    n.kind.as_str(),
                    n.name,
                    exported,
                    n.start_line
                ));
            }
            if defs.len() > max_syms {
                body.push_str(&format!("  … +{} more\n", defs.len() - max_syms));
            }

            for dep in cg.get_file_dependencies(&f.path)? {
                let d = dep.replace('\\', "/");
                if !in_scope.contains(&d) {
                    ext_deps.insert(d);
                }
            }
            for dep in cg.get_file_dependents(&f.path)? {
                let d = dep.replace('\\', "/");
                if !in_scope.contains(&d) {
                    ext_dependents.insert(d);
                }
            }
        }

        let scope_label = if prefix.is_empty() {
            "<project root>".to_string()
        } else {
            prefix.clone()
        };
        let mut report = format!(
            "Architecture overview: {}\n{} file(s), {} top-level definitions\n\n── Modules & key symbols ──{}",
            scope_label,
            files.len(),
            total_defs,
            body
        );
        report.push_str(&format!(
            "\n── Depends on (external, {}) ──\n",
            ext_deps.len()
        ));
        if ext_deps.is_empty() {
            report.push_str("  (none — self-contained)\n");
        } else {
            for d in ext_deps.iter().take(40) {
                report.push_str(&format!("  → {d}\n"));
            }
        }
        report.push_str(&format!(
            "\n── Depended on by (external, {}) ──\n",
            ext_dependents.len()
        ));
        if ext_dependents.is_empty() {
            report.push_str("  (none — leaf subsystem)\n");
        } else {
            for d in ext_dependents.iter().take(40) {
                report.push_str(&format!("  ← {d}\n"));
            }
        }

        Ok(self.text_result(&self.truncate_output(&report)))
    }
}
