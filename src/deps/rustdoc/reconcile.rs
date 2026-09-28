//! A shard's nodes, reconciled with what rustdoc says its crate holds —
//! run on the shard's unpublished database, before it is compacted.
//!
//! tree-sitter's grammar can lose an item rustdoc knows: nightly syntax in
//! an `impl` header (`impl<T> const Default for …`, `[const]` bounds) makes
//! its methods parse as bare functions (`unwrap`, not `Option::unwrap`), and
//! some items vanish (`Result::unwrap`, nightly `RefCell`). Each item of an
//! index whose span is one of the shard's files is matched to the node of
//! its name and kind at that line:
//!
//! * a method found as a bare function becomes the method (`Owner::name`);
//! * an item with no node gets one — only when its line is its own
//!   declaration (`fn name`, `struct Name` …), never a macro invocation or a
//!   `#[derive]` (generated items have no source of their own).

use std::collections::HashMap;

use super::model::{ApiIndex, ApiItem, ApiKind};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::types::{Edge, EdgeKind, Language, Node, NodeKind, Visibility};

/// What a reconciliation changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub renamed: usize,
    pub added: usize,
}

/// The node kinds an item of `kind` may be indexed as.
pub fn node_kinds(kind: ApiKind) -> &'static [NodeKind] {
    match kind {
        ApiKind::Struct => &[NodeKind::Struct],
        ApiKind::Enum => &[NodeKind::Enum],
        ApiKind::Union => &[NodeKind::Union],
        ApiKind::Trait => &[NodeKind::Trait],
        ApiKind::TypeAlias => &[NodeKind::TypeAlias],
        ApiKind::Function | ApiKind::Method => &[NodeKind::Method, NodeKind::Function],
        ApiKind::Variant => &[NodeKind::EnumMember],
        ApiKind::Constant | ApiKind::AssocConst => &[NodeKind::Constant],
        ApiKind::Static => &[NodeKind::Variable, NodeKind::Constant],
        ApiKind::Macro => &[NodeKind::Macro],
        ApiKind::Module | ApiKind::Primitive | ApiKind::AssocType => &[],
    }
}

/// The node of `candidates` (nodes of the item's file and name) that is the
/// item: of a matching kind, spanning its line, starting nearest to it —
/// exactly one such, or none.
pub fn matching_node<'n>(item: &ApiItem, candidates: &'n [Node]) -> Option<&'n Node> {
    let kinds = node_kinds(item.kind);
    let fits: Vec<&Node> = candidates
        .iter()
        .filter(|node| {
            node.name == item.name
                && kinds.contains(&node.kind)
                && node.start_line <= item.line
                && node.end_line.max(node.start_line) >= item.line
        })
        .collect();
    let best = fits.iter().map(|node| item.line - node.start_line).min()?;
    let mut nearest = fits
        .into_iter()
        .filter(|node| item.line - node.start_line == best);
    let one = nearest.next()?;
    nearest.next().is_none().then_some(one)
}

/// Reconcile the nodes of the shard `queries` (its source under `root`)
/// with `indexes`. `own_file` says which index files are this shard's
/// (relative to its source root, as nodes record them).
pub fn reconcile(
    queries: &QueryBuilder,
    root: &std::path::Path,
    indexes: &[&ApiIndex],
    own_file: &dyn Fn(&str) -> Option<String>,
) -> Result<Reconciled> {
    let mut by_file: HashMap<String, Vec<&ApiItem>> = HashMap::new();
    for index in indexes {
        for item in &index.items {
            if item.line == 0 || node_kinds(item.kind).is_empty() {
                continue;
            }
            if let Some(file) = own_file(&item.file) {
                by_file.entry(file).or_default().push(item);
            }
        }
    }
    let mut files: Vec<String> = by_file.keys().cloned().collect();
    files.sort();
    let mut done = Reconciled::default();
    queries.db().transaction(|| {
        for file in &files {
            let nodes = queries.get_nodes_by_file(file)?;
            if nodes.is_empty() {
                continue; // not indexed (budgets, tests, other targets)
            }
            let mut named: HashMap<&str, Vec<Node>> = HashMap::new();
            for node in &nodes {
                named
                    .entry(node.name.as_str())
                    .or_default()
                    .push(node.clone());
            }
            let file_node = nodes.iter().find(|node| node.kind == NodeKind::File);
            let mut source: Option<Vec<String>> = None;
            let mut seen = std::collections::HashSet::new();
            for item in &by_file[file] {
                if !seen.insert((item.name.as_str(), item.line, item.kind)) {
                    continue;
                }
                let candidates = named
                    .get(item.name.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if let Some(node) = matching_node(item, candidates) {
                    if let Some(renamed) = hoisted_method(item, node) {
                        queries.update_node(&renamed)?;
                        done.renamed += 1;
                    }
                    continue;
                }
                let lines = source.get_or_insert_with(|| {
                    std::fs::read_to_string(root.join(file))
                        .map(|text| text.lines().map(str::to_string).collect())
                        .unwrap_or_default()
                });
                let Some(node) = declared_node(item, file, lines) else {
                    continue;
                };
                if candidates.iter().any(|other| other.id == node.id) {
                    continue;
                }
                queries.insert_node(&node)?;
                if let Some(parent) = file_node {
                    let mut edge = Edge::new(&parent.id, &node.id, EdgeKind::Contains);
                    edge.line = Some(node.start_line);
                    queries.insert_edges(&[edge])?;
                }
                done.added += 1;
            }
        }
        Ok(())
    })?;
    Ok(done)
}

/// A method the grammar indexed as a bare function (its `impl` header
/// lost): the node as the method.
fn hoisted_method(item: &ApiItem, node: &Node) -> Option<Node> {
    let owner = item.owner.as_deref()?;
    if item.kind != ApiKind::Method
        || node.kind != NodeKind::Function
        || node.qualified_name.contains("::")
        || owner.is_empty()
        || !owner.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return None;
    }
    let mut renamed = node.clone();
    renamed.kind = NodeKind::Method;
    renamed.qualified_name = format!("{owner}::{}", item.name);
    Some(renamed)
}

/// A node for an item the index has no node for, when `lines` (its file)
/// declare it at its line.
fn declared_node(item: &ApiItem, file: &str, lines: &[String]) -> Option<Node> {
    let at = (item.line as usize).checked_sub(1)?;
    let text = lines.get(at)?;
    let code = text.split("//").next().unwrap_or("");
    let keyword = match item.kind {
        ApiKind::Function | ApiKind::Method => "fn",
        ApiKind::Struct => "struct",
        ApiKind::Enum => "enum",
        ApiKind::Union => "union",
        ApiKind::Trait => "trait",
        ApiKind::TypeAlias => "type",
        ApiKind::Constant | ApiKind::AssocConst => "const",
        ApiKind::Variant => "",
        _ => return None,
    };
    let declared = if keyword.is_empty() {
        code.trim_start().starts_with(item.name.as_str())
    } else {
        let needle = format!("{keyword} {}", item.name);
        code.find(&needle).is_some_and(|at| {
            let after = code[at + needle.len()..].chars().next();
            !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    };
    if !declared || code.contains('!') || code.trim_start().starts_with("#[") {
        return None;
    }
    let kind = *node_kinds(item.kind).first()?;
    let qualified_name = match (&item.owner, item.kind) {
        (Some(owner), ApiKind::Method | ApiKind::AssocConst | ApiKind::Variant)
            if owner.chars().all(|c| c.is_alphanumeric() || c == '_') =>
        {
            format!("{owner}::{}", item.name)
        }
        _ => item.name.clone(),
    };
    let signature = matches!(item.kind, ApiKind::Function | ApiKind::Method)
        .then(|| fn_signature(lines, at, &item.name))
        .flatten();
    let mut node = Node::new(
        generate_node_id(file, kind, &item.name, item.line),
        kind,
        item.name.clone(),
        qualified_name,
        file,
        Language::Rust,
        item.line,
        item.end_line.max(item.line),
    );
    node.signature = signature;
    node.visibility = Some(if code.trim_start().starts_with("pub ") || item.public {
        Visibility::Public
    } else {
        Visibility::Private
    });
    Some(node)
}

/// `(params) -> Ret` of the fn declared at `at` (as the index writes
/// signatures), read up to its body or `where` clause.
fn fn_signature(lines: &[String], at: usize, name: &str) -> Option<String> {
    let text: String = lines
        .iter()
        .skip(at)
        .take(20)
        .map(|line| line.split("//").next().unwrap_or("").trim())
        .collect::<Vec<_>>()
        .join(" ");
    let start = text.find(&format!("fn {name}"))? + 3 + name.len();
    let rest = &text[start..];
    // Skip the generic parameters.
    let mut depth = 0i32;
    let mut open = None;
    for (index, c) in rest.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            '(' if depth <= 0 => {
                open = Some(index);
                break;
            }
            _ => {}
        }
    }
    let rest = &rest[open?..];
    let mut depth = 0i32;
    let mut end = rest.len();
    for (index, c) in rest.char_indices() {
        match c {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            '{' | ';' if depth <= 0 => {
                end = index;
                break;
            }
            _ => {}
        }
        if depth <= 0 && rest[index..].starts_with(" where ") {
            end = index;
            break;
        }
    }
    let signature = rest[..end].split_whitespace().collect::<Vec<_>>().join(" ");
    (!signature.is_empty()).then_some(signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: ApiKind, name: &str, owner: Option<&str>, line: u32) -> ApiItem {
        ApiItem {
            kind,
            name: name.into(),
            canonical: String::new(),
            file: "core/src/result.rs".into(),
            line,
            end_line: line + 2,
            owner: owner.map(str::to_string),
            has_self: true,
            public: true,
        }
    }

    #[test]
    fn a_lost_method_is_declared_from_its_line() {
        let lines: Vec<String> = [
            "impl<T, E> Result<T, E> {",
            "    pub const fn unwrap(self) -> T",
            "    where",
            "        E: [const] fmt::Debug,",
            "    {",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let node = declared_node(
            &item(ApiKind::Method, "unwrap", Some("Result"), 2),
            "core/src/result.rs",
            &lines,
        )
        .unwrap();
        assert_eq!(node.qualified_name, "Result::unwrap");
        assert_eq!(node.kind, NodeKind::Method);
        assert_eq!(node.signature.as_deref(), Some("(self) -> T"));
        // A macro invocation is never a declaration.
        let lines: Vec<String> = vec!["    int_impl! { fn unwrap }".into()];
        assert!(
            declared_node(
                &item(ApiKind::Method, "unwrap", Some("u8"), 1),
                "x.rs",
                &lines
            )
            .is_none()
        );
    }

    #[test]
    fn hoisted_functions_become_methods() {
        let mut node = Node::new(
            "function:x",
            NodeKind::Function,
            "unwrap",
            "unwrap",
            "core/src/option.rs",
            Language::Rust,
            10,
            12,
        );
        let it = item(ApiKind::Method, "unwrap", Some("Option"), 10);
        assert_eq!(
            matching_node(&it, std::slice::from_ref(&node)).map(|n| n.id.as_str()),
            Some("function:x")
        );
        let renamed = hoisted_method(&it, &node).unwrap();
        assert_eq!(renamed.qualified_name, "Option::unwrap");
        assert_eq!(renamed.kind, NodeKind::Method);
        node.qualified_name = "Option::unwrap".into();
        assert!(hoisted_method(&it, &node).is_none());
    }
}
