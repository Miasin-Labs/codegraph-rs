//! Source slicing, line mapping, and graph node helpers.

use std::sync::LazyLock;

use regex::Regex;

use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::types::ResolutionContext;
use crate::types::{EdgeKind, Node, NodeKind};

pub(super) fn kebab_to_pascal(s: &str) -> String {
    s.split('-')
        .map(|p| {
            let mut chars = p.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// TS `sliceLines`: `content.split('\n').slice(startLine - 1, endLine).join('\n')`.
/// Returns `None` when either bound is falsy (0), mirroring the TS guard.
pub(super) fn slice_lines(content: &str, start_line: u32, end_line: u32) -> Option<String> {
    if start_line == 0 || end_line == 0 {
        return None;
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let start = ((start_line - 1) as usize).min(lines.len());
    let end = (end_line as usize).min(lines.len());
    if start >= end {
        return Some(String::new());
    }
    Some(lines[start..end].join("\n"))
}

/// TS call-site idiom `const src = content && sliceLines(...); if (!src) continue;`
/// — both a missing/empty file and an empty slice are skipped.
pub(super) fn node_source(ctx: &dyn ResolutionContext, n: &Node) -> Option<String> {
    let content = ctx.read_file(&n.file_path)?;
    if content.is_empty() {
        return None;
    }
    let src = slice_lines(&content, n.start_line, n.end_line)?;
    if src.is_empty() { None } else { Some(src) }
}

static REGISTRAR_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"this\.([0-9A-Za-z_]+)\.(?:add|push|set)\(").expect("valid regex")
});

pub(super) fn registrar_field(src: &str) -> Option<String> {
    REGISTRAR_FIELD_RE.captures(src).map(|m| m[1].to_string())
}

static DISPATCHER_FOR_OF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)of\s+(?:Array\.from\(\s*)?this\.([0-9A-Za-z_]+)").expect("valid regex")
});
static DISPATCHER_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)[0-9A-Za-z_]+\s*\(").expect("valid regex"));
static DISPATCHER_FOR_EACH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"this\.([0-9A-Za-z_]+)\.forEach\(").expect("valid regex"));

pub(super) fn dispatcher_field(src: &str) -> Option<String> {
    if let Some(for_of) = DISPATCHER_FOR_OF_RE.captures(src) {
        if DISPATCHER_CALL_RE.is_match(src) {
            return Some(for_of[1].to_string());
        }
    }
    DISPATCHER_FOR_EACH_RE
        .captures(src)
        .map(|m| m[1].to_string())
}

pub(super) fn is_fn_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::Component
    )
}

/// Innermost function/method node whose line range contains `line`.
pub(super) fn enclosing_fn(nodes_in_file: &[Node], line: u32) -> Option<&Node> {
    let mut best: Option<&Node> = None;
    for n in nodes_in_file {
        if !is_fn_kind(n.kind) {
            continue;
        }
        let end = n.end_line;
        if n.start_line <= line && end >= line {
            match best {
                Some(b) if n.start_line < b.start_line => {}
                // prefer the tightest (latest-starting) encloser
                _ => best = Some(n),
            }
        }
    }
    best
}

/// Count `'\n'` bytes — byte offsets from `regex` matches are char-boundary
/// safe, and newline counting over bytes equals the TS
/// `slice(0, idx).split('\n').length - 1`.
pub(super) fn count_newlines(s: &str) -> u32 {
    s.bytes().filter(|&b| b == b'\n').count() as u32
}

/// TS `lineOf`: `content.slice(0, idx).split('\n').length` (1-based line of a match index).
pub(super) fn line_of(content: &str, idx: usize) -> u32 {
    count_newlines(&content[..idx]) + 1
}

/// Methods directly contained by a class-like node.
pub(super) fn methods_of(queries: &QueryBuilder, class_id: &str) -> Result<Vec<Node>> {
    let mut out = Vec::new();
    for e in queries.get_outgoing_edges(class_id, Some(&[EdgeKind::Contains]), None)? {
        if let Some(n) = queries.get_node_by_id(&e.target)? {
            if n.kind == NodeKind::Method {
                out.push(n);
            }
        }
    }
    Ok(out)
}

/// Stream method + function nodes lazily. The synthesizers only scan-and-filter
/// down to a tiny matched subset, so materializing every function/method (which
/// is gigabytes on a symbol-dense project) just to iterate it once is what OOM'd
/// #610. Iterating keeps memory O(1) in the node count.
pub(super) fn for_each_method_and_function(
    queries: &QueryBuilder,
    mut f: impl FnMut(Node),
) -> Result<()> {
    queries.iterate_nodes_by_kind(NodeKind::Method, |n| {
        f(n);
        true
    })?;
    queries.iterate_nodes_by_kind(NodeKind::Function, |n| {
        f(n);
        true
    })?;
    Ok(())
}
