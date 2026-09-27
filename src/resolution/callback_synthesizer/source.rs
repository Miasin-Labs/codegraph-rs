//! Source slicing, line mapping, and graph node helpers.

use std::sync::LazyLock;

use regex::Regex;

use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::line_index;
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
/// Callers now slice shared sources through `line_index::slice_lines`; this
/// owned-string form stays as the pinned TS-parity reference for tests.
#[cfg(test)]
pub(super) fn slice_lines(content: &str, start_line: u32, end_line: u32) -> Option<String> {
    if start_line == 0 || end_line == 0 {
        return None;
    }
    // The byte range of those lines, found without collecting every line.
    let mut starts = std::iter::once(0).chain(content.match_indices('\n').map(|(at, _)| at + 1));
    let Some(from) = starts.nth((start_line - 1) as usize) else {
        return Some(String::new());
    };
    if start_line > end_line {
        return Some(String::new());
    }
    let to = starts
        .nth((end_line - start_line) as usize)
        .map_or(content.len(), |next| next - 1);
    Some(content[from..to].to_string())
}

/// TS call-site idiom `const src = content && sliceLines(...); if (!src) continue;`
/// — both a missing/empty file and an empty slice are skipped.
///
/// Runs once per function/method, so the file is shared (`read_file_arc`)
/// and sliced through the per-file line index — copying and splitting the
/// whole file per node was quadratic on bundled files.
pub(super) fn node_source(ctx: &dyn ResolutionContext, n: &Node) -> Option<String> {
    let content = ctx.read_file_arc(&n.file_path)?;
    if content.is_empty() {
        return None;
    }
    let src = line_index::slice_lines(&content, n.start_line, n.end_line)?;
    if src.is_empty() {
        None
    } else {
        Some(src.to_string())
    }
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

/// `enclosing_fn` answered by lookup, for files with many matches: which
/// function each line falls in, painted once (outer first, so the
/// latest-starting encloser — and on ties the last in order — wins, exactly
/// as `enclosing_fn` picks). Scanning every node per match was quadratic on
/// bundled files.
pub(super) struct FnIndex<'a> {
    nodes: &'a [Node],
    /// Per line, 1 + the index in `nodes` of its innermost function (0 = none).
    by_line: Vec<u32>,
}

impl<'a> FnIndex<'a> {
    pub(super) fn new(nodes: &'a [Node]) -> Self {
        let mut fns: Vec<usize> = (0..nodes.len())
            .filter(|&i| is_fn_kind(nodes[i].kind) && nodes[i].start_line <= nodes[i].end_line)
            .collect();
        // Stable, so equal starts keep their order and the last one paints last.
        fns.sort_by_key(|&i| nodes[i].start_line);
        let last = fns.iter().map(|&i| nodes[i].end_line).max().unwrap_or(0);
        let mut by_line = if fns.is_empty() {
            Vec::new()
        } else {
            vec![0u32; last as usize + 1]
        };
        for i in fns {
            let n = &nodes[i];
            by_line[n.start_line as usize..=n.end_line as usize].fill(i as u32 + 1);
        }
        Self { nodes, by_line }
    }

    /// The same node `enclosing_fn(nodes, line)` returns.
    pub(super) fn enclosing(&self, line: u32) -> Option<&'a Node> {
        let slot = *self.by_line.get(line as usize)?;
        slot.checked_sub(1).map(|i| &self.nodes[i as usize])
    }
}

/// Count `'\n'` bytes — byte offsets from `regex` matches are char-boundary
/// safe, and newline counting over bytes equals the TS
/// `slice(0, idx).split('\n').length - 1`.
pub(super) fn count_newlines(s: &str) -> u32 {
    s.bytes().filter(|&b| b == b'\n').count() as u32
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

#[cfg(test)]
mod tests {
    use super::{FnIndex, enclosing_fn};
    use crate::types::{Language, Node, NodeKind};

    fn node(id: &str, kind: NodeKind, start: u32, end: u32) -> Node {
        Node::new(id, kind, id, id, "f.ts", Language::Typescript, start, end)
    }

    #[test]
    fn fn_index_answers_like_enclosing_fn() {
        let nodes = vec![
            node("outer", NodeKind::Function, 1, 40),
            node("class", NodeKind::Class, 2, 30),
            node("a", NodeKind::Method, 5, 10),
            node("a_twin", NodeKind::Method, 5, 8),
            node("b", NodeKind::Component, 12, 20),
            node("inner", NodeKind::Function, 14, 16),
            node("bad", NodeKind::Function, 25, 22),
            node("late", NodeKind::Function, 35, 50),
        ];
        let index = FnIndex::new(&nodes);
        for line in 0..60 {
            assert_eq!(
                index.enclosing(line).map(|n| n.id.as_str()),
                enclosing_fn(&nodes, line).map(|n| n.id.as_str()),
                "line {line}"
            );
        }
        assert!(FnIndex::new(&[]).enclosing(3).is_none());
    }
}
