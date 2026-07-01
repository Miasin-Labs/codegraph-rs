//! Laravel Framework Resolver
//!
//! Handles Laravel-specific patterns for reference resolution.
//! Ported from `src/resolution/frameworks/laravel.ts`.

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

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn line_of(content: &str, idx: usize) -> u32 {
    content[..idx].matches('\n').count() as u32 + 1
}

/// Laravel facade mappings to underlying classes.
/// Exported for potential use in facade resolution.
pub static FACADE_MAPPINGS: &[(&str, &str)] = &[
    ("Auth", "Illuminate\\Auth\\AuthManager"),
    ("Cache", "Illuminate\\Cache\\CacheManager"),
    ("Config", "Illuminate\\Config\\Repository"),
    ("DB", "Illuminate\\Database\\DatabaseManager"),
    ("Event", "Illuminate\\Events\\Dispatcher"),
    ("File", "Illuminate\\Filesystem\\Filesystem"),
    ("Gate", "Illuminate\\Auth\\Access\\Gate"),
    ("Hash", "Illuminate\\Hashing\\HashManager"),
    ("Log", "Illuminate\\Log\\LogManager"),
    ("Mail", "Illuminate\\Mail\\Mailer"),
    ("Queue", "Illuminate\\Queue\\QueueManager"),
    ("Redis", "Illuminate\\Redis\\RedisManager"),
    ("Request", "Illuminate\\Http\\Request"),
    ("Response", "Illuminate\\Http\\Response"),
    ("Route", "Illuminate\\Routing\\Router"),
    ("Session", "Illuminate\\Session\\SessionManager"),
    ("Storage", "Illuminate\\Filesystem\\FilesystemManager"),
    ("URL", "Illuminate\\Routing\\UrlGenerator"),
    ("Validator", "Illuminate\\Validation\\Factory"),
    ("View", "Illuminate\\View\\Factory"),
];

static CONTROLLER_AT_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*Controller@\w+$").unwrap());
static MODEL_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Z][a-zA-Z]+)::(\w+)$").unwrap());
static FACADE_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(Auth|Cache|DB|Log|Mail|Queue|Session|Storage|Validator|Route|Request|Response)::(\w+)$",
    )
    .unwrap()
});
static CONTROLLER_METHOD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Z][a-zA-Z]+Controller)@(\w+)$").unwrap());

// Route::METHOD('/path', handler-expr)
// handler-expr can be: [Class::class, 'method'] | 'Controller@method' | Closure | Class::class
static ROUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"Route::(get|post|put|patch|delete|options|any)\s*\(\s*['"]([^'"]+)['"]\s*,\s*([^)]+)\)"#,
    )
    .unwrap()
});

// Route::resource('name', Controller::class) / Route::apiResource('name', Controller::class)
static RESOURCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"Route::(resource|apiResource)\s*\(\s*['"]([^'"]+)['"]\s*(?:,\s*([^)]+))?\)"#)
        .unwrap()
});

const LARAVEL_HELPERS: &[&str] = &[
    "route", "view", "config", "env", "app", "abort", "redirect", "response", "request", "session",
    "url", "asset", "mix",
];

/// Laravel framework resolver (TS `laravelResolver`).
pub struct LaravelResolver;

impl FrameworkResolver for LaravelResolver {
    fn name(&self) -> &str {
        "laravel"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Php])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for artisan file (Laravel signature)
        context.file_exists("artisan") || context.file_exists("app/Http/Kernel.php")
    }

    /// `Controller@method` route refs name no declared symbol, so resolveOne's
    /// pre-filter would drop them before resolve() runs (Pattern 4). Claim them —
    /// same hook the django ORM / Rails routing work needed.
    fn claims_reference(&self, name: &str) -> bool {
        CONTROLLER_AT_CLAIM_RE.is_match(name)
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();

        // Pattern 1: Model::method() - Eloquent static calls
        if let Some(model_match) = MODEL_CALL_RE.captures(name) {
            let class_name = &model_match[1];
            let method_name = &model_match[2];
            if let Some(result) = resolve_model_call(class_name, method_name, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 2: Facade calls - Auth::user(), Cache::get()
        if FACADE_CALL_RE.is_match(name) {
            // Facades typically resolve to external Laravel code
            // Mark as external but note the facade
            return None; // External, can't resolve to local node
        }

        // Pattern 3: Helper function calls - route(), view(), config()
        if LARAVEL_HELPERS.contains(&name) {
            // These are Laravel helpers - external
            return None;
        }

        // Pattern 4: Controller method references
        if let Some(controller_match) = CONTROLLER_METHOD_RE.captures(name) {
            let controller = &controller_match[1];
            let method = &controller_match[2];
            if let Some(result) = resolve_controller_method(controller, method, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !file_path.ends_with(".php") {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_millis();
        let safe = strip_comments_for_regex(content, CommentLang::Php);

        for m in ROUTE_RE.captures_iter(&safe) {
            let method = m.get(1).unwrap().as_str();
            let route_path = m.get(2).unwrap().as_str();
            let handler_expr = m.get(3).unwrap().as_str();
            let whole = m.get(0).unwrap();
            let line = line_of(&safe, whole.start());
            let upper = method.to_uppercase();
            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{upper}:{route_path}"),
                NodeKind::Route,
                format!("{upper} {route_path}"),
                format!("{file_path}::route:{route_path}"),
                file_path,
                Language::Php,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);

            if let Some(handler_name) = extract_laravel_handler(handler_expr) {
                references.push(UnresolvedRef {
                    from_node_id: route_id,
                    reference_name: handler_name,
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: Language::Php,
                    candidates: None,
                    metadata: None,
                });
            }
        }

        for m in RESOURCE_RE.captures_iter(&safe) {
            let resource_name = m.get(2).unwrap().as_str();
            let handler_expr = m.get(3).map(|g| g.as_str());
            let whole = m.get(0).unwrap();
            let line = line_of(&safe, whole.start());
            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:RESOURCE:{resource_name}"),
                NodeKind::Route,
                format!("resource:{resource_name}"),
                format!("{file_path}::route:{resource_name}"),
                file_path,
                Language::Php,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);

            if let Some(handler_expr) = handler_expr {
                if let Some(controller_name) = extract_laravel_handler(handler_expr) {
                    references.push(UnresolvedRef {
                        from_node_id: route_id,
                        reference_name: controller_name,
                        reference_kind: EdgeKind::Imports,
                        line,
                        column: 0,
                        file_path: file_path.to_string(),
                        language: Language::Php,
                        candidates: None,
                        metadata: None,
                    });
                }
            }
        }

        Some(FrameworkExtractionResult { nodes, references })
    }
}

static TUPLE_HANDLER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\[\s*([A-Za-z_\\][\w\\]*)::class\s*,\s*['"]([^'"]+)['"]\s*\]"#).unwrap()
});
static AT_HANDLER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^['"]([^'"@]+)@([^'"]+)['"]$"#).unwrap());
static CLASS_HANDLER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z_\\][\w\\]*)::class").unwrap());

/// Parse a Laravel route handler expression and return the symbol to link.
///  - `[Class::class, 'method']`  -> `method`
///  - `'Controller@method'`       -> `method`
///  - `Class::class`              -> `Class`
///  - anything else (closure etc) -> None
fn extract_laravel_handler(expr: &str) -> Option<String> {
    let trimmed = expr.trim();
    // strip namespace
    let short = |s: &str| s.rsplit('\\').next().unwrap_or(s).to_string();

    // [Class::class, 'method'] → `Class@method` (PRECISE — keep the controller, so
    // common action names like `index`/`show` resolve to the RIGHT controller, not
    // whichever one name-matching happens to pick first).
    if let Some(tuple_match) = TUPLE_HANDLER_RE.captures(trimmed) {
        return Some(format!("{}@{}", short(&tuple_match[1]), &tuple_match[2]));
    }

    // 'Controller@method' (possibly namespaced) → `Controller@method`
    if let Some(at_match) = AT_HANDLER_RE.captures(trimmed) {
        return Some(format!("{}@{}", short(&at_match[1]), &at_match[2]));
    }

    // Class::class (Route::resource controller) → `Class`
    if let Some(class_match) = CLASS_HANDLER_RE.captures(trimmed) {
        return Some(short(&class_match[1]));
    }

    None
}

/// Resolve a Model::method() call
fn resolve_model_call(
    class_name: &str,
    method_name: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    // Try app/Models/ first (Laravel 8+)
    let model_path = format!("app/Models/{class_name}.php");
    if context.file_exists(&model_path) {
        let nodes = context.get_nodes_in_file(&model_path);
        // Look for the method in this class
        if let Some(method_node) = nodes
            .iter()
            .find(|n| n.kind == NodeKind::Method && n.name == method_name)
        {
            return Some(method_node.id.clone());
        }
        // Return the class itself if method not found
        if let Some(class_node) = nodes
            .iter()
            .find(|n| n.kind == NodeKind::Class && n.name == class_name)
        {
            return Some(class_node.id.clone());
        }
    }

    // Try app/ (Laravel 7 and below)
    let model_path = format!("app/{class_name}.php");
    if context.file_exists(&model_path) {
        let nodes = context.get_nodes_in_file(&model_path);
        if let Some(method_node) = nodes
            .iter()
            .find(|n| n.kind == NodeKind::Method && n.name == method_name)
        {
            return Some(method_node.id.clone());
        }
        if let Some(class_node) = nodes
            .iter()
            .find(|n| n.kind == NodeKind::Class && n.name == class_name)
        {
            return Some(class_node.id.clone());
        }
    }

    None
}

/// Resolve a Controller@method reference
fn resolve_controller_method(
    controller: &str,
    method: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    // Try app/Http/Controllers/
    let controller_path = format!("app/Http/Controllers/{controller}.php");
    if context.file_exists(&controller_path) {
        let nodes = context.get_nodes_in_file(&controller_path);
        if let Some(method_node) = nodes
            .iter()
            .find(|n| n.kind == NodeKind::Method && n.name == method)
        {
            return Some(method_node.id.clone());
        }
    }

    // Try name-based lookup for namespaced controllers
    let controller_candidates = context.get_nodes_by_name(controller);
    for ctrl in &controller_candidates {
        if ctrl.kind == NodeKind::Class && ctrl.file_path.contains("Controllers") {
            let nodes_in_file = context.get_nodes_in_file(&ctrl.file_path);
            if let Some(method_node) = nodes_in_file
                .iter()
                .find(|n| n.kind == NodeKind::Method && n.name == method)
            {
                return Some(method_node.id.clone());
            }
        }
    }

    None
}
