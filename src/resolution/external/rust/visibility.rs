//! Which items another crate could name, and when several same-named
//! definitions are one item: `pub` (or a method of a trait or trait impl,
//! which the index records without `pub`), `cfg` twins, and an inherent
//! method before trait-impl methods of its name.

use super::lookup::MAX_HEADER_LINES;
use crate::resolution::external::open::{ForeignGraph, GraphCache};
use crate::resolution::types::ResolutionContext;
use crate::types::{EdgeKind, Language, Node, NodeKind, Visibility};

/// One item, or none. Definitions of one qualified name in one file are
/// one item only when `cfg` attributes pick between them; otherwise they
/// are overloads (`impl From<i8> for Value`, `impl From<String> for
/// Value`, … all index as `Value::from`) and which one runs depends on
/// types this pass does not know — so none is taken.
pub(super) fn unique(graph: &ForeignGraph, candidates: Vec<Node>) -> Option<Node> {
    if let Some(one) = one_item(graph, &candidates) {
        return Some(one);
    }
    // Method resolution tries a type's inherent methods before its trait
    // methods: one inherent method among trait impls is the one called.
    let inherent: Vec<Node> = candidates
        .iter()
        .filter(|node| {
            node.kind == NodeKind::Method && !in_trait_or_trait_impl(&graph.context, node)
        })
        .cloned()
        .collect();
    if inherent.is_empty() || inherent.len() == candidates.len() {
        return None;
    }
    one_item(graph, &inherent)
}

/// The one item `candidates` are (definitions differing only by `cfg`).
fn one_item(graph: &ForeignGraph, candidates: &[Node]) -> Option<Node> {
    let first = candidates.first()?;
    let one_place = candidates.iter().all(|node| {
        node.qualified_name == first.qualified_name && node.file_path == first.file_path
    });
    (one_place && (candidates.len() == 1 || cfg_variants(&graph.context, candidates)))
        .then(|| first.clone())
}

/// Every definition but at most one carries a `cfg` attribute: platform
/// variants of one item.
fn cfg_variants(context: &dyn ResolutionContext, nodes: &[Node]) -> bool {
    let Some(source) = nodes
        .first()
        .and_then(|node| context.read_file_arc(&node.file_path))
    else {
        return false;
    };
    let lines: Vec<&str> = source.split('\n').collect();
    let gated = nodes
        .iter()
        .filter(|node| {
            let at = (node.start_line as usize)
                .saturating_sub(1)
                .min(lines.len());
            lines[..at]
                .iter()
                .rev()
                .map(|line| line.trim())
                .take_while(|line| line.starts_with("#[") || line.starts_with("///"))
                .any(|line| line.contains("cfg(") || line.contains("cfg_attr("))
        })
        .count();
    gated + 1 >= nodes.len()
}

/// `node` is one of several same-named overloads in its file (see
/// [`unique`]).
pub(super) fn overloaded(cache: &GraphCache<'_>, graph: &ForeignGraph, node: &Node) -> bool {
    if !matches!(node.kind, NodeKind::Method | NodeKind::Function) {
        return false;
    }
    let key = (graph.index, format!("overload:{}", node.id));
    if let Some(known) = cache.memo.visible.borrow().get(&key) {
        return *known;
    }
    let same: Vec<Node> = graph
        .context
        .get_nodes_by_name(&node.name)
        .into_iter()
        .filter(|other| {
            other.qualified_name == node.qualified_name && other.file_path == node.file_path
        })
        .collect();
    let overloaded =
        same.len() > 1 && unique(graph, same).is_none_or(|chosen| chosen.id != node.id);
    cache.memo.visible.borrow_mut().insert(key, overloaded);
    overloaded
}

/// `node` can be the target of a reference of `kind` from another crate.
pub(super) fn admitted(
    cache: &GraphCache<'_>,
    graph: &ForeignGraph,
    node: &Node,
    kind: EdgeKind,
) -> bool {
    let shaped = match kind {
        EdgeKind::Implements | EdgeKind::Extends => node.kind == NodeKind::Trait,
        EdgeKind::Calls | EdgeKind::Instantiates => matches!(
            node.kind,
            NodeKind::Function
                | NodeKind::Method
                | NodeKind::Struct
                | NodeKind::EnumMember
                | NodeKind::Constant
                | NodeKind::Variable
        ),
        _ => matches!(
            node.kind,
            NodeKind::Struct
                | NodeKind::Enum
                | NodeKind::EnumMember
                | NodeKind::Union
                | NodeKind::Trait
                | NodeKind::TypeAlias
                | NodeKind::Function
                | NodeKind::Method
                | NodeKind::Constant
                | NodeKind::Variable
        ),
    };
    shaped
        && node.language == Language::Rust
        && graph.in_crate(&node.file_path)
        && visible_outside(cache, graph, node)
}

/// Another crate could name `node`: it is `pub`, has no visibility of its
/// own (variants, trait items, aliases as indexed), or is a method of a
/// trait or trait impl.
pub(super) fn visible_outside(cache: &GraphCache<'_>, graph: &ForeignGraph, node: &Node) -> bool {
    if node.visibility != Some(Visibility::Private) {
        return true;
    }
    if node.kind != NodeKind::Method {
        return false;
    }
    let key = (graph.index, node.id.clone());
    if let Some(known) = cache.memo.visible.borrow().get(&key) {
        return *known;
    }
    let visible = in_trait_or_trait_impl(&graph.context, node);
    cache.memo.visible.borrow_mut().insert(key, visible);
    visible
}

/// The block holding the method `node` is a `trait` or an `impl … for …`.
fn in_trait_or_trait_impl(context: &dyn ResolutionContext, node: &Node) -> bool {
    let Some(source) = context.read_file_arc(&node.file_path) else {
        return false;
    };
    let lines: Vec<&str> = source.split('\n').collect();
    let Some(at) = (node.start_line as usize).checked_sub(1) else {
        return false;
    };
    let Some(own) = lines.get(at).map(|line| indentation(line)) else {
        return false;
    };
    for line in lines[..at].iter().rev().take(MAX_HEADER_LINES) {
        let trimmed = line.trim();
        if trimmed.is_empty() || indentation(line) >= own {
            continue;
        }
        if trimmed == "{" || trimmed.starts_with("where") || trimmed.starts_with("//") {
            continue;
        }
        let header = strip_visibility(trimmed);
        let header = header.strip_prefix("unsafe ").unwrap_or(header);
        if header.starts_with("trait ") {
            return true;
        }
        return header.starts_with("impl") && header.contains(" for ");
    }
    false
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn strip_visibility(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("pub") else {
        return text;
    };
    let rest = rest.trim_start();
    match rest.strip_prefix('(') {
        Some(inner) => inner
            .split_once(')')
            .map_or(rest, |(_, after)| after.trim_start()),
        None => rest,
    }
}

#[cfg(test)]
mod tests {
    use super::strip_visibility;

    #[test]
    fn strips_visibility_prefixes() {
        assert_eq!(strip_visibility("pub trait X {"), "trait X {");
        assert_eq!(strip_visibility("pub(crate) trait X {"), "trait X {");
        assert_eq!(strip_visibility("impl X for Y {"), "impl X for Y {");
    }
}
