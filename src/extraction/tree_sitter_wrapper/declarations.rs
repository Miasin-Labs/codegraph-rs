use super::context::{named_children, now_ms};
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::extraction::tree_sitter_types::{NodeExtra, SyntaxNode};
use crate::types::{Edge, EdgeKind, Node, NodeKind};

impl<'a> TreeSitterExtractor<'a> {
    /// Create a Node object
    pub(super) fn create_node(
        &mut self,
        kind: NodeKind,
        name: &str,
        node: SyntaxNode<'_>,
        extra: NodeExtra,
    ) -> Option<Node> {
        // Skip nodes with empty/missing names — they are not meaningful symbols
        // and would cause FK violations when edges reference them (see issue #42)
        if name.is_empty() {
            return None;
        }

        let start_line = node.start_position().row as u32 + 1;
        let id = generate_node_id(&self.file_path, kind, name, start_line);

        // Some grammars (e.g. Dart) model a function/method body as a *sibling* of
        // the signature node, so the declaration node's own range is just the
        // signature line. Extend end_line to the resolved body when it sits beyond
        // the node so the node spans its body. Guarded to only ever extend.
        // end_byte is extended in lockstep so the byte range covers the body too.
        let mut end_line = node.end_position().row as u32 + 1;
        let mut end_byte = node.end_byte() as u32;
        if matches!(kind, NodeKind::Function | NodeKind::Method) {
            if let Some(ext) = self.extractor {
                if let Some(body) = ext.resolve_body(node, ext.body_field()) {
                    let body_end = body.end_position().row as u32 + 1;
                    if body_end > end_line {
                        end_line = body_end;
                    }
                    let body_end_byte = body.end_byte() as u32;
                    if body_end_byte > end_byte {
                        end_byte = body_end_byte;
                    }
                }
            }
        }

        let qualified_name = extra
            .qualified_name
            .unwrap_or_else(|| self.build_qualified_name(name));

        let new_node = Node {
            id: id.clone(),
            kind,
            name: name.to_string(),
            qualified_name,
            file_path: self.file_path.clone(),
            language: self.language,
            start_line,
            end_line,
            start_column: node.start_position().column as u32,
            end_column: node.end_position().column as u32,
            start_byte: Some(node.start_byte() as u32),
            end_byte: Some(end_byte),
            address: None,
            size: None,
            docstring: extra.docstring,
            signature: extra.signature,
            visibility: extra.visibility,
            is_exported: extra.is_exported,
            is_async: extra.is_async,
            is_static: extra.is_static,
            is_abstract: extra.is_abstract,
            decorators: None,
            type_parameters: None,
            updated_at: now_ms(),
        };

        self.nodes.push(new_node.clone());

        // Add containment edge from parent
        if let Some(parent_id) = self.node_stack.last() {
            self.edges
                .push(Edge::new(parent_id.clone(), id, EdgeKind::Contains));
        }

        Some(new_node)
    }

    /// Find first named child whose type is in the given list.
    /// Used to locate inner type nodes (e.g. enum_specifier inside a typedef).
    pub(super) fn find_child_by_types<'t>(
        &self,
        node: SyntaxNode<'t>,
        types: &[&str],
    ) -> Option<SyntaxNode<'t>> {
        named_children(node)
            .into_iter()
            .find(|c| types.contains(&c.kind()))
    }

    /// Find a `package_types` child under the root, create a `namespace` node
    /// for it, and return its id so the caller can scope top-level
    /// declarations underneath. Returns None when no package header is
    /// present (script files, .kts without a package).
    pub(super) fn extract_file_package(&mut self, root_node: SyntaxNode<'_>) -> Option<String> {
        let ext = self.extractor?;
        let types = ext.package_types();
        if types.is_empty() {
            return None;
        }

        let pkg_node = named_children(root_node)
            .into_iter()
            .find(|c| types.contains(&c.kind()))?;

        let pkg_name = ext.extract_package(pkg_node, self.source)?;
        if pkg_name.is_empty() {
            return None;
        }

        let ns = self.create_node(
            NodeKind::Namespace,
            &pkg_name,
            pkg_node,
            NodeExtra::default(),
        );
        ns.map(|n| n.id)
    }

    /// Build qualified name from node stack
    pub(super) fn build_qualified_name(&self, name: &str) -> String {
        // Build a qualified name from the semantic hierarchy only (no file path).
        // The file path is stored separately in file_path and pollutes FTS if included here.
        let mut parts: Vec<&str> = Vec::new();
        for node_id in &self.node_stack {
            if let Some(node) = self.nodes.iter().find(|n| &n.id == node_id) {
                if node.kind != NodeKind::File {
                    parts.push(&node.name);
                }
            }
        }
        parts.push(name);
        parts.join("::")
    }

    /// Check if the current node stack indicates we are inside a class-like node
    /// (class, struct, interface, trait). File nodes do not count as class-like.
    pub(super) fn is_inside_class_like_node(&self) -> bool {
        let Some(parent_id) = self.node_stack.last() else {
            return false;
        };
        let Some(parent_node) = self.nodes.iter().find(|n| &n.id == parent_id) else {
            return false;
        };
        // A `Module` is a namespace scope. Some languages (Ruby) hold methods
        // directly in a module body and need it to count as class-like; others
        // (Rust) treat free `fn`/`const`/`static` in a `mod` as functions and
        // module-level variables. The per-language flag decides.
        if parent_node.kind == NodeKind::Module {
            return self.extractor.is_some_and(|ext| ext.module_is_class_like());
        }
        matches!(
            parent_node.kind,
            NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Interface
                | NodeKind::Trait
                | NodeKind::Enum
                // An enum *variant* holds fields too (Rust struct-variant
                // `V { x: T }`), so it is a field-bearing scope.
                | NodeKind::EnumMember
        )
    }
}
