use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::super::edges::{edge_meta, synthesized_edge};
use super::super::source::{
    dispatcher_field,
    for_each_method_and_function,
    node_source,
    registrar_field,
};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, EdgeKind, Node, NodeKind};

static REGISTRAR_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(on[A-Z][0-9A-Za-z_]*|subscribe|addListener|addEventListener|register|watch|listen|addCallback)$",
    )
    .expect("valid regex")
});
static DISPATCHER_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(emit|trigger|notify|dispatch|fire|publish|flush)").expect("valid regex")
});
const MAX_CALLBACKS_PER_CHANNEL: usize = 40;

/// Phase 1: field-backed observer channels (registrar/dispatcher share a store).
pub(in crate::resolution::callback_synthesizer) fn field_channel_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    let mut registrars: Vec<(Node, String)> = Vec::new();
    let mut dispatchers: Vec<(Node, String)> = Vec::new();

    for_each_method_and_function(queries, |m| {
        let is_reg = REGISTRAR_NAME.is_match(&m.name);
        let is_disp = DISPATCHER_NAME.is_match(&m.name);
        if !is_reg && !is_disp {
            return;
        }
        let Some(src) = node_source(ctx, &m) else {
            return;
        };
        if is_reg {
            if let Some(f) = registrar_field(&src) {
                registrars.push((m.clone(), f));
            }
        }
        if is_disp {
            if let Some(f) = dispatcher_field(&src) {
                dispatchers.push((m, f));
            }
        }
    })?;

    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (reg_node, reg_field) in &registrars {
        let ch_dispatchers: Vec<&(Node, String)> = dispatchers
            .iter()
            .filter(|(d, f)| d.file_path == reg_node.file_path && f == reg_field)
            .collect();
        if ch_dispatchers.is_empty() {
            continue;
        }
        // Registrar names matched REGISTRAR_NAME (ASCII word chars only), so the
        // interpolation is metacharacter-free; `escape` is a no-op safety belt.
        let arg_re = match Regex::new(&format!(
            r"{}\s*\(\s*(?:this\.)?([0-9A-Za-z_]+)",
            regex::escape(&reg_node.name)
        )) {
            Ok(re) => re,
            Err(_) => continue,
        };
        let mut added = 0usize;
        for e in queries.get_incoming_edges(&reg_node.id, Some(&[EdgeKind::Calls]))? {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            let line = match e.line {
                Some(l) if l > 0 => l,
                _ => continue,
            };
            let Some(caller) = queries.get_node_by_id(&e.source)? else {
                continue;
            };
            let content = ctx.read_file(&caller.file_path);
            let line_text = content
                .as_deref()
                .and_then(|c| c.split('\n').nth((line - 1) as usize));
            let Some(am) = line_text.and_then(|t| arg_re.captures(t)) else {
                continue;
            };
            let cb_name = am[1].to_string();
            let fn_node = ctx
                .get_nodes_by_name(&cb_name)
                .into_iter()
                .find(|n| n.kind == NodeKind::Method || n.kind == NodeKind::Function);
            let Some(fn_node) = fn_node else { continue };
            for (disp_node, _) in &ch_dispatchers {
                if disp_node.id == fn_node.id {
                    continue;
                }
                let key = format!("{}>{}", disp_node.id, fn_node.id);
                if !seen.insert(key) {
                    continue;
                }
                edges.push(synthesized_edge(
                    &disp_node.id,
                    &fn_node.id,
                    Some(disp_node.start_line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("callback")),
                        ("via", Value::from(reg_node.name.as_str())),
                        ("field", Value::from(reg_field.as_str())),
                        // Where the callback was wired up (`scene.onUpdate(this.triggerRender)`).
                        // This is the #1 thing an agent reads/greps to explain the flow — surface
                        // it so node/trace/context can show it without a callers() + Read round-trip.
                        (
                            "registeredAt",
                            Value::from(format!("{}:{}", caller.file_path, line)),
                        ),
                    ]),
                ));
                added += 1;
            }
        }
    }
    Ok(edges)
}
