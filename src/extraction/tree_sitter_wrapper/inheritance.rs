use super::context::{find_named_child, named_children};
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::SyntaxNode;
use crate::types::{EdgeKind, UnresolvedReference};

impl<'a> TreeSitterExtractor<'a> {
    /// Push an unresolved reference (shared shorthand for inheritance refs).
    pub(super) fn push_ref(
        &mut self,
        from_node_id: &str,
        reference_name: String,
        reference_kind: EdgeKind,
        pos_node: SyntaxNode<'_>,
    ) {
        self.unresolved_references.push(UnresolvedReference {
            from_node_id: from_node_id.to_string(),
            reference_name,
            reference_kind,
            line: pos_node.start_position().row as u32 + 1,
            column: pos_node.start_position().column as u32,
            file_path: None,
            language: None,
            candidates: None,
        });
    }

    /// Extract inheritance relationships
    pub(super) fn extract_inheritance(&mut self, node: SyntaxNode<'_>, class_id: &str) {
        // Objective-C @interface MyClass : NSObject <ProtoA, ProtoB>
        if node.kind() == "class_interface" {
            if let Some(superclass) = get_child_by_field(node, "superclass") {
                let name = get_node_text(superclass, self.source).to_string();
                self.push_ref(class_id, name, EdgeKind::Extends, superclass);
            }
            for arg_list in named_children(node) {
                if arg_list.kind() != "parameterized_arguments" {
                    continue;
                }
                for type_name in named_children(arg_list) {
                    let type_id = named_children(type_name)
                        .into_iter()
                        .find(|c| c.kind() == "type_identifier" || c.kind() == "identifier");
                    let Some(type_id) = type_id else { continue };
                    let protocol_name = get_node_text(type_id, self.source).to_string();
                    self.push_ref(class_id, protocol_name, EdgeKind::Implements, type_id);
                }
            }
            return;
        }

        // Look for extends/implements clauses
        for child in named_children(node) {
            if matches!(
                child.kind(),
                "extends_clause" | "superclass" | "base_clause" /* PHP class extends */
                | "extends_interfaces" // Java interface extends
            ) {
                // Extract parent class/interface names
                // Java uses type_list wrapper: superclass -> type_identifier,
                // extends_interfaces -> type_list -> type_identifier
                let type_list = find_named_child(child, "type_list");
                let targets: Vec<Option<SyntaxNode<'_>>> = match type_list {
                    Some(tl) => named_children(tl).into_iter().map(Some).collect(),
                    None => vec![child.named_child(0)],
                };
                for target in targets.into_iter().flatten() {
                    let name = get_node_text(target, self.source).to_string();
                    self.push_ref(class_id, name, EdgeKind::Extends, target);
                }
            }

            // C++ base classes: `class Derived : public Base, private Other` →
            // base_class_clause holds access specifiers + base type(s). Emit an extends
            // ref per base type (skip the public/private/protected keywords).
            if child.kind() == "base_class_clause" {
                for t in named_children(child) {
                    if matches!(
                        t.kind(),
                        "type_identifier" | "qualified_identifier" | "template_type"
                    ) {
                        let name = get_node_text(t, self.source).to_string();
                        self.push_ref(class_id, name, EdgeKind::Extends, t);
                    }
                }
            }

            if matches!(
                child.kind(),
                "implements_clause" | "class_interface_clause"
                | "super_interfaces" // Java class implements
                | "interfaces" // Dart
            ) {
                // Extract implemented interfaces
                // Java uses type_list wrapper: super_interfaces -> type_list -> type_identifier
                let type_list = find_named_child(child, "type_list");
                let targets: Vec<SyntaxNode<'_>> = match type_list {
                    Some(tl) => named_children(tl),
                    None => named_children(child),
                };
                for iface in targets {
                    let name = get_node_text(iface, self.source).to_string();
                    self.push_ref(class_id, name, EdgeKind::Implements, iface);
                }
            }

            // Python superclass list: `class Flask(Scaffold, Mixin):`
            // argument_list contains identifier children for each parent class
            if child.kind() == "argument_list" && node.kind() == "class_definition" {
                for arg in named_children(child) {
                    if arg.kind() == "identifier" || arg.kind() == "attribute" {
                        let name = get_node_text(arg, self.source).to_string();
                        self.push_ref(class_id, name, EdgeKind::Extends, arg);
                    }
                }
            }

            // Go interface embedding: `type Querier interface { LabelQuerier; ... }`
            // constraint_elem wraps the embedded interface type identifier
            if child.kind() == "constraint_elem" {
                if let Some(type_id) = find_named_child(child, "type_identifier") {
                    let name = get_node_text(type_id, self.source).to_string();
                    self.push_ref(class_id, name, EdgeKind::Extends, type_id);
                }
            }

            // Go struct embedding: field_declaration without field_identifier
            // e.g. `type DB struct { *Head; Queryable }` — no field name means embedded type
            if child.kind() == "field_declaration" {
                let has_field_identifier = named_children(child)
                    .into_iter()
                    .any(|c| c.kind() == "field_identifier");
                if !has_field_identifier {
                    if let Some(type_id) = find_named_child(child, "type_identifier") {
                        let name = get_node_text(type_id, self.source).to_string();
                        self.push_ref(class_id, name, EdgeKind::Extends, type_id);
                    }
                }
            }

            // Rust trait supertraits: `trait SubTrait: SuperTrait + Display { ... }`
            // trait_bounds contains type_identifier, generic_type, or higher_ranked_trait_bound children
            if child.kind() == "trait_bounds" {
                for bound in named_children(child) {
                    let mut type_name: Option<String> = None;
                    let mut pos_node: Option<SyntaxNode<'_>> = None;

                    if bound.kind() == "type_identifier" {
                        type_name = Some(get_node_text(bound, self.source).to_string());
                        pos_node = Some(bound);
                    } else if bound.kind() == "generic_type" {
                        // e.g. `Deserialize<'de>`
                        if let Some(inner) = find_named_child(bound, "type_identifier") {
                            type_name = Some(get_node_text(inner, self.source).to_string());
                            pos_node = Some(inner);
                        }
                    } else if bound.kind() == "higher_ranked_trait_bound" {
                        // e.g. `for<'de> Deserialize<'de>`
                        let generic = find_named_child(bound, "generic_type");
                        let type_id = generic
                            .and_then(|g| find_named_child(g, "type_identifier"))
                            .or_else(|| find_named_child(bound, "type_identifier"));
                        if let Some(type_id) = type_id {
                            type_name = Some(get_node_text(type_id, self.source).to_string());
                            pos_node = Some(type_id);
                        }
                    }

                    if let (Some(type_name), Some(pos_node)) = (type_name, pos_node) {
                        self.push_ref(class_id, type_name, EdgeKind::Extends, pos_node);
                    }
                }
            }

            // C#: `class Movie : BaseItem, IPlugin` → base_list with identifier children
            // base_list combines both base class and interfaces in a single colon-separated list.
            // We emit all as 'extends' since the syntax doesn't distinguish them.
            if child.kind() == "base_list" {
                for base_type in named_children(child) {
                    // For generic base types like `ClientBase<T>`, extract just the type name
                    let name = if base_type.kind() == "generic_name" {
                        let inner = named_children(base_type)
                            .into_iter()
                            .find(|c| c.kind() == "identifier")
                            .unwrap_or(base_type);
                        get_node_text(inner, self.source).to_string()
                    } else {
                        get_node_text(base_type, self.source).to_string()
                    };
                    self.push_ref(class_id, name, EdgeKind::Extends, base_type);
                }
            }

            // Kotlin: `class Foo : Bar, Baz` → delegation_specifier > user_type > type_identifier
            // Also handles `class Foo : Bar()` → delegation_specifier > constructor_invocation > user_type
            if child.kind() == "delegation_specifier" {
                let user_type = find_named_child(child, "user_type");
                let constructor_invocation = find_named_child(child, "constructor_invocation");
                let target = user_type.or(constructor_invocation);
                if let Some(target) = target {
                    let type_id = if target.kind() == "user_type" {
                        find_named_child(target, "type_identifier").unwrap_or(target)
                    } else {
                        let inner_user_type = find_named_child(target, "user_type");
                        inner_user_type
                            .and_then(|ut| find_named_child(ut, "type_identifier"))
                            .or(inner_user_type)
                            .unwrap_or(target)
                    };
                    let name = get_node_text(type_id, self.source).to_string();
                    self.push_ref(class_id, name, EdgeKind::Extends, type_id);
                }
            }

            // Swift: inheritance_specifier > user_type > type_identifier
            // Used for class inheritance, protocol conformance, and protocol inheritance
            if child.kind() == "inheritance_specifier" {
                let user_type = find_named_child(child, "user_type");
                let type_id = user_type.and_then(|ut| find_named_child(ut, "type_identifier"));
                if let Some(type_id) = type_id {
                    let name = get_node_text(type_id, self.source).to_string();
                    self.push_ref(class_id, name, EdgeKind::Extends, type_id);
                }
            }

            // JavaScript class_heritage has bare identifier without extends_clause wrapper
            // e.g. `class Foo extends Bar {}` → class_heritage → identifier("Bar")
            if (child.kind() == "identifier" || child.kind() == "type_identifier")
                && node.kind() == "class_heritage"
            {
                let name = get_node_text(child, self.source).to_string();
                self.push_ref(class_id, name, EdgeKind::Extends, child);
            }

            // Recurse into container nodes (e.g. field_declaration_list in Go structs,
            // class_heritage in TypeScript which wraps extends_clause/implements_clause)
            if child.kind() == "field_declaration_list" || child.kind() == "class_heritage" {
                self.extract_inheritance(child, class_id);
            }
        }
    }
}
