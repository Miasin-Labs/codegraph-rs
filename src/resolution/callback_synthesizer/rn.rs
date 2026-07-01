//! React Native cross-language event-channel synthesis.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::ordered::{OrderedMap, OrderedSet};
use super::source::{enclosing_fn, line_of};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Node, NodeKind};

const EVENT_FANOUT_CAP: usize = 6;

// ObjC's `[self sendEventWithName:@"X" body:...]` shape (bracket syntax,
// `@` string literals).
static RN_OBJC_SEND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?-u:\b)sendEventWithName\s*:\s*@"([^"]+)""#).expect("valid regex")
});
// Swift's `sendEvent(withName: "X", body: ...)` shape — same RCTEventEmitter
// method, different call syntax. Both Objective-C and Swift subclass
// RCTEventEmitter so this catches the Swift-side equivalent emission sites
// (e.g. RNFusedLocation.swift's `sendEvent(withName: "geolocationDidChange",
// body: locationData)`).
static RN_SWIFT_SEND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?-u:\b)sendEvent\s*\(\s*withName\s*:\s*"([^"]+)""#).expect("valid regex")
});
// JVM-side emitter calls: `emitter.emit("X", body)`. Matches both Java
// and Kotlin syntax because the call form is identical. Restricted to
// JVM source files in the consumer so we don't re-process JS emits
// (which `eventEmitterEdges` already handles).
static RN_JVM_EMIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\.emit\s*\(\s*"([^"]+)"\s*,"#).expect("valid regex"));
// Match BOTH the named-handler form (`.addListener('x', fn)`) and an
// unnamed-handler form (`.addListener('x', listener)` where `listener` is a
// parameter — common in RN wrapper APIs).
static RN_ADDLISTENER_ANY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*([A-Za-z_][0-9A-Za-z_.]*)"#)
        .expect("valid regex")
});

/// React Native cross-language event channel (Phase 3 of the mixed-iOS/RN
/// bridging effort). Same shape as `eventEmitterEdges` but cross-language:
///
///   Native (ObjC, on RCTEventEmitter subclass):
///     `[self sendEventWithName:@"locationUpdate" body:@{...}];`
///
///   Native (Java/Kotlin, via the JS module dispatcher):
///     `emitter.emit("locationUpdate", body);`
///     `reactContext.getJSModule(RCTDeviceEventEmitter.class).emit("locationUpdate", body);`
///
///   JS (subscriber):
///     `new NativeEventEmitter(NativeModules.Geo).addListener("locationUpdate", handler);`
///     `DeviceEventEmitter.addListener("locationUpdate", handler);`
///
/// Synthesize: native dispatch site → JS handler, keyed by the literal
/// event name. Only matches NAMED handlers (the existing `ON_RE` named-
/// capture form). Inline arrow handlers like `addListener('x', d => …)`
/// aren't named at extraction time and would need link-through-body
/// support; matches the deliberate scope of the in-language synthesizer.
///
/// Provenance `'heuristic'`, synthesizedBy `'rn-event-channel'`.
pub(super) fn rn_event_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    // Native dispatchers (source = the native method whose body sends the
    // event) and JS handlers (target = the function/method registered as
    // the listener) keyed by event name.
    let mut native_dispatchers_by_event: OrderedMap<OrderedSet> = OrderedMap::new();
    let mut js_handlers_by_event: OrderedMap<OrderedMap<String>> = OrderedMap::new();

    for file in ctx.get_all_files() {
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if content.is_empty() {
            continue;
        }

        let nodes_in_file = ctx.get_nodes_in_file(&file);
        let mut add_dispatcher = |event: &str, line: u32| {
            let Some(disp) = enclosing_fn(&nodes_in_file, line) else {
                return;
            };
            native_dispatchers_by_event
                .entry_or_default(event)
                .add(&disp.id);
        };

        // ObjC side: `sendEventWithName:@"X"` only fires inside `.m`/`.mm`
        // files (RCTEventEmitter subclasses).
        if file.ends_with(".m") || file.ends_with(".mm") {
            for m in RN_OBJC_SEND_RE.captures_iter(&content) {
                let idx = m.get(0).expect("whole match").start();
                if !m[1].is_empty() {
                    add_dispatcher(&m[1], line_of(&content, idx));
                }
            }
        }

        // Swift side: same RCTEventEmitter method, parens/named-args syntax.
        if file.ends_with(".swift") {
            for m in RN_SWIFT_SEND_RE.captures_iter(&content) {
                let idx = m.get(0).expect("whole match").start();
                if !m[1].is_empty() {
                    add_dispatcher(&m[1], line_of(&content, idx));
                }
            }
        }

        // JVM side: `.emit("X", …)` in Java/Kotlin. (We pattern-match
        // anywhere in the file; the JS in-language path uses a separate
        // emitter object pattern and is already handled by eventEmitterEdges.)
        if file.ends_with(".java") || file.ends_with(".kt") {
            if !content.contains("RCTDeviceEventEmitter")
                && !content.contains("DeviceEventManagerModule")
                && !content.contains("getJSModule")
            {
                continue;
            }
            for m in RN_JVM_EMIT_RE.captures_iter(&content) {
                let idx = m.get(0).expect("whole match").start();
                if !m[1].is_empty() {
                    add_dispatcher(&m[1], line_of(&content, idx));
                }
            }
        }

        // JS subscribers (.addListener("X", handler)). Restrict to JS-family
        // files so a native file's `addListener:` (the ObjC method) doesn't
        // get mistaken for a JS subscription — they're entirely different
        // things despite sharing a name.
        if file.ends_with(".js")
            || file.ends_with(".jsx")
            || file.ends_with(".ts")
            || file.ends_with(".tsx")
            || file.ends_with(".mjs")
            || file.ends_with(".cjs")
        {
            for m in RN_ADDLISTENER_ANY_RE.captures_iter(&content) {
                let event = m[1].to_string();
                let arg = m[2].to_string();
                if event.is_empty() || arg.is_empty() {
                    continue;
                }
                let bare_name = match arg.rfind('.') {
                    Some(i) => &arg[i + 1..],
                    None => arg.as_str(),
                };
                let idx = m.get(0).expect("whole match").start();
                // Try a named-symbol match first (matches the in-language semantic).
                let named_handler = ctx
                    .get_nodes_by_name(bare_name)
                    .into_iter()
                    .find(|n| n.kind == NodeKind::Function || n.kind == NodeKind::Method);
                let mut target_id: Option<String> = named_handler.map(|n| n.id);
                if target_id.is_none() {
                    // Fall back to the enclosing function — the subscribe-wrapper
                    // pattern means the event fires THROUGH this function on its
                    // way to user code. Reachability-correct attribution.
                    target_id =
                        enclosing_fn(&nodes_in_file, line_of(&content, idx)).map(|n| n.id.clone());
                }
                if target_id.is_none() {
                    // Broader fallback for JS object-literal API shape
                    // (`const Foo = { watchX(...) { … addListener(...) … } }`):
                    // method shorthand inside an object literal isn't extracted
                    // as a method node, so enclosingFn returns null. Attribute to
                    // the smallest enclosing `constant` / `variable` node — that's
                    // the API surface a downstream caller would `import` and
                    // invoke. Reachability-correct.
                    let line = line_of(&content, idx);
                    let mut smallest: Option<&Node> = None;
                    for n in &nodes_in_file {
                        if n.kind != NodeKind::Constant && n.kind != NodeKind::Variable {
                            continue;
                        }
                        let end = n.end_line;
                        if n.start_line <= line && end >= line {
                            match smallest {
                                Some(s) if n.start_line < s.start_line => {}
                                _ => smallest = Some(n),
                            }
                        }
                    }
                    target_id = smallest.map(|n| n.id.clone());
                }
                let Some(target_id) = target_id else { continue };
                js_handlers_by_event
                    .entry_or_default(&event)
                    .set(&target_id, format!("{}:{}", file, line_of(&content, idx)));
            }
        }
    }

    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (event, dispatchers) in native_dispatchers_by_event.iter() {
        let Some(handlers) = js_handlers_by_event.get(event) else {
            continue;
        };
        // Same fan-out guard as the in-language channel: generic event names
        // (e.g. 'change', 'error', 'data') with many handlers/dispatchers
        // can't be matched precisely without receiver-type info.
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
                        ("synthesizedBy", Value::from("rn-event-channel")),
                        ("event", Value::from(event)),
                        ("registeredAt", Value::from(registered_at.as_str())),
                    ]),
                ));
            }
        }
    }
    edges
}
