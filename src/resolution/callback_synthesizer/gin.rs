//! Gin middleware-chain synthesis.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::ordered::OrderedMap;
use super::source::{line_of, node_source};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Language, Node, NodeKind};

const MAX_CALLBACKS_PER_CHANNEL: usize = 40;

// c.handlers[c.index](c)
static GIN_DISPATCH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.handlers\s*\[[^\]]*\]\s*\(").expect("valid regex"));
static GIN_REG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.(?:Use|GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Any|Handle)\s*\(")
        .expect("valid regex")
});

/// Balanced `(...)` body starting at the '(' index; None if unbalanced.
pub(super) fn go_balanced_args(s: &str, open_idx: usize) -> Option<&str> {
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
pub(super) fn go_split_args(args: &str) -> Vec<String> {
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
pub(super) fn go_handler_ident(expr: &str) -> Option<String> {
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
pub(super) fn gin_middleware_chain_edges(
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
        if content.is_empty() || !GIN_REG_RE.is_match(&content) {
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
