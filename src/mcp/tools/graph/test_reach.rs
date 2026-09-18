//! codegraph_tests — which tests exercise a symbol, from the call graph.
//!
//! Walks the symbol's dependents (the same reverse traversal `impact` uses)
//! and keeps the ones that are test code. No coverage file needed: agents in
//! the mined sessions ran 25,423 build/test commands but had no way to ask
//! "which tests should I run for this change?".

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::num_or;
use super::super::schema::ToolResult;
use crate::error::Result;
use crate::types::{Node, NodeKind};
use crate::utils::clamp;

/// Test code by the conventions of the languages codegraph indexes: test
/// directories, test file suffixes/prefixes, Rust inline `mod tests`, and
/// Go/Python/xUnit test-function names.
pub(super) fn is_test_node(node: &Node) -> bool {
    if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
        return false;
    }
    let path = node.file_path.replace('\\', "/").to_lowercase();
    let file = path.rsplit('/').next().unwrap_or(&path);
    let in_test_dir = ["tests/", "test/", "__tests__/", "spec/", "testing/"]
        .iter()
        .any(|dir| path.starts_with(dir) || path.contains(&format!("/{dir}")));
    let test_file = file.contains(".test.")
        || file.contains(".spec.")
        || file.contains("_test.")
        || file.contains("_spec.")
        || file.starts_with("test_")
        || file.ends_with("test.java")
        || file.ends_with("tests.java")
        || file.ends_with("tests.cs")
        || file.ends_with("test.kt");
    let inline_rust =
        node.qualified_name.contains("::tests::") || node.qualified_name.contains("::test::");
    let test_name = node.name.starts_with("test_")
        || (node.name.starts_with("Test") && node.name.len() > 4)
        || node.name.starts_with("should_");
    in_test_dir || test_file || inline_rust || test_name
}

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_tests(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let symbol = match self.validate_string(args.get("symbol"), "symbol") {
            Ok(s) => s,
            Err(r) => return Ok(r),
        };
        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let depth = clamp(num_or(args, "depth", 4.0), 1.0, 8.0) as u32;
        let limit = clamp(num_or(args, "limit", 40.0), 1.0, 200.0) as usize;

        let matches = self.find_all_symbols(&cg, &symbol)?;
        if matches.nodes.is_empty() {
            return Ok(self.text_result(&format!("Symbol \"{symbol}\" not found in the codebase")));
        }

        // file -> (line, name) for every test reaching any definition.
        let mut by_file: BTreeMap<String, BTreeMap<u32, String>> = BTreeMap::new();
        for node in &matches.nodes {
            if is_test_node(node) {
                by_file
                    .entry(node.file_path.clone())
                    .or_default()
                    .insert(node.start_line, node.name.clone());
            }
            let impact = cg.get_impact_radius(&node.id, Some(depth))?;
            for dependent in impact.nodes.values() {
                if is_test_node(dependent) {
                    by_file
                        .entry(dependent.file_path.clone())
                        .or_default()
                        .insert(dependent.start_line, dependent.name.clone());
                }
            }
        }

        let total: usize = by_file.values().map(BTreeMap::len).sum();
        if total == 0 {
            return Ok(self.text_result(&format!(
                "No test reaches `{symbol}` through the call graph within depth {depth} — \
                 it is likely untested (or only exercised through dynamic dispatch the index \
                 cannot see).{}",
                matches.note
            )));
        }
        let mut lines = vec![
            format!(
                "{total} test{} reach `{symbol}` (call graph, depth {depth}) across {} file{}:",
                if total == 1 { "" } else { "s" },
                by_file.len(),
                if by_file.len() == 1 { "" } else { "s" }
            ),
            String::new(),
        ];
        let mut shown = 0;
        'files: for (file, tests) in &by_file {
            lines.push(format!("**{file}**"));
            for (line, name) in tests {
                if shown == limit {
                    lines.push(format!("… {} more (raise `limit`)", total - shown));
                    break 'files;
                }
                lines.push(format!("- `{name}` :{line}"));
                shown += 1;
            }
        }
        lines.push(matches.note);
        Ok(self.text_result(&self.truncate_output(&lines.join("\n"))))
    }
}
