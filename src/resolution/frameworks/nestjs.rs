//! NestJS Framework Resolver
//!
//! Handles NestJS decorator-based routing across its transport layers:
//!   - HTTP:          `@Controller(prefix)` + `@Get/@Post/@Put/@Patch/@Delete/@Head/@Options/@All`
//!   - GraphQL:       `@Resolver` + `@Query/@Mutation/@Subscription`
//!   - Microservices: `@MessagePattern` / `@EventPattern`
//!   - WebSockets:    `@WebSocketGateway(namespace)` + `@SubscribeMessage(event)`
//!
//! Like the other framework extractors this is regex-over-source (comment-
//! stripped), not AST traversal. NestJS differs from Spring/ASP.NET in two ways
//! that this resolver has to account for:
//!
//!   1. An HTTP route's path is split across TWO decorators — the class-level
//!      `@Controller` prefix and the method-level `@Get`/`@Post` path — and both
//!      are frequently empty (`@Controller()`, `@Get()`). We pair each method
//!      decorator with its enclosing class and join the two paths.
//!
//!   2. `@Query()` is overloaded: it's a GraphQL *method* decorator (from
//!      `@nestjs/graphql`) AND a REST *parameter* decorator (from
//!      `@nestjs/common`). We only treat it as GraphQL when it sits inside an
//!      `@Resolver` class, which is what disambiguates the two.
//!
//! Ported from `src/resolution/frameworks/nestjs.ts`.

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

// ---------------------------------------------------------------------------
// Public surface — see comment at top of file. This file owns four NestJS
// concerns: HTTP routes, GraphQL ops, microservice handlers, WebSocket
// handlers, and (in post_extract below) cross-file RouterModule prefixing.
// ---------------------------------------------------------------------------

const HTTP_METHODS: [&str; 8] = [
    "Get", "Post", "Put", "Patch", "Delete", "Head", "Options", "All",
];
const GQL_OPS: [&str; 3] = ["Query", "Mutation", "Subscription"];

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// `nestjsResolver` — unit struct implementing [`FrameworkResolver`].
pub struct NestjsResolver;

const NESTJS_LANGUAGES: [Language; 2] = [Language::Typescript, Language::Javascript];

impl FrameworkResolver for NestjsResolver {
    fn name(&self) -> &str {
        "nestjs"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&NESTJS_LANGUAGES)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Primary, fast path: any @nestjs/* dependency in package.json.
        if let Some(package_json) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&package_json) {
                let mut keys: Vec<String> = Vec::new();
                for key in ["dependencies", "devDependencies"] {
                    if let Some(obj) = pkg.get(key).and_then(|v| v.as_object()) {
                        keys.extend(obj.keys().cloned());
                    }
                }
                if keys.iter().any(|k| k.starts_with("@nestjs/")) {
                    return true;
                }
            }
            // Invalid JSON — fall through to the source scan.
        }

        // Fallback: NestJS-specific decorators in conventionally named files.
        for file in context.get_all_files() {
            if file.ends_with(".controller.ts")
                || file.ends_with(".controller.js")
                || file.ends_with(".module.ts")
                || file.ends_with(".resolver.ts")
                || file.ends_with(".gateway.ts")
            {
                if let Some(content) = context.read_file(&file) {
                    if content.contains("@nestjs/")
                        || content.contains("@Controller")
                        || content.contains("@Module(")
                        || content.contains("@Resolver(")
                        || content.contains("@WebSocketGateway(")
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
        // Resolve provider/controller references (e.g. constructor-injected
        // `UsersService`) to their class, preferring the Nest file-name
        // convention (`*.service.ts`, `*.controller.ts`, …).
        for (suffix, convention) in PROVIDER_CONVENTIONS.iter() {
            if !suffix.is_match(&reference.reference_name) {
                continue;
            }
            let candidates: Vec<Node> = context
                .get_nodes_by_name(&reference.reference_name)
                .into_iter()
                .filter(|n| n.kind == NodeKind::Class)
                .collect();
            if candidates.is_empty() {
                return None;
            }
            let preferred = candidates.iter().find(|n| n.file_path.contains(convention));
            let has_preferred = preferred.is_some();
            let target = preferred.unwrap_or(&candidates[0]);
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: target.id.clone(),
                confidence: if has_preferred { 0.85 } else { 0.7 },
                resolved_by: ResolvedBy::Framework,
            });
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

        let add_route = |nodes: &mut Vec<Node>,
                         references: &mut Vec<UnresolvedRef>,
                         line: u32,
                         method: &str,
                         path: &str,
                         length: usize,
                         handler: Option<String>| {
            let mut node = Node::new(
                format!("route:{file_path}:{line}:{method}:{path}"),
                NodeKind::Route,
                format!("{method} {path}"),
                format!("{file_path}::{method}:{path}"),
                file_path,
                lang,
                line,
                line,
            );
            node.end_column = length as u32;
            node.updated_at = now;
            let node_id = node.id.clone();
            nodes.push(node);
            if let Some(handler) = handler {
                references.push(UnresolvedRef {
                    from_node_id: node_id,
                    reference_name: handler,
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: lang,
                    candidates: None,
                });
            }
        };

        let scopes = build_class_scopes(&safe);

        // HTTP routes: method decorator path joined onto the enclosing controller's prefix.
        for hit in find_decorators(&safe, &HTTP_METHODS) {
            let scope = scope_for(&scopes, hit.index);
            let prefix = match scope {
                Some(s) if s.kind == ClassKind::Controller => s.prefix.as_str(),
                _ => "",
            };
            let path = join_http_path(prefix, &parse_string_arg(&hit.args));
            add_route(
                &mut nodes,
                &mut references,
                line_at(&safe, hit.index),
                &hit.name.to_uppercase(),
                &path,
                hit.length,
                method_name_after(&safe, hit.end),
            );
        }

        // GraphQL operations: only inside an @Resolver class (disambiguates the
        // REST `@Query()` parameter decorator, which lives inside @Controller classes).
        for hit in find_decorators(&safe, &GQL_OPS) {
            let scope = scope_for(&scopes, hit.index);
            match scope {
                Some(s) if s.kind == ClassKind::Resolver => {}
                _ => continue,
            }
            let handler = method_name_after(&safe, hit.end);
            let name = parse_graphql_name(&hit.args, handler.as_deref());
            add_route(
                &mut nodes,
                &mut references,
                line_at(&safe, hit.index),
                &hit.name.to_uppercase(),
                &name,
                hit.length,
                handler,
            );
        }

        // Microservice message/event handlers.
        for hit in find_decorators(&safe, &["MessagePattern", "EventPattern"]) {
            let verb = if hit.name == "EventPattern" {
                "EVENT"
            } else {
                "MESSAGE"
            };
            let handler = method_name_after(&safe, hit.end);
            let arg = parse_string_arg(&hit.args);
            let path = if !arg.is_empty() {
                arg
            } else {
                handler.clone().unwrap_or_default()
            };
            add_route(
                &mut nodes,
                &mut references,
                line_at(&safe, hit.index),
                verb,
                &path,
                hit.length,
                handler,
            );
        }

        // WebSocket message handlers, prefixed with the gateway namespace when present.
        for hit in find_decorators(&safe, &["SubscribeMessage"]) {
            let scope = scope_for(&scopes, hit.index);
            let namespace = match scope {
                Some(s) if s.kind == ClassKind::Gateway => s.prefix.as_str(),
                _ => "",
            };
            let handler = method_name_after(&safe, hit.end);
            let arg = parse_string_arg(&hit.args);
            let event = if !arg.is_empty() {
                arg
            } else {
                handler.clone().unwrap_or_default()
            };
            let path = if !namespace.is_empty() {
                format!("{namespace}:{event}")
            } else {
                event
            };
            add_route(
                &mut nodes,
                &mut references,
                line_at(&safe, hit.index),
                "WS",
                &path,
                hit.length,
                handler,
            );
        }

        Some(FrameworkExtractionResult { nodes, references })
    }

    /// Cross-file finalization for `RouterModule.register([...])`. The per-file
    /// extract() above only sees `@Controller(prefix) + @Get(path)` — it can't
    /// learn about the route prefix supplied by a sibling `app.module.ts` like:
    ///
    /// ```text
    ///   RouterModule.register([
    ///     { path: 'admin', module: AdminModule, children: [
    ///       { path: 'users', module: UsersModule } ] } ])
    /// ```
    ///
    /// This pass scans every `*.module.{ts,js}` file, walks the registration
    /// tree to build a `Module → /full/prefix` map, walks each `@Module({
    /// controllers: [...] })` to build a `Controller → Module` map, and rewrites
    /// affected route nodes so `GET /` becomes `GET /admin/users` (and
    /// `@Controller('foo') + @Get(':id')` under that same module becomes
    /// `GET /admin/users/foo/:id`).
    ///
    /// The route node's `id` and `qualified_name` are deliberately preserved
    /// across the update: `id` because existing route→handler edges reference
    /// it, `qualified_name` because it still encodes the *original* in-file
    /// `method:path` — which keeps this pass idempotent (a second run recovers
    /// the same input regardless of how many times it has already prefixed).
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        static MODULE_FILE_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\.module\.(m?[jt]s|cjs)$").unwrap());

        // Insertion-ordered maps mirroring the TS `Map`s (first-write-wins
        // is enforced at insert time in the collect fns).
        let mut module_to_prefix: Vec<(String, String)> = Vec::new();
        let mut controller_to_module: Vec<(String, String)> = Vec::new();

        for file_path in context.get_all_files() {
            if !MODULE_FILE_RE.is_match(&file_path) {
                continue;
            }
            let Some(content) = context.read_file(&file_path) else {
                continue;
            };
            let safe =
                strip_comments_for_regex(&content, comment_lang(detect_language(&file_path)));
            collect_router_module_registrations(&safe, &mut module_to_prefix);
            collect_module_controllers(&safe, &mut controller_to_module);
        }

        let mut controller_to_prefix: Vec<(String, String)> = Vec::new();
        for (controller, module) in &controller_to_module {
            let prefix = module_to_prefix
                .iter()
                .find(|(m, _)| m == module)
                .map(|(_, p)| p.as_str());
            // `''` and `'/'` are no-op prefixes; skip them so we don't run updates
            // that would set name to the value it already has.
            if let Some(prefix) = prefix {
                if !prefix.is_empty() && prefix != "/" {
                    controller_to_prefix.push((controller.clone(), prefix.to_string()));
                }
            }
        }

        if controller_to_prefix.is_empty() {
            return Some(Vec::new());
        }

        let mut updates: Vec<Node> = Vec::new();
        for (controller_name, prefix) in &controller_to_prefix {
            let classes: Vec<Node> = context
                .get_nodes_by_name(controller_name)
                .into_iter()
                .filter(|n| n.kind == NodeKind::Class)
                .collect();
            for cls in &classes {
                let routes: Vec<Node> = context
                    .get_nodes_in_file(&cls.file_path)
                    .into_iter()
                    .filter(|n| n.kind == NodeKind::Route)
                    .collect();
                for route in &routes {
                    // Multiple controllers can live in one file (covered by the
                    // existing "attributes methods to the right controller" test);
                    // each route must be associated with the controller whose line
                    // range contains it.
                    if route.start_line < cls.start_line || route.start_line > cls.end_line {
                        continue;
                    }
                    if let Some(updated) = apply_module_prefix(route, prefix) {
                        if updated.name != route.name {
                            updates.push(updated);
                        }
                    }
                }
            }
        }

        Some(updates)
    }
}

// ---------------------------------------------------------------------------
// Provider resolution conventions
// ---------------------------------------------------------------------------

static PROVIDER_CONVENTIONS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        ("Service$", ".service."),
        ("Controller$", ".controller."),
        ("Resolver$", ".resolver."),
        ("Gateway$", ".gateway."),
        ("Repository$", ".repository."),
        ("Guard$", ".guard."),
        ("Interceptor$", ".interceptor."),
        ("Pipe$", ".pipe."),
        ("Module$", ".module."),
    ]
    .iter()
    .map(|(p, c)| (Regex::new(p).unwrap(), *c))
    .collect()
});

// ---------------------------------------------------------------------------
// Decorator scanning
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DecoratorHit {
    /// Decorator name without the leading `@` (e.g. `Get`).
    name: String,
    /// Raw text between the decorator's parentheses.
    args: String,
    /// Byte index of the leading `@` in the (comment-stripped) source.
    index: usize,
    /// Byte index just past the decorator's closing `)`.
    end: usize,
    /// Byte length of the whole `@Name(...)` decorator.
    length: usize,
}

/// Find every `@Name(...)` decorator whose name is in `names`. Uses a
/// string-aware balanced-paren reader for the argument list so type thunks
/// like `@Query(() => [User])` are captured whole rather than truncated at the
/// inner `()`.
fn find_decorators(safe: &str, names: &[&str]) -> Vec<DecoratorHit> {
    let mut hits = Vec::new();
    let re = Regex::new(&format!(r"@({})\s*\(", names.join("|"))).unwrap();
    let mut pos = 0usize;
    while pos <= safe.len() {
        let Some(c) = re.captures_at(safe, pos) else {
            break;
        };
        let whole = c.get(0).unwrap();
        let open_index = whole.end() - 1; // position of '('
        match read_args(safe, open_index) {
            Some((args, end)) => {
                hits.push(DecoratorHit {
                    name: c[1].to_string(),
                    args,
                    index: whole.start(),
                    end,
                    length: end - whole.start(),
                });
                pos = end; // resume past the args so nested text isn't re-scanned
            }
            None => {
                pos = whole.end();
            }
        }
    }
    hits
}

/// Read a balanced `(...)` starting at `open_index` (which must point at `(`).
/// String-aware, so parens inside string literals don't unbalance the count.
/// Returns the inner text and the byte index just past the closing `)`.
fn read_args(s: &str, open_index: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.get(open_index) != Some(&b'(') {
        return None;
    }
    let mut depth: i64 = 0;
    let mut in_str: Option<u8> = None;
    let mut i = open_index;
    while i < b.len() {
        let ch = b[i];
        if let Some(q) = in_str {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            in_str = Some(ch);
            i += 1;
            continue;
        }
        if ch == b'(' {
            depth += 1;
        } else if ch == b')' {
            depth -= 1;
            if depth == 0 {
                return Some((s[open_index + 1..i].to_string(), i + 1));
            }
        }
        i += 1;
    }
    None
}

/// Advance `*i` past any whitespace (TS sticky `/\s*/y`).
fn eat_ws(s: &str, i: &mut usize) {
    while *i < s.len() {
        let c = s[*i..].chars().next().unwrap();
        if c.is_whitespace() {
            *i += c.len_utf8();
        } else {
            break;
        }
    }
}

/// Starting just after a method decorator's `)`, return the name of the method
/// it decorates. Skips any further stacked decorators (`@UseGuards(...)`,
/// `@HttpCode(204)`, …) and access/async modifiers in between.
fn method_name_after(safe: &str, start: usize) -> Option<String> {
    static DECO_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^@[A-Za-z0-9_.]+").unwrap());
    static MODIFIER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:public|private|protected|async|static)\b").unwrap());
    static IDENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^([A-Za-z_$][A-Za-z0-9_$]*)\s*\(").unwrap());

    let mut i = start;

    // Skip stacked decorators.
    loop {
        eat_ws(safe, &mut i);
        if safe.as_bytes().get(i) != Some(&b'@') {
            break;
        }
        let Some(m) = DECO_NAME.find(&safe[i..]) else {
            break;
        };
        i += m.end();
        eat_ws(safe, &mut i);
        if safe.as_bytes().get(i) == Some(&b'(') {
            let (_, end) = read_args(safe, i)?;
            i = end;
        }
    }

    // Skip access/async/static modifiers.
    loop {
        eat_ws(safe, &mut i);
        if let Some(m) = MODIFIER.find(&safe[i..]) {
            if m.end() > 0 {
                i += m.end();
                continue;
            }
        }
        break;
    }

    eat_ws(safe, &mut i);
    IDENT.captures(&safe[i..]).map(|c| c[1].to_string())
}

// ---------------------------------------------------------------------------
// Class scopes (controller / resolver / gateway boundaries)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassKind {
    Controller,
    Resolver,
    Gateway,
    Other,
}

#[derive(Debug, Clone)]
struct ClassScope {
    kind: ClassKind,
    /// HTTP prefix (controller) or WS namespace (gateway); '' otherwise.
    prefix: String,
    start: usize,
    end: usize,
}

/// Build the list of class-level decorator scopes, sorted by position. Each
/// scope runs from its decorator up to the next class decorator (of any kind),
/// which lets a method decorator find its enclosing class regardless of how
/// many classes share a file.
fn build_class_scopes(safe: &str) -> Vec<ClassScope> {
    type PrefixOf = fn(&str) -> String;
    let defs: [(ClassKind, &str, PrefixOf); 6] = [
        (ClassKind::Controller, "Controller", parse_controller_prefix),
        (ClassKind::Resolver, "Resolver", |_| String::new()),
        (
            ClassKind::Gateway,
            "WebSocketGateway",
            parse_gateway_namespace,
        ),
        (ClassKind::Other, "Injectable", |_| String::new()),
        (ClassKind::Other, "Module", |_| String::new()),
        (ClassKind::Other, "Catch", |_| String::new()),
    ];

    let mut raw: Vec<(ClassKind, String, usize)> = Vec::new();
    for (kind, name, prefix_of) in &defs {
        for hit in find_decorators(safe, &[name]) {
            raw.push((*kind, prefix_of(&hit.args), hit.index));
        }
    }
    raw.sort_by_key(|r| r.2);

    let len = raw.len();
    raw.iter()
        .enumerate()
        .map(|(i, (kind, prefix, index))| ClassScope {
            kind: *kind,
            prefix: prefix.clone(),
            start: *index,
            end: if i + 1 < len {
                raw[i + 1].2
            } else {
                safe.len()
            },
        })
        .collect()
}

fn scope_for(scopes: &[ClassScope], index: usize) -> Option<&ClassScope> {
    scopes.iter().find(|s| index >= s.start && index < s.end)
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

/// First string literal anywhere in the args, or '' (covers `'x'`, `{ k: 'x' }`).
fn parse_string_arg(args: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"['"`]([^'"`]*)['"`]"#).unwrap());
    RE.captures(args)
        .map(|c| c[1].to_string())
        .unwrap_or_default()
}

/// `@Controller('users')` | `@Controller({ path: 'users', host })` | `@Controller(['a','b'])` | `@Controller()`.
fn parse_controller_prefix(args: &str) -> String {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"path\s*:\s*['"`]([^'"`]*)['"`]"#).unwrap());
    if let Some(c) = RE.captures(args) {
        return c[1].to_string();
    }
    parse_string_arg(args)
}

/// `@WebSocketGateway({ namespace: 'chat' })` | `@WebSocketGateway(81, { namespace: '/chat' })` | `@WebSocketGateway()`.
fn parse_gateway_namespace(args: &str) -> String {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"namespace\s*:\s*['"`]([^'"`]*)['"`]"#).unwrap());
    RE.captures(args)
        .map(|c| c[1].to_string())
        .unwrap_or_default()
}

/// GraphQL operation name. Prefers an explicit `{ name: 'x' }` or a leading
/// string literal (`@Query('users')`); otherwise the field name defaults to the
/// handler method name. Avoids mistaking a `description` string for the name.
fn parse_graphql_name(args: &str, handler: Option<&str>) -> String {
    static NAMED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"name\s*:\s*['"`]([^'"`]*)['"`]"#).unwrap());
    static LEAD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"^\s*['"`]([^'"`]*)['"`]"#).unwrap());
    if let Some(c) = NAMED.captures(args) {
        return c[1].to_string();
    }
    if let Some(c) = LEAD.captures(args) {
        return c[1].to_string();
    }
    handler.unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Join a controller prefix and method path into a single normalised `/path`.
fn join_http_path(prefix: &str, sub: &str) -> String {
    let parts: Vec<String> = [prefix, sub]
        .iter()
        .map(|p| p.trim().trim_matches('/').to_string())
        .filter(|p| !p.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

fn line_at(safe: &str, index: usize) -> u32 {
    (safe[..index].matches('\n').count() + 1) as u32
}

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

/// TS `/\.(m?js|tsx?|cjs)$/`.
fn has_js_extension(file_path: &str) -> bool {
    [".js", ".mjs", ".cjs", ".ts", ".tsx"]
        .iter()
        .any(|ext| file_path.ends_with(ext))
}

// ---------------------------------------------------------------------------
// RouterModule + @Module walkers (used by post_extract above)
// ---------------------------------------------------------------------------

/// Walk every `RouterModule.register([...])` call (and the equivalent
/// `RouterModule.forRoot([...])` and `forChild([...])` aliases) and populate
/// `out` with `Module → /full/prefix`. Recursive `children` arrays inherit
/// their parent's prefix.
///
/// First-write-wins: if the same module appears in two registrations we keep
/// the first prefix seen rather than overwriting. NestJS itself does the same.
fn collect_router_module_registrations(safe: &str, out: &mut Vec<(String, String)>) {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\bRouterModule\s*\.\s*(?:register|forRoot|forChild)\s*\(").unwrap()
    });
    let mut pos = 0usize;
    while pos <= safe.len() {
        let Some(m) = RE.find_at(safe, pos) else {
            break;
        };
        let open_index = m.end() - 1;
        match read_args(safe, open_index) {
            Some((args, end)) => {
                let items = parse_routes_array(&args);
                walk_routes_tree(&items, "", out);
                pos = end;
            }
            None => {
                pos = m.end();
            }
        }
    }
}

#[derive(Debug, Clone)]
struct RouteItem {
    path: String,
    module_name: Option<String>,
    children: Vec<RouteItem>,
}

/// Parse a `[ {...}, {...} ]` argument list into a list of `RouteItem`s. The
/// args are expected to be an inline literal — references to a `const routes:
/// Routes = [...]` declared earlier in the file aren't followed (rare in
/// practice; the registration is usually inline).
fn parse_routes_array(args: &str) -> Vec<RouteItem> {
    let trimmed = args.trim();
    if !trimmed.starts_with('[') {
        return Vec::new();
    }
    // Strip outer [ ... ] respecting balanced brackets.
    let Some(close) = matching_close(trimmed, 0) else {
        return Vec::new();
    };
    parse_route_objects(&trimmed[1..close])
}

fn parse_route_objects(s: &str) -> Vec<RouteItem> {
    // Recursion guard — nested `children` arrays drive depth.
    crate::ensure_sufficient_stack(|| parse_route_objects_inner(s))
}

fn parse_route_objects_inner(s: &str) -> Vec<RouteItem> {
    let mut items = Vec::new();
    for obj in split_top_level_objects(s) {
        let path = parse_string_field(&obj, &PATH_FIELD_RE);
        let module_name = parse_ident_field(&obj, &MODULE_FIELD_RE);
        let children_str = parse_array_field(&obj, &CHILDREN_FIELD_RE);
        let children = children_str
            .map(|c| parse_route_objects(&c))
            .unwrap_or_default();
        items.push(RouteItem {
            path,
            module_name,
            children,
        });
    }
    items
}

fn walk_routes_tree(items: &[RouteItem], parent_prefix: &str, out: &mut Vec<(String, String)>) {
    // Recursion guard — nested `children` arrays drive depth.
    crate::ensure_sufficient_stack(|| walk_routes_tree_inner(items, parent_prefix, out));
}

fn walk_routes_tree_inner(
    items: &[RouteItem],
    parent_prefix: &str,
    out: &mut Vec<(String, String)>,
) {
    for item in items {
        let my_prefix = join_http_path(parent_prefix, &item.path);
        if let Some(module_name) = &item.module_name {
            if !out.iter().any(|(m, _)| m == module_name) {
                out.push((module_name.clone(), my_prefix.clone()));
            }
        }
        if !item.children.is_empty() {
            walk_routes_tree(&item.children, &my_prefix, out);
        }
    }
}

/// Walk every `@Module(...)` decorator and populate `out` with
/// `Controller → enclosingModuleClassName`, based on the decorator's
/// `controllers: [...]` field and the class declaration that follows the
/// decorator (skipping stacked decorators and export/default/abstract
/// modifiers).
fn collect_module_controllers(safe: &str, out: &mut Vec<(String, String)>) {
    for hit in find_decorators(safe, &["Module"]) {
        let Some(class_name) = class_name_after(safe, hit.end) else {
            continue;
        };
        for controller in parse_controllers_field(&hit.args) {
            // First-write-wins, same as RouterModule, so a controller listed in two
            // modules picks up the one declared earliest in source.
            if !out.iter().any(|(c, _)| c == &controller) {
                out.push((controller, class_name.clone()));
            }
        }
    }
}

fn parse_controllers_field(args: &str) -> Vec<String> {
    static IDENT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*$").unwrap());
    let Some(inner) = parse_array_field(args, &CONTROLLERS_FIELD_RE) else {
        return Vec::new();
    };
    inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| IDENT_RE.is_match(s))
        .collect()
}

/// Starting just after a class decorator's `)`, return the name of the class
/// it decorates. Mirrors `method_name_after` for methods: skips stacked
/// decorators and `export`/`default`/`abstract` modifiers.
fn class_name_after(safe: &str, start: usize) -> Option<String> {
    static DECO_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^@[A-Za-z0-9_.]+").unwrap());
    static CLASS_DECL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][A-Za-z0-9_$]*)",
        )
        .unwrap()
    });

    let mut i = start;

    loop {
        eat_ws(safe, &mut i);
        if safe.as_bytes().get(i) != Some(&b'@') {
            break;
        }
        let Some(m) = DECO_NAME.find(&safe[i..]) else {
            break;
        };
        i += m.end();
        eat_ws(safe, &mut i);
        if safe.as_bytes().get(i) == Some(&b'(') {
            let (_, end) = read_args(safe, i)?;
            i = end;
        }
    }

    eat_ws(safe, &mut i);
    CLASS_DECL.captures(&safe[i..]).map(|c| c[1].to_string())
}

/// Recompute a route node's `name` by prepending `prefix` to the *original*
/// in-file path. The original is recovered from `qualified_name`, which the
/// per-file extract emits as `{filePath}::{method}:{path}` and which this
/// pass deliberately never mutates — that's what keeps the update idempotent.
fn apply_module_prefix(route: &Node, prefix: &str) -> Option<Node> {
    let sep = "::";
    let idx = route.qualified_name.find(sep)?;
    let tail = &route.qualified_name[idx + sep.len()..];
    let colon = tail.find(':')?;
    let method = &tail[..colon];
    let original = &tail[colon + 1..];
    let new_name = format!("{method} {}", join_http_path(prefix, original));
    let mut updated = route.clone();
    updated.name = new_name;
    updated.updated_at = now_ms();
    Some(updated)
}

// ---------------------------------------------------------------------------
// Small string utilities (object/array literal splitters)
// ---------------------------------------------------------------------------

/// Return the byte index of the bracket that closes the one at `open`, or None.
fn matching_close(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let opener = *b.get(open)?;
    if opener != b'[' && opener != b'{' && opener != b'(' {
        return None;
    }
    let mut depth: i64 = 0;
    let mut in_str: Option<u8> = None;
    let mut i = open;
    while i < b.len() {
        let ch = b[i];
        if let Some(q) = in_str {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            in_str = Some(ch);
            i += 1;
            continue;
        }
        if ch == b'{' || ch == b'[' || ch == b'(' {
            depth += 1;
        } else if ch == b'}' || ch == b']' || ch == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Split `s` into the contents of each top-level object literal. Brackets and
/// string literals are balanced so nested arrays/objects/strings inside an
/// object don't cause an early split.
fn split_top_level_objects(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut depth: i64 = 0;
    let mut obj_start: Option<usize> = None;
    let mut in_str: Option<u8> = None;
    let mut i = 0usize;
    while i < b.len() {
        let ch = b[i];
        if let Some(q) = in_str {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            in_str = Some(ch);
            i += 1;
            continue;
        }
        if depth == 0 && ch == b'{' {
            depth = 1;
            obj_start = Some(i);
            i += 1;
            continue;
        }
        if ch == b'{' || ch == b'[' || ch == b'(' {
            depth += 1;
        } else if ch == b'}' || ch == b']' || ch == b')' {
            depth -= 1;
            if depth == 0 {
                if let Some(start) = obj_start {
                    if ch == b'}' {
                        out.push(s[start + 1..i].to_string());
                        obj_start = None;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

// Field-reader regexes (TS built these per call from the field name; the only
// names ever used are fixed, so they're pre-compiled statics here).
static PATH_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:^|[,\{\s])path\s*:\s*['"`]([^'"`]*)['"`]"#).unwrap());
static MODULE_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[,\{\s])module\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)").unwrap());
static CHILDREN_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[,\{\s])children\s*:\s*\[").unwrap());
static CONTROLLERS_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[,\{\s])controllers\s*:\s*\[").unwrap());

/// Read a string-valued field — `key: 'value'` — out of one object literal's
/// body. Returns `''` if not present. The leading character class guards
/// against matching a field whose name *contains* the target as a suffix.
fn parse_string_field(obj: &str, re: &Regex) -> String {
    re.captures(obj)
        .map(|c| c[1].to_string())
        .unwrap_or_default()
}

/// Read an identifier-valued field — `key: SomeIdent` — out of one object body.
fn parse_ident_field(obj: &str, re: &Regex) -> Option<String> {
    re.captures(obj).map(|c| c[1].to_string())
}

/// Read an array-valued field — `key: [ ... ]` — as the raw inner text.
fn parse_array_field(obj: &str, re: &Regex) -> Option<String> {
    let m = re.find(obj)?;
    let open = m.end() - 1;
    let close = matching_close(obj, open)?;
    Some(obj[open + 1..close].to_string())
}
