use super::context::*;
use super::extractor::TreeSitterExtractor;
use super::type_annotations::is_type_annotation_language;
use crate::extraction::tree_sitter_helpers::*;
use crate::extraction::tree_sitter_types::*;
use crate::types::*;

impl<'a> TreeSitterExtractor<'a> {
    /// Extract a type alias (e.g. `export type X = ...` in TypeScript).
    /// For languages like Go, resolve_type_alias_kind detects when the type_spec
    /// wraps a struct or interface definition and creates the correct node kind.
    /// Returns true if children should be skipped (struct/interface handled body visiting).
    pub(super) fn extract_type_alias(&mut self, node: SyntaxNode<'_>) -> bool {
        let Some(ext) = self.extractor else {
            return false;
        };

        let name = extract_name(node, self.source, ext);
        if name == "<anonymous>" {
            return false;
        }
        let docstring = get_preceding_docstring(node, self.source);
        let is_exported = ext.is_exported(node, self.source);

        // Check if this type alias is actually a struct or interface definition
        // (e.g. Go: `type Foo struct { ... }` is a type_spec wrapping struct_type)
        let resolved_kind = ext.resolve_type_alias_kind(node, self.source);

        if resolved_kind == Some(NodeKind::Struct) {
            let Some(struct_node) = self.create_node(
                NodeKind::Struct,
                &name,
                node,
                NodeExtra {
                    docstring,
                    is_exported,
                    ..Default::default()
                },
            ) else {
                return true;
            };
            // Visit body children for field extraction
            self.node_stack.push(struct_node.id.clone());
            // Try Go-style 'type' field first, then find inner struct child (C typedef struct)
            let type_child = get_child_by_field(node, "type")
                .or_else(|| self.find_child_by_types(node, ext.struct_types()));
            if let Some(type_child) = type_child {
                // Extract struct embedding (e.g. Go: `type DB struct { *Head; Queryable }`)
                self.extract_inheritance(type_child, &struct_node.id);
                let body = get_child_by_field(type_child, ext.body_field()).unwrap_or(type_child);
                for child in named_children(body) {
                    self.visit_node(child);
                }
            }
            self.node_stack.pop();
            return true;
        }

        if resolved_kind == Some(NodeKind::Enum) {
            let Some(enum_node) = self.create_node(
                NodeKind::Enum,
                &name,
                node,
                NodeExtra {
                    docstring,
                    is_exported,
                    ..Default::default()
                },
            ) else {
                return true;
            };
            self.node_stack.push(enum_node.id.clone());
            // Find the inner enum type child (e.g. C: typedef enum { ... } name)
            let inner_enum = self.find_child_by_types(node, ext.enum_types());
            if let Some(inner_enum) = inner_enum {
                self.extract_inheritance(inner_enum, &enum_node.id);
                let body = ext
                    .resolve_body(inner_enum, ext.body_field())
                    .or_else(|| get_child_by_field(inner_enum, ext.body_field()));
                if let Some(body) = body {
                    for child in named_children(body) {
                        if ext.enum_member_types().contains(&child.kind()) {
                            self.extract_enum_members(child);
                        } else {
                            self.visit_node(child);
                        }
                    }
                }
            }
            self.node_stack.pop();
            return true;
        }

        if resolved_kind == Some(NodeKind::Interface) {
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
                return true;
            };
            // Extract interface inheritance from the inner type node
            if let Some(type_child) = get_child_by_field(node, "type") {
                self.extract_inheritance(type_child, &interface_node.id);
            }
            return true;
        }

        let type_alias_node = self.create_node(
            NodeKind::TypeAlias,
            &name,
            node,
            NodeExtra {
                docstring,
                is_exported,
                ..Default::default()
            },
        );

        // Extract type references from the alias value (e.g., `type X = ITextModel | null`)
        if let Some(type_alias_node) = type_alias_node {
            if is_type_annotation_language(self.language) {
                // The value is everything after the `=`, which is typically the last named child
                // In tree-sitter TS: type_alias_declaration has name + value children
                if let Some(value) = get_child_by_field(node, "value") {
                    self.extract_type_refs_from_subtree(value, &type_alias_node.id);
                    // `type X = { foo: T; bar(): T }` — make the members first-class
                    // property/method nodes under the type alias so `recorder.stop()`
                    // can attach the call edge to `RecorderHandle.stop` instead of
                    // an unrelated class method picked by path-proximity (#359).
                    if matches!(self.language, Language::Typescript | Language::Tsx) {
                        self.extract_ts_type_alias_members(value, &type_alias_node);
                    }
                }
            }
        }
        false
    }

    /// Surface the members of a TypeScript `type X = { ... }` (or intersection
    /// thereof) as `property` / `method` nodes under the type-alias node. Only
    /// walks the immediate object_type / intersection operands so anonymous
    /// nested object types inside generic arguments (`Promise<{ ok: true }>`)
    /// don't produce phantom members.
    pub(super) fn extract_ts_type_alias_members(
        &mut self,
        value: SyntaxNode<'_>,
        type_alias_node: &Node,
    ) {
        let mut object_types: Vec<SyntaxNode<'_>> = Vec::new();
        if value.kind() == "object_type" {
            object_types.push(value);
        } else if value.kind() == "intersection_type" {
            for op in named_children(value) {
                if op.kind() == "object_type" {
                    object_types.push(op);
                }
            }
        } else {
            return;
        }

        self.node_stack.push(type_alias_node.id.clone());
        for obj_type in object_types {
            for child in named_children(obj_type) {
                if child.kind() != "property_signature" && child.kind() != "method_signature" {
                    continue;
                }

                let member_name = get_child_by_field(child, "name")
                    .map(|n| get_node_text(n, self.source).to_string())
                    .unwrap_or_default();
                if member_name.is_empty() {
                    continue;
                }

                // `foo: () => T` and `foo(): T` are functionally a method on the
                // type contract. Treat the property_signature with a function-typed
                // annotation as a method too so call sites can resolve to it.
                // (Mirrors the TS chained ternary; the two Method arms are
                // deliberately distinct cases.)
                #[allow(clippy::if_same_then_else)]
                let member_kind = if child.kind() == "method_signature" {
                    NodeKind::Method
                } else if self.is_ts_function_typed_property(child) {
                    NodeKind::Method
                } else {
                    NodeKind::Property
                };

                let docstring = get_preceding_docstring(child, self.source);
                let signature = get_node_text(child, self.source).to_string();
                self.create_node(
                    member_kind,
                    &member_name,
                    child,
                    NodeExtra {
                        docstring,
                        signature: Some(signature),
                        qualified_name: Some(format!("{}::{}", type_alias_node.name, member_name)),
                        ..Default::default()
                    },
                );

                // Emit `references` edges from the type alias to types named in the
                // member's signature, matching the interface-member behavior added in
                // #432. We attach refs to the type-alias parent (consistent with
                // interface property_signature treatment).
                self.extract_type_annotations(child, &type_alias_node.id);
            }
        }
        self.node_stack.pop();
    }

    /// `foo: () => T` → property_signature whose type_annotation contains a
    /// `function_type`. Treat that as a method-shaped contract member, since
    /// the call site `obj.foo()` has identical semantics to `bar(): T`.
    pub(super) fn is_ts_function_typed_property(&self, property_signature: SyntaxNode<'_>) -> bool {
        let Some(type_anno) = get_child_by_field(property_signature, "type") else {
            return false;
        };
        named_children(type_anno)
            .into_iter()
            .any(|inner| inner.kind() == "function_type")
    }
}
