use super::calls::INSTANTIATION_KINDS;
use super::context::{extract_name, named_children};
use super::extractor::TreeSitterExtractor;
use crate::ensure_sufficient_stack;
use crate::extraction::tree_sitter_types::{ClassLikeKind, SyntaxNode};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

impl<'a> TreeSitterExtractor<'a> {
    /// Visit function body and extract calls (and structural nodes).
    ///
    /// In addition to call expressions, this also detects class/struct/enum
    /// definitions inside function bodies. This handles two cases:
    ///   1. Local class/struct/enum definitions (valid in C++, Java, etc.)
    ///   2. C++ macro misparsing — macros like NLOHMANN_JSON_NAMESPACE_BEGIN cause
    ///      tree-sitter to interpret the namespace block as a function_definition,
    ///      hiding real class/struct/enum nodes inside the "function body".
    pub(super) fn visit_function_body(&mut self, body: SyntaxNode<'_>, _function_id: &str) {
        if self.extractor.is_none() {
            return;
        }
        self.visit_for_calls_and_structure(body);
    }

    /// The TS `visitForCallsAndStructure` inner closure.
    ///
    /// Recursion guard: this is a second walker independent of `visit_node` —
    /// it descends whole function bodies, whose statement nesting is exactly
    /// where real-world depth blowups live (the llvm-project overflow
    /// recursed here).
    pub(super) fn visit_for_calls_and_structure(&mut self, node: SyntaxNode<'_>) {
        ensure_sufficient_stack(|| self.visit_for_calls_and_structure_inner(node));
    }

    pub(super) fn visit_for_calls_and_structure_inner(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };
        let node_type = node.kind();

        if ext.call_types().contains(&node_type) {
            self.extract_call(node);
        } else if INSTANTIATION_KINDS.contains(&node_type) {
            // `new Foo()` inside a function body — emit an `instantiates`
            // reference. Without this branch the body walker only knew
            // about `call_expression`, so constructor invocations
            // produced no graph edges at all.
            self.extract_instantiation(node);
            // Anonymous class with body: `new T() { ... }` (Java/C#). Extract as
            // a class so interface-impl synthesis (Phase 5.5) can bridge T's
            // methods to the overrides — same rationale as in visit_node.
            if let Some(anon_body) = self.find_anonymous_class_body(node) {
                self.extract_anonymous_class(node, anon_body);
                return;
            }
        } else if let Some(callee_name) = ext.extract_bare_call(node, self.source) {
            if !callee_name.is_empty() {
                if let Some(caller_id) = self.node_stack.last().cloned() {
                    self.unresolved_references.push(UnresolvedReference {
                        from_node_id: caller_id,
                        reference_name: callee_name,
                        reference_kind: EdgeKind::Calls,
                        line: node.start_position().row as u32 + 1,
                        column: node.start_position().column as u32,
                        file_path: None,
                        language: None,
                        candidates: None,
                        metadata: None,
                    });
                }
            }
        }

        // Value-path reference (Rust `let p = UnitStruct;`) → a `References`
        // edge from the enclosing scope to the referenced symbol.
        if let Some(value_ref) = ext.extract_value_reference(node, self.source) {
            if let Some(scope_id) = self.node_stack.last().cloned() {
                self.unresolved_references.push(UnresolvedReference {
                    from_node_id: scope_id,
                    reference_name: value_ref,
                    reference_kind: EdgeKind::References,
                    line: node.start_position().row as u32 + 1,
                    column: node.start_position().column as u32,
                    file_path: None,
                    language: None,
                    candidates: None,
                    metadata: None,
                });
            }
        }

        // Nested NAMED functions inside a body — function declarations and named
        // function expressions like `.on('mount', function onmount(){})` — become
        // their own nodes so the graph can link to them (callback handlers, local
        // helpers). Anonymous arrows/expressions fall through to the default
        // recursion below, keeping their inner calls attributed to the enclosing
        // function: this bounds the new nodes to NAMED functions only (no explosion,
        // no lost edges). extract_function walks the nested body itself, so we return.
        if ext.function_types().contains(&node_type) {
            let nested_name = extract_name(node, self.source, ext);
            if !nested_name.is_empty() && nested_name != "<anonymous>" {
                self.extract_function(node, None);
                return;
            }
        }

        // Extract structural nodes found inside function bodies.
        // Each extract method visits its own children, so we return after extracting.
        if ext.class_types().contains(&node_type) {
            match ext.classify_class_node(node, self.source) {
                ClassLikeKind::Struct => self.extract_struct(node),
                ClassLikeKind::Enum => self.extract_enum(node),
                ClassLikeKind::Interface => self.extract_interface(node),
                ClassLikeKind::Trait => self.extract_class(node, NodeKind::Trait),
                ClassLikeKind::Class => self.extract_class(node, NodeKind::Class),
            }
            return;
        }
        if ext.struct_types().contains(&node_type) {
            self.extract_struct(node);
            return;
        }
        if ext.enum_types().contains(&node_type) {
            self.extract_enum(node);
            return;
        }
        if ext.interface_types().contains(&node_type) {
            self.extract_interface(node);
            return;
        }

        for child in named_children(node) {
            self.visit_for_calls_and_structure(child);
        }
    }
}
