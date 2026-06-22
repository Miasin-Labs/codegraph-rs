use super::context::*;
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::*;
use crate::extraction::tree_sitter_types::*;
use crate::types::*;

impl<'a> TreeSitterExtractor<'a> {
    /// Extract a class property declaration (e.g. C# `public string Name { get; set; }`).
    /// Extracts as 'property' kind node inside the owning class.
    pub(super) fn extract_property(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        let docstring = get_preceding_docstring(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_static = ext.is_static(node, self.source).unwrap_or(false);

        let hook_name = ext.extract_property_name(node, self.source);
        let name = match hook_name {
            Some(h) => h,
            None => {
                let name_node = get_child_by_field(node, "name").or_else(|| {
                    named_children(node)
                        .into_iter()
                        .find(|c| c.kind() == "identifier")
                });
                match name_node {
                    Some(n) => get_node_text(n, self.source).to_string(),
                    None => return,
                }
            }
        };
        if name.is_empty() {
            return;
        }

        // Get property type from the type child (first named child that isn't modifier or identifier)
        let type_node = named_children(node).into_iter().find(|c| {
            !matches!(
                c.kind(),
                "modifier"
                    | "modifiers"
                    | "identifier"
                    | "accessor_list"
                    | "accessors"
                    | "equals_value_clause"
            )
        });
        let type_text = type_node.map(|t| get_node_text(t, self.source));
        let signature = match type_text {
            Some(t) => format!("{} {}", t, name),
            None => name.clone(),
        };

        let prop_node = self.create_node(
            NodeKind::Property,
            &name,
            node,
            NodeExtra {
                docstring,
                signature: Some(signature),
                visibility,
                is_static: Some(is_static),
                ..Default::default()
            },
        );

        // `@Inject() private svc: Foo` and similar — capture the
        // decorator->target relationship for class properties too.
        if let Some(prop_node) = prop_node {
            self.extract_decorators_for(node, &prop_node.id);
            // Emit `references` edges from the property to types named in its
            // type annotation (#381). The generic walker handles TS-style
            // `type_annotation` children; the C# branch walks the `type` field.
            self.extract_type_annotations(node, &prop_node.id);
        }
    }

    /// Extract a class field declaration (e.g. Java field_declaration, C# field_declaration).
    /// Extracts each declarator as a 'field' kind node inside the owning class.
    pub(super) fn extract_field(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        let docstring = get_preceding_docstring(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_static = ext.is_static(node, self.source).unwrap_or(false);

        // Java field_declaration: "private final String name = value;" → variable_declarator(s) are direct children
        // C# field_declaration: wraps in variable_declaration → variable_declarator(s)
        let mut declarators: Vec<SyntaxNode<'_>> = named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "variable_declarator")
            .collect();
        // C#: look inside variable_declaration wrapper
        if declarators.is_empty() {
            if let Some(var_decl) = find_named_child(node, "variable_declaration") {
                declarators = named_children(var_decl)
                    .into_iter()
                    .filter(|c| c.kind() == "variable_declarator")
                    .collect();
            }
        }

        // PHP property_declaration: property_element → variable_name → name
        if declarators.is_empty() {
            let prop_elements: Vec<SyntaxNode<'_>> = named_children(node)
                .into_iter()
                .filter(|c| c.kind() == "property_element")
                .collect();
            if !prop_elements.is_empty() {
                // Get type annotation if present (e.g. "string", "int", "?Foo")
                let type_node = named_children(node).into_iter().find(|c| {
                    !matches!(
                        c.kind(),
                        "visibility_modifier"
                            | "static_modifier"
                            | "readonly_modifier"
                            | "property_element"
                            | "var_modifier"
                    )
                });
                let type_text = type_node.map(|t| get_node_text(t, self.source).to_string());

                for elem in prop_elements {
                    let var_name = find_named_child(elem, "variable_name");
                    let name_node = var_name.and_then(|v| find_named_child(v, "name"));
                    let Some(name_node) = name_node else { continue };
                    let name = get_node_text(name_node, self.source).to_string();
                    let signature = match &type_text {
                        Some(t) => format!("{} ${}", t, name),
                        None => format!("${}", name),
                    };
                    self.create_node(
                        NodeKind::Field,
                        &name,
                        elem,
                        NodeExtra {
                            docstring: docstring.clone(),
                            signature: Some(signature),
                            visibility,
                            is_static: Some(is_static),
                            ..Default::default()
                        },
                    );
                }
                return;
            }
        }

        if !declarators.is_empty() {
            // Get field type from the type child
            // Java: type is a direct child of field_declaration
            // C#: type is inside variable_declaration wrapper
            let var_decl = find_named_child(node, "variable_declaration");
            let type_search_node = var_decl.unwrap_or(node);
            let type_node = named_children(type_search_node).into_iter().find(|c| {
                !matches!(
                    c.kind(),
                    "modifiers"
                        | "modifier"
                        | "variable_declarator"
                        | "variable_declaration"
                        | "marker_annotation"
                        | "annotation"
                )
            });
            let type_text = type_node.map(|t| get_node_text(t, self.source).to_string());

            for decl in declarators {
                let name_node = get_child_by_field(decl, "name").or_else(|| {
                    named_children(decl)
                        .into_iter()
                        .find(|c| c.kind() == "identifier")
                });
                let Some(name_node) = name_node else { continue };
                let name = get_node_text(name_node, self.source).to_string();
                let signature = match &type_text {
                    Some(t) => format!("{} {}", t, name),
                    None => name.clone(),
                };
                let field_node = self.create_node(
                    NodeKind::Field,
                    &name,
                    decl,
                    NodeExtra {
                        docstring: docstring.clone(),
                        signature: Some(signature),
                        visibility,
                        is_static: Some(is_static),
                        ..Default::default()
                    },
                );
                // Java/Kotlin annotations / TS field decorators sit on the
                // outer field_declaration, not on the individual declarator.
                if let Some(field_node) = field_node {
                    self.extract_decorators_for(node, &field_node.id);
                    // Same as properties: emit `references` to the field's annotated
                    // type. The outer `field_declaration` is the right scope to
                    // search from (#381).
                    self.extract_type_annotations(node, &field_node.id);
                }
            }
        } else {
            // Fallback: try to find an identifier child directly
            let name_node = get_child_by_field(node, "name").or_else(|| {
                named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")
            });
            if let Some(name_node) = name_node {
                let name = get_node_text(name_node, self.source).to_string();
                self.create_node(
                    NodeKind::Field,
                    &name,
                    node,
                    NodeExtra {
                        docstring,
                        visibility,
                        is_static: Some(is_static),
                        ..Default::default()
                    },
                );
            }
        }
    }
}
