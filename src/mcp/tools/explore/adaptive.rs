use std::collections::{HashMap, HashSet};

use super::super::format::{ExploreOutputBudget, FlowInfo, adaptive_explore_enabled, slice_lines};
use super::skeleton::{adaptive_header_names, render_skeleton};
use super::types::{FileGroup, RenderedFile};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::types::{EdgeKind, Node, NodeKind};

pub(in crate::mcp::tools::explore) fn render_adaptive_section(
    req: AdaptiveRequest<'_>,
) -> Result<Option<RenderedFile>> {
    let spare_named = req
        .group
        .nodes
        .iter()
        .any(|n| req.flow.unique_named_node_ids.contains(&n.id));
    let file_defines_super = file_defines_many_children(req.cg, req.group, req.super_many)?;
    let spared = spare_named && !file_defines_super;
    let has_spine_node = req
        .group
        .nodes
        .iter()
        .any(|n| req.flow.path_node_ids.contains(&n.id));
    let named_body_chars: usize = req
        .group
        .nodes
        .iter()
        .filter(|n| {
            callable_body(n.kind)
                && (req.flow.path_node_ids.contains(&n.id)
                    || req.flow.unique_named_node_ids.contains(&n.id))
        })
        .map(|n| slice_lines(req.file_lines, n.start_line as i64, n.end_line as i64).len())
        .sum();
    let on_spine_god_file = has_spine_node
        && named_body_chars > req.budget.max_chars_per_file
        && req.group.nodes.iter().any(|n| {
            callable_body(n.kind)
                && req.flow.unique_named_node_ids.contains(&n.id)
                && !req.flow.path_node_ids.contains(&n.id)
        });
    let is_sibling = if !has_spine_node {
        file_is_polymorphic_sibling(req.cg, req.group, req.sibling_super)?
    } else {
        false
    };
    if !adaptive_explore_enabled()
        || req.flow.path_node_ids.is_empty()
        || !(on_spine_god_file || (!has_spine_node && is_sibling && !spared))
    {
        return Ok(None);
    }
    let mut syms: Vec<&Node> = req
        .group
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::Import && n.kind != NodeKind::Export && n.start_line > 0)
        .collect();
    syms.sort_by_key(|n| n.start_line);
    let body_ids = choose_full_body_symbols(
        &syms,
        req.file_lines,
        req.flow,
        req.budget,
        file_defines_super,
    );
    let skeleton = render_skeleton(
        &syms,
        req.file_lines,
        req.flow,
        req.budget,
        req.with_line_numbers,
        &body_ids,
    );
    if skeleton.is_empty() {
        return Ok(None);
    }
    let names = adaptive_header_names(req.group, req.budget);
    let tag = if !body_ids.is_empty() {
        "focused (named methods in full, rest as signatures)"
    } else {
        "skeleton (signatures only)"
    };
    let body = skeleton.join("\n");
    Ok(Some(RenderedFile {
        header: format!("#### {} — {} · {}", req.file_path, names, tag),
        language: req.language.to_string(),
        cost: body.len() + 120,
        body,
    }))
}

pub(in crate::mcp::tools::explore) struct AdaptiveRequest<'a> {
    pub cg: &'a CodeGraph,
    pub file_path: &'a str,
    pub group: &'a FileGroup,
    pub file_lines: &'a [&'a str],
    pub language: &'a str,
    pub flow: &'a FlowInfo,
    pub budget: ExploreOutputBudget,
    pub with_line_numbers: bool,
    pub sibling_super: &'a mut HashMap<String, bool>,
    pub super_many: &'a mut HashMap<String, bool>,
}

fn callable_body(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::Component
    )
}

fn file_defines_many_children(
    cg: &CodeGraph,
    group: &FileGroup,
    super_many: &mut HashMap<String, bool>,
) -> Result<bool> {
    for n in &group.nodes {
        if !matches!(
            n.kind,
            NodeKind::Class
                | NodeKind::Interface
                | NodeKind::Struct
                | NodeKind::Trait
                | NodeKind::Protocol
                | NodeKind::TypeAlias
        ) {
            continue;
        }
        let many = match super_many.get(&n.id) {
            Some(&m) => m,
            None => {
                let m = cg
                    .get_incoming_edges(&n.id)?
                    .iter()
                    .filter(|x| x.kind == EdgeKind::Implements || x.kind == EdgeKind::Extends)
                    .count()
                    >= 3;
                super_many.insert(n.id.clone(), m);
                m
            }
        };
        if many {
            return Ok(true);
        }
    }
    Ok(false)
}

fn file_is_polymorphic_sibling(
    cg: &CodeGraph,
    group: &FileGroup,
    sibling_super: &mut HashMap<String, bool>,
) -> Result<bool> {
    for n in &group.nodes {
        for e in cg.get_outgoing_edges(&n.id)? {
            if e.kind != EdgeKind::Implements && e.kind != EdgeKind::Extends {
                continue;
            }
            let many = match sibling_super.get(&e.target) {
                Some(&m) => m,
                None => {
                    let m = cg
                        .get_incoming_edges(&e.target)?
                        .iter()
                        .filter(|x| x.kind == EdgeKind::Implements || x.kind == EdgeKind::Extends)
                        .count()
                        >= 3;
                    sibling_super.insert(e.target.clone(), m);
                    m
                }
            };
            if many {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn choose_full_body_symbols(
    syms: &[&Node],
    file_lines: &[&str],
    flow: &FlowInfo,
    budget: ExploreOutputBudget,
    file_defines_super: bool,
) -> HashSet<String> {
    let prio = |n: &Node| -> i32 {
        if !callable_body(n.kind) {
            99
        } else if flow.path_node_ids.contains(&n.id) {
            0
        } else if flow.unique_named_node_ids.contains(&n.id) {
            1
        } else if file_defines_super && flow.named_node_ids.contains(&n.id) {
            2
        } else {
            99
        }
    };
    let body_cap = budget.max_chars_per_file as f64 * 1.5;
    let mut body_ids = HashSet::new();
    let mut body_chars = 0.0;
    let mut prio_sorted: Vec<&&Node> = syms
        .iter()
        .filter(|n| prio(n) < 99 && n.end_line >= n.start_line)
        .collect();
    prio_sorted.sort_by_key(|n| prio(n));
    for n in prio_sorted {
        let sz = slice_lines(file_lines, n.start_line as i64, n.end_line as i64).len() as f64;
        if body_chars + sz > body_cap && !body_ids.is_empty() {
            continue;
        }
        body_ids.insert(n.id.clone());
        body_chars += sz;
    }
    body_ids
}
