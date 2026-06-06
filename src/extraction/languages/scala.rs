//! Scala language extraction config.
//!
//! Ported from `src/extraction/languages/scala.ts`.

use super::find_named_child;
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
use crate::types::{NodeKind, Visibility};

fn get_val_var_name(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let pattern_node = node.child_by_field_name("pattern")?;
    if pattern_node.kind() == "identifier" {
        return Some(get_node_text(pattern_node, source).to_string());
    }
    find_named_child(pattern_node, "identifier").map(|c| get_node_text(c, source).to_string())
}

fn extract_visibility(node: SyntaxNode<'_>, source: &str) -> Visibility {
    for i in 0..node.named_child_count() as u32 {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if child.kind() == "modifiers" || child.kind() == "access_modifier" {
            let text = get_node_text(child, source);
            if text.contains("private") {
                return Visibility::Private;
            }
            if text.contains("protected") {
                return Visibility::Protected;
            }
        }
    }
    Visibility::Public
}

pub struct ScalaExtractor;

impl LanguageExtractor for ScalaExtractor {
    fn function_types(&self) -> &[&str] {
        // top-level function_definition is handled via methodTypes (same pattern as Kotlin)
        &[]
    }
    fn class_types(&self) -> &[&str] {
        &["class_definition", "object_definition", "trait_definition"]
    }
    fn method_types(&self) -> &[&str] {
        &["function_definition", "function_declaration"]
    }
    fn interface_types(&self) -> &[&str] {
        &[]
    }
    fn struct_types(&self) -> &[&str] {
        &[]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_definition"]
    }
    fn enum_member_types(&self) -> &[&str] {
        // handled in visit_node — enum_case_definitions wraps the cases
        &[]
    }
    fn type_alias_types(&self) -> &[&str] {
        &["type_definition"]
    }
    fn import_types(&self) -> &[&str] {
        &["import_declaration"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        // val/var handled in visit_node (use `pattern` field, not `name`)
        &[]
    }
    fn field_types(&self) -> &[&str] {
        &[]
    }
    fn extra_class_node_types(&self) -> &[&str] {
        &[]
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
    fn interface_kind(&self) -> NodeKind {
        NodeKind::Trait
    }

    fn classify_class_node(&self, node: SyntaxNode<'_>, _source: &str) -> ClassLikeKind {
        if node.kind() == "trait_definition" {
            ClassLikeKind::Trait
        } else {
            ClassLikeKind::Class
        }
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = node.child_by_field_name("parameters");
        let return_type = node.child_by_field_name("return_type");
        if params.is_none() && return_type.is_none() {
            return None;
        }
        let mut sig = params
            .map(|p| get_node_text(p, source).to_string())
            .unwrap_or_default();
        if let Some(rt) = return_type {
            sig.push_str(": ");
            sig.push_str(get_node_text(rt, source));
        }
        if sig.is_empty() { None } else { Some(sig) }
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        Some(extract_visibility(node, source))
    }

    fn is_async(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        Some(false)
    }

    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        for i in 0..node.named_child_count() as u32 {
            if let Some(child) = node.named_child(i) {
                if child.kind() == "modifiers" && get_node_text(child, source).contains("static") {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        let t = node.kind();

        // val/var: name is in `pattern` field (identifier), not `name`
        if t == "val_definition" || t == "var_definition" {
            let Some(name) = get_val_var_name(node, ctx.source()) else {
                return false;
            };

            let is_in_class = match ctx.node_stack().last() {
                Some(parent_id) => ctx.nodes().iter().any(|n| {
                    n.id == *parent_id
                        && matches!(
                            n.kind,
                            NodeKind::Class
                                | NodeKind::Trait
                                | NodeKind::Interface
                                | NodeKind::Struct
                                | NodeKind::Enum
                                | NodeKind::Module
                        )
                }),
                None => false,
            };

            let kind = if is_in_class {
                NodeKind::Field
            } else if t == "val_definition" {
                NodeKind::Constant
            } else {
                NodeKind::Variable
            };
            let type_node = node.child_by_field_name("type");
            let sig = type_node.map(|tn| {
                format!(
                    "{} {}: {}",
                    if t == "val_definition" { "val" } else { "var" },
                    name,
                    get_node_text(tn, ctx.source())
                )
            });
            let visibility = extract_visibility(node, ctx.source());

            ctx.create_node(
                kind,
                &name,
                node,
                NodeExtra {
                    signature: sig,
                    visibility: Some(visibility),
                    ..Default::default()
                },
            );
            return true;
        }

        // enum_case_definitions wraps simple_enum_case / full_enum_case children
        if t == "enum_case_definitions" {
            for i in 0..node.named_child_count() as u32 {
                let Some(child) = node.named_child(i) else {
                    continue;
                };
                if child.kind() == "simple_enum_case" || child.kind() == "full_enum_case" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = get_node_text(name_node, ctx.source()).to_string();
                        ctx.create_node(NodeKind::EnumMember, &name, child, NodeExtra::default());
                    }
                }
            }
            return true;
        }

        // extension_definition: visit body children directly, no container node
        if t == "extension_definition" {
            if let Some(body) = node.child_by_field_name("body") {
                for i in 0..body.named_child_count() as u32 {
                    if let Some(child) = body.named_child(i) {
                        ctx.visit_node(child);
                    }
                }
            }
            return true;
        }

        false
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();
        if let Some(path_node) = node.child_by_field_name("path") {
            return ImportOutcome::Info(ImportInfo::new(
                get_node_text(path_node, source),
                import_text,
            ));
        }
        for i in 0..node.named_child_count() as u32 {
            if let Some(child) = node.named_child(i) {
                if child.kind() == "identifier" || child.kind() == "stable_identifier" {
                    return ImportOutcome::Info(ImportInfo::new(
                        get_node_text(child, source),
                        import_text,
                    ));
                }
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
    fn scala_smoke_extraction() {
        let source = "import scala.collection.mutable\n\nclass Ledger {\n  val limit: Int = 10\n  private def post(amount: Int): Unit = {\n    audit(amount)\n  }\n}\n\ntrait Auditable {\n  def audit(amount: Int): Unit\n}\n\nobject Ledger {\n  def apply(): Ledger = new Ledger\n}\n";
        let result = TreeSitterExtractor::new(
            "src/Ledger.scala",
            source,
            Some(Language::Scala),
            Some(&ScalaExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result
            .nodes
            .iter()
            .find(|n| n.name == "Ledger" && n.kind == NodeKind::Class)
            .expect("class node");
        assert_eq!(class.kind, NodeKind::Class);

        // trait_definition classified as trait
        let auditable = result.nodes.iter().find(|n| n.name == "Auditable").unwrap();
        assert_eq!(auditable.kind, NodeKind::Trait);

        let post = result.nodes.iter().find(|n| n.name == "post").unwrap();
        assert_eq!(post.kind, NodeKind::Method);
        assert_eq!(post.visibility, Some(Visibility::Private));

        // val inside class → field via visit_node
        let limit = result.nodes.iter().find(|n| n.name == "limit").unwrap();
        assert_eq!(limit.kind, NodeKind::Field);
        assert_eq!(limit.signature.as_deref(), Some("val limit: Int"));

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        // tree-sitter-scala 0.26 emits one `path:` field PER dotted segment;
        // `childForFieldName('path')` (and `child_by_field_name`) return the
        // FIRST, so the module name is the root segment — same result the TS
        // hook produces on this grammar (the TS suite only asserts the count).
        assert_eq!(import.name, "scala");
    }

    #[test]
    fn scala_top_level_val_is_constant() {
        let source = "val pi: Double = 3.14\nvar counter: Int = 0\n";
        let result = TreeSitterExtractor::new(
            "src/Consts.scala",
            source,
            Some(Language::Scala),
            Some(&ScalaExtractor),
        )
        .extract();
        let pi = result.nodes.iter().find(|n| n.name == "pi").unwrap();
        assert_eq!(pi.kind, NodeKind::Constant);
        let counter = result.nodes.iter().find(|n| n.name == "counter").unwrap();
        assert_eq!(counter.kind, NodeKind::Variable);
    }
}
