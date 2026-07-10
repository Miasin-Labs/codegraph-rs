use super::context::extract_name;
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_preceding_docstring};
use crate::extraction::tree_sitter_types::{NodeExtra, SyntaxNode};
use crate::types::{Edge, EdgeKind, NodeKind};

impl<'a> TreeSitterExtractor<'a> {
    /// Extract a method
    pub(super) fn extract_method(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        // For languages with receiver types (Go, Rust), include receiver in qualified name
        // so FTS can match "scrapeLoop.run" → qualified_name "...::scrapeLoop::run"
        let receiver_type = ext
            .get_receiver_type(node, self.source)
            .filter(|r| !r.is_empty());

        // For most languages, only extract as method if inside a class-like node
        // Languages with methods_are_top_level (e.g. Go) always treat them as methods
        // Languages with get_receiver_type (e.g. Rust) extract as method when receiver is found
        if !self.is_inside_class_like_node()
            && !ext.methods_are_top_level()
            && receiver_type.is_none()
        {
            // Skip method_definition nodes inside object literals (getters/setters/methods
            // in inline objects). These are ephemeral and create noise (e.g., Svelte context
            // objects: `ctx.set({ get view() { ... } })`).
            if node
                .parent()
                .is_some_and(|p| p.kind() == "object" || p.kind() == "object_expression")
            {
                let body = ext
                    .resolve_body(node, ext.body_field())
                    .or_else(|| get_child_by_field(node, ext.body_field()));
                if let Some(body) = body {
                    self.visit_function_body(body, "");
                }
                return;
            }
            // Not inside a class-like node and no receiver type, treat as function
            self.extract_function(node, None);
            return;
        }

        let name = extract_name(node, self.source, ext);

        // Check for misparse artifacts (e.g. C++ "switch" inside macro-confused class body)
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
        let is_async = ext.is_async(node, self.source);
        let is_static = ext.is_static(node, self.source);
        let return_type = ext.get_return_type(node, self.source);
        let qualified_name = receiver_type.as_ref().map(|r| format!("{}::{}", r, name));

        let Some(method_node) = self.create_node(
            NodeKind::Method,
            &name,
            node,
            NodeExtra {
                docstring,
                signature,
                visibility,
                is_async,
                is_static,
                return_type,
                qualified_name,
                ..Default::default()
            },
        ) else {
            return;
        };

        // For methods with a receiver type but no class-like parent on the stack
        // (e.g., Rust impl blocks), add a contains edge from the owning struct/trait
        if let Some(ref receiver) = receiver_type {
            if !self.is_inside_class_like_node() {
                let owner_id = self
                    .nodes
                    .iter()
                    .find(|n| {
                        n.name == *receiver
                            && n.file_path == self.file_path
                            && matches!(
                                n.kind,
                                NodeKind::Struct
                                    | NodeKind::Class
                                    | NodeKind::Enum
                                    | NodeKind::Trait
                            )
                    })
                    .map(|n| n.id.clone());
                if let Some(owner_id) = owner_id {
                    self.edges.push(Edge::new(
                        owner_id,
                        method_node.id.clone(),
                        EdgeKind::Contains,
                    ));
                }
            }
        }

        // Extract type annotations (parameter types and return type)
        self.extract_type_annotations(node, &method_node.id);

        // Extract decorators (`@Get('/list') list() {}`).
        self.extract_decorators_for(node, &method_node.id);

        // Push to stack and visit body
        self.node_stack.push(method_node.id.clone());
        let body = ext
            .resolve_body(node, ext.body_field())
            .or_else(|| get_child_by_field(node, ext.body_field()));
        if let Some(body) = body {
            self.visit_function_body(body, &method_node.id);
        }
        self.node_stack.pop();
    }
}
