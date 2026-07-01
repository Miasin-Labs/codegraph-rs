//! PHP language extraction config.
//!
//! Ported from `src/extraction/languages/php.ts`.

use super::{find_named_child, named_children};
use crate::extraction::tree_sitter_helpers::get_node_text;
use crate::extraction::tree_sitter_types::{
    ClassLikeKind,
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference, Visibility};

pub struct PhpExtractor;

impl LanguageExtractor for PhpExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &[&str] {
        &["class_declaration", "trait_declaration"]
    }
    fn method_types(&self) -> &[&str] {
        &["method_declaration"]
    }
    fn interface_types(&self) -> &[&str] {
        &["interface_declaration"]
    }
    fn struct_types(&self) -> &[&str] {
        &[]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_declaration"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enum_case"]
    }
    fn type_alias_types(&self) -> &[&str] {
        &[]
    }
    fn import_types(&self) -> &[&str] {
        &["namespace_use_declaration"]
    }
    fn call_types(&self) -> &[&str] {
        &[
            "function_call_expression",
            "member_call_expression",
            "scoped_call_expression",
        ]
    }
    fn variable_types(&self) -> &[&str] {
        &["const_declaration"]
    }
    fn field_types(&self) -> &[&str] {
        &["property_declaration"]
    }
    fn name_field(&self) -> &str {
        "name"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    fn params_field(&self) -> &str {
        "parameters"
    }
    fn return_field(&self) -> Option<&str> {
        Some("return_type")
    }

    fn classify_class_node(&self, node: SyntaxNode<'_>, _source: &str) -> ClassLikeKind {
        if node.kind() == "trait_declaration" {
            ClassLikeKind::Trait
        } else {
            ClassLikeKind::Class
        }
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "visibility_modifier" {
                    match get_node_text(child, source) {
                        "public" => return Some(Visibility::Public),
                        "private" => return Some(Visibility::Private),
                        "protected" => return Some(Visibility::Protected),
                        _ => {}
                    }
                }
            }
        }
        // PHP defaults to public
        Some(Visibility::Public)
    }

    fn is_static(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "static_modifier" {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        // Handle class constants: const_declaration inside classes
        // These are skipped by the main visitor because variableTypes check
        // excludes class-like contexts
        if node.kind() == "const_declaration" {
            let const_elements: Vec<_> = named_children(node)
                .into_iter()
                .filter(|c| c.kind() == "const_element")
                .collect();
            for elem in const_elements {
                let Some(name_node) = find_named_child(elem, "name") else {
                    continue;
                };
                let name = get_node_text(name_node, ctx.source()).to_string();
                ctx.create_node(NodeKind::Constant, &name, elem, NodeExtra::default());
            }
            return true; // handled
        }

        // Handle trait usage: use TraitName, OtherTrait; inside classes
        // Creates unresolved references that will be resolved to 'implements' edges
        if node.kind() == "use_declaration" {
            let names: Vec<_> = named_children(node)
                .into_iter()
                .filter(|c| c.kind() == "name" || c.kind() == "qualified_name")
                .collect();
            let parent_id = ctx.node_stack().last().cloned();
            if let Some(parent_id) = parent_id {
                for name_node in names {
                    let trait_name = get_node_text(name_node, ctx.source()).to_string();
                    let file_path = ctx.file_path().to_string();
                    ctx.add_unresolved_reference(UnresolvedReference {
                        from_node_id: parent_id.clone(),
                        reference_name: trait_name,
                        reference_kind: EdgeKind::Implements,
                        file_path: Some(file_path),
                        line: node.start_position().row as u32 + 1,
                        column: node.start_position().column as u32,
                        language: None,
                        candidates: None,
                        metadata: None,
                    });
                }
            }
            return true; // handled
        }

        false
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();

        // Check for grouped imports: use X\{A, B} - return null for core fallback
        let namespace_prefix = find_named_child(node, "namespace_name");
        let use_group = find_named_child(node, "namespace_use_group");
        if namespace_prefix.is_some() && use_group.is_some() {
            // Grouped imports create multiple nodes - let core handle
            return ImportOutcome::Declined;
        }

        // Single import - find namespace_use_clause
        if let Some(use_clause) = find_named_child(node, "namespace_use_clause") {
            if let Some(qualified_name) = find_named_child(use_clause, "qualified_name") {
                return ImportOutcome::Info(ImportInfo::new(
                    get_node_text(qualified_name, source),
                    import_text,
                ));
            }
            if let Some(name) = find_named_child(use_clause, "name") {
                return ImportOutcome::Info(ImportInfo::new(
                    get_node_text(name, source),
                    import_text,
                ));
            }
        }
        ImportOutcome::Declined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn php_smoke_extraction() {
        let source = "<?php\nuse App\\Services\\Mailer;\n\ntrait Loggable {\n    public function log() {}\n}\n\nclass Order {\n    use Loggable;\n    const STATUS = 'open';\n\n    private static function persist() {\n        save();\n    }\n}\n\nfunction save() {}\n";
        let result = TreeSitterExtractor::new(
            "src/Order.php",
            source,
            Some(Language::Php),
            Some(&PhpExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result.nodes.iter().find(|n| n.name == "Order").unwrap();
        assert_eq!(class.kind, NodeKind::Class);

        // trait_declaration classified as trait via classify_class_node
        let loggable = result.nodes.iter().find(|n| n.name == "Loggable").unwrap();
        assert_eq!(loggable.kind, NodeKind::Trait);

        let persist = result.nodes.iter().find(|n| n.name == "persist").unwrap();
        assert_eq!(persist.kind, NodeKind::Method);
        assert_eq!(persist.visibility, Some(Visibility::Private));
        assert_eq!(persist.is_static, Some(true));

        // class constant via visit_node hook
        let status = result.nodes.iter().find(|n| n.name == "STATUS").unwrap();
        assert_eq!(status.kind, NodeKind::Constant);

        // `use Loggable;` inside the class → implements reference
        let implements_ref = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_kind == EdgeKind::Implements)
            .expect("implements ref from trait use");
        assert_eq!(implements_ref.reference_name, "Loggable");

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "App\\Services\\Mailer");
    }
}
