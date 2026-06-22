use super::context::*;
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::*;
use crate::extraction::tree_sitter_types::*;
use crate::types::*;

impl<'a> TreeSitterExtractor<'a> {
    /// Extract an import
    ///
    /// Creates an import node with the full import statement stored in signature for searchability.
    /// Also creates unresolved references for resolution purposes.
    pub(super) fn extract_import(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        let import_text = get_node_text(node, self.source).trim().to_string();

        // Try language-specific hook first
        let mut hook_declined = false;
        match ext.extract_import(node, self.source) {
            ImportOutcome::Info(info) => {
                self.create_node(
                    NodeKind::Import,
                    &info.module_name,
                    node,
                    NodeExtra {
                        signature: Some(info.signature),
                        ..Default::default()
                    },
                );
                // Create unresolved reference unless the hook handled it
                if !info.handled_refs && !info.module_name.is_empty() {
                    if let Some(parent_id) = self.node_stack.last().cloned() {
                        self.unresolved_references.push(UnresolvedReference {
                            from_node_id: parent_id,
                            reference_name: info.module_name,
                            reference_kind: EdgeKind::Imports,
                            line: node.start_position().row as u32 + 1,
                            column: node.start_position().column as u32,
                            file_path: None,
                            language: None,
                            candidates: None,
                        });
                    }
                }
                return;
            }
            // Hook returned null — fall through to multi-import inline handlers only
            // (hook declining means "I didn't handle this" for multi-import cases,
            // NOT "use generic fallback" — the hook already declined)
            ImportOutcome::Declined => hook_declined = true,
            ImportOutcome::NotHandled => {}
        }

        // Multi-import cases that create multiple nodes (can't be expressed with single-return hook)

        // Python import_statement: import os, sys (creates one import per module)
        if self.language == Language::Python && node.kind() == "import_statement" {
            for child in named_children(node) {
                if child.kind() == "dotted_name" {
                    let name = get_node_text(child, self.source).to_string();
                    self.create_node(
                        NodeKind::Import,
                        &name,
                        node,
                        NodeExtra {
                            signature: Some(import_text.clone()),
                            ..Default::default()
                        },
                    );
                } else if child.kind() == "aliased_import" {
                    if let Some(dotted_name) = find_named_child(child, "dotted_name") {
                        let name = get_node_text(dotted_name, self.source).to_string();
                        self.create_node(
                            NodeKind::Import,
                            &name,
                            node,
                            NodeExtra {
                                signature: Some(import_text.clone()),
                                ..Default::default()
                            },
                        );
                    }
                }
            }
            return;
        }

        // Go imports: single or grouped (creates one import per spec)
        if self.language == Language::Go {
            let parent_id = self.node_stack.last().cloned();

            if let Some(import_spec_list) = find_named_child(node, "import_spec_list") {
                let specs: Vec<SyntaxNode<'_>> = named_children(import_spec_list)
                    .into_iter()
                    .filter(|c| c.kind() == "import_spec")
                    .collect();
                for spec in specs {
                    self.extract_go_import_spec(spec, parent_id.as_deref());
                }
            } else if let Some(import_spec) = find_named_child(node, "import_spec") {
                self.extract_go_import_spec(import_spec, parent_id.as_deref());
            }
            return;
        }

        // PHP grouped imports: use X\{A, B} (creates one import per item)
        if self.language == Language::Php {
            let namespace_prefix = find_named_child(node, "namespace_name");
            let use_group = find_named_child(node, "namespace_use_group");
            if let (Some(namespace_prefix), Some(use_group)) = (namespace_prefix, use_group) {
                let prefix = get_node_text(namespace_prefix, self.source).to_string();
                let use_clauses: Vec<SyntaxNode<'_>> = named_children(use_group)
                    .into_iter()
                    .filter(|c| {
                        c.kind() == "namespace_use_group_clause"
                            || c.kind() == "namespace_use_clause"
                    })
                    .collect();
                for clause in use_clauses {
                    let ns_name = find_named_child(clause, "namespace_name");
                    let name = match ns_name {
                        Some(ns) => find_named_child(ns, "name"),
                        None => find_named_child(clause, "name"),
                    };
                    if let Some(name) = name {
                        let full_path = format!("{}\\{}", prefix, get_node_text(name, self.source));
                        self.create_node(
                            NodeKind::Import,
                            &full_path,
                            node,
                            NodeExtra {
                                signature: Some(import_text.clone()),
                                ..Default::default()
                            },
                        );
                    }
                }
                return;
            }
        }

        // If a hook exists but returned null, it intentionally declined this node — don't create fallback
        if hook_declined {
            return;
        }

        // Generic fallback for languages without hooks
        self.create_node(
            NodeKind::Import,
            &import_text.clone(),
            node,
            NodeExtra {
                signature: Some(import_text),
                ..Default::default()
            },
        );
    }

    /// One Go import spec → import node + unresolved `imports` reference.
    pub(super) fn extract_go_import_spec(&mut self, spec: SyntaxNode<'_>, parent_id: Option<&str>) {
        let Some(string_literal) = find_named_child(spec, "interpreted_string_literal") else {
            return;
        };
        let import_path = get_node_text(string_literal, self.source).replace(['\'', '"'], "");
        if import_path.is_empty() {
            return;
        }
        let signature = get_node_text(spec, self.source).trim().to_string();
        self.create_node(
            NodeKind::Import,
            &import_path,
            spec,
            NodeExtra {
                signature: Some(signature),
                ..Default::default()
            },
        );
        // Create unresolved reference so the resolver can create imports edges
        if let Some(parent_id) = parent_id {
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: parent_id.to_string(),
                reference_name: import_path,
                reference_kind: EdgeKind::Imports,
                line: spec.start_position().row as u32 + 1,
                column: spec.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
            });
        }
    }
}
