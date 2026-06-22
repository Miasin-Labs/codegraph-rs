use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::super::edges::{edge_meta, synthesized_edge};
use super::super::ordered::{OrderedMap, OrderedSet};
use super::super::source::{enclosing_fn, line_of};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, NodeKind};

/// Skip events with more handlers/dispatchers than this (too generic without type info).
const EVENT_FANOUT_CAP: usize = 6;

static ON_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*(?:function\s+([0-9A-Za-z_]+)|(?:this\.)?([0-9A-Za-z_]+))"#,
    )
    .expect("valid regex")
});
static EMIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.(?:emit|fire|dispatchEvent)\(\s*['"]([^'"]+)['"]"#).expect("valid regex")
});

/// Phase 2: string-keyed EventEmitter channels (on('e', fn) ↔ emit('e')).
pub(in crate::resolution::callback_synthesizer) fn event_emitter_edges(
    ctx: &dyn ResolutionContext,
) -> Vec<Edge> {
    // event → dispatcher node ids
    let mut emits_by_event: OrderedMap<OrderedSet> = OrderedMap::new();
    // event → handler id → registration site (file:line)
    let mut handlers_by_event: OrderedMap<OrderedMap<String>> = OrderedMap::new();

    for file in ctx.get_all_files() {
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if content.is_empty() {
            continue;
        }
        let has_emit = content.contains(".emit(")
            || content.contains(".fire(")
            || content.contains(".dispatchEvent(");
        let has_on = content.contains(".on(")
            || content.contains(".once(")
            || content.contains(".addListener(");
        if !has_emit && !has_on {
            continue;
        }
        let nodes_in_file = ctx.get_nodes_in_file(&file);

        if has_emit {
            for m in EMIT_RE.captures_iter(&content) {
                let idx = m.get(0).expect("whole match").start();
                let Some(disp) = enclosing_fn(&nodes_in_file, line_of(&content, idx)) else {
                    continue;
                };
                emits_by_event.entry_or_default(&m[1]).add(&disp.id);
            }
        }
        if has_on {
            for m in ON_RE.captures_iter(&content) {
                let handler_name = m
                    .get(2)
                    .or_else(|| m.get(3))
                    .map(|g| g.as_str().to_string());
                let Some(handler_name) = handler_name else {
                    continue;
                };
                if handler_name.is_empty() {
                    continue;
                }
                let handler = ctx
                    .get_nodes_by_name(&handler_name)
                    .into_iter()
                    .find(|n| n.kind == NodeKind::Function || n.kind == NodeKind::Method);
                let Some(handler) = handler else { continue };
                let idx = m.get(0).expect("whole match").start();
                handlers_by_event
                    .entry_or_default(&m[1])
                    .set(&handler.id, format!("{}:{}", file, line_of(&content, idx)));
            }
        }
    }

    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (event, dispatchers) in emits_by_event.iter() {
        let Some(handlers) = handlers_by_event.get(event) else {
            continue;
        };
        // Precision guard: a generic event name with many handlers/dispatchers can't
        // be matched without receiver-type info (Phase 3) — skip rather than over-link.
        if dispatchers.len() > EVENT_FANOUT_CAP || handlers.len() > EVENT_FANOUT_CAP {
            continue;
        }
        for d in dispatchers.iter() {
            for (h, registered_at) in handlers.iter() {
                if d == h {
                    continue;
                }
                let key = format!("{}>{}", d, h);
                if !seen.insert(key) {
                    continue;
                }
                edges.push(synthesized_edge(
                    d,
                    h,
                    None,
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("event-emitter")),
                        ("event", Value::from(event)),
                        ("registeredAt", Value::from(registered_at.as_str())),
                    ]),
                ));
            }
        }
    }
    edges
}
