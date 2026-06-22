//! Blast support for graph MCP tools.

use std::collections::HashSet;

use super::super::context::ToolHandler;
use super::super::format::{OrderedNodeMap, display_symbol};
use crate::codegraph::CodeGraph;
use crate::search::is_test_file;
use crate::types::{Node, NodeKind};

impl ToolHandler {
    pub(in crate::mcp::tools) fn build_blast_radius_section(
        &self,
        cg: &CodeGraph,
        roots: &[String],
        nodes: &OrderedNodeMap,
    ) -> String {
        const ROOT_CAP: usize = 5; // only the symbols the query actually targeted
        const FILE_CAP: usize = 4; // caller files listed per symbol before "+N more"
        fn meaningful(kind: NodeKind) -> bool {
            matches!(
                kind,
                NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::Class
                    | NodeKind::Interface
                    | NodeKind::Struct
                    | NodeKind::Trait
                    | NodeKind::Protocol
                    | NodeKind::Enum
                    | NodeKind::TypeAlias
                    | NodeKind::Component
                    | NodeKind::Constant
                    | NodeKind::Variable
                    | NodeKind::Property
                    | NodeKind::Field
            )
        }
        let rel = |p: &str| p.replace('\\', "/");

        let root_nodes: Vec<&Node> = roots
            .iter()
            .filter_map(|id| nodes.get(id))
            .filter(|n| meaningful(n.kind))
            .take(ROOT_CAP)
            .collect();
        if root_nodes.is_empty() {
            return String::new();
        }

        let mut entries: Vec<String> = Vec::new();
        for root in root_nodes {
            let callers = cg.get_callers(&root.id, None).unwrap_or_default();

            let mut seen: HashSet<String> = HashSet::new();
            let mut uniq: Vec<Node> = Vec::new();
            for c in callers {
                if seen.insert(c.node.id.clone()) {
                    uniq.push(c.node);
                }
            }
            if uniq.is_empty() {
                continue; // no blast radius → nothing to flag
            }

            let mut caller_files: Vec<String> = Vec::new();
            let mut file_seen: HashSet<String> = HashSet::new();
            for n in &uniq {
                let f = rel(&n.file_path);
                if file_seen.insert(f.clone()) {
                    caller_files.push(f);
                }
            }
            let test_files: Vec<&String> =
                caller_files.iter().filter(|f| is_test_file(f)).collect();
            let non_test: Vec<&String> = caller_files.iter().filter(|f| !is_test_file(f)).collect();

            let shown = non_test
                .iter()
                .take(FILE_CAP)
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let more = if non_test.len() > FILE_CAP {
                format!(" +{} more", non_test.len() - FILE_CAP)
            } else {
                String::new()
            };
            let where_part = if !non_test.is_empty() {
                format!(" in {shown}{more}")
            } else {
                String::new()
            };
            let tests = if !test_files.is_empty() {
                format!(
                    "; tests: {}{}",
                    test_files
                        .iter()
                        .take(FILE_CAP)
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    if test_files.len() > FILE_CAP {
                        format!(" +{}", test_files.len() - FILE_CAP)
                    } else {
                        String::new()
                    }
                )
            } else {
                "; ⚠️ no covering tests found".to_string()
            };

            entries.push(format!(
                "- `{}` ({}:{}) — {} caller{}{}{}",
                display_symbol(root),
                rel(&root.file_path),
                root.start_line,
                uniq.len(),
                if uniq.len() == 1 { "" } else { "s" },
                where_part,
                tests
            ));
        }
        if entries.is_empty() {
            return String::new();
        }

        let mut lines = vec![
            "### Blast radius — what depends on these (update/verify before editing)".to_string(),
            String::new(),
        ];
        lines.extend(entries);
        lines.push(String::new());
        lines.join("\n")
    }
}
