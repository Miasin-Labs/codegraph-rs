//! C# language extraction config.
//!
//! Ported from `src/extraction/languages/csharp.ts`.

use super::named_children;
use crate::extraction::tree_sitter_helpers::get_node_text;
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::Visibility;

pub struct CsharpExtractor;

impl LanguageExtractor for CsharpExtractor {
    fn function_types(&self) -> &[&str] {
        &[]
    }
    fn class_types(&self) -> &[&str] {
        &["class_declaration"]
    }
    fn method_types(&self) -> &[&str] {
        &["method_declaration", "constructor_declaration"]
    }
    fn interface_types(&self) -> &[&str] {
        &["interface_declaration"]
    }
    fn struct_types(&self) -> &[&str] {
        &["struct_declaration"]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_declaration"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enum_member_declaration"]
    }
    fn type_alias_types(&self) -> &[&str] {
        &[]
    }
    fn import_types(&self) -> &[&str] {
        &["using_directive"]
    }
    fn call_types(&self) -> &[&str] {
        &["invocation_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &["local_declaration_statement"]
    }
    fn field_types(&self) -> &[&str] {
        &["field_declaration"]
    }
    fn property_types(&self) -> &[&str] {
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
        Some("type")
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifier" {
                    match get_node_text(child, source) {
                        "public" => return Some(Visibility::Public),
                        "private" => return Some(Visibility::Private),
                        "protected" => return Some(Visibility::Protected),
                        "internal" => return Some(Visibility::Internal),
                        _ => {}
                    }
                }
            }
        }
        // C# defaults to private
        Some(Visibility::Private)
    }

    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifier" && get_node_text(child, source) == "static" {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn is_async(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifier" && get_node_text(child, source) == "async" {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();
        // C# using directives: using System, using System.Collections.Generic,
        // using static X, using Alias = X
        if let Some(qualified_name) = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "qualified_name")
        {
            return ImportOutcome::Info(ImportInfo::new(
                get_node_text(qualified_name, source),
                import_text,
            ));
        }
        // Simple namespace like "using System;" - get the first identifier
        if let Some(identifier) = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "identifier")
        {
            return ImportOutcome::Info(ImportInfo::new(
                get_node_text(identifier, source),
                import_text,
            ));
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
    fn csharp_smoke_extraction() {
        let source = "using System;\nusing System.Collections.Generic;\n\npublic class Cart\n{\n    private List<string> items;\n\n    public string Name { get; set; }\n\n    public static async Task Save()\n    {\n        Validate();\n    }\n}\n\ninterface IShape { }\n\nstruct Vec2 { }\n\nenum Color { Red, Green }\n";
        let result = TreeSitterExtractor::new(
            "src/Cart.cs",
            source,
            Some(Language::Csharp),
            Some(&CsharpExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result.nodes.iter().find(|n| n.name == "Cart").unwrap();
        assert_eq!(class.kind, NodeKind::Class);
        assert_eq!(class.visibility, Some(Visibility::Public));

        let save = result.nodes.iter().find(|n| n.name == "Save").unwrap();
        assert_eq!(save.kind, NodeKind::Method);
        assert_eq!(save.is_static, Some(true));
        assert_eq!(save.is_async, Some(true));

        let prop = result.nodes.iter().find(|n| n.name == "Name").unwrap();
        assert_eq!(prop.kind, NodeKind::Property);

        let field = result.nodes.iter().find(|n| n.name == "items").unwrap();
        assert_eq!(field.kind, NodeKind::Field);

        let iface = result.nodes.iter().find(|n| n.name == "IShape").unwrap();
        assert_eq!(iface.kind, NodeKind::Interface);
        let vec2 = result.nodes.iter().find(|n| n.name == "Vec2").unwrap();
        assert_eq!(vec2.kind, NodeKind::Struct);
        let color = result.nodes.iter().find(|n| n.name == "Color").unwrap();
        assert_eq!(color.kind, NodeKind::Enum);

        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Import)
            .collect();
        assert!(imports.iter().any(|n| n.name == "System"));
        assert!(
            imports
                .iter()
                .any(|n| n.name == "System.Collections.Generic")
        );
    }
}
