//! COBOL extraction configuration.
//!
//! COBOL's flat, column-oriented AST is handled through `visit_node` rather
//! than the generic block-language dispatch.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

use super::named_children;
use crate::extraction::tree_sitter_helpers::get_child_by_field;
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

pub struct CobolExtractor;

static EXEC_CICS_PROGRAM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?is)\b(?:LINK|XCTL)\b.*?\bPROGRAM\s*\(\s*(?:['"]([A-Za-z0-9$#@-]+)['"]|([A-Za-z0-9-]+))\s*\)"#,
    )
    .expect("valid CICS PROGRAM regex")
});
static EXEC_CICS_TRANSID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?is)\b(?:RETURN|START)\b.*?\bTRANSID\s*\(\s*(?:['"]([A-Za-z0-9$#@]{1,4})['"]|([A-Za-z0-9-]+))\s*\)"#,
    )
    .expect("valid CICS TRANSID regex")
});
static EXEC_SQL_INCLUDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)\bSQL\b.*?\bINCLUDE\s+([A-Za-z0-9$#@-]+)").expect("valid SQL INCLUDE regex")
});
static VALUE_LITERAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\bVALUE\s+['"]([A-Za-z0-9$#@-]+)['"]"#).expect("valid COBOL VALUE regex")
});
static SQL_INCLUDE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^([ \t]*(?:[0-9]{6})?[ \t]+EXEC\s+SQL\s+INCLUDE\s+[A-Za-z0-9$#@-]+\s+END-EXEC)([ \t]|$)",
    )
    .expect("valid SQL INCLUDE line regex")
});
static TERMINATED_EXEC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)END-EXEC\s*\.").expect("valid terminated EXEC regex"));
static FREE_FORMAT_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^([ \t]*)(IDENTIFICATION\s+DIVISION|ID\s+DIVISION|PROGRAM-ID\b|\d{2}[ \t]+[A-Za-z])",
    )
    .expect("valid COBOL format marker regex")
});
static SPECIAL_REGISTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(RETURN-CODE|SQLCODE|SQLSTATE|TALLY|EIB[A-Z-]+|DFH[A-Z-]+|WHEN-COMPILED|LENGTH|ADDRESS)$",
    )
    .expect("valid COBOL special-register regex")
});

fn is_free_format(source: &str) -> bool {
    source.lines().find_map(|line| {
        FREE_FORMAT_MARKER
            .captures(line)
            .and_then(|captures| captures.get(1))
            .map(|leading| leading.as_str().len() < 7)
    }) == Some(true)
}

fn terminate_sql_includes(source: &str) -> String {
    source
        .split('\n')
        .map(|line| {
            let (content, carriage_return) = line
                .strip_suffix('\r')
                .map_or((line, ""), |content| (content, "\r"));
            if !content.to_ascii_uppercase().contains("END-EXEC")
                || TERMINATED_EXEC.is_match(content)
            {
                return line.to_string();
            }
            let Some(captures) = SQL_INCLUDE_LINE.captures(content) else {
                return line.to_string();
            };
            let head = captures.get(1).expect("head capture").as_str();
            let suffix = if captures
                .get(2)
                .is_some_and(|value| !value.as_str().is_empty())
            {
                content.get(head.len() + 1..).unwrap_or("")
            } else {
                ""
            };
            format!("{head}.{suffix}{carriage_return}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_source(source: &str) -> String {
    if !is_free_format(source) {
        return terminate_sql_includes(source);
    }

    let shifted = source
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("CGWIDE {line}")
            } else if line.is_empty() {
                String::new()
            } else {
                format!("       {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    terminate_sql_includes(&shifted)
}

fn node_text(node: SyntaxNode<'_>, source: &str) -> String {
    let lines: Vec<_> = source.split('\n').collect();
    let start = node.start_position();
    let end = node.end_position();
    if start.row >= lines.len() || end.row >= lines.len() || start.row > end.row {
        return String::new();
    }
    let free_format = is_free_format(source);
    let adjusted_column = |row: usize, column: usize| {
        let shift = if free_format && (row == 0 || !lines[row].is_empty()) {
            7
        } else {
            0
        };
        column.saturating_sub(shift)
    };
    if start.row == end.row {
        return bounded_slice(
            lines[start.row],
            adjusted_column(start.row, start.column),
            adjusted_column(end.row, end.column),
        )
        .to_string();
    }

    let mut parts = Vec::with_capacity(end.row - start.row + 1);
    parts.push(
        lines[start.row]
            .get(adjusted_column(start.row, start.column)..)
            .unwrap_or("")
            .to_string(),
    );
    for line in lines.iter().take(end.row).skip(start.row + 1) {
        parts.push((*line).to_string());
    }
    parts.push(
        lines[end.row]
            .get(..adjusted_column(end.row, end.column).min(lines[end.row].len()))
            .unwrap_or("")
            .to_string(),
    );
    parts.join("\n")
}

fn bounded_slice(line: &str, from: usize, to: usize) -> &str {
    line.get(from.min(line.len())..to.min(line.len()))
        .unwrap_or("")
}

fn collapse(text: &str, cap: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= cap {
        flat
    } else {
        let mut value: String = flat.chars().take(cap.saturating_sub(3)).collect();
        value.push_str("...");
        value
    }
}

fn current_scope(ctx: &dyn ExtractorContext) -> Option<String> {
    ctx.node_stack().last().cloned()
}

fn add_ref(
    ctx: &mut dyn ExtractorContext,
    from_node_id: Option<&str>,
    reference_name: &str,
    reference_kind: EdgeKind,
    at: SyntaxNode<'_>,
) {
    let Some(from_node_id) = from_node_id.filter(|_| !reference_name.is_empty()) else {
        return;
    };
    let file_path = ctx.file_path().to_string();
    ctx.add_unresolved_reference(UnresolvedReference {
        from_node_id: from_node_id.to_string(),
        reference_name: reference_name.to_string(),
        reference_kind,
        line: at.start_position().row as u32 + 1,
        column: at.start_position().column as u32,
        file_path: Some(file_path),
        language: None,
        candidates: None,
        metadata: None,
    });
}

fn handle_copy(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) {
    let Some(book) = get_child_by_field(node, "book") else {
        return;
    };
    let name = node_text(book, ctx.source())
        .trim()
        .trim_matches(['\'', '"'])
        .to_string();
    if name.is_empty() {
        return;
    }
    let signature = collapse(&node_text(node, ctx.source()), 120);
    ctx.create_node(
        NodeKind::Import,
        &name,
        node,
        NodeExtra {
            signature: Some(signature),
            ..Default::default()
        },
    );
    let scope = current_scope(ctx);
    add_ref(ctx, scope.as_deref(), &name, EdgeKind::Imports, node);
}

fn deref_same_file_value(name: &str, ctx: &dyn ExtractorContext) -> Option<String> {
    ctx.nodes()
        .iter()
        .find(|node| {
            node.file_path == ctx.file_path()
                && matches!(
                    node.kind,
                    NodeKind::Variable | NodeKind::Field | NodeKind::Constant
                )
                && node.name.eq_ignore_ascii_case(name)
        })
        .and_then(|node| node.signature.as_deref())
        .and_then(|signature| VALUE_LITERAL.captures(signature))
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
}

fn handle_exec(node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext, from_node_id: Option<&str>) {
    let text = node_text(node, ctx.source());

    if let Some(captures) = EXEC_CICS_PROGRAM.captures(&text) {
        let program = captures
            .get(1)
            .map(|value| value.as_str().to_string())
            .or_else(|| {
                captures
                    .get(2)
                    .and_then(|value| deref_same_file_value(value.as_str(), ctx))
            });
        if let Some(program) = program {
            add_ref(ctx, from_node_id, &program, EdgeKind::Calls, node);
        }
    }

    if let Some(captures) = EXEC_CICS_TRANSID.captures(&text) {
        let transaction = captures
            .get(1)
            .map(|value| value.as_str().to_string())
            .or_else(|| {
                captures
                    .get(2)
                    .and_then(|value| deref_same_file_value(value.as_str(), ctx))
            });
        if let Some(transaction) = transaction.filter(|value| {
            !value.is_empty()
                && value.len() <= 4
                && value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "$#@".contains(character))
        }) {
            add_ref(
                ctx,
                from_node_id,
                &format!("cics-transid:{}", transaction.to_ascii_uppercase()),
                EdgeKind::Calls,
                node,
            );
        }
    }

    if let Some(include) = EXEC_SQL_INCLUDE
        .captures(&text)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
    {
        ctx.create_node(
            NodeKind::Import,
            &include,
            node,
            NodeExtra {
                signature: Some(collapse(&text, 120)),
                ..Default::default()
            },
        );
        let scope = from_node_id
            .map(str::to_string)
            .or_else(|| current_scope(ctx));
        add_ref(ctx, scope.as_deref(), &include, EdgeKind::Imports, node);
    }
}

struct DataItem<'tree> {
    node: SyntaxNode<'tree>,
    level: u32,
    name: Option<String>,
}

fn walk_data_entries(entries: Vec<SyntaxNode<'_>>, ctx: &mut dyn ExtractorContext) {
    let items: Vec<_> = entries
        .iter()
        .map(|entry| {
            if entry.kind() != "data_description" {
                return None;
            }
            let children = named_children(*entry);
            let level = children
                .iter()
                .find(|child| child.kind() == "level_number")
                .and_then(|level| node_text(*level, ctx.source()).trim().parse().ok())
                .unwrap_or(1);
            let name = children
                .iter()
                .find(|child| child.kind() == "entry_name")
                .map(|name| node_text(*name, ctx.source()).trim().to_string());
            Some(DataItem {
                node: *entry,
                level,
                name,
            })
        })
        .collect();

    let is_top_level = |level| matches!(level, 1 | 66 | 77);
    let mut open: Vec<u32> = Vec::new();

    for (entry, item) in entries.into_iter().zip(items) {
        if entry.kind() == "copy_statement" {
            handle_copy(entry, ctx);
            continue;
        }
        if entry.kind() == "exec_statement" {
            let scope = current_scope(ctx);
            handle_exec(entry, ctx, scope.as_deref());
            continue;
        }
        let Some(item) = item else {
            continue;
        };

        let is_condition = item.level == 88;
        if !is_condition {
            let close_level = if is_top_level(item.level) {
                0
            } else {
                item.level
            };
            while open.last().is_some_and(|level| *level >= close_level) {
                open.pop();
                ctx.pop_scope();
            }
        }

        let Some(name) = item
            .name
            .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case("FILLER"))
        else {
            continue;
        };
        let kind = if is_condition {
            NodeKind::Constant
        } else if open.is_empty() {
            NodeKind::Variable
        } else {
            NodeKind::Field
        };
        let signature = collapse(&node_text(item.node, ctx.source()), 120);
        let created = ctx.create_node(
            kind,
            &name,
            item.node,
            NodeExtra {
                signature: Some(signature),
                ..Default::default()
            },
        );
        if let Some(created) = created.filter(|_| !is_condition) {
            ctx.push_scope(created.id);
            open.push(item.level);
        }
    }

    while open.pop().is_some() {
        ctx.pop_scope();
    }
}

fn target_base_name(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    crate::ensure_sufficient_stack(|| target_base_name_inner(node))
}

fn target_base_name_inner(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    if node.kind() == "WORD" {
        return Some(node);
    }
    named_children(node).into_iter().find_map(target_base_name)
}

fn emit_write_refs(
    statement: SyntaxNode<'_>,
    fields: &[&str],
    from_node_id: Option<&str>,
    ctx: &mut dyn ExtractorContext,
) {
    for field in fields {
        let mut cursor = statement.walk();
        let targets: Vec<_> = statement
            .children_by_field_name(field, &mut cursor)
            .collect();
        for target in targets {
            let Some(word) = target_base_name(target) else {
                continue;
            };
            let name = node_text(word, ctx.source()).trim().to_string();
            if name.is_empty() || SPECIAL_REGISTER.is_match(&name) {
                continue;
            }
            add_ref(ctx, from_node_id, &name, EdgeKind::References, word);
        }
    }
}

fn collect_refs(node: SyntaxNode<'_>, from_node_id: Option<&str>, ctx: &mut dyn ExtractorContext) {
    crate::ensure_sufficient_stack(|| collect_refs_inner(node, from_node_id, ctx));
}

fn collect_refs_inner(
    node: SyntaxNode<'_>,
    from_node_id: Option<&str>,
    ctx: &mut dyn ExtractorContext,
) {
    match node.kind() {
        "move_statement" => emit_write_refs(node, &["dst"], from_node_id, ctx),
        "add_statement" => emit_write_refs(node, &["to", "giving"], from_node_id, ctx),
        "compute_statement" => emit_write_refs(node, &["left"], from_node_id, ctx),
        "subtract_statement" => {
            let mut cursor = node.walk();
            let has_giving = node
                .children_by_field_name("giving", &mut cursor)
                .next()
                .is_some();
            emit_write_refs(
                node,
                &[if has_giving { "giving" } else { "from" }],
                from_node_id,
                ctx,
            );
        }
        "perform_statement_call_proc" => {
            if let Some(procedure) = get_child_by_field(node, "procedure") {
                for label in named_children(procedure) {
                    if label.kind() == "label" {
                        let name = node_text(label, ctx.source()).trim().to_string();
                        add_ref(ctx, from_node_id, &name, EdgeKind::Calls, label);
                    }
                }
            }
        }
        "call_statement" => {
            if let Some(target) =
                get_child_by_field(node, "x").filter(|target| target.kind() == "string")
            {
                let name = node_text(target, ctx.source())
                    .trim()
                    .trim_matches(['\'', '"'])
                    .to_string();
                add_ref(ctx, from_node_id, &name, EdgeKind::Calls, target);
            }
        }
        "goto_statement" => {
            if let Some(target) = get_child_by_field(node, "to") {
                let name = node_text(target, ctx.source()).trim().to_string();
                add_ref(ctx, from_node_id, &name, EdgeKind::Calls, target);
            }
        }
        "exec_statement" => handle_exec(node, ctx, from_node_id),
        "copy_statement" => handle_copy(node, ctx),
        _ => {
            for child in named_children(node) {
                collect_refs(child, from_node_id, ctx);
            }
        }
    }
}

fn header_name(header: SyntaxNode<'_>, source: &str) -> String {
    let text = node_text(header, source);
    text.trim()
        .trim_end_matches('.')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

fn walk_procedure_children(children: Vec<SyntaxNode<'_>>, ctx: &mut dyn ExtractorContext) {
    let mut current_function = current_scope(ctx);
    let mut section_pushed = false;

    for child in children {
        if child.kind() == "section_header" {
            if section_pushed {
                ctx.pop_scope();
                section_pushed = false;
            }
            let name = header_name(child, ctx.source());
            if let Some(section) = ctx.create_node(
                NodeKind::Function,
                &name,
                child,
                NodeExtra {
                    signature: Some("SECTION".to_string()),
                    ..Default::default()
                },
            ) {
                current_function = Some(section.id.clone());
                ctx.push_scope(section.id);
                section_pushed = true;
            }
        } else if child.kind() == "paragraph_header" {
            let name = header_name(child, ctx.source());
            if let Some(paragraph) =
                ctx.create_node(NodeKind::Function, &name, child, NodeExtra::default())
            {
                current_function = Some(paragraph.id);
            }
        } else {
            collect_refs(child, current_function.as_deref(), ctx);
        }
    }

    if section_pushed {
        ctx.pop_scope();
    }
}

fn program_name(program: SyntaxNode<'_>, source: &str) -> Option<String> {
    let identification = named_children(program)
        .into_iter()
        .find(|child| child.kind() == "identification_division")?;
    let name = named_children(identification)
        .into_iter()
        .find(|child| child.kind() == "program_name")?;
    let value = node_text(name, source)
        .trim()
        .trim_end_matches('.')
        .trim_matches(['\'', '"'])
        .to_string();
    (!value.is_empty()).then_some(value)
}

impl LanguageExtractor for CobolExtractor {
    fn pre_parse<'a>(&self, source: &'a str, _file_path: &str) -> Cow<'a, str> {
        Cow::Owned(normalize_source(source))
    }

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
        "name"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    fn params_field(&self) -> &str {
        "parameters"
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        match node.kind() {
            "program_definition" => {
                let module = program_name(node, ctx.source()).and_then(|name| {
                    ctx.create_node(NodeKind::Module, &name, node, NodeExtra::default())
                });
                if let Some(module) = &module {
                    ctx.push_scope(module.id.clone());
                }
                for child in named_children(node) {
                    ctx.visit_node(child);
                }
                if module.is_some() {
                    ctx.pop_scope();
                }
                true
            }
            "procedure_division" => {
                walk_procedure_children(named_children(node), ctx);
                true
            }
            "working_storage_section" | "record_description_list" => {
                walk_data_entries(named_children(node), ctx);
                true
            }
            "copybook_fragment" => {
                let children = named_children(node);
                if children
                    .iter()
                    .any(|child| child.kind() == "record_description_list")
                {
                    for child in children {
                        ctx.visit_node(child);
                    }
                } else {
                    walk_procedure_children(children, ctx);
                }
                true
            }
            "copy_statement" => {
                handle_copy(node, ctx);
                true
            }
            "exec_statement" => {
                let scope = current_scope(ctx);
                handle_exec(node, ctx, scope.as_deref());
                true
            }
            _ => false,
        }
    }
}
