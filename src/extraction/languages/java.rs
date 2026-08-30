//! Java language extraction config.
//!
//! Ported from `src/extraction/languages/java.ts`.

use std::collections::HashSet;

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{NodeKind, Visibility};

const LOMBOK_LOG_ANNOTATIONS: [&str; 9] = [
    "Slf4j",
    "Log4j",
    "Log4j2",
    "Log",
    "CommonsLog",
    "JBossLog",
    "Flogger",
    "XSlf4j",
    "CustomLog",
];

fn lombok_annotation_names(node: SyntaxNode<'_>, source: &str) -> HashSet<String> {
    let Some(modifiers) = named_children(node)
        .into_iter()
        .find(|child| child.kind() == "modifiers")
    else {
        return HashSet::new();
    };
    named_children(modifiers)
        .into_iter()
        .filter(|child| matches!(child.kind(), "marker_annotation" | "annotation"))
        .filter_map(|annotation| get_child_by_field(annotation, "name"))
        .filter_map(|name| {
            get_node_text(name, source)
                .trim()
                .rsplit('.')
                .next()
                .map(str::to_string)
        })
        .collect()
}

fn modifier_text_of<'a>(node: SyntaxNode<'_>, source: &'a str) -> &'a str {
    named_children(node)
        .into_iter()
        .find(|child| child.kind() == "modifiers")
        .map_or("", |modifiers| get_node_text(modifiers, source))
}

fn capitalize_java(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn lombok_getter_name(field_name: &str, is_boolean_primitive: bool) -> String {
    if is_boolean_primitive
        && field_name
            .strip_prefix("is")
            .and_then(|suffix| suffix.chars().next())
            .is_some_and(char::is_uppercase)
    {
        return field_name.to_string();
    }
    let prefix = if is_boolean_primitive { "is" } else { "get" };
    format!("{prefix}{}", capitalize_java(field_name))
}

fn lombok_setter_name(field_name: &str, is_boolean_primitive: bool) -> String {
    let base = if is_boolean_primitive
        && field_name
            .strip_prefix("is")
            .and_then(|suffix| suffix.chars().next())
            .is_some_and(char::is_uppercase)
    {
        &field_name[2..]
    } else {
        field_name
    };
    format!("set{}", capitalize_java(base))
}

fn normalize_java_type(type_node: Option<SyntaxNode<'_>>, source: &str) -> Option<String> {
    let node = type_node?;
    if matches!(
        node.kind(),
        "void_type" | "integral_type" | "floating_point_type" | "boolean_type" | "array_type"
    ) {
        return None;
    }
    let raw = get_node_text(node, source).trim();
    let base = raw.split('<').next()?.trim().rsplit('.').next()?.trim();
    (!base.is_empty()
        && base
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric()))
    .then(|| base.to_string())
}

fn has_modifier(modifiers: &str, expected: &str) -> bool {
    modifiers
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|modifier| modifier == expected)
}

fn synthesize_lombok_members(class_node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) {
    let source = ctx.source().to_string();
    let class_annotations = lombok_annotation_names(class_node, &source);
    let class_getter = class_annotations.contains("Getter");
    let class_setter = class_annotations.contains("Setter");
    let is_data = class_annotations.contains("Data");
    let is_value = class_annotations.contains("Value");
    let has_builder =
        class_annotations.contains("Builder") || class_annotations.contains("SuperBuilder");
    let has_to_string = is_data || is_value || class_annotations.contains("ToString");
    let has_equals = is_data || is_value || class_annotations.contains("EqualsAndHashCode");
    let log_annotation = class_annotations
        .iter()
        .find(|annotation| LOMBOK_LOG_ANNOTATIONS.contains(&annotation.as_str()))
        .cloned();

    let Some(body) = get_child_by_field(class_node, "body") else {
        return;
    };
    let fields: Vec<_> = named_children(body)
        .into_iter()
        .filter(|child| child.kind() == "field_declaration")
        .collect();
    let class_has_lombok = class_getter
        || class_setter
        || is_data
        || is_value
        || has_builder
        || has_to_string
        || has_equals
        || log_annotation.is_some();
    if !class_has_lombok
        && !fields
            .iter()
            .any(|field| !lombok_annotation_names(*field, &source).is_empty())
    {
        return;
    }

    let Some(class_id) = ctx.node_stack().last() else {
        return;
    };
    let Some(class_record) = ctx.nodes().iter().find(|node| &node.id == class_id) else {
        return;
    };
    let class_name = class_record.name.clone();
    let class_qualified_name = class_record.qualified_name.clone();
    let mut taken_methods = HashSet::new();
    let mut taken_fields = HashSet::new();
    for node in ctx.nodes() {
        if node.file_path == ctx.file_path()
            && node.qualified_name == format!("{class_qualified_name}::{}", node.name)
        {
            match node.kind {
                NodeKind::Method | NodeKind::Function => {
                    taken_methods.insert(node.name.clone());
                }
                NodeKind::Field | NodeKind::Variable | NodeKind::Constant | NodeKind::Property => {
                    taken_fields.insert(node.name.clone());
                }
                _ => {}
            }
        }
    }

    let class_name_node = get_child_by_field(class_node, "name").unwrap_or(class_node);
    let mut emit_method = |name: String,
                           anchor: SyntaxNode<'_>,
                           signature: String,
                           annotation: &str,
                           return_type: Option<String>,
                           is_static: Option<bool>| {
        if name.is_empty() || !taken_methods.insert(name.clone()) {
            return;
        }
        ctx.create_node(
            NodeKind::Method,
            &name,
            anchor,
            NodeExtra {
                visibility: Some(Visibility::Public),
                signature: Some(signature),
                docstring: Some(format!("Lombok-generated ({annotation})")),
                decorators: Some(vec!["lombok".to_string()]),
                is_static,
                return_type,
                ..Default::default()
            },
        );
    };

    for field in fields {
        let modifiers = modifier_text_of(field, &source);
        if has_modifier(modifiers, "static") {
            continue;
        }
        let is_final = has_modifier(modifiers, "final");
        let field_annotations = lombok_annotation_names(field, &source);
        let field_getter = field_annotations.contains("Getter");
        let field_setter = field_annotations.contains("Setter");
        let want_getter = class_getter || is_data || is_value || field_getter;
        let want_setter = (class_setter || is_data || field_setter) && !is_final;
        if !want_getter && !want_setter {
            continue;
        }

        let type_node = get_child_by_field(field, "type");
        let type_text = type_node
            .map(|node| get_node_text(node, &source).trim())
            .unwrap_or("Object");
        let is_boolean_primitive = type_node.is_some_and(|node| node.kind() == "boolean_type");
        let return_type = normalize_java_type(type_node, &source);
        for declarator in named_children(field)
            .into_iter()
            .filter(|child| child.kind() == "variable_declarator")
        {
            let Some(name_node) = get_child_by_field(declarator, "name") else {
                continue;
            };
            let field_name = get_node_text(name_node, &source).trim();
            if want_getter {
                let name = lombok_getter_name(field_name, is_boolean_primitive);
                let annotation = if field_getter {
                    "@Getter"
                } else if is_data {
                    "@Data"
                } else if is_value {
                    "@Value"
                } else {
                    "@Getter"
                };
                emit_method(
                    name.clone(),
                    name_node,
                    format!("{type_text} {name}()"),
                    annotation,
                    return_type.clone(),
                    None,
                );
            }
            if want_setter {
                let name = lombok_setter_name(field_name, is_boolean_primitive);
                let annotation = if field_setter {
                    "@Setter"
                } else if is_data {
                    "@Data"
                } else {
                    "@Setter"
                };
                emit_method(
                    name.clone(),
                    name_node,
                    format!("void {name}({type_text} {field_name})"),
                    annotation,
                    None,
                    None,
                );
            }
        }
    }

    if has_builder {
        let annotation = if class_annotations.contains("SuperBuilder") {
            "@SuperBuilder"
        } else {
            "@Builder"
        };
        emit_method(
            "builder".to_string(),
            class_name_node,
            format!("static {class_name}.{class_name}Builder builder()"),
            annotation,
            Some(format!("{class_name}Builder")),
            Some(true),
        );
    }
    if has_to_string {
        let annotation = if is_data {
            "@Data"
        } else if is_value {
            "@Value"
        } else {
            "@ToString"
        };
        emit_method(
            "toString".to_string(),
            class_name_node,
            "String toString()".to_string(),
            annotation,
            None,
            None,
        );
    }
    if has_equals {
        let annotation = if is_data {
            "@Data"
        } else if is_value {
            "@Value"
        } else {
            "@EqualsAndHashCode"
        };
        emit_method(
            "equals".to_string(),
            class_name_node,
            "boolean equals(Object o)".to_string(),
            annotation,
            None,
            None,
        );
        emit_method(
            "hashCode".to_string(),
            class_name_node,
            "int hashCode()".to_string(),
            annotation,
            None,
            None,
        );
    }

    if let Some(annotation) = log_annotation {
        if taken_fields.insert("log".to_string()) {
            ctx.create_node(
                NodeKind::Field,
                "log",
                class_name_node,
                NodeExtra {
                    visibility: Some(Visibility::Private),
                    is_static: Some(true),
                    signature: Some("Logger log".to_string()),
                    docstring: Some(format!("Lombok-generated (@{annotation})")),
                    decorators: Some(vec!["lombok".to_string()]),
                    ..Default::default()
                },
            );
        }
    }
}

pub struct JavaExtractor;

impl LanguageExtractor for JavaExtractor {
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
        &[]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_declaration"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enum_constant"]
    }
    fn type_alias_types(&self) -> &[&str] {
        &[]
    }
    fn import_types(&self) -> &[&str] {
        &["import_declaration"]
    }
    fn call_types(&self) -> &[&str] {
        &["method_invocation"]
    }
    fn variable_types(&self) -> &[&str] {
        &["local_variable_declaration"]
    }
    fn field_types(&self) -> &[&str] {
        &["field_declaration"]
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

    fn synthesize_members(&self, class_node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) {
        synthesize_lombok_members(class_node, ctx);
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let return_type = get_child_by_field(node, "type");
        let params_text = get_node_text(params, source);
        Some(match return_type {
            Some(rt) => format!("{} {}", get_node_text(rt, source), params_text),
            None => params_text.to_string(),
        })
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "modifiers" {
                let text = get_node_text(child, source);
                if text.contains("public") {
                    return Some(Visibility::Public);
                }
                if text.contains("private") {
                    return Some(Visibility::Private);
                }
                if text.contains("protected") {
                    return Some(Visibility::Protected);
                }
            }
        }
        None
    }

    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "modifiers" && get_node_text(child, source).contains("static") {
                return Some(true);
            }
        }
        Some(false)
    }

    fn is_const(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        Some((0..node.child_count() as u32).any(|index| {
            node.child(index).is_some_and(|child| {
                if child.kind() != "modifiers" {
                    return false;
                }
                let modifiers = get_node_text(child, source);
                modifiers
                    .split_whitespace()
                    .any(|modifier| modifier == "static")
                    && modifiers
                        .split_whitespace()
                        .any(|modifier| modifier == "final")
            })
        }))
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();
        if let Some(scoped_id) = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "scoped_identifier")
        {
            return ImportOutcome::Info(ImportInfo::new(
                get_node_text(scoped_id, source),
                import_text,
            ));
        }
        ImportOutcome::Declined
    }

    fn package_types(&self) -> &[&str] {
        &["package_declaration"]
    }

    fn extract_package(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // package_declaration → scoped_identifier or identifier (single-segment)
        named_children(node)
            .into_iter()
            .find(|c| c.kind() == "scoped_identifier" || c.kind() == "identifier")
            .map(|id| get_node_text(id, source).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn java_smoke_extraction() {
        let source = "package com.example.app;\n\nimport java.util.List;\n\npublic class Account {\n    private static int count;\n\n    public void deposit(int amount) {\n        validate(amount);\n    }\n}\n\ninterface Validator {\n    boolean validate(int x);\n}\n\nenum Status { OPEN, CLOSED }\n";
        let result = TreeSitterExtractor::new(
            "src/Account.java",
            source,
            Some(Language::Java),
            Some(&JavaExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result.nodes.iter().find(|n| n.name == "Account").unwrap();
        assert_eq!(class.kind, NodeKind::Class);
        assert_eq!(class.visibility, Some(Visibility::Public));

        let method = result.nodes.iter().find(|n| n.name == "deposit").unwrap();
        assert_eq!(method.kind, NodeKind::Method);
        assert_eq!(method.signature.as_deref(), Some("void (int amount)"));

        let field = result.nodes.iter().find(|n| n.name == "count").unwrap();
        assert_eq!(field.kind, NodeKind::Field);

        let iface = result.nodes.iter().find(|n| n.name == "Validator").unwrap();
        assert_eq!(iface.kind, NodeKind::Interface);

        let status = result.nodes.iter().find(|n| n.name == "Status").unwrap();
        assert_eq!(status.kind, NodeKind::Enum);
        let open = result.nodes.iter().find(|n| n.name == "OPEN").unwrap();
        assert_eq!(open.kind, NodeKind::EnumMember);

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "java.util.List");

        // package_declaration wraps top-level declarations in a namespace node
        let ns = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Namespace)
            .expect("namespace node");
        assert_eq!(ns.name, "com.example.app");
    }
}
