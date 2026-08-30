mod declarations;
mod shadowing;

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;

pub(super) use declarations::{c_declarator_identifier, swift_property_info};
use shadowing::prune_shadowed_targets;

use super::context::named_children;
use super::extractor::TreeSitterExtractor;
use crate::extraction::generated_detection::is_generated_file;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::SyntaxNode;
use crate::types::{Edge, EdgeKind, Language, Metadata, NodeKind};

const MAX_VALUE_REFERENCE_NODES: usize = 20_000;

#[derive(Default)]
pub(super) struct ValueReferenceState {
    targets: HashMap<String, String>,
    target_counts: HashMap<String, usize>,
    scopes: Vec<ValueReferenceScope>,
}

struct ValueReferenceScope {
    graph_id: String,
    syntax_id: usize,
    name: String,
}

impl TreeSitterExtractor<'_> {
    pub(super) fn is_class_scope_constant_assignment(&self, node: SyntaxNode<'_>) -> bool {
        node.kind() == "assignment"
            && get_child_by_field(node, "left")
                .or_else(|| node.named_child(0))
                .is_some_and(|left| left.kind() == "constant")
    }

    pub(super) fn capture_value_reference_scope(
        &mut self,
        kind: NodeKind,
        name: &str,
        graph_id: &str,
        node: SyntaxNode<'_>,
    ) {
        let target_kind = if self.language == Language::Pascal {
            kind == NodeKind::Constant
        } else {
            matches!(kind, NodeKind::Constant | NodeKind::Variable)
        };
        if target_kind
            && name.len() >= 3
            && name
                .bytes()
                .any(|byte| byte == b'_' || byte.is_ascii_uppercase())
        {
            let parent_is_shared_scope = self.node_stack.last().is_some_and(|parent_id| {
                self.nodes.iter().any(|parent| {
                    parent.id == *parent_id
                        && matches!(
                            parent.kind,
                            NodeKind::File
                                | NodeKind::Class
                                | NodeKind::Module
                                | NodeKind::Struct
                                | NodeKind::Enum
                        )
                })
            });
            if parent_is_shared_scope {
                self.value_references
                    .targets
                    .insert(name.to_string(), graph_id.to_string());
                *self
                    .value_references
                    .target_counts
                    .entry(name.to_string())
                    .or_default() += 1;
            }
        }

        if matches!(
            kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Constant | NodeKind::Variable
        ) {
            self.value_references.scopes.push(ValueReferenceScope {
                graph_id: graph_id.to_string(),
                syntax_id: node.id(),
                name: name.to_string(),
            });
        }
    }

    pub(super) fn flush_value_references(&mut self, root: SyntaxNode<'_>) {
        let mut state = std::mem::take(&mut self.value_references);
        if !self.value_references_enabled
            || !supports_value_references(self.language)
            || state.targets.is_empty()
            || state.scopes.is_empty()
            || is_generated_file(&self.file_path)
        {
            return;
        }

        prune_shadowed_targets(root, self.source, &mut state);
        if state.targets.is_empty() {
            return;
        }

        let wanted: HashSet<_> = state.scopes.iter().map(|scope| scope.syntax_id).collect();
        let mut syntax_nodes = HashMap::with_capacity(wanted.len());
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if wanted.contains(&node.id()) {
                syntax_nodes.insert(node.id(), node);
            }
            stack.extend(named_children(node));
        }

        for scope in state.scopes {
            let Some(scope_node) = syntax_nodes.get(&scope.syntax_id).copied() else {
                continue;
            };
            self.emit_value_references_for_scope(scope_node, &scope, &state.targets);
        }
    }

    fn emit_value_references_for_scope(
        &mut self,
        scope_node: SyntaxNode<'_>,
        scope: &ValueReferenceScope,
        targets: &HashMap<String, String>,
    ) {
        let mut stack = vec![scope_node];
        if let Some(sibling) = scope_node.next_named_sibling() {
            if matches!(sibling.kind(), "function_body" | "block") {
                stack.push(sibling);
            }
        }
        let mut seen = HashSet::new();
        let mut visited = 0;
        while let Some(node) = stack.pop() {
            if visited >= MAX_VALUE_REFERENCE_NODES {
                break;
            }
            visited += 1;
            if matches!(
                node.kind(),
                "identifier" | "constant" | "name" | "simple_identifier"
            ) {
                let reference_name = get_node_text(node, self.source);
                if let Some(target_id) = targets.get(reference_name) {
                    if target_id != &scope.graph_id
                        && reference_name != scope.name
                        && seen.insert(target_id.clone())
                    {
                        let mut metadata = Metadata::new();
                        metadata.insert("valueRef".to_string(), serde_json::Value::Bool(true));
                        let mut edge = Edge::new(
                            scope.graph_id.clone(),
                            target_id.clone(),
                            EdgeKind::References,
                        );
                        edge.metadata = Some(metadata);
                        self.edges.push(edge);
                        self.unresolved_references.retain(|reference| {
                            reference.from_node_id != scope.graph_id
                                || reference.reference_name != reference_name
                                || reference.reference_kind != EdgeKind::References
                        });
                    }
                }
            }
            stack.extend(named_children(node));
        }
    }
}

const fn supports_value_references(language: Language) -> bool {
    matches!(
        language,
        Language::Typescript
            | Language::Javascript
            | Language::Tsx
            | Language::Arkts
            | Language::Go
            | Language::Python
            | Language::Rust
            | Language::Ruby
            | Language::C
            | Language::Java
            | Language::Csharp
            | Language::Php
            | Language::Scala
            | Language::Kotlin
            | Language::Swift
            | Language::Dart
            | Language::Pascal
    )
}

pub(super) fn value_references_enabled(value: Option<&OsStr>) -> bool {
    value != Some(OsStr::new("0"))
}
