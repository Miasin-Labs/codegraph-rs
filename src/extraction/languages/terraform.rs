//! Terraform / OpenTofu extraction configuration.
//!
//! The HCL grammar exposes declarations as generic `block` nodes. The first
//! identifier names the Terraform construct and the following labels identify
//! the declaration, so extraction is driven entirely by `visit_node`.

use std::collections::VecDeque;

use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

pub struct TerraformExtractor;

const BUILTIN_HEADS: &[&str] = &["each", "count", "self", "path", "terraform"];
const BUILTIN_KEYWORDS: &[&str] = &["null", "true", "false"];
const MODULE_META_ARGS: &[&str] = &[
    "source",
    "version",
    "count",
    "for_each",
    "providers",
    "depends_on",
];

#[derive(Debug)]
struct Reference {
    qualified_name: String,
    line: u32,
    column: u32,
}

#[derive(Debug)]
struct BlockDecl {
    kind: NodeKind,
    name: String,
    qualified_name: String,
    signature: String,
}

fn named_children<'tree>(node: SyntaxNode<'tree>) -> impl Iterator<Item = SyntaxNode<'tree>> {
    (0..node.named_child_count() as u32).filter_map(move |index| node.named_child(index))
}

fn named_child_of_kind<'tree>(node: SyntaxNode<'tree>, kind: &str) -> Option<SyntaxNode<'tree>> {
    named_children(node).find(|child| child.kind() == kind)
}

fn string_lit_value(node: SyntaxNode<'_>, source: &str) -> String {
    named_child_of_kind(node, "template_literal")
        .map(|literal| get_node_text(literal, source).to_string())
        .unwrap_or_default()
}

fn read_block_header(block: SyntaxNode<'_>, source: &str) -> Option<(String, Vec<String>)> {
    let mut children = named_children(block);
    let first = children.next()?;
    if first.kind() != "identifier" {
        return None;
    }

    let block_type = get_node_text(first, source).to_string();
    let mut labels = Vec::new();
    for child in children {
        match child.kind() {
            "string_lit" => labels.push(string_lit_value(child, source)),
            "identifier" => labels.push(get_node_text(child, source).to_string()),
            _ => break,
        }
    }
    Some((block_type, labels))
}

fn get_block_body(block: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    named_child_of_kind(block, "body")
}

fn collect_references(expr: SyntaxNode<'_>, source: &str) -> Vec<Reference> {
    let mut references = Vec::new();
    let mut queue = VecDeque::from([expr]);

    while let Some(node) = queue.pop_front() {
        if node.kind() == "variable_expr" {
            references.extend(references_from_variable_expr(node, source));
        }
        queue.extend(named_children(node));
    }

    references
}

fn references_from_variable_expr(var_expr: SyntaxNode<'_>, source: &str) -> Vec<Reference> {
    let Some(identifier) = named_child_of_kind(var_expr, "identifier") else {
        return Vec::new();
    };
    let head = get_node_text(identifier, source);
    if BUILTIN_HEADS.contains(&head) || BUILTIN_KEYWORDS.contains(&head) {
        return Vec::new();
    }

    let mut attrs = Vec::new();
    let mut cursor = var_expr.next_named_sibling();
    while let Some(node) = cursor {
        match node.kind() {
            "get_attr" => {
                let Some(attr) = named_child_of_kind(node, "identifier") else {
                    break;
                };
                attrs.push(get_node_text(attr, source).to_string());
                cursor = node.next_named_sibling();
            }
            "index" | "new_index" | "legacy_index" | "splat" | "attr_splat" | "full_splat" => {
                cursor = node.next_named_sibling();
            }
            _ => break,
        }
    }

    let line = var_expr.start_position().row as u32 + 1;
    let column = var_expr.start_position().column as u32;
    qualify_reference(head, &attrs)
        .into_iter()
        .map(|qualified_name| Reference {
            qualified_name,
            line,
            column,
        })
        .collect()
}

fn qualify_reference(head: &str, attrs: &[String]) -> Vec<String> {
    match head {
        "var" => attrs
            .first()
            .map(|name| vec![format!("var.{name}")])
            .unwrap_or_default(),
        "local" => attrs
            .first()
            .map(|name| vec![format!("local.{name}")])
            .unwrap_or_default(),
        "module" => {
            let Some(module) = attrs.first() else {
                return Vec::new();
            };
            let mut references = vec![format!("module.{module}")];
            if let Some(output) = attrs.get(1) {
                references.push(format!("module.{module}:output.{output}"));
            }
            if attrs.get(1).is_some_and(|segment| segment == "outputs") {
                if let Some(output) = attrs.get(2) {
                    references.push(format!("module.{module}:remote-output.{output}"));
                }
            }
            references
        }
        "data" => match (attrs.first(), attrs.get(1)) {
            (Some(data_type), Some(name)) => vec![format!("data.{data_type}.{name}")],
            _ => Vec::new(),
        },
        _ => attrs
            .first()
            .map(|name| vec![format!("{head}.{name}")])
            .unwrap_or_default(),
    }
}

fn add_reference(
    ctx: &mut dyn ExtractorContext,
    from_node_id: &str,
    reference_name: String,
    reference_kind: EdgeKind,
    line: u32,
    column: u32,
) {
    ctx.add_unresolved_reference(UnresolvedReference {
        from_node_id: from_node_id.to_string(),
        reference_name,
        reference_kind,
        line,
        column,
        file_path: None,
        language: None,
        candidates: None,
        metadata: None,
    });
}

impl LanguageExtractor for TerraformExtractor {
    fn function_types(&self) -> &[&str] {
        &[]
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
        &[]
    }

    fn enum_types(&self) -> &[&str] {
        &[]
    }

    fn type_alias_types(&self) -> &[&str] {
        &[]
    }

    fn import_types(&self) -> &[&str] {
        &[]
    }

    fn call_types(&self) -> &[&str] {
        &[]
    }

    fn variable_types(&self) -> &[&str] {
        &[]
    }

    fn name_field(&self) -> &str {
        ""
    }

    fn body_field(&self) -> &str {
        ""
    }

    fn params_field(&self) -> &str {
        ""
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        if node.kind() != "block" {
            if node.kind() == "attribute"
                && ctx.file_path().ends_with(".tfvars")
                && node.parent().is_some_and(|parent| {
                    parent.kind() == "body"
                        && parent
                            .parent()
                            .is_some_and(|grandparent| grandparent.kind() == "config_file")
                })
            {
                let identifier = named_child_of_kind(node, "identifier");
                let file_node_id = ctx.node_stack().first().cloned();
                if let (Some(identifier), Some(file_node_id)) = (identifier, file_node_id) {
                    let name = get_node_text(identifier, ctx.source()).to_string();
                    add_reference(
                        ctx,
                        &file_node_id,
                        format!("var.{name}"),
                        EdgeKind::References,
                        node.start_position().row as u32 + 1,
                        node.start_position().column as u32,
                    );
                }
                return true;
            }
            return false;
        }

        let source = ctx.source().to_string();
        let Some((block_type, labels)) = read_block_header(node, &source) else {
            return false;
        };
        let body = get_block_body(node);

        if block_type == "locals" && labels.is_empty() {
            emit_locals(body, ctx);
            return true;
        }

        if block_type == "terraform" && labels.is_empty() {
            return true;
        }

        if matches!(block_type.as_str(), "moved" | "import" | "removed") && labels.is_empty() {
            let file_node_id = ctx.node_stack().first().cloned();
            if let (Some(body), Some(file_node_id)) = (body, file_node_id) {
                emit_references_in_body(body, ctx, &file_node_id, true, &[]);
            }
            return true;
        }

        if block_type == "assert" && labels.is_empty() {
            let file_node_id = ctx.node_stack().first().cloned();
            if let (Some(body), Some(file_node_id)) = (body, file_node_id) {
                emit_references_in_body(body, ctx, &file_node_id, true, &[]);
            }
            return true;
        }

        let Some(mut declaration) = describe_block(&block_type, &labels) else {
            return false;
        };

        if block_type == "provider" {
            if let (Some(body), Some(provider)) = (body, labels.first()) {
                if let Some(alias) =
                    read_string_attr(body, "alias", &source).filter(|alias| !alias.is_empty())
                {
                    declaration.name = format!("{provider}.{alias}");
                    declaration.qualified_name = format!("provider.{provider}.{alias}");
                    declaration.signature = format!("provider \"{provider}\" alias=\"{alias}\"");
                }
            }
        }

        let created = ctx.create_node(
            declaration.kind,
            &declaration.name,
            node,
            NodeExtra {
                qualified_name: Some(declaration.qualified_name),
                signature: Some(declaration.signature),
                is_exported: Some(declaration.kind == NodeKind::Variable),
                ..Default::default()
            },
        );
        let Some(created) = created else {
            return true;
        };

        if let Some(body) = body {
            ctx.push_scope(created.id.clone());
            let skip_top_attrs: &[&str] = match block_type.as_str() {
                "resource" | "data" => {
                    emit_provider_selection_ref(body, ctx, &created.id);
                    &["provider"]
                }
                "module" => {
                    emit_module_providers_refs(body, ctx, &created.id);
                    &["providers"]
                }
                _ => &[],
            };
            emit_references_in_body(body, ctx, &created.id, false, skip_top_attrs);
            if block_type == "module" {
                if let Some(module_name) = labels.first() {
                    emit_module_wiring(module_name, node, body, ctx, &created.id);
                }
            }
            ctx.pop_scope();
        }

        true
    }
}

fn emit_module_wiring(
    module_name: &str,
    block: SyntaxNode<'_>,
    body: SyntaxNode<'_>,
    ctx: &mut dyn ExtractorContext,
    from_node_id: &str,
) {
    let source = ctx.source().to_string();
    for attr in named_children(body).filter(|child| child.kind() == "attribute") {
        let Some(identifier) = named_child_of_kind(attr, "identifier") else {
            continue;
        };
        let attr_name = get_node_text(identifier, &source);
        if attr_name == "source" {
            let expression = named_child_of_kind(attr, "expression");
            let literal = expression.and_then(find_string_lit);
            let module_source = literal
                .map(|literal| string_lit_value(literal, &source))
                .unwrap_or_default();
            if module_source.starts_with("./") || module_source.starts_with("../") {
                add_reference(
                    ctx,
                    from_node_id,
                    format!("module.{module_name}:file"),
                    EdgeKind::Imports,
                    block.start_position().row as u32 + 1,
                    block.start_position().column as u32,
                );
            }
            continue;
        }
        if MODULE_META_ARGS.contains(&attr_name) {
            continue;
        }
        add_reference(
            ctx,
            from_node_id,
            format!("module.{module_name}:var.{attr_name}"),
            EdgeKind::References,
            attr.start_position().row as u32 + 1,
            attr.start_position().column as u32,
        );
    }
}

fn find_string_lit(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut queue = VecDeque::from([node]);
    while let Some(current) = queue.pop_front() {
        if current.kind() == "string_lit" {
            return Some(current);
        }
        queue.extend(named_children(current));
    }
    None
}

fn describe_block(block_type: &str, labels: &[String]) -> Option<BlockDecl> {
    let first = labels.first()?;
    match block_type {
        "resource" => {
            let second = labels.get(1)?;
            Some(BlockDecl {
                kind: NodeKind::Class,
                name: format!("{first}.{second}"),
                qualified_name: format!("{first}.{second}"),
                signature: format!("resource \"{first}\" \"{second}\""),
            })
        }
        "data" => {
            let second = labels.get(1)?;
            Some(BlockDecl {
                kind: NodeKind::Class,
                name: format!("{first}.{second}"),
                qualified_name: format!("data.{first}.{second}"),
                signature: format!("data \"{first}\" \"{second}\""),
            })
        }
        "module" => Some(BlockDecl {
            kind: NodeKind::Module,
            name: first.clone(),
            qualified_name: format!("module.{first}"),
            signature: format!("module \"{first}\""),
        }),
        "variable" => Some(BlockDecl {
            kind: NodeKind::Variable,
            name: first.clone(),
            qualified_name: format!("var.{first}"),
            signature: format!("variable \"{first}\""),
        }),
        "output" => Some(BlockDecl {
            kind: NodeKind::Variable,
            name: first.clone(),
            qualified_name: format!("output.{first}"),
            signature: format!("output \"{first}\""),
        }),
        "provider" => Some(BlockDecl {
            kind: NodeKind::Namespace,
            name: first.clone(),
            qualified_name: format!("provider.{first}"),
            signature: format!("provider \"{first}\""),
        }),
        _ => None,
    }
}

fn emit_locals(body: Option<SyntaxNode<'_>>, ctx: &mut dyn ExtractorContext) {
    let Some(body) = body else {
        return;
    };
    let source = ctx.source().to_string();

    for attr in named_children(body).filter(|child| child.kind() == "attribute") {
        let Some(identifier) = named_child_of_kind(attr, "identifier") else {
            continue;
        };
        let name = get_node_text(identifier, &source).to_string();
        let created = ctx.create_node(
            NodeKind::Constant,
            &name,
            attr,
            NodeExtra {
                qualified_name: Some(format!("local.{name}")),
                signature: Some(format!("local.{name}")),
                ..Default::default()
            },
        );
        let Some(created) = created else {
            continue;
        };
        let Some(expression) = named_child_of_kind(attr, "expression") else {
            continue;
        };

        ctx.push_scope(created.id.clone());
        for reference in collect_references(expression, &source) {
            add_reference(
                ctx,
                &created.id,
                reference.qualified_name,
                EdgeKind::References,
                reference.line,
                reference.column,
            );
        }
        ctx.pop_scope();
    }
}

fn emit_references_in_body(
    body: SyntaxNode<'_>,
    ctx: &mut dyn ExtractorContext,
    from_node_id: &str,
    suppress_scoped: bool,
    skip_top_attrs: &[&str],
) {
    let source = ctx.source().to_string();
    let mut queue = VecDeque::new();
    for child in named_children(body) {
        if child.kind() == "attribute" && !skip_top_attrs.is_empty() {
            if let Some(identifier) = named_child_of_kind(child, "identifier") {
                if skip_top_attrs.contains(&get_node_text(identifier, &source)) {
                    continue;
                }
            }
        }
        queue.push_back(child);
    }

    while let Some(node) = queue.pop_front() {
        if node.kind() == "expression" {
            for reference in collect_references(node, &source) {
                if suppress_scoped && reference.qualified_name.contains(':') {
                    continue;
                }
                add_reference(
                    ctx,
                    from_node_id,
                    reference.qualified_name,
                    EdgeKind::References,
                    reference.line,
                    reference.column,
                );
            }
            continue;
        }
        queue.extend(named_children(node));
    }
}

fn read_string_attr(body: SyntaxNode<'_>, name: &str, source: &str) -> Option<String> {
    for attr in named_children(body).filter(|child| child.kind() == "attribute") {
        let Some(identifier) = named_child_of_kind(attr, "identifier") else {
            continue;
        };
        if get_node_text(identifier, source) != name {
            continue;
        }
        let expression = named_child_of_kind(attr, "expression")?;
        let literal = find_string_lit(expression)?;
        return Some(string_lit_value(literal, source));
    }
    None
}

fn emit_provider_selection_ref(
    body: SyntaxNode<'_>,
    ctx: &mut dyn ExtractorContext,
    from_node_id: &str,
) {
    let source = ctx.source().to_string();
    for attr in named_children(body).filter(|child| child.kind() == "attribute") {
        let Some(identifier) = named_child_of_kind(attr, "identifier") else {
            continue;
        };
        if get_node_text(identifier, &source) != "provider" {
            continue;
        }
        let Some(expression) = named_child_of_kind(attr, "expression") else {
            return;
        };
        if let Some(selection) = provider_selection_from_expr(expression, &source) {
            add_reference(
                ctx,
                from_node_id,
                format!("provider.{selection}"),
                EdgeKind::References,
                attr.start_position().row as u32 + 1,
                attr.start_position().column as u32,
            );
        }
        return;
    }
}

fn emit_module_providers_refs(
    body: SyntaxNode<'_>,
    ctx: &mut dyn ExtractorContext,
    from_node_id: &str,
) {
    let source = ctx.source().to_string();
    for attr in named_children(body).filter(|child| child.kind() == "attribute") {
        let Some(identifier) = named_child_of_kind(attr, "identifier") else {
            continue;
        };
        if get_node_text(identifier, &source) != "providers" {
            continue;
        }

        let mut queue = VecDeque::from([attr]);
        while let Some(node) = queue.pop_front() {
            if node.kind() == "object_elem" {
                let selection = get_child_by_field(node, "val")
                    .and_then(|value| provider_selection_from_expr(value, &source));
                if let Some(selection) = selection {
                    add_reference(
                        ctx,
                        from_node_id,
                        format!("provider.{selection}"),
                        EdgeKind::References,
                        node.start_position().row as u32 + 1,
                        node.start_position().column as u32,
                    );
                }
                continue;
            }
            queue.extend(named_children(node));
        }
        return;
    }
}

fn provider_selection_from_expr(expr: SyntaxNode<'_>, source: &str) -> Option<String> {
    let mut queue = VecDeque::from([expr]);
    while let Some(node) = queue.pop_front() {
        if node.kind() == "variable_expr" {
            let identifier = named_child_of_kind(node, "identifier")?;
            let head = get_node_text(identifier, source);
            let next = node.next_named_sibling();
            if let Some(attr) = next.filter(|sibling| sibling.kind() == "get_attr") {
                let attr_identifier = named_child_of_kind(attr, "identifier")?;
                if attr.next_named_sibling().is_some() {
                    return None;
                }
                return Some(format!("{head}.{}", get_node_text(attr_identifier, source)));
            }
            return next.is_none().then(|| head.to_string());
        }
        if matches!(node.kind(), "function_call" | "conditional" | "for_expr") {
            return None;
        }
        queue.extend(named_children(node));
    }
    None
}
