use super::context::{named_children, strip_qualifier};
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{NodeExtra, SyntaxNode};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

impl<'a> TreeSitterExtractor<'a> {
    /// `new Foo(...)` / `Foo::new(...)` / object_creation_expression —
    /// emit an `instantiates` reference to the class name. The resolver
    /// then links it to the class node, producing the `instantiates`
    /// edge that powers "what creates instances of X" queries.
    ///
    /// Children are still walked so nested calls inside the constructor
    /// arguments (`new Foo(bar())`) get their own `calls` references.
    pub(super) fn extract_instantiation(&mut self, node: SyntaxNode<'_>) {
        let Some(from_id) = self.node_stack.last().cloned() else {
            return;
        };

        // The class name is in the `constructor`/`type`/first-named-child
        // depending on grammar.
        let ctor = get_child_by_field(node, "constructor")
            .or_else(|| get_child_by_field(node, "type"))
            .or_else(|| get_child_by_field(node, "name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };

        let mut class_name = get_node_text(ctor, self.source).to_string();
        // Strip type-argument suffix first: `new Map<K, V>()` would
        // otherwise produce class_name 'Map<K, V>' (the constructor
        // field is a `generic_type` node) and resolution would fail
        // because no class is named with the angle-bracket suffix.
        if let Some(lt_idx) = class_name.find('<') {
            if lt_idx > 0 {
                class_name.truncate(lt_idx);
            }
        }
        // For namespaced/qualified constructors (`new ns.Foo()`,
        // `new ns::Foo()`) keep the trailing identifier — that's what
        // matches a class node in the index.
        class_name = strip_qualifier(&class_name);
        class_name = class_name.trim().to_string();

        if !class_name.is_empty() {
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: from_id,
                reference_name: class_name,
                reference_kind: EdgeKind::Instantiates,
                line: node.start_position().row as u32 + 1,
                column: node.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
                metadata: None,
            });
        }
    }

    /// Find a `class_body` child of an `object_creation_expression` — the
    /// marker for an anonymous class (`new T() { ... }`). Returns the body
    /// node so the caller can walk it as the anon class's members.
    pub(super) fn find_anonymous_class_body<'t>(
        &self,
        node: SyntaxNode<'t>,
    ) -> Option<SyntaxNode<'t>> {
        named_children(node)
            .into_iter()
            // Java: `class_body`. C# uses the same node kind.
            .find(|c| c.kind() == "class_body" || c.kind() == "declaration_list")
    }

    /// Extract a Java/C# anonymous class — `new T() { ...members }`. Emits a
    /// `class` node named `<T$anon@line>`, an `extends` reference to T (so
    /// Phase 5.5 interface-impl can bridge), and walks the body so its
    /// `method_declaration` members become method nodes under the anon class.
    ///
    /// Why this matters: without anon-class extraction, the overrides inside
    /// a lambda-returned `new T() { @Override int foo(){...} }` are not nodes,
    /// so a call through T.foo (the abstract parent method) has no static
    /// target — the agent has to Read the file to find the implementation.
    pub(super) fn extract_anonymous_class(&mut self, node: SyntaxNode<'_>, body: SyntaxNode<'_>) {
        if self.extractor.is_none() {
            return;
        }

        // The instantiated type sits in the same field/position that
        // extract_instantiation reads from. Use the same lookup so the anon
        // class's `extends` target matches the `instantiates` edge.
        let type_node = get_child_by_field(node, "constructor")
            .or_else(|| get_child_by_field(node, "type"))
            .or_else(|| get_child_by_field(node, "name"))
            .or_else(|| node.named_child(0));
        let mut type_name = type_node
            .map(|t| get_node_text(t, self.source).to_string())
            .unwrap_or_else(|| "Object".to_string());
        if let Some(lt_idx) = type_name.find('<') {
            if lt_idx > 0 {
                type_name.truncate(lt_idx);
            }
        }
        type_name = strip_qualifier(&type_name);
        type_name = type_name.trim().to_string();
        if type_name.is_empty() {
            type_name = "Object".to_string();
        }

        let anon_name = format!("<{}$anon@{}>", type_name, node.start_position().row + 1);
        let Some(class_node) =
            self.create_node(NodeKind::Class, &anon_name, node, NodeExtra::default())
        else {
            return;
        };

        // The anonymous class implicitly extends/implements the named type.
        // We can't tell at extraction time whether T is a class or an interface,
        // so emit `extends`. Resolution will still bind T to whatever it is, and
        // Phase 5.5 (which already handles both `extends` and `implements`) will
        // bridge T's methods to the override names found in the anon body.
        // (TS quirk preserved: this reference's line is NOT +1-adjusted.)
        self.unresolved_references.push(UnresolvedReference {
            from_node_id: class_node.id.clone(),
            reference_name: type_name,
            reference_kind: EdgeKind::Extends,
            line: type_node
                .map(|t| t.start_position().row)
                .unwrap_or_else(|| node.start_position().row) as u32,
            column: type_node
                .map(|t| t.start_position().column)
                .unwrap_or_else(|| node.start_position().column) as u32,
            file_path: None,
            language: None,
            candidates: None,
            metadata: None,
        });

        // Walk the body's children so method_declaration nodes inside become
        // method nodes scoped to the anon class.
        self.node_stack.push(class_node.id);
        for child in named_children(body) {
            self.visit_node(child);
        }
        self.node_stack.pop();
    }
}
