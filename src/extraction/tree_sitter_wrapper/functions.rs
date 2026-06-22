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
    /// Extract a function
    pub(super) fn extract_function(&mut self, node: SyntaxNode<'_>, name_override: Option<&str>) {
        let Some(ext) = self.extractor else { return };

        // If the language provides get_receiver_type and this function has a receiver
        // (e.g., Rust function_item inside an impl block), extract as method instead
        if ext
            .get_receiver_type(node, self.source)
            .is_some_and(|r| !r.is_empty())
        {
            self.extract_method(node);
            return;
        }

        // name_override is supplied only for explicitly-named anonymous functions the
        // caller resolved itself (e.g. arrow values of exported-const object members
        // — SvelteKit actions). Inline-object arrows reached by the general walker
        // get no override, so they still fall through to the <anonymous> skip below.
        let mut name = match name_override {
            Some(n) => n.to_string(),
            None => extract_name(node, self.source, ext),
        };
        // For arrow functions and function expressions assigned to variables,
        // resolve the name from the parent variable_declarator.
        // e.g. `export const useAuth = () => { ... }` — the arrow_function node
        // has no `name` field; the name lives on the variable_declarator.
        if name_override.is_none()
            && name == "<anonymous>"
            && (node.kind() == "arrow_function" || node.kind() == "function_expression")
        {
            if let Some(parent) = node.parent() {
                if parent.kind() == "variable_declarator" {
                    if let Some(var_name) = get_child_by_field(parent, "name") {
                        name = get_node_text(var_name, self.source).to_string();
                    }
                }
            }
        }
        if name == "<anonymous>" {
            // Don't emit a node for the anonymous wrapper itself, but still visit its
            // body: AMD/RequireJS and CommonJS module wrappers (`define([], function(){…})`,
            // `(function(){…})()`) hold named inner functions and calls that would
            // otherwise be lost — the dispatcher set skip_children, so nothing else
            // descends into this subtree. (#528)
            let body = ext
                .resolve_body(node, ext.body_field())
                .or_else(|| get_child_by_field(node, ext.body_field()));
            if let Some(body) = body {
                self.visit_function_body(body, "");
            }
            return;
        }

        // Check for misparse artifacts (e.g. C++ macros causing "namespace detail" functions)
        // Skip the node but still visit the body for calls and structural nodes
        if ext.is_misparsed_function(&name, node) {
            let body = ext
                .resolve_body(node, ext.body_field())
                .or_else(|| get_child_by_field(node, ext.body_field()));
            if let Some(body) = body {
                self.visit_function_body(body, "");
            }
            return;
        }

        let docstring = get_preceding_docstring(node, self.source);
        let signature = ext.get_signature(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_exported = ext.is_exported(node, self.source);
        let is_async = ext.is_async(node, self.source);
        let is_static = ext.is_static(node, self.source);

        let Some(func_node) = self.create_node(
            NodeKind::Function,
            &name,
            node,
            NodeExtra {
                docstring,
                signature,
                visibility,
                is_exported,
                is_async,
                is_static,
                ..Default::default()
            },
        ) else {
            return;
        };

        // Extract type annotations (parameter types and return type)
        self.extract_type_annotations(node, &func_node.id);

        // Extract decorators applied to the function (rare in JS/TS but
        // present in Python `@decorator def f():` and Java/Kotlin
        // annotations on free functions).
        self.extract_decorators_for(node, &func_node.id);

        // Push to stack and visit body
        self.node_stack.push(func_node.id.clone());
        let body = ext
            .resolve_body(node, ext.body_field())
            .or_else(|| get_child_by_field(node, ext.body_field()));
        if let Some(body) = body {
            self.visit_function_body(body, &func_node.id);
        }
        self.node_stack.pop();
    }

    /// Extract a class
    pub(super) fn extract_class(&mut self, node: SyntaxNode<'_>, kind: NodeKind) {
        let Some(ext) = self.extractor else { return };

        let name = extract_name(node, self.source, ext);
        let docstring = get_preceding_docstring(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_exported = ext.is_exported(node, self.source);

        let Some(class_node) = self.create_node(
            kind,
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

        // Extract extends/implements
        self.extract_inheritance(node, &class_node.id);

        // Extract decorators applied to the class (`@Foo class X {}`).
        self.extract_decorators_for(node, &class_node.id);

        // Push to stack and visit body
        self.node_stack.push(class_node.id.clone());
        let body = ext
            .resolve_body(node, ext.body_field())
            .or_else(|| get_child_by_field(node, ext.body_field()))
            .unwrap_or(node);

        // Visit all children for methods and properties
        for child in named_children(body) {
            self.visit_node(child);
        }
        self.node_stack.pop();
    }
}
