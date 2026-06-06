//! Python language extraction config.
//!
//! Ported from `src/extraction/languages/python.ts`.

use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};

pub struct PythonExtractor;

impl LanguageExtractor for PythonExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &[&str] {
        &["class_definition"]
    }
    fn method_types(&self) -> &[&str] {
        // Methods are functions inside classes
        &["function_definition"]
    }
    fn interface_types(&self) -> &[&str] {
        &[]
    }
    fn struct_types(&self) -> &[&str] {
        &[]
    }
    fn enum_types(&self) -> &[&str] {
        &[]
    }
    fn type_alias_types(&self) -> &[&str] {
        &[]
    }
    fn import_types(&self) -> &[&str] {
        &["import_statement", "import_from_statement"]
    }
    fn call_types(&self) -> &[&str] {
        &["call"]
    }
    fn variable_types(&self) -> &[&str] {
        // Python uses assignment for variable declarations
        &["assignment"]
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

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let return_type = get_child_by_field(node, "return_type");
        let mut sig = get_node_text(params, source).to_string();
        if let Some(rt) = return_type {
            sig.push_str(" -> ");
            sig.push_str(get_node_text(rt, source));
        }
        Some(sig)
    }

    fn is_async(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        let prev = node.prev_sibling();
        Some(prev.map(|p| p.kind() == "async").unwrap_or(false))
    }

    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        // Check for @staticmethod decorator
        if let Some(prev) = node.prev_named_sibling() {
            if prev.kind() == "decorator" {
                let text = get_node_text(prev, source);
                return Some(text.contains("staticmethod"));
            }
        }
        Some(false)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();
        if node.kind() == "import_from_statement" {
            if let Some(module_node) = node.child_by_field_name("module_name") {
                return ImportOutcome::Info(ImportInfo::new(
                    get_node_text(module_node, source),
                    import_text,
                ));
            }
        }
        // import_statement creates multiple imports - return null for core fallback
        ImportOutcome::Declined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn python_smoke_extraction() {
        let source = "from os import path\n\nclass Greeter:\n    @staticmethod\n    def shout(msg):\n        print(msg)\n\n    def greet(self, who) -> str:\n        return self.format(who)\n\nasync def run():\n    pass\n";
        let result = TreeSitterExtractor::new(
            "src/app.py",
            source,
            Some(Language::Python),
            Some(&PythonExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result.nodes.iter().find(|n| n.name == "Greeter").unwrap();
        assert_eq!(class.kind, NodeKind::Class);

        let greet = result.nodes.iter().find(|n| n.name == "greet").unwrap();
        assert_eq!(greet.kind, NodeKind::Method);
        assert_eq!(greet.qualified_name, "Greeter::greet");
        assert_eq!(greet.signature.as_deref(), Some("(self, who) -> str"));

        let shout = result.nodes.iter().find(|n| n.name == "shout").unwrap();
        assert_eq!(shout.is_static, Some(true));

        let run = result.nodes.iter().find(|n| n.name == "run").unwrap();
        assert_eq!(run.kind, NodeKind::Function);
        // TS-parity: the hook checks `previousSibling === 'async'`, but
        // tree-sitter-python places `async` INSIDE function_definition (first
        // child), so detection misses — identical to the TS behavior on the
        // same grammar shape (the TS suite does not assert Python isAsync).
        assert_eq!(run.is_async, Some(false));

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "os");
    }
}
