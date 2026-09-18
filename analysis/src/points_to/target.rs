//! What a call op can be matched against: each analysed function's name,
//! owner and file.

use std::collections::HashMap;
use std::path::Path;

use crate::graph::CodeGraph;
use crate::ir::IrFunction;
use crate::nodes::{NodeId, NodeKind};

/// A function as a call target.
pub(super) struct Target<'g> {
    pub(super) name: &'g str,
    /// The second-to-last segment of the qualified name: `Foo` for
    /// `Foo::new`, `outer` for a function nested in `outer`, `None` for a
    /// top-level free function.
    pub(super) owner: Option<&'g str>,
    /// A method or associated function: `owner` names a struct, enum or
    /// trait, and the function is not nested in another function's body
    /// (a nested `fn` in a method is named after the method's type).
    pub(super) type_owned: bool,
    file: Option<&'g Path>,
}

impl<'g> Target<'g> {
    /// One target per entry of `functions`, in the same order.
    pub(super) fn all(graph: &'g CodeGraph, functions: &[(&NodeId, &'g IrFunction)]) -> Vec<Self> {
        let nested = nested_functions(graph, functions);
        // Owner name -> "names a struct, enum or trait"; methods share owners.
        let mut type_names: HashMap<&'g str, bool> = HashMap::new();
        let mut names_type = |owner: &'g str| {
            *type_names.entry(owner).or_insert_with(|| {
                [NodeKind::Struct, NodeKind::Enum, NodeKind::Trait]
                    .into_iter()
                    .any(|kind| !graph.nodes_by_kind_name(kind, owner).is_empty())
            })
        };
        functions
            .iter()
            .zip(nested)
            .map(|((id, ir), nested)| {
                let Some(node) = graph.get_node(id) else {
                    return Self {
                        name: &ir.name,
                        owner: None,
                        type_owned: false,
                        file: None,
                    };
                };
                let owner = owner_segment(&node.qualified_name);
                Self {
                    name: &node.name,
                    owner,
                    type_owned: !nested && owner.is_some_and(&mut names_type),
                    file: Some(node.file_path.as_path()),
                }
            })
            .collect()
    }

    /// Does a module path segment `seg` name this target's file or one of
    /// its directories (`helpers::f` for `src/helpers.rs`, `pkg.F` for
    /// `pkg/f.go`)?
    pub(super) fn lives_in_module(&self, seg: &str) -> bool {
        let wanted = normalize_module(seg);
        self.file.is_some_and(|file| {
            file.with_extension("")
                .components()
                .any(|c| normalize_module(&c.as_os_str().to_string_lossy()) == wanted)
        })
    }
}

fn owner_segment(qualified: &str) -> Option<&str> {
    let mut segments = qualified.rsplit("::");
    segments.next();
    segments.next()
}

fn normalize_module(seg: &str) -> String {
    seg.to_lowercase().replace('-', "_")
}

/// For each entry of `functions`: does another one's byte range in the
/// same file strictly contain it?
fn nested_functions(graph: &CodeGraph, functions: &[(&NodeId, &IrFunction)]) -> Vec<bool> {
    let mut by_file: HashMap<&Path, Vec<(usize, usize, usize)>> = HashMap::new();
    for (pos, (id, _)) in functions.iter().enumerate() {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        let range = &node.span.byte_range;
        if range.start < range.end {
            by_file.entry(node.file_path.as_path()).or_default().push((
                range.start,
                range.end,
                pos,
            ));
        }
    }
    let mut nested = vec![false; functions.len()];
    for spans in by_file.values_mut() {
        // Outer ranges first; a stack of the ranges still open.
        spans.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let mut open: Vec<(usize, usize)> = Vec::new();
        for &(start, end, pos) in spans.iter() {
            while open.last().is_some_and(|&(_, open_end)| open_end <= start) {
                open.pop();
            }
            nested[pos] = open.last().is_some_and(|&(open_start, open_end)| {
                (open_start, open_end) != (start, end) && end <= open_end
            });
            open.push((start, end));
        }
    }
    nested
}
