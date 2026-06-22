//! Callback / observer edge synthesis — Phase 1 + 2.
//!
//! Closes dynamic-dispatch holes where a dispatcher invokes callbacks registered
//! elsewhere. Two channel shapes:
//!
//!  (1) Field-backed observer (Phase 1):
//!      `onUpdate(cb) { this.callbacks.add(cb); }`            // registrar
//!      `triggerUpdate() { for (cb of this.callbacks) cb(); }` // dispatcher
//!      `scene.onUpdate(this.triggerRender)`                  // registration
//!      → synthesize triggerUpdate → triggerRender
//!
//!  (2) String-keyed EventEmitter (Phase 2):
//!      `this.on('mount', function onmount(){...})`           // registration
//!      `fn.emit('mount', this)`                              // dispatch
//!      → synthesize (method containing emit('mount')) → onmount
//!
//! Whole-graph pass after base resolution. High-precision/low-recall by design:
//! named callbacks only; field channels paired by file+field; EventEmitter
//! channels capped by event fan-out (generic names like 'error' skipped — they
//! need receiver-type matching, deferred to Phase 3). All synthesized edges are
//! tagged `provenance:'heuristic'`. See docs/design/callback-edge-synthesis.md.
//!
//! Ported from `src/resolution/callback-synthesizer.ts`. Regexes use explicit
//! ASCII classes (`[0-9A-Za-z_]`, `(?-u:\b)`) so identifier matching keeps
//! JS-regex semantics (JS `\w`/`\b` are ASCII-only; Rust's default is Unicode).

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::db::QueryBuilder;
use crate::error::Result;
use crate::extraction::generated_detection::is_generated_file;
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, EdgeKind, Language, Metadata, Node, NodeKind, Provenance};

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
static SETSTATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"this\.setState\s*\(").expect("valid regex"));
/// Flutter: `setState((){…})` / `this.setState`.
static FLUTTER_SETSTATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)setState\s*\(").expect("valid regex"));
static JSX_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([A-Z][A-Za-z0-9_]*)[\s/>]").expect("valid regex"));
const MAX_JSX_CHILDREN: usize = 30;
// Vue SFC templates: kebab-case child components (<el-button> → ElButton) and
// event bindings (@click="fn" / v-on:click="fn"). PascalCase children (<VPNav/>)
// are already caught by JSX_TAG_RE via the SFC component node.
static VUE_KEBAB_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([a-z][a-z0-9]*(?:-[a-z0-9]+)+)[\s/>]").expect("valid regex"));
static VUE_HANDLER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:@|v-on:)([a-zA-Z][0-9A-Za-z_-]*)(?:\.[0-9A-Za-z_]+)*\s*=\s*"([^"]+)""#)
        .expect("valid regex")
});
// Composable/hook destructure: `const { close: closeSidebar } = useSidebarControl()`.
// Captures the destructure body + the called composable; only `use*` calls qualify.
static VUE_DESTRUCTURE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:const|let|var)\s*\{([^}]+)\}\s*=\s*([0-9A-Za-z_]+)\s*\(").expect("valid regex")
});

// Closure-collection dynamic dispatch (language-agnostic, Swift-first). A method
// appends a closure to a collection property; another method iterates that
// property *invoking each element* (`coll.forEach { $0() }` / `{ it() }`). The
// element-invoke (`$0(` / `it(`) PROVES the collection holds closures, so pairing
// a dispatcher to same-named registrars (`.append`/`.add`/`.push`/`.insert`,
// incl. Swift `prop.write { $0.append }`) is high-precision. Cross-file/class by
// design: Alamofire appends in `DataRequest.validate` but iterates in the base
// `Request.didCompleteTask` — neither same-file nor same-class pairing reaches it.
static CC_DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([0-9A-Za-z_]+)\.forEach\s*\{\s*(?:\$0|it)\s*\(").expect("valid regex")
});
static CC_APPEND_WRITE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"([0-9A-Za-z_]+)\.write\s*\{\s*\$0(?:\.([0-9A-Za-z_]+))?\.(?:append|add|push|insert)\s*\(",
    )
    .expect("valid regex")
});
static CC_APPEND_DIRECT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([0-9A-Za-z_]+)\.(?:append|add|push|insert)\s*\(").expect("valid regex")
});
/// Skip a field name with more dispatchers/registrars than this (too generic to pair confidently).
const CC_FANOUT_CAP: usize = 8;

fn kebab_to_pascal(s: &str) -> String {
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
fn slice_lines(content: &str, start_line: u32, end_line: u32) -> Option<String> {
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
fn node_source(ctx: &dyn ResolutionContext, n: &Node) -> Option<String> {
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

fn registrar_field(src: &str) -> Option<String> {
    REGISTRAR_FIELD_RE.captures(src).map(|m| m[1].to_string())
}

static DISPATCHER_FOR_OF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)of\s+(?:Array\.from\(\s*)?this\.([0-9A-Za-z_]+)").expect("valid regex")
});
static DISPATCHER_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)[0-9A-Za-z_]+\s*\(").expect("valid regex"));
static DISPATCHER_FOR_EACH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"this\.([0-9A-Za-z_]+)\.forEach\(").expect("valid regex"));

fn dispatcher_field(src: &str) -> Option<String> {
    if let Some(for_of) = DISPATCHER_FOR_OF_RE.captures(src) {
        if DISPATCHER_CALL_RE.is_match(src) {
            return Some(for_of[1].to_string());
        }
    }
    DISPATCHER_FOR_EACH_RE
        .captures(src)
        .map(|m| m[1].to_string())
}

fn is_fn_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::Component
    )
}

/// Innermost function/method node whose line range contains `line`.
fn enclosing_fn(nodes_in_file: &[Node], line: u32) -> Option<&Node> {
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
fn count_newlines(s: &str) -> u32 {
    s.bytes().filter(|&b| b == b'\n').count() as u32
}

/// TS `lineOf`: `content.slice(0, idx).split('\n').length` (1-based line of a match index).
fn line_of(content: &str, idx: usize) -> u32 {
    count_newlines(&content[..idx]) + 1
}

/// Build edge metadata preserving the TS object-literal key order
/// (serde_json is compiled with `preserve_order`).
fn edge_meta(entries: Vec<(&str, Value)>) -> Metadata {
    let mut m = Metadata::new();
    for (k, v) in entries {
        m.insert(k.to_string(), v);
    }
    m
}

fn synthesized_edge(source: &str, target: &str, line: Option<u32>, metadata: Metadata) -> Edge {
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind: EdgeKind::Calls,
        metadata: Some(metadata),
        line,
        column: None,
        provenance: Some(Provenance::Heuristic),
    }
}

/// Insertion-ordered string-keyed map (JS `Map` parity: `set` on an existing
/// key updates the value but keeps the original position; iteration follows
/// first-insertion order).
struct OrderedMap<V> {
    keys: Vec<String>,
    map: HashMap<String, V>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> OrderedMap<V> {
    fn new() -> Self {
        OrderedMap {
            keys: Vec::new(),
            map: HashMap::new(),
        }
    }
    fn len(&self) -> usize {
        self.keys.len()
    }
    fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    fn get(&self, k: &str) -> Option<&V> {
        self.map.get(k)
    }
    fn contains_key(&self, k: &str) -> bool {
        self.map.contains_key(k)
    }
    fn set(&mut self, k: &str, v: V) {
        if !self.map.contains_key(k) {
            self.keys.push(k.to_string());
        }
        self.map.insert(k.to_string(), v);
    }
    fn entry_or_default(&mut self, k: &str) -> &mut V
    where
        V: Default,
    {
        if !self.map.contains_key(k) {
            self.keys.push(k.to_string());
            self.map.insert(k.to_string(), V::default());
        }
        self.map.get_mut(k).expect("just inserted")
    }
    fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.keys
            .iter()
            .map(move |k| (k.as_str(), self.map.get(k).expect("key tracked")))
    }
}

/// Insertion-ordered string set (JS `Set` parity).
#[derive(Default)]
struct OrderedSet {
    items: Vec<String>,
    seen: HashSet<String>,
}

impl OrderedSet {
    fn add(&mut self, v: &str) {
        if self.seen.insert(v.to_string()) {
            self.items.push(v.to_string());
        }
    }
    fn len(&self) -> usize {
        self.items.len()
    }
    fn iter(&self) -> std::slice::Iter<'_, String> {
        self.items.iter()
    }
}

/// Stream method + function nodes lazily. The synthesizers only scan-and-filter
/// down to a tiny matched subset, so materializing every function/method (which
/// is gigabytes on a symbol-dense project) just to iterate it once is what OOM'd
/// #610. Iterating keeps memory O(1) in the node count.
fn for_each_method_and_function(queries: &QueryBuilder, mut f: impl FnMut(Node)) -> Result<()> {
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

/// Phase 1: field-backed observer channels (registrar/dispatcher share a store).
fn field_channel_edges(queries: &QueryBuilder, ctx: &dyn ResolutionContext) -> Result<Vec<Edge>> {
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
fn closure_collection_edges(
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

/// Phase 2: string-keyed EventEmitter channels (on('e', fn) ↔ emit('e')).
fn event_emitter_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
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

/// Methods directly contained by a class-like node.
fn methods_of(queries: &QueryBuilder, class_id: &str) -> Result<Vec<Node>> {
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

/// Phase 4: React class-component re-render. `this.setState(...)` re-runs the
/// component's `render()`, but that hop is React-internal — no static edge — so a
/// flow like "mutation → setState → canvas repaint" dead-ends at setState even
/// though `render → getRenderableElements → …` is fully call-connected after it.
/// Bridge it: for each class that has a `render` method, link every sibling method
/// whose body calls `this.setState(` → `render`. The setState gate keeps this to
/// React class components (a non-React class with a `render` method won't call
/// `this.setState`). Over-approximation (all setState methods reach render) is
/// accepted — it's reachability-correct, like the callback channels.
fn react_render_edges(queries: &QueryBuilder, ctx: &dyn ResolutionContext) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for cls in queries.get_nodes_by_kind(NodeKind::Class)? {
        let children = methods_of(queries, &cls.id)?;
        let Some(render) = children.iter().find(|n| n.name == "render") else {
            continue;
        };
        let mut added = 0usize;
        for m in &children {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            if m.id == render.id {
                continue;
            }
            let Some(src) = node_source(ctx, m) else {
                continue;
            };
            if !SETSTATE_RE.is_match(&src) {
                continue;
            }
            let key = format!("{}>{}", m.id, render.id);
            if !seen.insert(key) {
                continue;
            }
            edges.push(synthesized_edge(
                &m.id,
                &render.id,
                Some(m.start_line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("react-render")),
                    ("via", Value::from("setState")),
                    (
                        "registeredAt",
                        Value::from(format!("{}:{}", render.file_path, render.start_line)),
                    ),
                ]),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

/// Phase 4b: Flutter setState → build (the Dart analog of react-render). In a
/// StatefulWidget's State class, `setState(() {…})` re-runs `build(context)`, but
/// that hop is framework-internal (Flutter calls build), so a flow like
/// "onPressed → _increment → setState → rebuilt UI" dead-ends at setState. Bridge
/// it: for each Dart class with a `build` method, link every sibling method whose
/// body calls `setState(` → `build`. The setState gate + `.dart` file keep this to
/// Flutter State classes. Over-approximation accepted (reachability-correct).
fn flutter_build_edges(queries: &QueryBuilder, ctx: &dyn ResolutionContext) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for cls in queries.get_nodes_by_kind(NodeKind::Class)? {
        let children = methods_of(queries, &cls.id)?;
        let Some(build) = children.iter().find(|n| n.name == "build") else {
            continue;
        };
        if !build.file_path.ends_with(".dart") {
            continue;
        }
        let mut added = 0usize;
        for m in &children {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            if m.id == build.id {
                continue;
            }
            let Some(src) = node_source(ctx, m) else {
                continue;
            };
            if !FLUTTER_SETSTATE_RE.is_match(&src) {
                continue;
            }
            let key = format!("{}>{}", m.id, build.id);
            if !seen.insert(key) {
                continue;
            }
            edges.push(synthesized_edge(
                &m.id,
                &build.id,
                Some(m.start_line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("flutter-build")),
                    ("via", Value::from("setState")),
                    (
                        "registeredAt",
                        Value::from(format!("{}:{}", build.file_path, build.start_line)),
                    ),
                ]),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

/// Phase 4c: C++ virtual override. A call through a base/interface pointer
/// (`db->Get(...)`, `iter->Next()`) dispatches at runtime to a subclass override,
/// but that hop is a vtable indirection — no static call edge — so a flow stops at
/// the abstract base method. Bridge it like react-render: for each C++ class that
/// `extends` a base, link each base method → the subclass method of the same name
/// (the override), so trace/callees from the interface method reach the
/// implementation(s). Over-approximation accepted (reachability-correct); capped
/// per class and gated to C++ to avoid touching other languages' dispatch.
fn cpp_override_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for cls in queries.get_nodes_by_kind(NodeKind::Class)? {
        let sub_methods: Vec<Node> = methods_of(queries, &cls.id)?
            .into_iter()
            .filter(|n| n.language == Language::Cpp)
            .collect();
        if sub_methods.is_empty() {
            continue;
        }
        for ext in queries.get_outgoing_edges(&cls.id, Some(&[EdgeKind::Extends]), None)? {
            let Some(base) = queries.get_node_by_id(&ext.target)? else {
                continue;
            };
            if base.language != Language::Cpp || base.id == cls.id {
                continue;
            }
            // JS `new Map(...)` semantics: a later same-name method overwrites.
            let mut base_methods: HashMap<String, Node> = HashMap::new();
            for bm in methods_of(queries, &base.id)? {
                base_methods.insert(bm.name.clone(), bm);
            }
            let mut added = 0usize;
            for m in &sub_methods {
                if added >= MAX_CALLBACKS_PER_CHANNEL {
                    break;
                }
                let Some(bm) = base_methods.get(&m.name) else {
                    continue;
                };
                if bm.id == m.id {
                    continue;
                }
                let key = format!("{}>{}", bm.id, m.id);
                if !seen.insert(key) {
                    continue;
                }
                edges.push(synthesized_edge(
                    &bm.id,
                    &m.id,
                    Some(bm.start_line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("cpp-override")),
                        ("via", Value::from(m.name.as_str())),
                        (
                            "registeredAt",
                            Value::from(format!("{}:{}", m.file_path, m.start_line)),
                        ),
                    ]),
                ));
                added += 1;
            }
        }
    }
    Ok(edges)
}

/// Languages whose static `implements`/`extends` edges should bridge an
/// interface (or abstract base) method to the matching concrete-class method.
/// The set is "languages with explicit nominal subtyping and a single class
/// kind that holds methods" — i.e. the shape this loop expects. Swift and
/// Scala fit shape-wise (Swift `protocol`/`class`, Scala `trait`/`class`)
/// and are included; their concrete-side nodes can be a `struct` (Swift)
/// or an `object` (Scala) so the loop also iterates those kinds.
fn is_iface_override_lang(lang: Language) -> bool {
    matches!(
        lang,
        Language::Java
            | Language::Kotlin
            | Language::Csharp
            | Language::Typescript
            | Language::Javascript
            | Language::Swift
            | Language::Scala
    )
}

/// Phase 5.5: interface / abstract dispatch (Java, Kotlin). A call through an
/// injected interface (`@Autowired FooService svc; svc.list()`) or an abstract
/// base dispatches at runtime to the implementing class's override — a vtable
/// indirection with no static call edge — so a request→service flow stops at the
/// interface method. Bridge it like cpp-override: for each class that
/// `implements` an interface (or `extends` an abstract base), link each
/// base/interface method → the class's same-name method (the override) so
/// trace/callees reach the implementation. Over-approximation accepted
/// (reachability-correct); capped per class, gated to JVM languages.
fn interface_override_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Concrete-side kinds vary by language: `class` covers Java / Kotlin /
    // C# / TS / Swift-classes / Scala-classes; `struct` covers Swift value
    // types that conform to protocols. Iterate both.
    let concrete_kinds = [NodeKind::Class, NodeKind::Struct];
    for kind in concrete_kinds {
        for cls in queries.get_nodes_by_kind(kind)? {
            let impl_methods: Vec<Node> = methods_of(queries, &cls.id)?
                .into_iter()
                .filter(|n| is_iface_override_lang(n.language))
                .collect();
            if impl_methods.is_empty() {
                continue;
            }
            for sup in queries.get_outgoing_edges(
                &cls.id,
                Some(&[EdgeKind::Implements, EdgeKind::Extends]),
                None,
            )? {
                let Some(base) = queries.get_node_by_id(&sup.target)? else {
                    continue;
                };
                if !is_iface_override_lang(base.language) || base.id == cls.id {
                    continue;
                }
                // Group impl methods by name to handle OVERLOADS: an interface `list()` and
                // `list(params)` are distinct nodes and a call may resolve to either, so
                // link every base overload → every same-name impl overload (keying by name
                // alone would drop all but one and miss the resolved overload).
                let mut impl_by_name: HashMap<&str, Vec<&Node>> = HashMap::new();
                for m in &impl_methods {
                    impl_by_name.entry(m.name.as_str()).or_default().push(m);
                }
                let mut added = 0usize;
                for bm in methods_of(queries, &base.id)? {
                    if added >= MAX_CALLBACKS_PER_CHANNEL {
                        break;
                    }
                    let Some(impls) = impl_by_name.get(bm.name.as_str()) else {
                        continue;
                    };
                    for m in impls {
                        if added >= MAX_CALLBACKS_PER_CHANNEL {
                            break;
                        }
                        if bm.id == m.id {
                            continue;
                        }
                        let key = format!("{}>{}", bm.id, m.id);
                        if !seen.insert(key) {
                            continue;
                        }
                        edges.push(synthesized_edge(
                            &bm.id,
                            &m.id,
                            Some(bm.start_line),
                            edge_meta(vec![
                                ("synthesizedBy", Value::from("interface-impl")),
                                ("via", Value::from(m.name.as_str())),
                                (
                                    "registeredAt",
                                    Value::from(format!("{}:{}", m.file_path, m.start_line)),
                                ),
                            ]),
                        ));
                        added += 1;
                    }
                }
            }
        }
    }
    Ok(edges)
}

/// Go gRPC stub → impl bridge. The protoc-gen-go-grpc codegen emits an
/// `UnimplementedXxxServer` struct in `*_grpc.pb.go` carrying one method
/// per service RPC; the real handler is a hand-written struct in another
/// file (`x/bank/keeper/msg_server.go::msgServer.Send` in cosmos-sdk).
/// Go's structural typing means no `implements` edge exists for our
/// resolver to follow, so `trace("Send","SendCoins")` lands on the
/// empty stub and reports "no path" (validated empirically — the cosmos
/// Q1 r1 trace failure that drove this work).
///
/// Bridge: for each `UnimplementedXxxServer` whose RPC-method names are
/// a SUBSET of some other Go struct's method names, emit `calls` edges
/// `stub.method → impl.method` (paired by name). Excludes the gRPC
/// internal markers `mustEmbedUnimplementedXxxServer` and
/// `testEmbeddedByValue`, and skips candidate impls that themselves
/// live in a generated file (their `xxxClient` / sibling stubs would
/// otherwise look like impls).
///
/// Multiple candidates is allowed and capped at MAX_CALLBACKS_PER_CHANNEL —
/// a service often has both a production impl and one or more test
/// mocks; linking to all preserves trace utility without false-favoring.
///
/// Provenance: `heuristic`, `synthesizedBy: 'go-grpc-stub-impl'`. The
/// stub's source line is the wiring site shown in the trace trail.
fn go_grpc_stub_impl_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    static STUB_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^Unimplemented.*Server$").expect("valid regex"));
    // gRPC internal-helper methods that appear on every Unimplemented*Server;
    // not part of the service contract, so exclude when computing the RPC-method
    // signature used to match impls.
    fn is_internal_marker(n: &str) -> bool {
        n.starts_with("mustEmbed") || n == "testEmbeddedByValue"
    }

    // Methods directly contained by each Go struct, name-only. Built once.
    let mut method_names_by_struct: HashMap<String, HashSet<String>> = HashMap::new();
    let mut method_nodes_by_struct: HashMap<String, Vec<Node>> = HashMap::new();
    let mut go_structs: Vec<Node> = Vec::new();
    for s in queries.get_nodes_by_kind(NodeKind::Struct)? {
        if s.language != Language::Go {
            continue;
        }
        let ms = methods_of(queries, &s.id)?;
        method_names_by_struct.insert(s.id.clone(), ms.iter().map(|m| m.name.clone()).collect());
        method_nodes_by_struct.insert(s.id.clone(), ms);
        go_structs.push(s);
    }

    for stub in &go_structs {
        if !STUB_RE.is_match(&stub.name) {
            continue;
        }
        // The stub MUST live in a generated file — that's what tells us this is
        // a protoc-emitted scaffold rather than someone naming a struct
        // `UnimplementedXxxServer` by hand. Without this gate we'd also bridge
        // such hand-written structs and create misleading edges.
        if !is_generated_file(&stub.file_path) {
            continue;
        }

        let stub_methods: Vec<&Node> = method_nodes_by_struct
            .get(&stub.id)
            .map(|ms| ms.iter().filter(|m| !is_internal_marker(&m.name)).collect())
            .unwrap_or_default();
        if stub_methods.is_empty() {
            continue;
        }
        let stub_method_names: Vec<&str> = stub_methods.iter().map(|m| m.name.as_str()).collect();

        for cand in &go_structs {
            if cand.id == stub.id {
                continue;
            }
            // Skip generated-file candidates — they're siblings (msgClient,
            // UnsafeMsgServer, …) whose method sets coincidentally match.
            if is_generated_file(&cand.file_path) {
                continue;
            }

            let Some(cand_names) = method_names_by_struct.get(&cand.id) else {
                continue;
            };
            // Subset: every RPC method must exist on the candidate by name.
            // Signature-level match would tighten this further, but name-match
            // alone already gives one-to-one pairing in real codebases because
            // gRPC method-name sets are highly distinctive (Send + MultiSend +
            // UpdateParams + SetSendEnabled is unique to bank's MsgServer).
            if !stub_method_names.iter().all(|n| cand_names.contains(*n)) {
                continue;
            }

            let empty: Vec<Node> = Vec::new();
            let cand_methods = method_nodes_by_struct.get(&cand.id).unwrap_or(&empty);
            let mut added = 0usize;
            for sm in &stub_methods {
                if added >= MAX_CALLBACKS_PER_CHANNEL {
                    break;
                }
                for cm in cand_methods {
                    if added >= MAX_CALLBACKS_PER_CHANNEL {
                        break;
                    }
                    if cm.name != sm.name {
                        continue;
                    }
                    let key = format!("{}>{}", sm.id, cm.id);
                    if !seen.insert(key) {
                        continue;
                    }
                    edges.push(synthesized_edge(
                        &sm.id,
                        &cm.id,
                        Some(sm.start_line),
                        edge_meta(vec![
                            ("synthesizedBy", Value::from("go-grpc-stub-impl")),
                            ("via", Value::from(cm.name.as_str())),
                            (
                                "registeredAt",
                                Value::from(format!("{}:{}", cm.file_path, cm.start_line)),
                            ),
                        ]),
                    ));
                    added += 1;
                }
            }
        }
    }
    Ok(edges)
}

/// Phase 5: React JSX child rendering. A component that returns `<Child .../>`
/// mounts Child — React calls it — but JSX instantiation isn't a static call edge,
/// so a render tree (App.render → StaticCanvas → renderStaticScene) breaks at the
/// JSX hop. Link parent → each capitalized JSX child it renders. File-oriented
/// (read each JSX file once). Precision gate: the child name must resolve to a
/// component/function/class node — TS generics like `Array<Foo>` resolve to a type
/// (or nothing) and are dropped.
fn react_jsx_child_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for file in ctx.get_all_files() {
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if content.is_empty() || (!content.contains("</") && !content.contains("/>")) {
            continue; // JSX-file gate
        }
        let parents: Vec<Node> = ctx
            .get_nodes_in_file(&file)
            .into_iter()
            .filter(|n| is_fn_kind(n.kind))
            .collect();
        for parent in &parents {
            let Some(src) = slice_lines(&content, parent.start_line, parent.end_line) else {
                continue;
            };
            if src.is_empty() || (!src.contains("</") && !src.contains("/>")) {
                continue;
            }
            let mut names = OrderedSet::default();
            for m in JSX_TAG_RE.captures_iter(&src) {
                names.add(&m[1]);
            }
            let mut added = 0usize;
            for name in names.iter() {
                if added >= MAX_JSX_CHILDREN {
                    break;
                }
                let child = ctx.get_nodes_by_name(name).into_iter().find(|n| {
                    n.kind == NodeKind::Component
                        || n.kind == NodeKind::Function
                        || n.kind == NodeKind::Class
                });
                let Some(child) = child else { continue };
                if child.id == parent.id {
                    continue;
                }
                let key = format!("{}>{}", parent.id, child.id);
                if !seen.insert(key) {
                    continue;
                }
                edges.push(synthesized_edge(
                    &parent.id,
                    &child.id,
                    Some(parent.start_line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("jsx-render")),
                        ("via", Value::from(name.as_str())),
                    ]),
                ));
                added += 1;
            }
        }
    }
    edges
}

static VUE_TEMPLATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<template[^>]*>([\s\S]*)</template>").expect("valid regex"));
static VUE_SCRIPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<script[^>]*>([\s\S]*?)</script>").expect("valid regex"));
static VUE_USE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^use[A-Z]").expect("valid regex"));
static VUE_DESTRUCTURE_PART_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([0-9A-Za-z_]+)\s*(?::\s*([0-9A-Za-z_]+))?$").expect("valid regex")
});
static VUE_HANDLER_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z_][0-9A-Za-z_]*)").expect("valid regex"));

/// Phase 6: Vue SFC templates. The `.vue` extractor only parses `<script>`, so
/// template usage is invisible — child components and event handlers used ONLY in
/// the template have no edge to them. PascalCase children (`<VPNav/>`) are already
/// caught by reactJsxChildEdges (which scans the SFC component node), so this adds
/// the two Vue-specific shapes:
///   - kebab-case children: `<el-button>` → `ElButton` component (renders).
///   - event bindings: `@click="onClick"` / `v-on:submit="save"` → handler method.
///
/// Scoped to the `<template>` block of `.vue` files; resolution gate (kebab→
/// component, handler→function/method) keeps precision; inline arrows / `$emit`
/// skipped.
fn vue_template_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let component_kinds = [NodeKind::Component, NodeKind::Function, NodeKind::Class];
    let handler_kinds = [NodeKind::Method, NodeKind::Function];
    // A composable's returned member may be a fn (`function close(){}`) or an
    // arrow assigned to a const (`const close = () => {}`).
    let return_kinds = [
        NodeKind::Method,
        NodeKind::Function,
        NodeKind::Variable,
        NodeKind::Constant,
    ];
    for file in ctx.get_all_files() {
        if !file.ends_with(".vue") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if content.is_empty() {
            continue;
        }
        let tpl = VUE_TEMPLATE_RE
            .captures(&content)
            .and_then(|c| c.get(1))
            .map(|g| g.as_str().to_string());
        let Some(tpl) = tpl else { continue };
        if tpl.is_empty() {
            continue;
        }
        let comp = ctx
            .get_nodes_in_file(&file)
            .into_iter()
            .find(|n| n.kind == NodeKind::Component);
        let Some(comp) = comp else { continue };

        // Composable-destructure map: alias → (composable, key). Lets us resolve a
        // template handler that isn't a local function but a destructured composable
        // return (`@click="closeSidebar"` ← `const { close: closeSidebar } = useSidebarControl()`).
        let script = VUE_SCRIPT_RE
            .captures(&content)
            .and_then(|c| c.get(1))
            .map(|g| g.as_str().to_string())
            .unwrap_or_default();
        let mut destructured: HashMap<String, (String, String)> = HashMap::new();
        for dm in VUE_DESTRUCTURE_RE.captures_iter(&script) {
            if !VUE_USE_RE.is_match(&dm[2]) {
                continue; // composables / hooks only
            }
            for part in dm[1].split(',') {
                // key | key: alias
                if let Some(pm) = VUE_DESTRUCTURE_PART_RE.captures(part.trim()) {
                    let alias = pm
                        .get(2)
                        .map(|g| g.as_str())
                        .unwrap_or_else(|| pm.get(1).expect("group 1").as_str());
                    destructured.insert(alias.to_string(), (dm[2].to_string(), pm[1].to_string()));
                }
            }
        }

        let mut added = 0usize;
        let add_edge = |target: Option<&Node>,
                        meta: Metadata,
                        edges: &mut Vec<Edge>,
                        seen: &mut HashSet<String>,
                        added: &mut usize| {
            let Some(target) = target else { return };
            if *added >= MAX_JSX_CHILDREN || target.id == comp.id {
                return;
            }
            let synthesized_by = meta
                .get("synthesizedBy")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let k = format!("{}>{}>{}", comp.id, target.id, synthesized_by);
            if !seen.insert(k) {
                return;
            }
            edges.push(synthesized_edge(
                &comp.id,
                &target.id,
                Some(comp.start_line),
                meta,
            ));
            *added += 1;
        };
        // Prefer a target in THIS SFC (handlers live in the same file's script) —
        // avoids cross-file mis-match when a name repeats across a monorepo.
        let resolve = |name: &str, kinds: &[NodeKind]| -> Option<Node> {
            let matches: Vec<Node> = ctx
                .get_nodes_by_name(name)
                .into_iter()
                .filter(|n| kinds.contains(&n.kind))
                .collect();
            matches
                .iter()
                .find(|n| n.file_path == file)
                .cloned()
                .or_else(|| matches.into_iter().next())
        };

        for m in VUE_KEBAB_RE.captures_iter(&tpl) {
            let target = resolve(&kebab_to_pascal(&m[1]), &component_kinds);
            add_edge(
                target.as_ref(),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("jsx-render")),
                    ("via", Value::from(&m[1])),
                ]),
                &mut edges,
                &mut seen,
                &mut added,
            );
        }
        for m in VUE_HANDLER_RE.captures_iter(&tpl) {
            let event = m[1].to_string();
            let expr = m[2].trim().to_string();
            if expr.contains("=>") || expr.starts_with('$') {
                continue; // inline arrow / $emit
            }
            let Some(nm) = VUE_HANDLER_NAME_RE.captures(&expr) else {
                continue;
            };
            let name = nm[1].to_string();
            if let Some(direct) = resolve(&name, &handler_kinds) {
                add_edge(
                    Some(&direct),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("vue-handler")),
                        ("event", Value::from(event.as_str())),
                    ]),
                    &mut edges,
                    &mut seen,
                    &mut added,
                );
                continue;
            }
            // Composable-destructure handler → resolve to the composable's returned fn.
            let Some((composable_name, key)) = destructured.get(&name) else {
                continue;
            };
            let composable = resolve(composable_name, &handler_kinds);
            // Resolve to the SPECIFIC returned member (e.g. `close`) defined in the
            // composable's file. No fallback to the composable itself — the component
            // already has a static `useX()` call edge, so that would just be redundant
            // and less precise.
            let key_fn = composable.as_ref().and_then(|c| {
                ctx.get_nodes_by_name(key)
                    .into_iter()
                    .find(|n| return_kinds.contains(&n.kind) && n.file_path == c.file_path)
            });
            if let Some(key_fn) = key_fn {
                add_edge(
                    Some(&key_fn),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("vue-handler")),
                        ("event", Value::from(event.as_str())),
                        ("via", Value::from(composable_name.as_str())),
                    ]),
                    &mut edges,
                    &mut seen,
                    &mut added,
                );
            }
        }
    }
    edges
}

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
fn rn_event_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
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

/// Phase 6 — React Native Fabric/Codegen view component bridge.
///
/// The Fabric framework extractor (`frameworks/fabric.ts`) emits
/// `component` nodes named after the JS-visible component (e.g.
/// `RNSScreenStack`) from each `codegenNativeComponent<Props>('Name')`
/// spec declaration. The native implementation lives in an ObjC++/.mm or
/// Kotlin/Java class whose name follows one of RN's conventions:
///
///   - Exact: `RNSScreenStack`
///   - With suffix: `RNSScreenStackView`, `RNSScreenStackViewManager`,
///     `RNSScreenStackComponentView`, `RNSScreenStackManager`
///
/// This synthesizer walks every Fabric component node and looks for a
/// native class matching one of those names; when found, emits a
/// `calls` edge `component → native class` (provenance `'heuristic'`,
/// `synthesizedBy:'fabric-native-impl'`) so trace from JSX usage of the
/// component continues into native.
///
/// The convention-based suffix lookup is precise: there's no name
/// collision in RN view-manager codebases by design (Codegen output would
/// conflict otherwise).
const FABRIC_NATIVE_SUFFIXES: [&str; 5] = ["", "View", "ViewManager", "ComponentView", "Manager"];

fn fabric_native_impl_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // The Fabric extractor IDs are prefixed `fabric-component:` so we can
    // filter to just those without iterating all `component` nodes.
    let components: Vec<Node> = ctx
        .get_nodes_by_kind(NodeKind::Component)
        .into_iter()
        .filter(|n| n.id.starts_with("fabric-component:"))
        .collect();
    if components.is_empty() {
        return edges;
    }

    // Pre-index native classes by name for O(1) lookup.
    let mut native_classes_by_name: HashMap<String, Vec<Node>> = HashMap::new();
    for n in ctx.get_nodes_by_kind(NodeKind::Class) {
        if n.language != Language::Objc
            && n.language != Language::Kotlin
            && n.language != Language::Java
            && n.language != Language::Cpp
        {
            continue;
        }
        native_classes_by_name
            .entry(n.name.clone())
            .or_default()
            .push(n);
    }

    for component in &components {
        for suffix in FABRIC_NATIVE_SUFFIXES {
            let candidate = format!("{}{}", component.name, suffix);
            let Some(matches) = native_classes_by_name.get(&candidate) else {
                continue;
            };
            if matches.is_empty() {
                continue;
            }
            // Link the component node to every matching native class (iOS +
            // Android each have one).
            for native in matches {
                let key = format!("{}>{}", component.id, native.id);
                if !seen.insert(key) {
                    continue;
                }
                edges.push(synthesized_edge(
                    &component.id,
                    &native.id,
                    None,
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("fabric-native-impl")),
                        (
                            "viaSuffix",
                            Value::from(if suffix.is_empty() { "(exact)" } else { suffix }),
                        ),
                        ("componentName", Value::from(component.name.as_str())),
                    ]),
                ));
            }
        }
    }

    edges
}

/// MyBatis: link a Java mapper interface method to the XML statement that holds
/// its SQL. The XML extractor (`src/extraction/mybatis-extractor.ts`) qualifies
/// each `<select|insert|update|delete|sql id="X">` as `<namespace>::<id>` where
/// `<namespace>` is the Java FQN of the mapper interface. A Java method's
/// qualifiedName ends with `<ClassName>::<methodName>`, so we suffix-match the
/// last two segments of the XML qualified name to find a unique Java method by
/// `<ClassName>::<methodName>` (`ClassName` = last dotted segment of the XML
/// namespace). Cross-mapper `<include refid="other.X">` references go through
/// the normal qualified-name resolver — only the Java↔XML bridge is synthetic.
///
/// Precision over recall: ambiguous mappers (multiple Java classes with the
/// same simple name) are dropped. We need-not bridge by package because Java
/// mapper interfaces are typically uniquely named within a project.
fn mybatis_java_xml_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Index Java methods by `<ClassName>::<methodName>` for O(1) lookup.
    let mut java_index: HashMap<String, Vec<Node>> = HashMap::new();
    queries.iterate_nodes_by_kind(NodeKind::Method, |m| {
        if m.language != Language::Java && m.language != Language::Kotlin {
            return true;
        }
        let parts: Vec<&str> = m.qualified_name.split("::").collect();
        if parts.len() < 2 {
            return true;
        }
        let last = parts[parts.len() - 1];
        let cls = parts[parts.len() - 2];
        if last.is_empty() || cls.is_empty() {
            return true;
        }
        let key = format!("{}::{}", cls, last);
        java_index.entry(key).or_default().push(m);
        true
    })?;

    queries.iterate_nodes_by_kind(NodeKind::Method, |xml| {
        if xml.language != Language::Xml {
            return true;
        }
        // Qualified name: `<namespace>::<id>`. Extract the simple class name.
        let Some(colon_idx) = xml.qualified_name.rfind("::") else {
            return true;
        };
        let namespace = &xml.qualified_name[..colon_idx];
        let id = &xml.qualified_name[colon_idx + 2..];
        if namespace.is_empty() || id.is_empty() {
            return true;
        }
        let class_name = match namespace.rfind('.') {
            Some(dot_idx) => &namespace[dot_idx + 1..],
            None => namespace,
        };
        let Some(candidates) = java_index.get(&format!("{}::{}", class_name, id)) else {
            return true;
        };
        if candidates.is_empty() {
            return true;
        }
        // Drop ambiguous matches (multiple same-name classes); the user can
        // disambiguate by adding the package-suffix match in a future enhancement.
        if candidates.len() > 1 {
            return true;
        }
        let java = &candidates[0];
        let key = format!("{}>{}", java.id, xml.id);
        if !seen.insert(key) {
            return true;
        }
        edges.push(synthesized_edge(
            &java.id,
            &xml.id,
            Some(java.start_line),
            edge_meta(vec![
                ("synthesizedBy", Value::from("mybatis-java-xml")),
                ("via", Value::from(format!("{}.{}", class_name, id))),
                (
                    "registeredAt",
                    Value::from(format!("{}:{}", xml.file_path, xml.start_line)),
                ),
            ]),
        ));
        true
    })?;
    Ok(edges)
}

// c.handlers[c.index](c)
static GIN_DISPATCH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.handlers\s*\[[^\]]*\]\s*\(").expect("valid regex"));
static GIN_REG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.(?:Use|GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Any|Handle)\s*\(")
        .expect("valid regex")
});
static GIN_METHODS_TEST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.(?:GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Any|Handle)\(").expect("valid regex")
});

/// Balanced `(...)` body starting at the '(' index; None if unbalanced.
fn go_balanced_args(s: &str, open_idx: usize) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut depth = 0i64;
    let mut i = open_idx;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[open_idx + 1..i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split a top-level comma list, respecting nested () [] {}.
fn go_split_args(args: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut depth = 0i64;
    let mut cur = String::new();
    for c in args.chars() {
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

static GO_TRAILING_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(\s*\)$").expect("valid regex"));
static GO_TAIL_IDENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\.|^)([A-Za-z_][0-9A-Za-z_]*)$").expect("valid regex"));

/// Tail ident of a handler arg: `gin.Logger()`→`Logger`, `mw`→`mw`; None for string paths / closures.
fn go_handler_ident(expr: &str) -> Option<String> {
    let trimmed = expr.trim();
    let cleaned = GO_TRAILING_CALL_RE.replace(trimmed, ""); // drop a trailing call ()
    let cleaned = cleaned.as_ref();
    if cleaned.is_empty()
        || cleaned.starts_with('"')
        || cleaned.starts_with('`')
        || cleaned.starts_with("func")
    {
        return None;
    }
    GO_TAIL_IDENT_RE.captures(cleaned).map(|c| c[1].to_string())
}

/// Gin middleware chain. Gin runs its entire handler chain through one dynamic
/// line in `(*Context).Next`:
///     `for c.index < len(c.handlers) { c.handlers[c.index](c); c.index++ }`
/// `c.handlers` is a `HandlersChain` (`[]HandlerFunc`) assembled at registration
/// time by `combineHandlers` from the funcs passed to `r.Use(...)` /
/// `r.GET("/path", h...)` / `r.Handle(...)`. Because the call is a computed index
/// into a runtime-built slice, tree-sitter resolves `c.handlers[c.index](c)` to
/// NOTHING — so `callees(Next)` is just the `len()` helper and the flow
/// `ServeHTTP → handleHTTPRequest → Next` dead-ends at the exact symbol the
/// "how do requests flow through the middleware chain" question is about. The
/// agent then re-queries Next and falls back to Read/grep (validated: the gin
/// WITH-arm rabbit-holed on precisely this dead-end).
///
/// Bridge it: find the chain DISPATCHER (a Go method whose body invokes a
/// `handlers` slice by index) and link it → every HandlerFunc registered via a
/// gin registration call, so `callees(Next)` and `trace(ServeHTTP, <handler>)`
/// connect end-to-end. Named handlers only (`gin.Logger()` → `Logger`,
/// `authMiddleware`); inline closures are anonymous and skipped. Like
/// react-render / interface-impl this is a deliberate over-approximation —
/// reachability-correct (any registered handler CAN run for some route), capped,
/// and gated on the dispatcher existing so it never runs on non-gin Go repos.
/// Provenance `heuristic`, `synthesizedBy:'gin-middleware-chain'`; `registeredAt`
/// is the `.Use`/`.GET` site an agent would otherwise grep for.
fn gin_middleware_chain_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    // 1. Find the chain dispatcher(s): a Go method that invokes a `handlers` slice by index.
    let mut dispatchers: Vec<Node> = Vec::new();
    queries.iterate_nodes_by_kind(NodeKind::Method, |n| {
        if n.language != Language::Go {
            return true;
        }
        if let Some(src) = node_source(ctx, &n) {
            if GIN_DISPATCH_RE.is_match(&src) {
                dispatchers.push(n);
            }
        }
        true
    })?;
    if dispatchers.is_empty() {
        return Ok(Vec::new()); // not a gin repo — bail
    }

    // 2. Collect handler identifiers registered via gin registration calls
    //    (.Use / .GET / … / .Handle). String args (paths/methods) and inline
    //    closures are dropped by goHandlerIdent; the rest are HandlerFuncs.
    let mut registered: OrderedMap<String> = OrderedMap::new(); // name → registeredAt (file:line)
    for file in ctx.get_all_files() {
        if !file.ends_with(".go") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if content.is_empty()
            || (!content.contains(".Use(") && !GIN_METHODS_TEST_RE.is_match(&content))
        {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Go);
        for m in GIN_REG_RE.find_iter(&safe) {
            let paren_idx = m.end() - 1;
            let Some(arg_str) = go_balanced_args(&safe, paren_idx) else {
                continue;
            };
            let line = line_of(&safe, m.start());
            for arg in go_split_args(arg_str) {
                if let Some(name) = go_handler_ident(&arg) {
                    if !registered.contains_key(&name) {
                        registered.set(&name, format!("{}:{}", file, line));
                    }
                }
            }
        }
    }
    if registered.is_empty() {
        return Ok(Vec::new());
    }

    // 3. Link each dispatcher → each registered handler node (dedup, capped).
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for disp in &dispatchers {
        let mut added = 0usize;
        for (name, registered_at) in registered.iter() {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            let handler = ctx.get_nodes_by_name(name).into_iter().find(|n| {
                (n.kind == NodeKind::Function || n.kind == NodeKind::Method)
                    && n.language == Language::Go
            });
            let Some(handler) = handler else { continue };
            if handler.id == disp.id {
                continue;
            }
            let key = format!("{}>{}", disp.id, handler.id);
            if !seen.insert(key) {
                continue;
            }
            edges.push(synthesized_edge(
                &disp.id,
                &handler.id,
                Some(disp.start_line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("gin-middleware-chain")),
                    ("via", Value::from(name)),
                    ("registeredAt", Value::from(registered_at.as_str())),
                ]),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

/// Synthesize dispatcher→callback edges (field observers + EventEmitters +
/// React re-render + JSX children + Vue templates + RN event channel +
/// Fabric native-impl + MyBatis Java↔XML + Gin middleware chain). Returns the
/// count added. Errors never throw into indexing — the TS callers wrap in
/// try/catch; Rust callers handle the `Result`.
pub fn synthesize_callback_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<usize> {
    let field_edges = field_channel_edges(queries, ctx)?;
    let closure_coll_edges = closure_collection_edges(queries, ctx)?;
    let emitter_edges = event_emitter_edges(ctx);
    let render_edges = react_render_edges(queries, ctx)?;
    let jsx_edges = react_jsx_child_edges(ctx);
    let vue_edges = vue_template_edges(ctx);
    let flutter_edges = flutter_build_edges(queries, ctx)?;
    let cpp_edges = cpp_override_edges(queries)?;
    let iface_edges = interface_override_edges(queries)?;
    let go_grpc_edges = go_grpc_stub_impl_edges(queries)?;
    let rn_event_edges_list = rn_event_edges(ctx);
    let fabric_native_edges = fabric_native_impl_edges(ctx);
    let mybatis_edges = mybatis_java_xml_edges(queries)?;
    let gin_edges = gin_middleware_chain_edges(queries, ctx)?;

    let mut merged: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for e in field_edges
        .into_iter()
        .chain(closure_coll_edges)
        .chain(emitter_edges)
        .chain(render_edges)
        .chain(jsx_edges)
        .chain(vue_edges)
        .chain(flutter_edges)
        .chain(cpp_edges)
        .chain(iface_edges)
        .chain(go_grpc_edges)
        .chain(rn_event_edges_list)
        .chain(fabric_native_edges)
        .chain(mybatis_edges)
        .chain(gin_edges)
    {
        let key = format!("{}>{}", e.source, e.target);
        if !seen.insert(key) {
            continue;
        }
        merged.push(e);
    }
    if !merged.is_empty() {
        queries.insert_edges(&merged)?;
    }
    Ok(merged.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_to_pascal_matches_ts() {
        assert_eq!(kebab_to_pascal("el-button"), "ElButton");
        assert_eq!(kebab_to_pascal("my-fancy-tag"), "MyFancyTag");
        assert_eq!(kebab_to_pascal("single"), "Single");
    }

    #[test]
    fn slice_lines_matches_ts_semantics() {
        let content = "a\nb\nc\nd";
        assert_eq!(slice_lines(content, 2, 3).unwrap(), "b\nc");
        assert_eq!(slice_lines(content, 1, 99).unwrap(), "a\nb\nc\nd");
        // falsy bounds → None (TS `if (!startLine || !endLine) return null`)
        assert!(slice_lines(content, 0, 3).is_none());
        assert!(slice_lines(content, 2, 0).is_none());
        // out-of-range start → empty (TS slice clamps to [])
        assert_eq!(slice_lines(content, 10, 12).unwrap(), "");
    }

    #[test]
    fn registrar_and_dispatcher_field_extraction() {
        assert_eq!(
            registrar_field("onUpdate(cb) { this.callbacks.add(cb); }").as_deref(),
            Some("callbacks")
        );
        assert_eq!(
            registrar_field("onX(cb) { this.listeners.push(cb); }").as_deref(),
            Some("listeners")
        );
        assert!(registrar_field("noop() { return 1; }").is_none());

        // for-of + an invocation in the body
        assert_eq!(
            dispatcher_field("triggerUpdate() { for (const cb of this.callbacks) cb(); }")
                .as_deref(),
            Some("callbacks")
        );
        // Array.from variant
        assert_eq!(
            dispatcher_field("t() { for (const cb of Array.from( this.subs)) cb(); }").as_deref(),
            Some("subs")
        );
        // forEach variant
        assert_eq!(
            dispatcher_field("notify() { this.watchers.forEach((w) => w()); }").as_deref(),
            Some("watchers")
        );
        assert!(dispatcher_field("idle() { return; }").is_none());
    }

    #[test]
    fn closure_collection_regexes_match_swift_shapes() {
        let caps = CC_DISPATCH_RE
            .captures("validators.forEach { $0() }")
            .unwrap();
        assert_eq!(&caps[1], "validators");
        // Kotlin `it()` form
        assert!(CC_DISPATCH_RE.is_match("handlers.forEach { it() }"));
        // non-invoking forEach is NOT a dispatcher
        assert!(!CC_DISPATCH_RE.is_match("names.forEach { print($0) }"));

        let w = CC_APPEND_WRITE_RE
            .captures("validators.write { $0.append(validator) }")
            .unwrap();
        assert_eq!(&w[1], "validators");
        assert!(w.get(2).is_none());
        // nested property form captures group 2
        let w2 = CC_APPEND_WRITE_RE
            .captures("state.write { $0.streams.append(stream) }")
            .unwrap();
        assert_eq!(&w2[1], "state");
        assert_eq!(&w2[2], "streams");

        // direct append mis-captures `$0` as `0` — rejected by the digits guard
        let a = CC_APPEND_DIRECT_RE
            .captures("$0.append(validator)")
            .unwrap();
        assert_eq!(&a[1], "0");
    }

    #[test]
    fn enclosing_fn_prefers_tightest_encloser() {
        let outer = Node::new(
            "outer",
            NodeKind::Function,
            "outer",
            "outer",
            "a.ts",
            Language::Typescript,
            1,
            20,
        );
        let inner = Node::new(
            "inner",
            NodeKind::Method,
            "inner",
            "inner",
            "a.ts",
            Language::Typescript,
            5,
            10,
        );
        let not_fn = Node::new(
            "cls",
            NodeKind::Class,
            "cls",
            "cls",
            "a.ts",
            Language::Typescript,
            1,
            20,
        );
        let nodes = vec![outer, inner, not_fn];
        assert_eq!(enclosing_fn(&nodes, 7).unwrap().id, "inner");
        assert_eq!(enclosing_fn(&nodes, 15).unwrap().id, "outer");
        assert!(enclosing_fn(&nodes, 25).is_none());
    }

    #[test]
    fn go_helpers_match_ts() {
        // balanced args
        let s = r#"r.GET("/ping", gin.Logger(), handlePing)"#;
        let open = s.find('(').unwrap();
        assert_eq!(
            go_balanced_args(s, open).unwrap(),
            r#""/ping", gin.Logger(), handlePing"#
        );
        assert!(go_balanced_args("(unbalanced", 0).is_none());

        // split args respecting nesting
        assert_eq!(
            go_split_args(r#""/p", f(a, b), g"#),
            vec![
                r#""/p""#.to_string(),
                " f(a, b)".to_string(),
                " g".to_string()
            ]
        );

        // handler ident
        assert_eq!(go_handler_ident("gin.Logger()").as_deref(), Some("Logger"));
        assert_eq!(
            go_handler_ident(" authMiddleware ").as_deref(),
            Some("authMiddleware")
        );
        assert!(go_handler_ident(r#""/path""#).is_none());
        assert!(go_handler_ident("func(c *gin.Context) {}").is_none());
        assert!(go_handler_ident("`raw`").is_none());
    }

    #[test]
    fn ordered_map_and_set_preserve_insertion_order() {
        let mut m: OrderedMap<u32> = OrderedMap::new();
        m.set("b", 1);
        m.set("a", 2);
        m.set("b", 3); // update keeps position (JS Map.set parity)
        let entries: Vec<(&str, u32)> = m.iter().map(|(k, v)| (k, *v)).collect();
        assert_eq!(entries, vec![("b", 3), ("a", 2)]);

        let mut s = OrderedSet::default();
        s.add("x");
        s.add("y");
        s.add("x");
        let items: Vec<&String> = s.iter().collect();
        assert_eq!(items, vec!["x", "y"]);
        assert_eq!(s.len(), 2);
    }
}
