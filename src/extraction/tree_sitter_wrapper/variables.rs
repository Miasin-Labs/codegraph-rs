use super::context::{extract_name, find_named_child, init_signature, named_children};
use super::extractor::TreeSitterExtractor;
use super::value_references::{c_declarator_identifier, swift_property_info};
use crate::extraction::tree_sitter_helpers::{
    get_child_by_field,
    get_node_text,
    get_preceding_docstring,
    get_preceding_docstring_from_source,
};
use crate::extraction::tree_sitter_types::{NodeExtra, SyntaxNode};
use crate::types::{Language, NodeKind};

impl<'a> TreeSitterExtractor<'a> {
    /// Extract a variable declaration (const, let, var, etc.)
    ///
    /// Extracts top-level and module-level variable declarations.
    /// Captures the variable name and first 100 chars of initializer in signature for searchability.
    pub(super) fn extract_variable(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        // Different languages have different variable declaration structures
        // TypeScript/JavaScript: lexical_declaration contains variable_declarator children
        // Python: assignment has left (identifier) and right (value)
        // Go: var_declaration, short_var_declaration, const_declaration

        let is_const = ext.is_const(node, self.source).unwrap_or(false);
        let kind = if is_const {
            NodeKind::Constant
        } else {
            NodeKind::Variable
        };
        let is_js_like = matches!(
            self.language,
            Language::Typescript | Language::Javascript | Language::Tsx | Language::Jsx
        );
        let docstring = if is_js_like {
            get_preceding_docstring_from_source(self.source, node.start_byte())
        } else {
            get_preceding_docstring(node, self.source)
        };
        let is_exported = ext.is_exported(node, self.source).unwrap_or(false);

        // Extract variable declarators based on language
        if is_js_like {
            // Handle lexical_declaration and variable_declaration
            // These contain one or more variable_declarator children
            for child in named_children(node) {
                if child.kind() != "variable_declarator" {
                    continue;
                }
                let name_node = get_child_by_field(child, "name");
                let value_node = get_child_by_field(child, "value");

                let Some(name_node) = name_node else { continue };
                // Skip destructured patterns (e.g., `let { x, y } = $props()` in Svelte)
                // These produce ugly multi-line names like "{ class: className }"
                if name_node.kind() == "object_pattern" || name_node.kind() == "array_pattern" {
                    continue;
                }
                let name = get_node_text(name_node, self.source).to_string();
                // Arrow functions / function expressions: extract as function instead of variable
                if let Some(value) = value_node {
                    if value.kind() == "arrow_function" || value.kind() == "function_expression" {
                        self.extract_function(value, None);
                        continue;
                    }
                }

                // Capture first 100 chars of initializer for context (stored in signature for searchability)
                let init_sig = value_node.map(|v| init_signature(get_node_text(v, self.source)));

                let var_node = self.create_node(
                    kind,
                    &name,
                    child,
                    NodeExtra {
                        docstring: docstring.clone(),
                        signature: init_sig,
                        is_exported: Some(is_exported),
                        ..Default::default()
                    },
                );

                // Extract type annotation references (e.g., const x: ITextModel = ...)
                if let Some(ref var_node) = var_node {
                    self.extract_variable_type_annotation(child, &var_node.id);
                }

                // Exported const object-of-functions — extract each function-valued
                // property as a function named by its key + walk its body so its
                // calls are captured. Two shapes, both keyed on AST shape (not on any
                // library name):
                //   `export const actions = { default: async () => {} }` — object is
                //     the DIRECT value (SvelteKit form actions / handler maps / route
                //     tables).
                //   `export const useStore = create((set, get) => ({ fetchUser:
                //     async () => {} }))` — object is RETURNED by an initializer call,
                //     possibly through middleware wrappers (persist/devtools/immer).
                //     Covers Zustand/Redux/Pinia/MobX stores generically.
                // Scoped to EXPORTED consts to exclude inline-object noise
                // (`ctx.set({...})`) the object-method skip deliberately avoids.
                let object_of_fns = match value_node {
                    Some(v) if v.kind() == "object" || v.kind() == "object_expression" => Some(v),
                    Some(v) if v.kind() == "call_expression" => {
                        self.find_initializer_returned_object(v, 0)
                    }
                    _ => None,
                };
                let extract_object_methods = is_exported && object_of_fns.is_some();

                // Visit the initializer body for calls — EXCEPT object literals (their
                // function-valued properties are extracted below) and the store-factory
                // call whose returned object we extract method-by-method below (walking
                // the whole call would re-visit those method arrows and mis-attribute
                // their inner calls to the file/module scope).
                if let Some(value) = value_node {
                    if value.kind() != "object"
                        && value.kind() != "object_expression"
                        && !(extract_object_methods && value.kind() == "call_expression")
                    {
                        self.visit_function_body(value, "");
                    }
                }

                if extract_object_methods {
                    if let Some(obj) = object_of_fns {
                        self.extract_object_literal_functions(obj);
                    }
                }
            }
        } else if matches!(self.language, Language::Python | Language::Ruby) {
            // Python/Ruby assignment: left = right
            let left = get_child_by_field(node, "left").or_else(|| node.named_child(0));
            let right = get_child_by_field(node, "right").or_else(|| node.named_child(1));

            if let Some(left) = left {
                if left.kind() == "identifier"
                    || (self.language == Language::Ruby && left.kind() == "constant")
                {
                    let name = get_node_text(left, self.source).to_string();
                    // Python constants are usually UPPER_CASE
                    let init_sig = right.map(|r| init_signature(get_node_text(r, self.source)));

                    self.create_node(
                        kind,
                        &name,
                        node,
                        NodeExtra {
                            docstring,
                            signature: init_sig,
                            ..Default::default()
                        },
                    );
                }
            }
        } else if self.language == Language::Go {
            // Go: var_declaration, short_var_declaration, const_declaration.
            // Collect every var_spec/const_spec. A grouped `var ( ... )` wraps
            // its specs in a `var_spec_list` node, while a grouped
            // `const ( ... )` does NOT (its specs are direct children) — a
            // tree-sitter-go grammar asymmetry. Filtering only direct children
            // makes a grouped `var` block produce zero nodes, so flatten the
            // `*_spec_list` wrapper too.
            let mut specs: Vec<SyntaxNode<'_>> = Vec::new();
            for child in named_children(node) {
                if child.kind() == "var_spec" || child.kind() == "const_spec" {
                    specs.push(child);
                } else if child.kind() == "var_spec_list" || child.kind() == "const_spec_list" {
                    for inner in named_children(child) {
                        if inner.kind() == "var_spec" || inner.kind() == "const_spec" {
                            specs.push(inner);
                        }
                    }
                }
            }

            for spec in specs {
                let name_node = spec.named_child(0);
                if let Some(name_node) = name_node {
                    if name_node.kind() == "identifier" {
                        let name = get_node_text(name_node, self.source).to_string();
                        let value_node = if spec.named_child_count() > 1 {
                            spec.named_child(spec.named_child_count() as u32 - 1)
                        } else {
                            None
                        };
                        let init_sig =
                            value_node.map(|v| init_signature(get_node_text(v, self.source)));
                        let spec_kind = if node.kind() == "const_declaration" {
                            NodeKind::Constant
                        } else {
                            NodeKind::Variable
                        };
                        // Go export rule = leading uppercase. The is_exported
                        // hook reads the `name` field, so feed it the SPEC
                        // (whose `name` field IS the identifier) — NOT the
                        // declaration node (no `name` field -> always false).
                        // Recompute per-spec here; do NOT reuse the shared
                        // declaration-level `is_exported` above (which other
                        // languages compute off the declaration node).
                        let spec_exported = ext.is_exported(spec, self.source).unwrap_or(false);

                        let var_node = self.create_node(
                            spec_kind,
                            &name,
                            spec,
                            NodeExtra {
                                docstring: docstring.clone(),
                                signature: init_sig,
                                is_exported: Some(spec_exported),
                                ..Default::default()
                            },
                        );

                        // Walk the initializer so composite literals and calls
                        // in a package-level `var c = pkg.New()` (including a
                        // grouped `var ( logger = pkg.NewLogger("x") )`) are
                        // extracted as instantiates/calls dependencies — the
                        // body walker only covers initializers inside functions,
                        // not these top-level declarations. Scope the walk to the
                        // declared symbol so a call inside the initializer
                        // attributes to the var instead of leaking to the file
                        // node (which reads as "no caller"), issue #693.
                        if let Some(value_field) = get_child_by_field(spec, "value") {
                            if let Some(ref var_node) = var_node {
                                self.node_stack.push(var_node.id.clone());
                                self.visit_function_body(value_field, &var_node.id);
                                self.node_stack.pop();
                            } else {
                                self.visit_function_body(value_field, "");
                            }
                        }
                    }
                }
            }

            // Handle short_var_declaration (:=)
            if node.kind() == "short_var_declaration" {
                let left = get_child_by_field(node, "left");
                let right = get_child_by_field(node, "right");

                if let Some(left) = left {
                    // Can be expression_list with multiple identifiers
                    let identifiers: Vec<SyntaxNode<'_>> = if left.kind() == "expression_list" {
                        named_children(left)
                            .into_iter()
                            .filter(|c| c.kind() == "identifier")
                            .collect()
                    } else {
                        vec![left]
                    };

                    for id in identifiers {
                        let name = get_node_text(id, self.source).to_string();
                        let init_sig = right.map(|r| init_signature(get_node_text(r, self.source)));

                        self.create_node(
                            NodeKind::Variable,
                            &name,
                            node,
                            NodeExtra {
                                docstring: docstring.clone(),
                                signature: init_sig,
                                ..Default::default()
                            },
                        );
                    }
                }
            }
        } else if self.language == Language::Swift {
            let (name_node, is_let, is_computed) = swift_property_info(node, self.source);
            if let Some(name_node) = name_node.filter(|_| !is_computed) {
                self.create_node(
                    if is_let {
                        NodeKind::Constant
                    } else {
                        NodeKind::Variable
                    },
                    get_node_text(name_node, self.source),
                    node,
                    NodeExtra {
                        docstring,
                        is_exported: Some(is_exported),
                        ..Default::default()
                    },
                );
            }
        } else if self.language == Language::C {
            for child in named_children(node) {
                if !matches!(
                    child.kind(),
                    "init_declarator" | "pointer_declarator" | "array_declarator"
                ) {
                    continue;
                }
                let Some(name_node) = c_declarator_identifier(child) else {
                    continue;
                };
                let name = get_node_text(name_node, self.source);
                let value = if child.kind() == "init_declarator" {
                    get_child_by_field(child, "value")
                } else {
                    None
                };
                self.create_node(
                    kind,
                    name,
                    child,
                    NodeExtra {
                        docstring: docstring.clone(),
                        signature: value
                            .map(|value| init_signature(get_node_text(value, self.source))),
                        is_exported: Some(is_exported),
                        ..Default::default()
                    },
                );
            }
        } else if matches!(self.language, Language::Lua | Language::Luau) {
            // Lua/Luau: variable_declaration → assignment_statement → variable_list
            //      (name: identifier...) = expression_list. `local x, y = 1, 2`
            //      declares multiple names; only plain identifiers are locals.
            let assign = find_named_child(node, "assignment_statement").unwrap_or(node);
            let var_list = find_named_child(assign, "variable_list");
            let expr_list = find_named_child(assign, "expression_list");
            let values: Vec<SyntaxNode<'_>> = expr_list.map(named_children).unwrap_or_default();
            let names: Vec<SyntaxNode<'_>> = var_list
                .map(|v| {
                    named_children(v)
                        .into_iter()
                        .filter(|c| c.kind() == "identifier")
                        .collect()
                })
                .unwrap_or_default();
            for (i, name_node) in names.into_iter().enumerate() {
                let name = get_node_text(name_node, self.source).to_string();
                if name.is_empty() {
                    continue;
                }
                let init_sig = values
                    .get(i)
                    .map(|v| init_signature(get_node_text(*v, self.source)));
                self.create_node(
                    kind,
                    &name,
                    name_node,
                    NodeExtra {
                        docstring: docstring.clone(),
                        signature: init_sig,
                        is_exported: Some(is_exported),
                        ..Default::default()
                    },
                );
            }
        } else {
            // Generic fallback for other languages
            // Try to find identifier children
            for child in named_children(node) {
                if child.kind() == "identifier" || child.kind() == "variable_declarator" {
                    let name = if child.kind() == "identifier" {
                        get_node_text(child, self.source).to_string()
                    } else {
                        extract_name(child, self.source, ext)
                    };

                    if !name.is_empty() && name != "<anonymous>" {
                        let variable = self.create_node(
                            kind,
                            &name,
                            child,
                            NodeExtra {
                                docstring: docstring.clone(),
                                is_exported: Some(is_exported),
                                ..Default::default()
                            },
                        );
                        if let Some(variable) = variable {
                            self.extract_variable_type_annotation(node, &variable.id);
                        }
                    }
                }
            }
        }
    }
}
