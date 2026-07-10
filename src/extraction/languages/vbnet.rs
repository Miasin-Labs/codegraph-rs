//! Visual Basic .NET extraction configuration.
//!
//! Ported from `src/extraction/languages/vbnet.ts`.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

use super::named_children;
use crate::extraction::tree_sitter_helpers::get_node_text;
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{NodeKind, Visibility};

pub struct VbnetExtractor;

static GENERIC_ARGUMENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\(\s*Of\b[^)]*\)").expect("valid VB generic regex"));
static SIMPLE_IDENTIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("valid VB identifier regex"));

fn has_modifier(node: SyntaxNode<'_>, source: &str, expected: &str) -> bool {
    let expected = expected.to_ascii_lowercase();
    (0..node.child_count() as u32).any(|index| {
        node.child(index).is_some_and(|child| {
            if !matches!(child.kind(), "member_modifier" | "modifier" | "modifiers") {
                return false;
            }
            let normalized = get_node_text(child, source)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();
            if expected.contains(' ') {
                normalized.contains(&expected)
            } else {
                normalized
                    .split_whitespace()
                    .any(|word| word == expected.as_str())
            }
        })
    })
}

fn extract_return_type(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let type_node = node.child_by_field_name("return_type").or_else(|| {
        let as_clause = named_children(node)
            .into_iter()
            .find(|child| child.kind() == "as_clause")?;
        as_clause.child_by_field_name("declared_type")
    })?;
    if matches!(type_node.kind(), "predefined_type" | "array_type") {
        return None;
    }

    let mut value = get_node_text(type_node, source).trim().to_string();
    while value.ends_with('?') {
        value.pop();
    }
    value = GENERIC_ARGUMENTS.replace_all(&value, "").into_owned();
    let candidate = value.rsplit('.').next()?.trim();
    SIMPLE_IDENTIFIER
        .is_match(candidate)
        .then(|| candidate.to_string())
}

impl LanguageExtractor for VbnetExtractor {
    fn pre_parse<'a>(&self, source: &'a str, _file_path: &str) -> Cow<'a, str> {
        if source.ends_with('\n') {
            Cow::Borrowed(source)
        } else {
            Cow::Owned(format!("{source}\n"))
        }
    }

    fn function_types(&self) -> &[&str] {
        &[]
    }
    fn class_types(&self) -> &[&str] {
        &[
            "class_declaration",
            "module_declaration",
            "class_block",
            "module_block",
        ]
    }
    fn method_types(&self) -> &[&str] {
        &[
            "method_declaration",
            "constructor_declaration",
            "external_method_declaration",
            "interface_method_declaration",
            "abstract_method_declaration",
        ]
    }
    fn interface_types(&self) -> &[&str] {
        &["interface_declaration", "interface_block"]
    }
    fn struct_types(&self) -> &[&str] {
        &["structure_declaration", "structure_block"]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_declaration", "enum_block"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enum_member_declaration", "enum_member"]
    }
    fn type_alias_types(&self) -> &[&str] {
        &["delegate_declaration"]
    }
    fn package_types(&self) -> &[&str] {
        &["namespace_declaration", "namespace_block"]
    }
    fn import_types(&self) -> &[&str] {
        &["imports_statement"]
    }
    fn call_types(&self) -> &[&str] {
        &[
            "invocation_expression",
            "array_access_expression",
            "generic_invocation_expression",
            "invocation",
        ]
    }
    fn variable_types(&self) -> &[&str] {
        &[
            "declaration_statement",
            "dim_statement",
            "const_declaration",
        ]
    }
    fn field_types(&self) -> &[&str] {
        &["field_declaration"]
    }
    fn property_types(&self) -> &[&str] {
        &[
            "property_declaration",
            "interface_property_declaration",
            "abstract_property_declaration",
        ]
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

    fn resolve_body<'t>(&self, node: SyntaxNode<'t>, _body_field: &str) -> Option<SyntaxNode<'t>> {
        Some(node)
    }

    fn get_return_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        extract_return_type(node, source)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        if has_modifier(node, source, "private") {
            Some(Visibility::Private)
        } else if has_modifier(node, source, "protected")
            || has_modifier(node, source, "protected friend")
        {
            Some(Visibility::Protected)
        } else if has_modifier(node, source, "friend") {
            Some(Visibility::Internal)
        } else {
            Some(Visibility::Public)
        }
    }

    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        Some(has_modifier(node, source, "shared"))
    }

    fn is_const(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        Some(
            node.kind() == "const_declaration"
                || has_modifier(node, source, "const")
                || (has_modifier(node, source, "shared") && has_modifier(node, source, "readonly")),
        )
    }

    fn is_async(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        Some(has_modifier(node, source, "async"))
    }

    fn extract_package(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        node.child_by_field_name("name")
            .map(|name| get_node_text(name, source).to_string())
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let signature = get_node_text(node, source).trim();
        let name = named_children(node).into_iter().rev().find(|child| {
            matches!(
                child.kind(),
                "qualified_name"
                    | "simple_name"
                    | "global_qualified_name"
                    | "namespace_name"
                    | "identifier"
            )
        });
        match name {
            Some(name) => {
                ImportOutcome::Info(ImportInfo::new(get_node_text(name, source), signature))
            }
            None => ImportOutcome::Declined,
        }
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        if matches!(
            node.kind(),
            "event_declaration" | "custom_event_declaration"
        ) {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = get_node_text(name_node, ctx.source()).to_string();
                ctx.create_node(NodeKind::Field, &name, node, NodeExtra::default());
            }
            return true;
        }

        if node.kind() == "constructor_declaration" {
            if let Some(constructor) =
                ctx.create_node(NodeKind::Method, "New", node, NodeExtra::default())
            {
                let id = constructor.id;
                ctx.push_scope(id.clone());
                ctx.visit_function_body(node, &id);
                ctx.pop_scope();
            }
            return true;
        }

        false
    }
}
