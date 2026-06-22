use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::super::edges::{edge_meta, synthesized_edge};
use super::super::ordered::OrderedMap;
use super::super::source::{count_newlines, for_each_method_and_function, node_source};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Node};

pub(in crate::resolution::callback_synthesizer) static CC_DISPATCH_RE: LazyLock<Regex> =
    LazyLock::new(|| {
        Regex::new(r"([0-9A-Za-z_]+)\.forEach\s*\{\s*(?:\$0|it)\s*\(").expect("valid regex")
    });
pub(in crate::resolution::callback_synthesizer) static CC_APPEND_WRITE_RE: LazyLock<Regex> =
    LazyLock::new(|| {
        Regex::new(
        r"([0-9A-Za-z_]+)\.write\s*\{\s*\$0(?:\.([0-9A-Za-z_]+))?\.(?:append|add|push|insert)\s*\(",
    )
    .expect("valid regex")
    });
pub(in crate::resolution::callback_synthesizer) static CC_APPEND_DIRECT_RE: LazyLock<Regex> =
    LazyLock::new(|| {
        Regex::new(r"([0-9A-Za-z_]+)\.(?:append|add|push|insert)\s*\(").expect("valid regex")
    });
/// Skip a field name with more dispatchers/registrars than this (too generic to pair confidently).
const CC_FANOUT_CAP: usize = 8;

/// Closure-collection dispatch: dispatcher iterates a closure-collection property
/// invoking each element; registrar appends a closure to the same-named property.
/// Emits dispatcher → registrar so a flow reaches the registration site (where the
/// appended closure's body — and its callers — live). High-precision: the
/// dispatcher's element-invoke is the gate (a `.forEach` that does NOT invoke its
/// element is ignored), so a repo with no closure-collection dispatch yields zero
/// edges regardless of how many `.append`/`.push` sites it has.
///
/// Pairs globally by field name (cross-file/class is required — see Alamofire's
/// base-class `Request.didCompleteTask` iterating `validators` appended by the
/// subclass `DataRequest.validate`), bounded by a fan-out cap so a generic field
/// name shared across unrelated classes can't fan out into noise.
pub(in crate::resolution::callback_synthesizer) fn closure_collection_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    // field → dispatcher methods + forEach line
    let mut dispatchers: OrderedMap<Vec<(Node, u32)>> = OrderedMap::new();
    // field → registrar methods + append line
    let mut registrars: OrderedMap<Vec<(Node, u32)>> = OrderedMap::new();

    static DIGITS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[0-9]+$").expect("valid regex"));

    fn add_reg(
        registrars: &mut OrderedMap<Vec<(Node, u32)>>,
        field: Option<&str>,
        node: &Node,
        abs_line: u32,
    ) {
        let Some(field) = field else { return };
        // `$0.append` mis-captures the `0`; the write-RE owns that field
        if field.is_empty() || DIGITS_RE.is_match(field) {
            return;
        }
        let arr = registrars.entry_or_default(field);
        if !arr.iter().any(|(n, _)| n.id == node.id) {
            arr.push((node.clone(), abs_line));
        }
    }

    for_each_method_and_function(queries, |m| {
        let Some(src) = node_source(ctx, &m) else {
            return;
        };
        let has_for_each = src.contains(".forEach");
        let has_append = src.contains(".append(")
            || src.contains(".add(")
            || src.contains(".push(")
            || src.contains(".insert(");
        if !has_for_each && !has_append {
            return;
        }
        let line_at = |idx: usize| m.start_line + count_newlines(&src[..idx]);

        if has_for_each {
            for d in CC_DISPATCH_RE.captures_iter(&src) {
                let field = d[1].to_string();
                let idx = d.get(0).expect("whole match").start();
                let arr = dispatchers.entry_or_default(&field);
                if !arr.iter().any(|(n, _)| n.id == m.id) {
                    arr.push((m.clone(), line_at(idx)));
                }
            }
        }
        if has_append {
            for w in CC_APPEND_WRITE_RE.captures_iter(&src) {
                // nested `$0.streams` else the `.write` receiver
                let field = w
                    .get(2)
                    .or_else(|| w.get(1))
                    .map(|g| g.as_str().to_string());
                let idx = w.get(0).expect("whole match").start();
                add_reg(&mut registrars, field.as_deref(), &m, line_at(idx));
            }
            for a in CC_APPEND_DIRECT_RE.captures_iter(&src) {
                let field = a[1].to_string();
                let idx = a.get(0).expect("whole match").start();
                add_reg(&mut registrars, Some(&field), &m, line_at(idx));
            }
        }
    })?;

    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (field, disps) in dispatchers.iter() {
        let Some(regs) = registrars.get(field) else {
            continue;
        };
        if regs.is_empty() {
            continue;
        }
        if disps.len() > CC_FANOUT_CAP || regs.len() > CC_FANOUT_CAP {
            continue; // generic field — can't pair confidently
        }
        for (disp_node, disp_line) in disps {
            for (reg_node, reg_line) in regs {
                if disp_node.id == reg_node.id {
                    continue;
                }
                let key = format!("{}>{}", disp_node.id, reg_node.id);
                if !seen.insert(key) {
                    continue;
                }
                edges.push(synthesized_edge(
                    &disp_node.id,
                    &reg_node.id,
                    Some(*disp_line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("closure-collection")),
                        ("field", Value::from(field)),
                        (
                            "registeredAt",
                            Value::from(format!("{}:{}", reg_node.file_path, reg_line)),
                        ),
                    ]),
                ));
            }
        }
    }
    Ok(edges)
}
