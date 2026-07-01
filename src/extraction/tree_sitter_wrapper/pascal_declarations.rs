use super::context::{find_named_child, named_children};
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{NodeExtra, SyntaxNode};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

impl<'a> TreeSitterExtractor<'a> {
    /// Extract a Pascal declType node (class, interface, enum, or type alias)
    pub(super) fn extract_pascal_decl_type(&mut self, node: SyntaxNode<'_>) {
        let Some(name_node) = get_child_by_field(node, "name") else {
            return;
        };
        let name = get_node_text(name_node, self.source).to_string();

        // Find the inner type declaration
        let decl_class = find_named_child(node, "declClass");
        let decl_intf = find_named_child(node, "declIntf");
        let type_child = find_named_child(node, "type");

        if let Some(decl_class) = decl_class {
            if let Some(class_node) =
                self.create_node(NodeKind::Class, &name, node, NodeExtra::default())
            {
                // Extract inheritance from typeref children of declClass
                self.extract_pascal_inheritance(decl_class, &class_node.id);
                // Visit class body
                self.node_stack.push(class_node.id);
                for child in named_children(decl_class) {
                    self.visit_node(child);
                }
                self.node_stack.pop();
            }
        } else if let Some(decl_intf) = decl_intf {
            if let Some(iface_node) =
                self.create_node(NodeKind::Interface, &name, node, NodeExtra::default())
            {
                // Visit interface members
                self.node_stack.push(iface_node.id);
                for child in named_children(decl_intf) {
                    self.visit_node(child);
                }
                self.node_stack.pop();
            }
        } else if let Some(type_child) = type_child {
            // Check if it contains a declEnum
            if let Some(decl_enum) = find_named_child(type_child, "declEnum") {
                if let Some(enum_node) =
                    self.create_node(NodeKind::Enum, &name, node, NodeExtra::default())
                {
                    // Extract enum members
                    self.node_stack.push(enum_node.id);
                    for child in named_children(decl_enum) {
                        if child.kind() == "declEnumValue" {
                            if let Some(member_name) = get_child_by_field(child, "name") {
                                let member = get_node_text(member_name, self.source).to_string();
                                self.create_node(
                                    NodeKind::EnumMember,
                                    &member,
                                    child,
                                    NodeExtra::default(),
                                );
                            }
                        }
                    }
                    self.node_stack.pop();
                }
            } else {
                // Simple type alias: type TFoo = string / type TFoo = Integer
                self.create_node(NodeKind::TypeAlias, &name, node, NodeExtra::default());
            }
        } else {
            // Fallback: could be a forward declaration or simple alias
            self.create_node(NodeKind::TypeAlias, &name, node, NodeExtra::default());
        }
    }

    /// Extract Pascal uses clause into individual import nodes
    pub(super) fn extract_pascal_uses(&mut self, node: SyntaxNode<'_>) {
        let import_text = get_node_text(node, self.source).trim().to_string();
        for child in named_children(node) {
            if child.kind() == "moduleName" {
                let unit_name = get_node_text(child, self.source).to_string();
                self.create_node(
                    NodeKind::Import,
                    &unit_name,
                    child,
                    NodeExtra {
                        signature: Some(import_text.clone()),
                        ..Default::default()
                    },
                );
                // Create unresolved reference for resolution
                if let Some(parent_id) = self.node_stack.last().cloned() {
                    self.unresolved_references.push(UnresolvedReference {
                        from_node_id: parent_id,
                        reference_name: unit_name,
                        reference_kind: EdgeKind::Imports,
                        line: child.start_position().row as u32 + 1,
                        column: child.start_position().column as u32,
                        file_path: None,
                        language: None,
                        candidates: None,
                        metadata: None,
                    });
                }
            }
        }
    }

    /// Extract a Pascal constant declaration
    pub(super) fn extract_pascal_const(&mut self, node: SyntaxNode<'_>) {
        let Some(name_node) = get_child_by_field(node, "name") else {
            return;
        };
        let name = get_node_text(name_node, self.source).to_string();
        let default_value = find_named_child(node, "defaultValue");
        let sig = default_value.map(|d| get_node_text(d, self.source).to_string());
        self.create_node(
            NodeKind::Constant,
            &name,
            node,
            NodeExtra {
                signature: sig,
                ..Default::default()
            },
        );
    }

    /// Extract Pascal inheritance (extends/implements) from declClass typeref children
    pub(super) fn extract_pascal_inheritance(
        &mut self,
        decl_class: SyntaxNode<'_>,
        class_id: &str,
    ) {
        let typerefs: Vec<SyntaxNode<'_>> = named_children(decl_class)
            .into_iter()
            .filter(|c| c.kind() == "typeref")
            .collect();
        for (i, type_ref) in typerefs.into_iter().enumerate() {
            let name = get_node_text(type_ref, self.source).to_string();
            let kind = if i == 0 {
                EdgeKind::Extends
            } else {
                EdgeKind::Implements
            };
            self.push_ref(class_id, name, kind, type_ref);
        }
    }
}
