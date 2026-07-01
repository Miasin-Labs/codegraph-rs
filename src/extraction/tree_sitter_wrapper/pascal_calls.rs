use std::collections::HashMap;

use super::context::{find_named_child, named_children};
use super::extractor::TreeSitterExtractor;
use crate::ensure_sufficient_stack;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::SyntaxNode;
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

impl<'a> TreeSitterExtractor<'a> {
    /// Extract calls and resolve method context from a Pascal defProc (implementation body).
    /// Does not create a new node — the declaration was already captured from the interface section.
    pub(super) fn extract_pascal_def_proc(&mut self, node: SyntaxNode<'_>) {
        // Find the matching declaration node by name to use as call parent
        let Some(decl_proc) = find_named_child(node, "declProc") else {
            return;
        };

        let Some(name_node) = get_child_by_field(decl_proc, "name") else {
            return;
        };
        let full_name = get_node_text(name_node, self.source).trim().to_string();
        // full_name is like "TAuthService.Create"
        let short_name = full_name
            .rsplit('.')
            .next()
            .unwrap_or(&full_name)
            .to_string();
        let full_name_key = full_name.to_lowercase();
        let short_name_key = short_name.to_lowercase();

        // Build method index on first use (O(n) once, then O(1) per lookup)
        if self.method_index.is_none() {
            let mut index: HashMap<String, String> = HashMap::new();
            for n in &self.nodes {
                if n.kind == NodeKind::Method || n.kind == NodeKind::Function {
                    let name_key = n.name.to_lowercase();
                    // Keep first seen short-name mapping to avoid silently overwriting earlier entries.
                    index.entry(name_key).or_insert_with(|| n.id.clone());

                    // For Pascal methods, also index qualified forms (e.g. TAuthService.Create).
                    if n.kind == NodeKind::Method {
                        let qualified_parts: Vec<&str> = n.qualified_name.split("::").collect();
                        if qualified_parts.len() >= 2 {
                            // Create suffix keys so both "Module.Class.Method" and "Class.Method" can resolve.
                            for i in 0..qualified_parts.len() - 1 {
                                let scoped_name = qualified_parts[i..].join(".").to_lowercase();
                                index.insert(scoped_name, n.id.clone());
                            }
                        }
                    }
                }
            }
            self.method_index = Some(index);
        }

        let index = self.method_index.as_ref().expect("method index built");
        let parent_id = index
            .get(&full_name_key)
            .or_else(|| index.get(&short_name_key))
            .cloned()
            .or_else(|| self.node_stack.last().cloned());
        let Some(parent_id) = parent_id else { return };

        // Visit the block for calls
        if let Some(block) = find_named_child(node, "block") {
            self.node_stack.push(parent_id);
            self.visit_pascal_block(block);
            self.node_stack.pop();
        }
    }

    /// Extract function calls from a Pascal expression
    pub(super) fn extract_pascal_call(&mut self, node: SyntaxNode<'_>) {
        let Some(caller_id) = self.node_stack.last().cloned() else {
            return;
        };

        // Get the callee name — first child is typically the identifier or exprDot
        let Some(first_child) = node.named_child(0) else {
            return;
        };

        let mut callee_name = String::new();
        if first_child.kind() == "exprDot" {
            // Qualified call: Obj.Method(...)
            let identifiers: Vec<String> = named_children(first_child)
                .into_iter()
                .filter(|c| c.kind() == "identifier")
                .map(|c| get_node_text(c, self.source).to_string())
                .collect();
            if !identifiers.is_empty() {
                callee_name = identifiers.join(".");
            }
        } else if first_child.kind() == "identifier" {
            callee_name = get_node_text(first_child, self.source).to_string();
        }

        if !callee_name.is_empty() {
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: caller_id,
                reference_name: callee_name,
                reference_kind: EdgeKind::Calls,
                line: node.start_position().row as u32 + 1,
                column: node.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
                metadata: None,
            });
        }

        // Also visit arguments for nested calls
        if let Some(args) = find_named_child(node, "exprArgs") {
            self.visit_pascal_block(args);
        }
    }

    /// Recursively visit a Pascal block/statement tree for call expressions
    pub(super) fn visit_pascal_block(&mut self, node: SyntaxNode<'_>) {
        // Recursion guard — statement trees nest arbitrarily deep.
        ensure_sufficient_stack(|| {
            for child in named_children(node) {
                if child.kind() == "exprCall" {
                    self.extract_pascal_call(child);
                } else if child.kind() == "exprDot" {
                    // Check if exprDot contains an exprCall
                    for grandchild in named_children(child) {
                        if grandchild.kind() == "exprCall" {
                            self.extract_pascal_call(grandchild);
                        }
                    }
                } else {
                    self.visit_pascal_block(child);
                }
            }
        });
    }
}
