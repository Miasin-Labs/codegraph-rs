//! Express/Node.js Framework Resolver
//!
//! Handles Express and general Node.js patterns.
//! Ported from `src/resolution/frameworks/express.ts`.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::{
    FrameworkExtractionResult,
    FrameworkResolver,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{EdgeKind, Language, Node, NodeKind};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn extract_tail_ident(expr: &str) -> Option<String> {
    static TAIL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$").unwrap());
    let cleaned: String = expr.chars().filter(|c| !c.is_whitespace()).collect();
    let cleaned = cleaned.strip_suffix("()").unwrap_or(&cleaned);
    TAIL.captures(cleaned).map(|c| c[1].to_string())
}

/// Index of the delimiter matching the one at `open`, skipping string/template
/// literals so a `)` or `}` inside a string doesn't throw off the balance.
fn match_delim(s: &str, open: usize, oc: u8, cc: u8) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth: i64 = 0;
    let mut i = open;
    while i < b.len() {
        let ch = b[i];
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            let q = ch;
            i += 1;
            while i < b.len() && b[i] != q {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1; // step past the closing quote (mirrors the TS for-loop i++)
            continue;
        }
        if ch == oc {
            depth += 1;
        } else if ch == cc {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

// Express res/req methods + common JS builtins — calls to these inside a handler
// body are framework/noise, not the business flow we want to surface as route edges.
static RESERVED_CALLS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "json",
        "jsonp",
        "send",
        "sendStatus",
        "sendFile",
        "status",
        "end",
        "redirect",
        "render",
        "set",
        "get",
        "header",
        "type",
        "format",
        "attachment",
        "download",
        "cookie",
        "clearCookie",
        "append",
        "location",
        "vary",
        "links",
        "accepts",
        "is",
        "next",
        "then",
        "catch",
        "finally",
        "resolve",
        "reject",
        "all",
        "race",
        "map",
        "filter",
        "forEach",
        "reduce",
        "find",
        "push",
        "pop",
        "slice",
        "splice",
        "includes",
        "keys",
        "values",
        "entries",
        "assign",
        "parse",
        "stringify",
        "log",
        "error",
        "warn",
        "info",
        "String",
        "Number",
        "Boolean",
        "Array",
        "Object",
        "Date",
        "Math",
        "JSON",
        "Promise",
        "require",
        "fail",
    ]
    .into_iter()
    .collect()
});

/// `expressResolver` — unit struct implementing [`FrameworkResolver`].
pub struct ExpressResolver;

const EXPRESS_LANGUAGES: [Language; 2] = [Language::Javascript, Language::Typescript];

impl FrameworkResolver for ExpressResolver {
    fn name(&self) -> &str {
        "express"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&EXPRESS_LANGUAGES)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for Express in package.json
        if let Some(package_json) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&package_json) {
                let deps = merged_deps(&pkg);
                if ["express", "fastify", "koa", "hapi"]
                    .iter()
                    .any(|k| dep_truthy(&deps, k))
                {
                    return true;
                }
            }
        }

        // Check for common Express patterns
        for file in context.get_all_files() {
            if file.contains("routes")
                || file.contains("controllers")
                || file.contains("middleware")
            {
                if let Some(content) = context.read_file(&file) {
                    if content.contains("express")
                        || content.contains("app.get")
                        || content.contains("router.get")
                    {
                        return true;
                    }
                }
            }
        }

        false
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // Pattern 1: Middleware references
        if is_middleware_name(&reference.reference_name) {
            if let Some(result) = resolve_middleware(&reference.reference_name, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 2: Controller method references
        static CONTROLLER_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^([A-Za-z0-9_]+)Controller\.([A-Za-z0-9_]+)$").unwrap());
        if let Some(m) = CONTROLLER_RE.captures(&reference.reference_name) {
            if let Some(result) = resolve_controller_method(&m[1], &m[2], context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 3: Service/helper references
        static SERVICE_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^([A-Za-z0-9_]+)(Service|Helper|Utils?)\.([A-Za-z0-9_]+)$").unwrap()
        });
        if let Some(m) = SERVICE_RE.captures(&reference.reference_name) {
            let service_name = format!("{}{}", &m[1], &m[2]);
            if let Some(result) = resolve_service_method(&service_name, &m[3], context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !has_js_extension(file_path) {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_ms();
        let lang = detect_language(file_path);
        let safe = strip_comments_for_regex(content, comment_lang(lang));
        // Match the route head up to the first arg: (app|router).METHOD('/path',
        // (NOT the whole call — handlers are often inline arrows whose `)`/`{}` the
        // old single-regex couldn't span, so inline-handler routes connected to nothing.)
        static HEAD: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r#"\b(app|router)\.(get|post|put|patch|delete|all|use)\s*\(\s*['"]([^'"]+)['"]\s*,"#,
            )
            .unwrap()
        });
        for m in HEAD.captures_iter(&safe) {
            let whole = m.get(0).unwrap();
            let method = &m[2];
            let route_path = &m[3];
            if method == "use" && !route_path.starts_with('/') {
                continue;
            }
            let line = line_at(&safe, whole.start());
            let method_upper = method.to_uppercase();
            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{method_upper}:{route_path}"),
                NodeKind::Route,
                format!("{method_upper} {route_path}"),
                format!("{file_path}::{method_upper}:{route_path}"),
                file_path,
                lang,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_node_id = route_node.id.clone();
            nodes.push(route_node);

            // The full argument list = balanced parens from the route call's open paren.
            let open_paren = safe[whole.start()..].find('(').map(|i| i + whole.start());
            let close_paren = open_paren.and_then(|op| match_delim(&safe, op, b'(', b')'));
            let args = match (open_paren, close_paren) {
                (Some(op), Some(cp)) if cp > op => &safe[op + 1..cp],
                _ => "",
            };
            let arrow_at = args.find("=>");

            if let Some(arrow_at) = arrow_at {
                // Inline arrow handler (`router.post('/x', async (req,res) => {…})`). The
                // arrow is anonymous, so its body — the actual request→service flow — would
                // be lost. Attribute the body's calls to the route node as `calls` edges so
                // `trace(route, service)` connects. Body = balanced `{…}` after `=>`, or the
                // single-expression tail for `=> expr` arrows.
                let after_arrow = &args[arrow_at + 2..];
                let brace_at = after_arrow.find('{');
                let mut body = after_arrow;
                if let Some(brace_at) = brace_at {
                    if after_arrow[..brace_at].trim().is_empty() {
                        if let Some(end) = match_delim(after_arrow, brace_at, b'{', b'}') {
                            if end > brace_at {
                                body = &after_arrow[brace_at + 1..end];
                            }
                        }
                    }
                }
                static CALL_RE: LazyLock<Regex> =
                    LazyLock::new(|| Regex::new(r"\b([A-Za-z_$][A-Za-z0-9_$]*)\s*\(").unwrap());
                let mut seen: HashSet<String> = HashSet::new();
                for cm in CALL_RE.captures_iter(body) {
                    let name = &cm[1];
                    if seen.contains(name) || RESERVED_CALLS.contains(name) {
                        continue;
                    }
                    seen.insert(name.to_string());
                    references.push(UnresolvedRef {
                        from_node_id: route_node_id.clone(),
                        reference_name: name.to_string(),
                        reference_kind: EdgeKind::Calls,
                        line,
                        column: 0,
                        file_path: file_path.to_string(),
                        language: lang,
                        candidates: None,
                    });
                }
            } else {
                // Named handler: the LAST comma-separated arg (earlier ones are middleware).
                let parts: Vec<&str> = args
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();
                let handler_name = parts.last().and_then(|last| extract_tail_ident(last));
                if let Some(handler_name) = handler_name {
                    references.push(UnresolvedRef {
                        from_node_id: route_node_id.clone(),
                        reference_name: handler_name,
                        reference_kind: EdgeKind::References,
                        line,
                        column: 0,
                        file_path: file_path.to_string(),
                        language: lang,
                        candidates: None,
                    });
                }
            }
        }
        Some(FrameworkExtractionResult { nodes, references })
    }
}

/// TS `/\.(m?js|tsx?|cjs)$/` — the file extensions the extractor runs on.
fn has_js_extension(file_path: &str) -> bool {
    [".js", ".mjs", ".cjs", ".ts", ".tsx"]
        .iter()
        .any(|ext| file_path.ends_with(ext))
}

/// 1-based line number of the byte offset `index`.
fn line_at(s: &str, index: usize) -> u32 {
    (s[..index].matches('\n').count() + 1) as u32
}

/// `{ ...pkg.dependencies, ...pkg.devDependencies }`
fn merged_deps(pkg: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut deps = serde_json::Map::new();
    for key in ["dependencies", "devDependencies"] {
        if let Some(obj) = pkg.get(key).and_then(|v| v.as_object()) {
            for (k, v) in obj {
                deps.insert(k.clone(), v.clone());
            }
        }
    }
    deps
}

/// JS truthiness check on a dependency entry (`if (deps.express)`).
fn dep_truthy(deps: &serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    match deps.get(key) {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// Check if a name looks like middleware
fn is_middleware_name(name: &str) -> bool {
    static MIDDLEWARE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        [
            r"(?i)^auth$",
            r"(?i)^authenticate$",
            r"(?i)^authorization$",
            r"(?i)^validate",
            r"(?i)^sanitize",
            r"(?i)^rateLimit",
            r"(?i)^cors$",
            r"(?i)^helmet$",
            r"(?i)^logger$",
            r"(?i)^errorHandler$",
            r"(?i)^notFound$",
            r"(?i)Middleware$",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    });
    MIDDLEWARE_PATTERNS.iter().any(|p| p.is_match(name))
}

/// TS `name.replace(/Middleware$/i, '')`
fn strip_middleware_suffix(name: &str) -> String {
    static SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)Middleware$").unwrap());
    SUFFIX.replace(name, "").into_owned()
}

/// Resolve middleware reference using name-based lookup
fn resolve_middleware(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    // Try exact name first
    let candidates = context.get_nodes_by_name(name);
    let name_lower = name.to_lowercase();
    let base_name = strip_middleware_suffix(name);
    let base_lower = base_name.to_lowercase();
    if let Some(m) = candidates.iter().find(|n| {
        let n_lower = n.name.to_lowercase();
        n_lower == name_lower || n_lower == base_lower
    }) {
        return Some(m.id.clone());
    }

    // Try without Middleware suffix
    if base_name != name {
        let base_candidates = context.get_nodes_by_name(&base_name);
        const MIDDLEWARE_DIRS: [&str; 2] = ["/middleware/", "/middlewares/"];
        let preferred: Vec<&Node> = base_candidates
            .iter()
            .filter(|n| MIDDLEWARE_DIRS.iter().any(|d| n.file_path.contains(d)))
            .collect();
        if let Some(first) = preferred.first() {
            return Some(first.id.clone());
        }
        if let Some(first) = base_candidates.first() {
            return Some(first.id.clone());
        }
    }

    None
}

/// Resolve controller method using name-based lookup
fn resolve_controller_method(
    controller: &str,
    method: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    // Look for the method name directly
    let method_candidates = context.get_nodes_by_name(method);
    let controller_lower = controller.to_lowercase();
    let method_nodes: Vec<&Node> = method_candidates
        .iter()
        .filter(|n| {
            (n.kind == NodeKind::Method || n.kind == NodeKind::Function)
                && n.file_path.to_lowercase().contains(&controller_lower)
        })
        .collect();

    if let Some(first) = method_nodes.first() {
        return Some(first.id.clone());
    }

    // Fall back: look for controller class, then find the method in its file
    let controller_name = format!("{controller}Controller");
    for ctrl in context.get_nodes_by_name(&controller_name) {
        let nodes_in_file = context.get_nodes_in_file(&ctrl.file_path);
        if let Some(method_node) = nodes_in_file.iter().find(|n| {
            (n.kind == NodeKind::Method || n.kind == NodeKind::Function) && n.name == method
        }) {
            return Some(method_node.id.clone());
        }
    }

    None
}

/// Resolve service/helper method using name-based lookup
fn resolve_service_method(
    service_name: &str,
    method: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    static SERVICE_SUFFIX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(Service|Helper|Utils?)$").unwrap());
    // Look for the method in files matching the service name
    let method_candidates = context.get_nodes_by_name(method);
    let stripped = SERVICE_SUFFIX.replace(service_name, "").to_lowercase();
    let method_nodes: Vec<&Node> = method_candidates
        .iter()
        .filter(|n| {
            (n.kind == NodeKind::Method || n.kind == NodeKind::Function)
                && n.file_path.to_lowercase().contains(&stripped)
        })
        .collect();

    method_nodes.first().map(|n| n.id.clone())
}

/// Detect language from file extension
fn detect_language(file_path: &str) -> Language {
    if file_path.ends_with(".ts") || file_path.ends_with(".tsx") {
        Language::Typescript
    } else {
        Language::Javascript
    }
}

fn comment_lang(lang: Language) -> CommentLang {
    match lang {
        Language::Typescript => CommentLang::Typescript,
        _ => CommentLang::Javascript,
    }
}
