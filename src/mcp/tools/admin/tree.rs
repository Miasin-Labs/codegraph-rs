//! Tree renderer for codegraph_files.

use std::collections::HashMap;

use super::super::context::ToolHandler;
use super::super::format::locale_cmp;

impl ToolHandler {
    pub(in crate::mcp::tools::admin) fn format_files_tree(
        &self,
        files: &[(&str, &str, u32)],
        include_metadata: bool,
        max_depth: Option<usize>,
    ) -> String {
        struct TreeNode {
            name: String,
            children: Vec<TreeNode>,
            child_index: HashMap<String, usize>,
            file: Option<(String, u32)>,
        }
        impl TreeNode {
            fn new(name: &str) -> Self {
                TreeNode {
                    name: name.to_string(),
                    children: Vec::new(),
                    child_index: HashMap::new(),
                    file: None,
                }
            }
        }

        let mut root = TreeNode::new("");

        for (path, language, node_count) in files {
            let parts: Vec<&str> = path.split('/').collect();
            let mut current = &mut root;

            for (i, part) in parts.iter().enumerate() {
                if part.is_empty() {
                    continue;
                }
                let idx = match current.child_index.get(*part) {
                    Some(&idx) => idx,
                    None => {
                        current.children.push(TreeNode::new(part));
                        let idx = current.children.len() - 1;
                        current.child_index.insert(part.to_string(), idx);
                        idx
                    }
                };
                current = &mut current.children[idx];

                // If this is the last part, it's a file
                if i == parts.len() - 1 {
                    current.file = Some((language.to_string(), *node_count));
                }
            }
        }

        let mut lines: Vec<String> = vec![
            format!("## Project Structure ({} files)", files.len()),
            String::new(),
        ];

        fn render_node(
            node: &TreeNode,
            prefix: &str,
            is_last: bool,
            depth: usize,
            max_depth: Option<usize>,
            include_metadata: bool,
            lines: &mut Vec<String>,
        ) {
            // Recursion guard — directory-tree depth drives the recursion.
            crate::ensure_sufficient_stack(|| {
                render_node_inner(
                    node,
                    prefix,
                    is_last,
                    depth,
                    max_depth,
                    include_metadata,
                    lines,
                )
            });
        }

        fn render_node_inner(
            node: &TreeNode,
            prefix: &str,
            is_last: bool,
            depth: usize,
            max_depth: Option<usize>,
            include_metadata: bool,
            lines: &mut Vec<String>,
        ) {
            if let Some(md) = max_depth {
                if depth > md {
                    return;
                }
            }

            let connector = if is_last { "└── " } else { "├── " };
            let child_prefix = if is_last { "    " } else { "│   " };

            if !node.name.is_empty() {
                let mut line = format!("{prefix}{connector}{}", node.name);
                if let (Some((language, node_count)), true) = (&node.file, include_metadata) {
                    line.push_str(&format!(" ({language}, {node_count} symbols)"));
                }
                lines.push(line);
            }

            let mut children: Vec<&TreeNode> = node.children.iter().collect();
            // Sort: directories first, then files, both alphabetically
            children.sort_by(|a, b| {
                let a_is_dir = !a.children.is_empty() && a.file.is_none();
                let b_is_dir = !b.children.is_empty() && b.file.is_none();
                if a_is_dir != b_is_dir {
                    return if a_is_dir {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Greater
                    };
                }
                locale_cmp(&a.name, &b.name)
            });

            let count = children.len();
            for (i, child) in children.into_iter().enumerate() {
                let next_prefix = if !node.name.is_empty() {
                    format!("{prefix}{child_prefix}")
                } else {
                    prefix.to_string()
                };
                render_node(
                    child,
                    &next_prefix,
                    i == count - 1,
                    depth + 1,
                    max_depth,
                    include_metadata,
                    lines,
                );
            }
        }

        render_node(&root, "", true, 0, max_depth, include_metadata, &mut lines);

        lines.join("\n")
    }

    // =========================================================================
    // Symbol resolution helpers
    // =========================================================================
}
