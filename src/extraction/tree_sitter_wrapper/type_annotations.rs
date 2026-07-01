use super::context::{find_named_child, named_children};
use super::extractor::TreeSitterExtractor;
use crate::ensure_sufficient_stack;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::SyntaxNode;
use crate::types::{EdgeKind, Language};

/// Languages that support type annotations (TypeScript, etc.)
pub(super) fn is_type_annotation_language(language: Language) -> bool {
    matches!(
        language,
        Language::Typescript
            | Language::Tsx
            | Language::Dart
            | Language::Kotlin
            | Language::Swift
            | Language::Rust
            | Language::Go
            | Language::Java
            | Language::Csharp
    )
}
/// Built-in/primitive type names that shouldn't create references
const BUILTIN_TYPES: &[&str] = &[
    "string",
    "number",
    "boolean",
    "void",
    "null",
    "undefined",
    "never",
    "any",
    "unknown",
    "object",
    "symbol",
    "bigint",
    "true",
    "false",
    // Rust
    "str",
    "bool",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "f32",
    "f64",
    "char",
    // Java/C#
    "int",
    "long",
    "short",
    "byte",
    "float",
    "double",
    // Go
    "int8",
    "int16",
    "int32",
    "int64",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "float32",
    "float64",
    "complex64",
    "complex128",
    "rune",
    "error",
];

impl<'a> TreeSitterExtractor<'a> {
    /// Extract type references from type annotations on a function/method/field node.
    /// Creates 'references' edges for parameter types, return types, and field types.
    pub(super) fn extract_type_annotations(&mut self, node: SyntaxNode<'_>, node_id: &str) {
        let Some(ext) = self.extractor else { return };
        if !is_type_annotation_language(self.language) {
            linkscope::event_fields(
                "codegraph.extract.type_annotations_unsupported",
                [
                    linkscope::TraceField::text("language", self.language.as_str()),
                    linkscope::TraceField::text("node_kind", node.kind()),
                ],
            );
            return;
        }

        // C# tree-sitter doesn't produce `type_identifier` leaves — it uses
        // `identifier`, `predefined_type`, `qualified_name`, `generic_name`,
        // etc. — so the generic walker below emits zero references for it.
        // Dispatch to a C#-aware path that only walks type-position subtrees
        // (the `type` field of a parameter/method/property/field), so
        // parameter NAMES never accidentally surface as type refs (#381).
        if self.language == Language::Csharp {
            self.extract_csharp_type_refs(node, node_id);
            return;
        }

        // Extract parameter type annotations
        let params_field = ext.params_field();
        let params_field = if params_field.is_empty() {
            "parameters"
        } else {
            params_field
        };
        if let Some(params) = get_child_by_field(node, params_field) {
            self.extract_type_refs_from_subtree(params, node_id);
        }

        // Extract return type annotation
        let return_field = match ext.return_field() {
            Some(f) if !f.is_empty() => f,
            _ => "return_type",
        };
        if let Some(return_type) = get_child_by_field(node, return_field) {
            self.extract_type_refs_from_subtree(return_type, node_id);
        }

        // Extract direct type annotation (for class fields like `model: ITextModel`)
        if let Some(type_annotation) = find_named_child(node, "type_annotation") {
            self.extract_type_refs_from_subtree(type_annotation, node_id);
        }
    }

    /// Extract C# type references from a node that owns a type position —
    /// a method/constructor declaration, a property declaration, or a
    /// field declaration (which wraps `variable_declaration → type`).
    ///
    /// Walks ONLY into known type fields, so parameter names like
    /// `request` in `Build(UserDto request)` are never mis-emitted as
    /// type references. Closes #381.
    pub(super) fn extract_csharp_type_refs(&mut self, node: SyntaxNode<'_>, node_id: &str) {
        // Return type / property type — the field is named `type`.
        if let Some(direct_type) = get_child_by_field(node, "type") {
            self.walk_csharp_type_position(direct_type, node_id);
        }

        // Field declarations wrap declarators in a `variable_declaration`
        // whose `type` field carries the type. The outer `field_declaration`
        // has no `type` field of its own, so the call above is a no-op here
        // and we descend one level.
        if let Some(var_decl) = find_named_child(node, "variable_declaration") {
            if let Some(vd_type) = get_child_by_field(var_decl, "type") {
                self.walk_csharp_type_position(vd_type, node_id);
            }
        }

        // Method / constructor parameters. The field name on
        // `method_declaration` is `parameters`; it points at a
        // `parameter_list` whose `parameter` children each have their own
        // `type` field. Walking ONLY the type field skips parameter NAMES,
        // which would otherwise mis-emit as type references.
        if let Some(params) = get_child_by_field(node, "parameters") {
            for child in named_children(params) {
                if child.kind() != "parameter" {
                    continue;
                }
                if let Some(param_type) = get_child_by_field(child, "type") {
                    self.walk_csharp_type_position(param_type, node_id);
                }
            }
        }
    }

    /// Walk a C# subtree that is KNOWN to be in a type position
    /// (return type, parameter type, property type, field type, generic
    /// argument). Identifiers here are type names, not parameter names.
    pub(super) fn walk_csharp_type_position(&mut self, node: SyntaxNode<'_>, from_node_id: &str) {
        // Recursion guard — generated generic types nest arbitrarily deep.
        ensure_sufficient_stack(|| self.walk_csharp_type_position_inner(node, from_node_id));
    }

    pub(super) fn walk_csharp_type_position_inner(
        &mut self,
        node: SyntaxNode<'_>,
        from_node_id: &str,
    ) {
        // `predefined_type` is int/string/bool/etc. — never a project ref.
        if node.kind() == "predefined_type" {
            return;
        }

        // Bare type name: `Foo` in `Foo bar`, or the `Foo` inside `List<Foo>`.
        if node.kind() == "identifier" {
            let name = get_node_text(node, self.source).to_string();
            if !name.is_empty() && !BUILTIN_TYPES.contains(&name.as_str()) {
                self.push_ref(from_node_id, name, EdgeKind::References, node);
            }
            return;
        }

        // `Namespace.Foo` → the rightmost identifier is the type. Emit the
        // trailing simple name as the reference.
        if node.kind() == "qualified_name" {
            let text = get_node_text(node, self.source);
            let last = text.split('.').next_back().unwrap_or(text).to_string();
            if !last.is_empty() && !BUILTIN_TYPES.contains(&last.as_str()) {
                self.push_ref(from_node_id, last, EdgeKind::References, node);
            }
            return;
        }

        // `(int Code, Foo Payload)` — tuple element has BOTH a `type` and a
        // `name` field; descending into all named children would mis-emit
        // the element name (`Code`, `Payload`) as a type ref. Walk only the
        // type field.
        if node.kind() == "tuple_element" {
            if let Some(t) = get_child_by_field(node, "type") {
                self.walk_csharp_type_position(t, from_node_id);
            }
            return;
        }

        // Composite type nodes — recurse into named children. Covers
        // `generic_name` (head identifier + `type_argument_list`),
        // `nullable_type`, `array_type`, `pointer_type`, `tuple_type`,
        // `ref_type`, and any newer wrapping shapes the grammar adds.
        // Identifiers reached here are all type-positional (parameter/field
        // names are gated out before we descend).
        for child in named_children(node) {
            self.walk_csharp_type_position(child, from_node_id);
        }
    }

    /// Extract type references from a variable's type annotation.
    pub(super) fn extract_variable_type_annotation(&mut self, node: SyntaxNode<'_>, node_id: &str) {
        if !is_type_annotation_language(self.language) {
            return;
        }

        // Find type_annotation child (covers TS `: Type`, Rust `: Type`, etc.)
        if let Some(type_annotation) = find_named_child(node, "type_annotation") {
            self.extract_type_refs_from_subtree(type_annotation, node_id);
        }
    }

    /// Recursively walk a subtree and extract all type_identifier references.
    /// Handles unions, intersections, generics, arrays, etc.
    pub(super) fn extract_type_refs_from_subtree(
        &mut self,
        node: SyntaxNode<'_>,
        from_node_id: &str,
    ) {
        // Recursion guard — generated type expressions nest arbitrarily deep.
        ensure_sufficient_stack(|| {
            if node.kind() == "type_identifier" {
                let type_name = get_node_text(node, self.source).to_string();
                if !type_name.is_empty() && !BUILTIN_TYPES.contains(&type_name.as_str()) {
                    self.push_ref(from_node_id, type_name, EdgeKind::References, node);
                }
                return; // type_identifier is a leaf
            }

            // Recurse into children (handles union_type, intersection_type, generic_type, etc.)
            for child in named_children(node) {
                self.extract_type_refs_from_subtree(child, from_node_id);
            }
        });
    }
}
