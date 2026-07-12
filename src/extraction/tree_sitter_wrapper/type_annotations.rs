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
            | Language::Vyper
            | Language::Move
            | Language::Cairo
            | Language::Sway
            | Language::Fe
            | Language::Go
            | Language::Java
            | Language::Csharp
    )
}

fn is_web3_type_annotation_language(language: Language) -> bool {
    matches!(
        language,
        Language::Vyper | Language::Move | Language::Cairo | Language::Sway | Language::Fe
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

        if is_web3_type_annotation_language(self.language) {
            self.extract_web3_type_annotations(node, node_id);
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

    /// Web3 grammars disagree on both the parameter container and the type
    /// leaf kind. Select type-position subtrees here so identifier-shaped
    /// parameter names never reach the language-specific walker.
    fn extract_web3_type_annotations(&mut self, node: SyntaxNode<'_>, node_id: &str) {
        let owner = if self.language == Language::Cairo && node.kind() != "function" {
            find_named_child(node, "function").unwrap_or(node)
        } else {
            node
        };

        if let Some(direct_type) = get_child_by_field(owner, "type") {
            self.walk_web3_type_position(direct_type, node_id);
        }

        let parameters = get_child_by_field(owner, "parameters").or_else(|| {
            (self.language == Language::Fe)
                .then(|| find_named_child(owner, "parameter_list"))
                .flatten()
        });
        if let Some(parameters) = parameters {
            for parameter in named_children(parameters) {
                self.extract_web3_parameter_type(parameter, node_id);
            }
        }

        if let Some(return_type) = get_child_by_field(owner, "return_type") {
            self.walk_web3_type_position(return_type, node_id);
        }

        if let Some(type_annotation) = find_named_child(owner, "type_annotation") {
            self.walk_web3_type_position(type_annotation, node_id);
        }
    }

    fn extract_web3_parameter_type(&mut self, parameter: SyntaxNode<'_>, node_id: &str) {
        if let Some(parameter_type) = get_child_by_field(parameter, "type") {
            self.walk_web3_type_position(parameter_type, node_id);
            return;
        }

        // Move wraps mutable parameters in `mut_function_parameter`.
        if parameter.kind() == "mut_function_parameter" {
            for child in named_children(parameter) {
                self.extract_web3_parameter_type(child, node_id);
            }
            return;
        }

        // Cairo/Sway permit bare implicit type entries in their parameter
        // container. Their type names are `type_identifier`, so walking these
        // direct children cannot pick up an ordinary parameter identifier.
        if matches!(self.language, Language::Cairo | Language::Sway)
            && matches!(
                parameter.kind(),
                "type_identifier"
                    | "scoped_type_identifier"
                    | "generic_type"
                    | "tuple_type"
                    | "snapshot_type"
                    | "array_type"
                    | "reference_type"
            )
        {
            self.walk_web3_type_position(parameter, node_id);
        }
    }

    fn walk_web3_type_position(&mut self, node: SyntaxNode<'_>, node_id: &str) {
        ensure_sufficient_stack(|| self.walk_web3_type_position_inner(node, node_id));
    }

    fn walk_web3_type_position_inner(&mut self, node: SyntaxNode<'_>, node_id: &str) {
        if node.kind() == "type_identifier" {
            self.push_web3_type_ref(node_id, get_node_text(node, self.source), node);
            return;
        }

        match self.language {
            Language::Vyper => match node.kind() {
                "identifier" => {
                    self.push_web3_type_ref(node_id, get_node_text(node, self.source), node);
                    return;
                }
                // The grammar can collapse a bare annotation into a leaf `type`.
                "type" if node.named_child_count() == 0 => {
                    self.push_web3_type_ref(node_id, get_node_text(node, self.source), node);
                    return;
                }
                // `pkg.Token`: the package is a qualifier, not a second type.
                "member_type" => {
                    self.push_web3_type_ref(node_id, get_node_text(node, self.source), node);
                    return;
                }
                // A subscript's index is a size expression, not a type.
                "subscript" => {
                    if let Some(value) = get_child_by_field(node, "value") {
                        self.walk_web3_type_position(value, node_id);
                    }
                    return;
                }
                _ => {}
            },
            Language::Move => match node.kind() {
                "module_access" => {
                    self.push_web3_type_ref(node_id, get_node_text(node, self.source), node);
                    for child in named_children(node) {
                        if child.kind() == "type_arguments" {
                            self.walk_web3_type_position(child, node_id);
                        }
                    }
                    return;
                }
                "primitive_type" => return,
                _ => {}
            },
            Language::Fe => match node.kind() {
                "path" => {
                    self.push_web3_type_ref(node_id, get_node_text(node, self.source), node);
                    return;
                }
                // Only the element is type-positioned; the length is an expression.
                "array_type" => {
                    if let Some(element) = get_child_by_field(node, "element") {
                        self.walk_web3_type_position(element, node_id);
                    }
                    return;
                }
                // `Item = Type`: Item names an associated slot, not a type use.
                "assoc_type_generic_arg" => {
                    if let Some(value) = get_child_by_field(node, "type") {
                        self.walk_web3_type_position(value, node_id);
                    }
                    return;
                }
                "self_type" | "never_type" => return,
                _ => {}
            },
            Language::Cairo | Language::Sway => {}
            _ => return,
        }

        for child in named_children(node) {
            self.walk_web3_type_position(child, node_id);
        }
    }

    fn push_web3_type_ref(&mut self, from_node_id: &str, raw: &str, node: SyntaxNode<'_>) {
        let raw = raw.split('<').next().unwrap_or(raw).trim();
        let name = raw
            .rsplit("::")
            .next()
            .unwrap_or(raw)
            .rsplit('.')
            .next()
            .unwrap_or(raw)
            .trim_start_matches(['$', '@', '&', '*'])
            .trim();
        let language_builtin = match self.language {
            Language::Vyper => {
                matches!(
                    name,
                    "String"
                        | "Bytes"
                        | "DynArray"
                        | "HashMap"
                        | "constant"
                        | "decimal"
                        | "immutable"
                        | "indexed"
                        | "address"
                        | "public"
                        | "transient"
                ) || ["bytes", "int", "uint"].iter().any(|prefix| {
                    name.strip_prefix(prefix).is_some_and(|width| {
                        !width.is_empty() && width.bytes().all(|b| b.is_ascii_digit())
                    })
                })
            }
            Language::Move => name == "vector",
            Language::Cairo => matches!(name, "bytes31" | "felt252" | "i256" | "u256"),
            Language::Sway => matches!(name, "b256" | "u256"),
            Language::Fe => matches!(name, "address" | "b256" | "bytes31" | "i256" | "u256"),
            _ => false,
        };
        if !name.is_empty() && name != "Self" && !language_builtin && !BUILTIN_TYPES.contains(&name)
        {
            self.push_ref(from_node_id, name.to_string(), EdgeKind::References, node);
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

        if is_web3_type_annotation_language(self.language) {
            self.extract_web3_type_annotations(node, node_id);
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
        if is_web3_type_annotation_language(self.language) {
            self.walk_web3_type_position(node, from_node_id);
            return;
        }

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

#[cfg(test)]
mod tests {
    use crate::extraction::languages::extractor_for;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{EdgeKind, Language};

    #[test]
    fn web3_parameter_and_return_types_do_not_emit_parameter_names() {
        let cases = [
            (
                Language::Vyper,
                "contract.vy",
                "def ping(request: Request) -> Response:\n    pass\n",
            ),
            (
                Language::Move,
                "module.move",
                "module 0x1::m { public fun ping(request: Request): Response { abort 0 } }",
            ),
            (
                Language::Cairo,
                "module.cairo",
                "fn ping(request: Request) -> Response { request }",
            ),
            (
                Language::Sway,
                "module.sw",
                "script; fn ping(request: Request) -> Response { revert(0) }",
            ),
            (
                Language::Fe,
                "module.fe",
                "pub fn ping(request: Request) -> Response {}",
            ),
        ];

        for (language, path, source) in cases {
            let result =
                TreeSitterExtractor::new(path, source, Some(language), extractor_for(language))
                    .extract();
            assert!(result.errors.is_empty(), "{language}: {:?}", result.errors);

            let mut references: Vec<_> = result
                .unresolved_references
                .iter()
                .filter(|reference| reference.reference_kind == EdgeKind::References)
                .map(|reference| reference.reference_name.as_str())
                .collect();
            references.sort_unstable();
            assert_eq!(
                references,
                ["Request", "Response"],
                "{language}: nodes={:#?}, unresolved={:#?}",
                result.nodes,
                result.unresolved_references
            );
        }
    }

    #[test]
    fn web3_primitive_names_remain_references_in_rust() {
        let source = r#"
struct u256;
struct i256;
struct address;
struct b256;
struct bytes31;
struct felt252;

fn use_all(a: u256, b: i256, c: address, d: b256, e: bytes31) -> felt252 {}
"#;
        let result = TreeSitterExtractor::new(
            "types.rs",
            source,
            Some(Language::Rust),
            extractor_for(Language::Rust),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let mut references: Vec<_> = result
            .unresolved_references
            .iter()
            .filter(|reference| reference.reference_kind == EdgeKind::References)
            .map(|reference| reference.reference_name.as_str())
            .collect();
        references.sort_unstable();
        assert_eq!(
            references,
            ["address", "b256", "bytes31", "felt252", "i256", "u256"]
        );
    }
}
