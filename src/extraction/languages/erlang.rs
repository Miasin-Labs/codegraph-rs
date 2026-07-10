//! Erlang extraction configuration.
//!
//! Node names follow the WhatsApp/tree-sitter-erlang grammar used upstream.

use std::collections::HashSet;

use super::named_children;
use crate::extraction::tree_sitter_helpers::{
    get_child_by_field,
    get_node_text,
    get_preceding_docstring,
};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

pub struct ErlangExtractor;

enum ModuleExports {
    All,
    Names(HashSet<String>),
}

fn atom_text(node: SyntaxNode<'_>, source: &str) -> String {
    let text = get_node_text(node, source);
    text.strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .unwrap_or(text)
        .to_string()
}

fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn capped(text: String, cap: usize) -> String {
    text.chars().take(cap).collect()
}

fn module_exports(node: SyntaxNode<'_>, source: &str) -> ModuleExports {
    let mut root = node;
    while let Some(parent) = root.parent() {
        root = parent;
    }

    let mut exports = HashSet::new();
    for form in named_children(root) {
        if form.kind() == "compile_options_attribute"
            && get_node_text(form, source).contains("export_all")
        {
            return ModuleExports::All;
        }
        if form.kind() != "export_attribute" {
            continue;
        }
        for function_arity in named_children(form) {
            if function_arity.kind() != "fa" {
                continue;
            }
            if let Some(function) = get_child_by_field(function_arity, "fun") {
                exports.insert(atom_text(function, source));
            }
        }
    }
    ModuleExports::Names(exports)
}

fn preceding_spec<'tree>(
    node: SyntaxNode<'tree>,
    name: &str,
    source: &str,
) -> Option<SyntaxNode<'tree>> {
    let mut previous = node.prev_named_sibling();
    while previous.is_some_and(|candidate| candidate.kind() == "comment") {
        previous = previous.and_then(|candidate| candidate.prev_named_sibling());
    }
    let spec = previous.filter(|candidate| candidate.kind() == "spec")?;
    let function = get_child_by_field(spec, "fun")?;
    (atom_text(function, source) == name).then_some(spec)
}

fn clause_header(clause: SyntaxNode<'_>, source: &str) -> Option<String> {
    let end = get_child_by_field(clause, "body")
        .map(|body| body.start_byte())
        .unwrap_or_else(|| clause.end_byte());
    let header = collapse_ws(source.get(clause.start_byte()..end).unwrap_or(""));
    (!header.is_empty()).then_some(header)
}

fn previous_clause_has_name(node: SyntaxNode<'_>, name: &str, source: &str) -> bool {
    let mut previous = node.prev_named_sibling();
    while previous.is_some_and(|candidate| candidate.kind() == "comment") {
        previous = previous.and_then(|candidate| candidate.prev_named_sibling());
    }
    let Some(previous) = previous.filter(|candidate| candidate.kind() == "fun_decl") else {
        return false;
    };
    named_children(previous)
        .into_iter()
        .find(|candidate| candidate.kind() == "function_clause")
        .and_then(|clause| get_child_by_field(clause, "name"))
        .is_some_and(|function| atom_text(function, source) == name)
}

fn handle_fun_decl(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
    let clauses: Vec<_> = named_children(node)
        .into_iter()
        .filter(|child| child.kind() == "function_clause")
        .collect();
    let Some(first) = clauses.first().copied() else {
        return true;
    };
    let Some(name_node) = get_child_by_field(first, "name") else {
        return true;
    };
    let name = atom_text(name_node, ctx.source());
    if name.is_empty() {
        return true;
    }

    if previous_clause_has_name(node, &name, ctx.source()) {
        let existing = ctx
            .nodes()
            .iter()
            .rev()
            .find(|candidate| {
                candidate.kind == NodeKind::Function
                    && candidate.name == name
                    && candidate.file_path == ctx.file_path()
            })
            .map(|candidate| candidate.id.clone());
        if let Some(id) = existing {
            ctx.push_scope(id.clone());
            for clause in clauses {
                ctx.visit_function_body(clause, &id);
            }
            ctx.pop_scope();
            return true;
        }
    }

    let spec = preceding_spec(node, &name, ctx.source());
    let exports = module_exports(node, ctx.source());
    let signature = spec
        .map(|spec| capped(collapse_ws(get_node_text(spec, ctx.source())), 300))
        .or_else(|| clause_header(first, ctx.source()));
    let docstring = get_preceding_docstring(spec.unwrap_or(node), ctx.source());
    let is_exported = match exports {
        ModuleExports::All => true,
        ModuleExports::Names(names) => names.contains(&name),
    };

    let Some(function) = ctx.create_node(
        NodeKind::Function,
        &name,
        node,
        NodeExtra {
            docstring,
            signature,
            is_exported: Some(is_exported),
            ..Default::default()
        },
    ) else {
        return true;
    };
    let id = function.id;
    ctx.push_scope(id.clone());
    for clause in clauses {
        ctx.visit_function_body(clause, &id);
    }
    ctx.pop_scope();
    true
}

fn handle_record_decl(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
    let Some(name_node) = get_child_by_field(node, "name") else {
        return true;
    };
    let name = atom_text(name_node, ctx.source());
    let docstring = get_preceding_docstring(node, ctx.source());
    let signature = capped(collapse_ws(get_node_text(node, ctx.source())), 300);
    if let Some(record) = ctx.create_node(
        NodeKind::Struct,
        &name,
        node,
        NodeExtra {
            docstring,
            signature: Some(signature),
            ..Default::default()
        },
    ) {
        ctx.push_scope(record.id);
        for field in named_children(node) {
            if field.kind() != "record_field" {
                continue;
            }
            if let Some(field_name) = get_child_by_field(field, "name") {
                let name = atom_text(field_name, ctx.source());
                ctx.create_node(NodeKind::Field, &name, field, NodeExtra::default());
            }
        }
        ctx.pop_scope();
    }
    true
}

fn handle_type_alias(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
    let name = get_child_by_field(node, "name")
        .and_then(|type_name| get_child_by_field(type_name, "name"));
    if let Some(name_node) = name {
        let name = atom_text(name_node, ctx.source());
        let signature = capped(collapse_ws(get_node_text(node, ctx.source())), 200);
        ctx.create_node(
            NodeKind::TypeAlias,
            &name,
            node,
            NodeExtra {
                signature: Some(signature),
                ..Default::default()
            },
        );
    }
    true
}

fn handle_pp_define(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
    let name_node =
        get_child_by_field(node, "lhs").and_then(|left| get_child_by_field(left, "name"));
    let Some(name_node) = name_node else {
        return true;
    };
    let name = get_node_text(name_node, ctx.source()).to_string();
    let signature = capped(collapse_ws(get_node_text(node, ctx.source())), 200);
    let macro_node = ctx.create_node(
        NodeKind::Constant,
        &name,
        node,
        NodeExtra {
            signature: Some(signature),
            ..Default::default()
        },
    );
    if let (Some(macro_node), Some(replacement)) =
        (macro_node, get_child_by_field(node, "replacement"))
    {
        let id = macro_node.id;
        ctx.push_scope(id.clone());
        ctx.visit_function_body(replacement, &id);
        ctx.pop_scope();
    }
    true
}

fn handle_behaviour(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
    let name = get_child_by_field(node, "name").map(|name| atom_text(name, ctx.source()));
    let parent = ctx.node_stack().last().cloned();
    if let (Some(name), Some(parent)) = (name, parent) {
        let file_path = ctx.file_path().to_string();
        ctx.add_unresolved_reference(UnresolvedReference {
            from_node_id: parent,
            reference_name: name,
            reference_kind: EdgeKind::Implements,
            line: node.start_position().row as u32 + 1,
            column: node.start_position().column as u32,
            file_path: Some(file_path),
            language: None,
            candidates: None,
            metadata: None,
        });
    }
    true
}

fn handle_app_resource_tuple(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
    let Some(parent) = ctx.node_stack().last().cloned() else {
        return true;
    };
    let file_path = ctx.file_path().to_string();
    let children = named_children(node);
    let Some(properties) = children
        .get(2)
        .copied()
        .filter(|child| child.kind() == "list")
    else {
        return true;
    };

    for property in named_children(properties) {
        let pair = named_children(property);
        if property.kind() != "tuple" || pair.len() < 2 || pair[0].kind() != "atom" {
            continue;
        }
        let key = atom_text(pair[0], ctx.source());
        let value = pair[1];
        let mut add_reference = |target: SyntaxNode<'_>, kind: EdgeKind| {
            let name = atom_text(target, ctx.source());
            if !name.is_empty() {
                ctx.add_unresolved_reference(UnresolvedReference {
                    from_node_id: parent.clone(),
                    reference_name: name,
                    reference_kind: kind,
                    line: target.start_position().row as u32 + 1,
                    column: target.start_position().column as u32,
                    file_path: Some(file_path.clone()),
                    language: None,
                    candidates: None,
                    metadata: None,
                });
            }
        };

        if key == "mod" && value.kind() == "tuple" {
            if let Some(module) = named_children(value)
                .into_iter()
                .next()
                .filter(|child| child.kind() == "atom")
            {
                add_reference(module, EdgeKind::References);
            }
        } else if matches!(key.as_str(), "applications" | "included_applications")
            && value.kind() == "list"
        {
            for application in named_children(value) {
                if application.kind() == "atom" {
                    add_reference(application, EdgeKind::Imports);
                }
            }
        }
    }
    true
}

impl LanguageExtractor for ErlangExtractor {
    fn function_types(&self) -> &[&str] {
        &["fun_decl"]
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
        &["record_decl"]
    }
    fn enum_types(&self) -> &[&str] {
        &[]
    }
    fn type_alias_types(&self) -> &[&str] {
        &["type_alias", "opaque"]
    }
    fn import_types(&self) -> &[&str] {
        &["import_attribute", "pp_include", "pp_include_lib"]
    }
    fn call_types(&self) -> &[&str] {
        &[
            "call",
            "internal_fun",
            "external_fun",
            "record_expr",
            "record_update_expr",
            "record_index_expr",
            "record_field_expr",
            "macro_call_expr",
        ]
    }
    fn variable_types(&self) -> &[&str] {
        &[]
    }
    fn name_field(&self) -> &str {
        "name"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    fn params_field(&self) -> &str {
        "args"
    }
    fn package_types(&self) -> &[&str] {
        &["module_attribute"]
    }

    fn extract_package(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        get_child_by_field(node, "name").map(|name| atom_text(name, source))
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        if node.kind() == "import_attribute" {
            return match get_child_by_field(node, "module") {
                Some(module) => ImportOutcome::Info(ImportInfo::new(
                    atom_text(module, source),
                    capped(collapse_ws(get_node_text(node, source)), 200),
                )),
                None => ImportOutcome::Declined,
            };
        }

        let Some(file) = get_child_by_field(node, "file") else {
            return ImportOutcome::Declined;
        };
        let path = get_node_text(file, source).trim_matches('"');
        if path.is_empty() {
            ImportOutcome::Declined
        } else {
            ImportOutcome::Info(ImportInfo::new(path, get_node_text(node, source).trim()))
        }
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        match node.kind() {
            "fun_decl" => handle_fun_decl(node, ctx),
            "record_decl" => handle_record_decl(node, ctx),
            "type_alias" | "opaque" => handle_type_alias(node, ctx),
            "pp_define" => handle_pp_define(node, ctx),
            "behaviour_attribute" => handle_behaviour(node, ctx),
            "spec" | "callback" => true,
            "tuple" => {
                let is_root_tuple = node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "source_file");
                let is_app_file = {
                    let path = ctx.file_path().to_ascii_lowercase();
                    path.ends_with(".app") || path.ends_with(".app.src")
                };
                let is_application = named_children(node)
                    .into_iter()
                    .next()
                    .filter(|first| first.kind() == "atom")
                    .is_some_and(|first| atom_text(first, ctx.source()) == "application");
                if is_root_tuple && is_app_file && is_application {
                    handle_app_resource_tuple(node, ctx)
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}
