//! Move language extraction config.

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference, Visibility};

fn normalized_access(node: SyntaxNode<'_>, source: &str) -> String {
    let mut depth = 0;
    get_node_text(node, source)
        .chars()
        .filter(|&ch| match ch {
            '<' => {
                depth += 1;
                false
            }
            '>' if depth > 0 => {
                depth -= 1;
                false
            }
            '!' if depth == 0 => false,
            _ => depth == 0,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn push_unique(paths: &mut Vec<String>, path: String) {
    if !path.is_empty() && !paths.contains(&path) {
        paths.push(path);
    }
}

fn collect_group_imports(
    member: SyntaxNode<'_>,
    prefix: &str,
    source: &str,
    paths: &mut Vec<String>,
) {
    let module = get_child_by_field(member, "module")
        .map(|node| get_node_text(node, source).trim())
        .filter(|name| !name.is_empty());
    let nested: Vec<_> = named_children(member)
        .into_iter()
        .filter(|child| child.kind() == "use_member")
        .collect();

    if let Some(module) = module {
        let module_path = format!("{prefix}::{module}");
        if nested.is_empty()
            || nested
                .iter()
                .any(|child| get_child_by_field(*child, "module").is_none())
        {
            push_unique(paths, module_path.clone());
        }
        for child in nested
            .into_iter()
            .filter(|child| get_child_by_field(*child, "module").is_some())
        {
            collect_group_imports(child, &module_path, source, paths);
        }
    } else if let Some(member) = get_child_by_field(member, "member") {
        push_unique(
            paths,
            format!("{prefix}::{}", get_node_text(member, source).trim()),
        );
    }
}

fn import_paths(node: SyntaxNode<'_>, source: &str) -> Vec<String> {
    let Some(import) = named_children(node).into_iter().next() else {
        return Vec::new();
    };

    match import.kind() {
        "use_fun" => named_children(import)
            .into_iter()
            .find(|child| child.kind() == "module_access")
            .map(|source_path| vec![normalized_access(source_path, source)])
            .unwrap_or_default(),
        "use_module" | "use_module_member" => named_children(import)
            .into_iter()
            .find(|child| child.kind() == "module_identity")
            .map(|module| vec![get_node_text(module, source).trim().to_string()])
            .unwrap_or_default(),
        "use_module_members" => {
            if let Some(module) = named_children(import)
                .into_iter()
                .find(|child| child.kind() == "module_identity")
            {
                return vec![get_node_text(module, source).trim().to_string()];
            }

            let Some(address) = get_child_by_field(import, "address") else {
                return Vec::new();
            };
            let prefix = get_node_text(address, source).trim();
            let mut paths = Vec::new();
            for member in named_children(import)
                .into_iter()
                .filter(|child| child.kind() == "use_member")
            {
                collect_group_imports(member, prefix, source, &mut paths);
            }
            paths
        }
        _ => Vec::new(),
    }
}

pub struct MoveExtractor;

impl LanguageExtractor for MoveExtractor {
    fn function_types(&self) -> &[&str] {
        &[
            "function_definition",
            "macro_function_definition",
            "native_function_definition",
        ]
    }

    fn class_types(&self) -> &[&str] {
        &[]
    }

    fn method_types(&self) -> &[&str] {
        &[]
    }

    fn interface_types(&self) -> &[&str] {
        &[]
    }

    fn struct_types(&self) -> &[&str] {
        &["struct_definition", "native_struct_definition"]
    }

    fn enum_types(&self) -> &[&str] {
        &["enum_definition"]
    }

    fn type_alias_types(&self) -> &[&str] {
        &[]
    }

    fn import_types(&self) -> &[&str] {
        &["use_declaration"]
    }

    fn call_types(&self) -> &[&str] {
        &[]
    }

    fn variable_types(&self) -> &[&str] {
        &[]
    }

    fn name_field(&self) -> &str {
        "name"
    }

    fn body_field(&self) -> &str {
        "struct_fields"
    }

    fn params_field(&self) -> &str {
        "parameters"
    }

    fn return_field(&self) -> Option<&str> {
        Some("return_type")
    }

    fn module_is_class_like(&self) -> bool {
        false
    }

    fn struct_is_definition_without_body(&self) -> bool {
        true
    }

    fn resolve_body<'tree>(
        &self,
        node: SyntaxNode<'tree>,
        _body_field: &str,
    ) -> Option<SyntaxNode<'tree>> {
        match node.kind() {
            "function_definition" | "macro_function_definition" => get_child_by_field(node, "body"),
            "struct_definition" => get_child_by_field(node, "struct_fields"),
            "enum_definition" => get_child_by_field(node, "enum_variants"),
            _ => None,
        }
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let parameters = get_child_by_field(node, "parameters")?;
        let mut signature = get_node_text(parameters, source).to_string();
        if let Some(return_type) = get_child_by_field(node, "return_type") {
            signature.push(' ');
            signature.push_str(get_node_text(return_type, source));
        }
        Some(signature)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        let name = get_child_by_field(node, "name")?;
        let header = source.get(node.start_byte()..name.start_byte())?;
        Some(
            if header
                .split_whitespace()
                .any(|part| part.starts_with("public"))
            {
                Visibility::Public
            } else {
                Visibility::Private
            },
        )
    }

    fn is_exported(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        Some(self.get_visibility(node, source) == Some(Visibility::Public))
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        match node.kind() {
            "module_definition" => {
                let Some(identity) = get_child_by_field(node, "module_identity") else {
                    return false;
                };
                let name = get_node_text(identity, ctx.source()).to_string();
                let Some(module) = ctx.create_node(
                    NodeKind::Module,
                    &name,
                    node,
                    NodeExtra {
                        signature: Some(name.clone()),
                        ..Default::default()
                    },
                ) else {
                    return true;
                };
                ctx.push_scope(module.id);
                if let Some(body) = get_child_by_field(node, "module_body") {
                    for child in named_children(body) {
                        ctx.visit_node(child);
                    }
                }
                ctx.pop_scope();
                true
            }
            "constant" => {
                let Some(name_node) = get_child_by_field(node, "name") else {
                    return true;
                };
                let (name, signature) = {
                    let source = ctx.source();
                    (
                        get_node_text(name_node, source).to_string(),
                        get_node_text(node, source).trim().to_string(),
                    )
                };
                ctx.create_node(
                    NodeKind::Constant,
                    &name,
                    node,
                    NodeExtra {
                        signature: Some(signature),
                        ..Default::default()
                    },
                );
                true
            }
            "field_annotation" => {
                let Some(name_node) = get_child_by_field(node, "field") else {
                    return true;
                };
                let (name, signature) = {
                    let source = ctx.source();
                    (
                        get_node_text(name_node, source).to_string(),
                        get_node_text(node, source).trim().to_string(),
                    )
                };
                ctx.create_node(
                    NodeKind::Field,
                    &name,
                    node,
                    NodeExtra {
                        signature: Some(signature),
                        ..Default::default()
                    },
                );
                true
            }
            "variant" => {
                let Some(name_node) = get_child_by_field(node, "variant_name") else {
                    return true;
                };
                let name = get_node_text(name_node, ctx.source()).to_string();
                let Some(variant) =
                    ctx.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default())
                else {
                    return true;
                };
                if let Some(fields) = get_child_by_field(node, "fields") {
                    ctx.push_scope(variant.id);
                    for child in named_children(fields) {
                        if child.kind() == "positional_fields" {
                            for (index, field_type) in named_children(child).into_iter().enumerate()
                            {
                                ctx.create_node(
                                    NodeKind::Field,
                                    &index.to_string(),
                                    field_type,
                                    NodeExtra {
                                        signature: Some(
                                            get_node_text(field_type, ctx.source()).to_string(),
                                        ),
                                        ..Default::default()
                                    },
                                );
                            }
                        } else {
                            ctx.visit_node(child);
                        }
                    }
                    ctx.pop_scope();
                }
                true
            }
            "use_declaration" => {
                let signature = get_node_text(node, ctx.source()).trim().to_string();
                for module_name in import_paths(node, ctx.source()) {
                    ctx.create_node(
                        NodeKind::Import,
                        &module_name,
                        node,
                        NodeExtra {
                            signature: Some(signature.clone()),
                            ..Default::default()
                        },
                    );
                    if let Some(parent_id) = ctx.node_stack().last().cloned() {
                        ctx.add_unresolved_reference(UnresolvedReference {
                            from_node_id: parent_id,
                            reference_name: module_name,
                            reference_kind: EdgeKind::Imports,
                            line: node.start_position().row as u32 + 1,
                            column: node.start_position().column as u32,
                            file_path: None,
                            language: None,
                            candidates: None,
                            metadata: None,
                        });
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn extract_bare_call(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let access = match node.kind() {
            "call_expression" => named_children(node)
                .into_iter()
                .find(|child| child.kind() == "name_expression")
                .and_then(|name| get_child_by_field(name, "access")),
            "macro_call_expression" => get_child_by_field(node, "access")
                .and_then(|macro_access| get_child_by_field(macro_access, "access")),
            _ => None,
        }?;
        let name = normalized_access(access, source);
        (!name.is_empty()).then_some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::Language;

    #[test]
    fn move_smoke_extraction() {
        let source = r#"
module 0x1::vault {
    use 0x1::coin::{Self, Coin};
    use 0x1::{object::{Self, UID}, balance::Supply};
    use fun 0x1::coin::value as Coin.value;
    const MAX: u64 = 100;

    public struct Vault has key { balance: u64 }
    enum State { Open, Closed { reason: u64 }, Pending(u64, Coin) }

    public fun deposit(amount: u64): u64 {
        assert!(amount > 0, 0);
        coin::value<Coin>(amount)
    }
}
"#;
        let result = TreeSitterExtractor::new(
            "sources/vault.move",
            source,
            Some(Language::Move),
            Some(&MoveExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.nodes.len(), 17, "nodes: {:#?}", result.nodes);
        assert_eq!(
            result.unresolved_references.len(),
            6,
            "references: {:#?}",
            result.unresolved_references
        );

        let module = result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Module)
            .expect("Move module");
        assert_eq!(module.name, "0x1::vault");
        let deposit = result
            .nodes
            .iter()
            .find(|node| node.name == "deposit")
            .expect("deposit function");
        assert_eq!(deposit.qualified_name, "0x1::vault::deposit");

        let mut imports: Vec<&str> = result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Import)
            .map(|node| node.name.as_str())
            .collect();
        imports.sort_unstable();
        assert_eq!(
            imports,
            [
                "0x1::balance",
                "0x1::coin",
                "0x1::coin::value",
                "0x1::object",
            ]
        );

        let mut fields: Vec<&str> = result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Field)
            .map(|node| node.qualified_name.as_str())
            .collect();
        fields.sort_unstable();
        assert!(fields.contains(&"0x1::vault::State::Pending::0"));
        assert!(fields.contains(&"0x1::vault::State::Pending::1"));

        let mut references: Vec<(&EdgeKind, &str)> = result
            .unresolved_references
            .iter()
            .map(|reference| (&reference.reference_kind, reference.reference_name.as_str()))
            .collect();
        references.sort_unstable_by_key(|(_, name)| *name);
        assert!(references.contains(&(&EdgeKind::Calls, "coin::value")));
        assert!(references.contains(&(&EdgeKind::Calls, "assert")));
        assert!(
            !references
                .iter()
                .any(|(_, name)| name.contains('<') || name.contains('!'))
        );
        assert!(references.contains(&(&EdgeKind::Imports, "0x1::coin::value")));
    }
}
