use super::{FileRecord, dim, get_glyphs};

/// Tree node for `printFileTree`.
struct TreeNode {
    name: String,
    /// Insertion-ordered children (TS `Map`); render sorts dirs-first by name.
    children: Vec<TreeNode>,
    file: Option<(String, u32)>, // (language, node_count)
}

impl TreeNode {
    fn child_index(&mut self, name: &str) -> usize {
        if let Some(i) = self.children.iter().position(|c| c.name == name) {
            return i;
        }
        self.children.push(TreeNode {
            name: name.to_string(),
            children: Vec::new(),
            file: None,
        });
        self.children.len() - 1
    }
}

/// Print files as a tree (TS `printFileTree`).
pub(crate) fn print_file_tree(
    files: &[FileRecord],
    include_metadata: bool,
    max_depth: Option<i64>,
) {
    let mut root = TreeNode {
        name: String::new(),
        children: Vec::new(),
        file: None,
    };

    for file in files {
        let parts: Vec<&str> = file.path.split('/').collect();
        let mut current = &mut root;

        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            let idx = current.child_index(part);
            current = &mut current.children[idx];
            if i == parts.len() - 1 {
                current.file = Some((file.language.as_str().to_string(), file.node_count));
            }
        }
    }

    fn render_node(
        node: &TreeNode,
        prefix: &str,
        is_last: bool,
        depth: i64,
        include_metadata: bool,
        max_depth: Option<i64>,
    ) {
        // Recursion guard — tree depth grows with nested children.
        codegraph::ensure_sufficient_stack(|| {
            render_node_inner(node, prefix, is_last, depth, include_metadata, max_depth)
        });
    }

    fn render_node_inner(
        node: &TreeNode,
        prefix: &str,
        is_last: bool,
        depth: i64,
        include_metadata: bool,
        max_depth: Option<i64>,
    ) {
        if let Some(max) = max_depth {
            if depth > max {
                return;
            }
        }

        let glyphs = get_glyphs();
        let connector = if is_last {
            glyphs.tree_last
        } else {
            glyphs.tree_branch
        };
        let child_prefix = if is_last { "    " } else { glyphs.tree_pipe };

        if !node.name.is_empty() {
            let mut line = format!("{prefix}{connector}{}", node.name);
            if include_metadata {
                if let Some((language, node_count)) = &node.file {
                    line.push_str(&dim(&format!(" ({language}, {node_count} symbols)")));
                }
            }
            println!("{line}");
        }

        let mut children: Vec<&TreeNode> = node.children.iter().collect();
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
            a.name.cmp(&b.name)
        });

        for (i, child) in children.iter().enumerate() {
            let next_prefix = if !node.name.is_empty() {
                format!("{prefix}{child_prefix}")
            } else {
                prefix.to_string()
            };
            render_node(
                child,
                &next_prefix,
                i == children.len() - 1,
                depth + 1,
                include_metadata,
                max_depth,
            );
        }
    }

    render_node(&root, "", true, 0, include_metadata, max_depth);
}
