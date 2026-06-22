use super::context::{extract_name, named_children};
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::{
    get_child_by_field,
    get_node_text,
    get_preceding_docstring,
};
use crate::extraction::tree_sitter_types::{NodeExtra, SyntaxNode};
use crate::types::NodeKind;

impl<'a> TreeSitterExtractor<'a> {
    /// Extract an interface/protocol/trait
    pub(super) fn extract_interface(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        let name = extract_name(node, self.source, ext);
        let docstring = get_preceding_docstring(node, self.source);
        let is_exported = ext.is_exported(node, self.source);

        let kind = ext.interface_kind();

        let Some(interface_node) = self.create_node(
            kind,
            &name,
            node,
            NodeExtra {
                docstring,
                is_exported,
                ..Default::default()
            },
        ) else {
            return;
        };

        // Extract extends (interface inheritance)
        self.extract_inheritance(node, &interface_node.id);

        // Visit body children for interface methods and nested types
        self.node_stack.push(interface_node.id.clone());
        let body = ext
            .resolve_body(node, ext.body_field())
            .or_else(|| get_child_by_field(node, ext.body_field()))
            .unwrap_or(node);
        for child in named_children(body) {
            self.visit_node(child);
        }
        self.node_stack.pop();
    }

    /// Extract a struct
    pub(super) fn extract_struct(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        // A missing body field is a definition for some grammars and a forward
        // declaration for others. Rust unit structs (`struct Unit;`) and tuple
        // structs (`struct T(u32)`) have no `field_declaration_list` body but
        // ARE complete definitions — dropping them loses marker/newtype types.
        // C/C++ forward declarations (`struct Foo;`) are NOT definitions and
        // must still be skipped. `struct_is_definition_without_body()` lets each
        // language decide; the default keeps the old skip-on-no-body behavior.
        let body = get_child_by_field(node, ext.body_field());
        if body.is_none() && !ext.struct_is_definition_without_body() {
            return;
        }

        let name = extract_name(node, self.source, ext);
        let docstring = get_preceding_docstring(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_exported = ext.is_exported(node, self.source);

        let Some(struct_node) = self.create_node(
            NodeKind::Struct,
            &name,
            node,
            NodeExtra {
                docstring,
                visibility,
                is_exported,
                ..Default::default()
            },
        ) else {
            return;
        };

        // Extract inheritance (e.g. Swift: struct HTTPMethod: RawRepresentable)
        self.extract_inheritance(node, &struct_node.id);
        // Rust `#[derive(Clone, Serialize, …)]` → Implements edges.
        self.extract_rust_derives(node, &struct_node.id);

        // Push to stack for field extraction. A unit struct has no body and
        // nothing to descend into. A tuple struct's positional fields live in an
        // `ordered_field_declaration_list` (no field names) — extract them as
        // index-named Field nodes (`0`, `1`, …) so the field types are still
        // reachable; a regular `field_declaration_list` is walked normally.
        self.node_stack.push(struct_node.id.clone());
        if let Some(body) = body {
            if body.kind() == "ordered_field_declaration_list" {
                self.extract_tuple_fields(body);
            } else {
                for child in named_children(body) {
                    self.visit_node(child);
                }
            }
        }
        self.node_stack.pop();
    }

    /// Extract positional fields of a tuple struct / tuple enum-variant
    /// (`struct Pair(pub u32, String)`). The grammar gives an
    /// `ordered_field_declaration_list` whose children are the field *types*
    /// (with an optional `visibility_modifier`); there are no field names, so
    /// each field is named by its positional index.
    pub(super) fn extract_tuple_fields(&mut self, body: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };
        let mut index = 0u32;
        for child in named_children(body) {
            // Skip non-type children (visibility modifiers, attributes).
            if matches!(
                child.kind(),
                "visibility_modifier" | "attribute_item" | "inner_attribute_item"
            ) {
                continue;
            }
            let type_text = get_node_text(child, self.source);
            let name = index.to_string();
            let visibility = ext.get_visibility(child, self.source);
            self.create_node(
                NodeKind::Field,
                &name,
                child,
                NodeExtra {
                    signature: Some(format!("{name}: {type_text}")),
                    visibility,
                    ..Default::default()
                },
            );
            index += 1;
        }
    }

    /// Extract an enum
    pub(super) fn extract_enum(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        // Skip forward declarations and type references (no body = not a definition)
        let Some(body) = ext
            .resolve_body(node, ext.body_field())
            .or_else(|| get_child_by_field(node, ext.body_field()))
        else {
            return;
        };

        let name = extract_name(node, self.source, ext);
        let docstring = get_preceding_docstring(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_exported = ext.is_exported(node, self.source);

        let Some(enum_node) = self.create_node(
            NodeKind::Enum,
            &name,
            node,
            NodeExtra {
                docstring,
                visibility,
                is_exported,
                ..Default::default()
            },
        ) else {
            return;
        };

        // Extract inheritance (e.g. Swift: enum AFError: Error)
        self.extract_inheritance(node, &enum_node.id);
        // Rust `#[derive(Clone, Serialize, …)]` → Implements edges.
        self.extract_rust_derives(node, &enum_node.id);

        // Push to stack and visit body children (enum members, nested types, methods)
        self.node_stack.push(enum_node.id.clone());

        for child in named_children(body) {
            if ext.enum_member_types().contains(&child.kind()) {
                self.extract_enum_members(child);
            } else {
                self.visit_node(child);
            }
        }
        self.node_stack.pop();
    }

    /// Extract enum member names from an enum member node.
    /// Handles multi-case declarations (Swift: `case put, delete`) and single-case patterns.
    pub(super) fn extract_enum_members(&mut self, node: SyntaxNode<'_>) {
        // Try field-based name first (e.g. Rust enum_variant has a 'name' field)
        if let Some(name_node) = get_child_by_field(node, "name") {
            let name = get_node_text(name_node, self.source).to_string();
            let member = self.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
            // Struct-variant (`V { x: T }`) and tuple-variant (`V(T, U)`) fields:
            // scope them under the variant so they're reachable as
            // `Enum::Variant::field`. The grammar puts them in the variant's
            // `body` field (a field_declaration_list or its ordered form).
            if let (Some(member), Some(body)) = (member, get_child_by_field(node, "body")) {
                self.node_stack.push(member.id.clone());
                if body.kind() == "ordered_field_declaration_list" {
                    self.extract_tuple_fields(body);
                } else {
                    for child in named_children(body) {
                        self.visit_node(child);
                    }
                }
                self.node_stack.pop();
            }
            return;
        }

        // Check for identifier-like children (Swift: simple_identifier, TS: property_identifier)
        let mut found = false;
        for child in named_children(node) {
            if matches!(
                child.kind(),
                "simple_identifier" | "identifier" | "property_identifier"
            ) {
                let name = get_node_text(child, self.source).to_string();
                self.create_node(NodeKind::EnumMember, &name, child, NodeExtra::default());
                found = true;
            }
        }

        // If the node itself IS the identifier (e.g. TS property_identifier directly in enum body)
        if !found && node.named_child_count() == 0 {
            let name = get_node_text(node, self.source).to_string();
            self.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
        }
    }
}
