//! Python Framework Resolver
//!
//! Handles Django, Flask, and FastAPI patterns.
//! Ported from `src/resolution/frameworks/python.ts`.

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

/// 1-based line of a byte offset (TS `content.slice(0, idx).split('\n').length`).
fn line_of(content: &str, idx: usize) -> u32 {
    content[..idx].matches('\n').count() as u32 + 1
}

// =============================================================================
// django
// =============================================================================

/// Django framework resolver (TS `djangoResolver`).
pub struct DjangoResolver;

static DJANGO_SINGLE_CAP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][a-z]+$").unwrap());

// path('url', handler, name=...) / re_path(r'...', handler) / url(r'...', handler)
// Capture groups: 1=function name, 2=url string, 3=handler expr
// Handler expr may contain one balanced () pair (e.g. View.as_view(), include('x.y'))
static DJANGO_ROUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(path|re_path|url)\s*\(\s*r?['"]([^'"]+)['"]\s*,\s*([\w.]+(?:\s*\([^)]*\))?)"#)
        .unwrap()
});

// DRF router registration: `router.register(r'articles', ArticleViewSet)`
static DJANGO_ROUTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\.register\s*\(\s*r?['"]([^'"]+)['"]\s*,\s*([\w.]+)"#).unwrap());
static DJANGO_PREFIX_TRIM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\^|/?\$$").unwrap());
static DJANGO_VIEWSET_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"View(Set)?$").unwrap());

impl FrameworkResolver for DjangoResolver {
    fn name(&self) -> &str {
        "django"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Python])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if let Some(requirements) = context.read_file("requirements.txt") {
            if requirements.to_lowercase().contains("django") {
                return true;
            }
        }
        if let Some(setup) = context.read_file("setup.py") {
            if setup.to_lowercase().contains("django") {
                return true;
            }
        }
        if let Some(pyproject) = context.read_file("pyproject.toml") {
            if pyproject.to_lowercase().contains("django") {
                return true;
            }
        }
        context.file_exists("manage.py")
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        if name.ends_with("Model") || DJANGO_SINGLE_CAP_RE.is_match(name) {
            if let Some(result) = resolve_by_name_and_kind(name, CLASS_KINDS, MODEL_DIRS, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }
        if name.ends_with("View") || name.ends_with("ViewSet") {
            if let Some(result) = resolve_by_name_and_kind(name, VIEW_KINDS, VIEW_DIRS, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }
        if name.ends_with("Form") {
            if let Some(result) = resolve_by_name_and_kind(name, CLASS_KINDS, FORM_DIRS, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }
        // ORM dynamic dispatch: QuerySet._fetch_all (and siblings) call
        // `self._iterable_class(self)` — a runtime dispatch to the iterable class
        // (default ModelIterable) whose __iter__ runs the SQL compiler. Static
        // parsing can't resolve an attribute-as-callable, so it leaves an unresolved
        // `_iterable_class` ref and a hole in the QuerySet→compiler chain. Bridge it
        // to ModelIterable.__iter__ so the flow actually exists in the graph.
        if name == "_iterable_class" {
            if let Some(target) = resolve_model_iterable_iter(context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: target,
                    confidence: 0.7,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }
        None
    }

    /// Let the ORM dynamic-dispatch ref reach resolve() despite no symbol being
    /// named `_iterable_class` (it's a QuerySet attribute, not a declared method).
    fn claims_reference(&self, name: &str) -> bool {
        name == "_iterable_class"
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !file_path.ends_with(".py") {
            return Some(FrameworkExtractionResult::default());
        }

        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_millis();
        let safe = strip_comments_for_regex(content, CommentLang::Python);

        for m in DJANGO_ROUTE_RE.captures_iter(&safe) {
            let url_path = m.get(2).unwrap().as_str();
            let handler_expr = m.get(3).unwrap().as_str();
            let whole = m.get(0).unwrap();
            let line = line_of(&safe, whole.start());

            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{url_path}"),
                NodeKind::Route,
                url_path,
                format!("{file_path}::route:{url_path}"),
                file_path,
                Language::Python,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);

            if let Some((target_name, target_kind)) = resolve_handler_name(handler_expr.trim()) {
                references.push(UnresolvedRef {
                    from_node_id: route_id,
                    reference_name: target_name,
                    reference_kind: target_kind,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: Language::Python,
                    candidates: None,
                    metadata: None,
                });
            }
        }

        // DRF router registration: `router.register(r'articles', ArticleViewSet)` →
        // route → the ViewSet class (the core CRUD endpoints, which path()/url() miss).
        // The STRING first arg separates this from `admin.site.register(Model, Admin)`
        // (whose first arg is a model class, not a string); the View/ViewSet suffix on
        // the 2nd arg keeps it to DRF viewsets.
        for m in DJANGO_ROUTER_RE.captures_iter(&safe) {
            let prefix = DJANGO_PREFIX_TRIM_RE.replace_all(m.get(1).unwrap().as_str(), "");
            let viewset = m.get(2).unwrap().as_str().split('.').next_back().unwrap();
            if !DJANGO_VIEWSET_RE.is_match(viewset) {
                continue;
            }
            let whole = m.get(0).unwrap();
            let line = line_of(&safe, whole.start());
            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:VIEWSET:{prefix}"),
                NodeKind::Route,
                format!("VIEWSET /{prefix}"),
                format!("{file_path}::route:{prefix}"),
                file_path,
                Language::Python,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);
            references.push(UnresolvedRef {
                from_node_id: route_id,
                reference_name: viewset.to_string(),
                reference_kind: EdgeKind::References,
                line,
                column: 0,
                file_path: file_path.to_string(),
                language: Language::Python,
                candidates: None,
                metadata: None,
            });
        }

        Some(FrameworkExtractionResult { nodes, references })
    }
}

/// Find ModelIterable.__iter__ — the default iterable QuerySet invokes via
/// `self._iterable_class(self)`. Its __iter__ statically calls the SQL compiler,
/// so linking the dynamic dispatch here closes the QuerySet→SQL call chain.
/// (Over-approximates to the default iterable; .values()/.values_list() swap in
/// other BaseIterable subclasses, but ModelIterable is the canonical path.)
fn resolve_model_iterable_iter(context: &dyn ResolutionContext) -> Option<String> {
    let cls = context
        .get_nodes_by_name("ModelIterable")
        .into_iter()
        .find(|n| n.kind == NodeKind::Class)?;
    let iter = context
        .get_nodes_by_name("__iter__")
        .into_iter()
        .find(|n| {
            n.file_path == cls.file_path
                && n.start_line >= cls.start_line
                && n.start_line <= cls.end_line
        })?;
    Some(iter.id)
}

static INCLUDE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^include\s*\(\s*['"]([^'"]+)['"]"#).unwrap());
static AS_VIEW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.as_view\s*\([^)]*\)\s*$").unwrap());
static TRAILING_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.\w+\s*\([^)]*\)\s*$").unwrap());
static PY_IDENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap());

/// Parse a Django URL handler expression and return the symbol/module to link.
/// Returns None for shapes we can't confidently link (e.g. lambdas).
fn resolve_handler_name(expr: &str) -> Option<(String, EdgeKind)> {
    // include('module.path')
    if let Some(include_match) = INCLUDE_RE.captures(expr) {
        return Some((include_match[1].to_string(), EdgeKind::Imports));
    }

    // Strip trailing .as_view(...) or .as_view()
    let head = AS_VIEW_RE.replace(expr, "");
    // Drop any other trailing method call
    let head = TRAILING_CALL_RE.replace(&head, "");

    let dotted: Vec<&str> = head.split('.').filter(|s| !s.is_empty()).collect();
    let last = *dotted.last()?;
    if !PY_IDENT_RE.is_match(last) {
        return None;
    }

    Some((last.to_string(), EdgeKind::References))
}

// =============================================================================
// flask
// =============================================================================

/// Flask framework resolver (TS `flaskResolver`).
pub struct FlaskResolver;

static FLASK_DEP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bflask\b").unwrap());
static FLASK_ENTRYPOINT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|/)(app|application|main|wsgi|__init__)\.py$").unwrap());
static FLASK_CALL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bFlask\s*\(").unwrap());
static FLASK_IMPORT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bimport\s+flask\b|\bfrom\s+flask\b").unwrap());

// Flask: @x.route('/path', methods=[...] | (...)) — the handler is the next
// `def`, allowing intervening decorators (@login_required) and stacked
// @x.route() lines. methods may be a list OR a tuple (methods=('GET',)).
static FLASK_DECORATOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"@(\w+)\.route\s*\(\s*['"]([^'"]*)['"](?:\s*,\s*methods\s*=\s*[\[(]([^\])]+)[\])])?\s*\)"#,
    )
    .unwrap()
});

impl FrameworkResolver for FlaskResolver {
    fn name(&self) -> &str {
        "flask"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Python])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        for f in ["requirements.txt", "pyproject.toml", "Pipfile", "setup.py"] {
            if let Some(c) = context.read_file(f) {
                if FLASK_DEP_RE.is_match(&c) {
                    return true;
                }
            }
        }
        // Any app entrypoint (root OR subdir, e.g. conduit/app.py) that imports flask
        // and instantiates Flask(...) — covers Flask(__name__), Flask(__name__.split…),
        // and the app-factory pattern. Bounded to entrypoint-named files.
        let entrypoints: Vec<String> = context
            .get_all_files()
            .into_iter()
            .filter(|f| FLASK_ENTRYPOINT_RE.is_match(f))
            .take(50)
            .collect();
        for f in &entrypoints {
            if let Some(c) = context.read_file(f) {
                if FLASK_CALL_RE.is_match(&c) && FLASK_IMPORT_RE.is_match(&c) {
                    return true;
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
        let name = reference.reference_name.as_str();
        if name.ends_with("_bp") || name.ends_with("_blueprint") {
            if let Some(result) = resolve_by_name_and_kind(name, VARIABLE_KINDS, &[], context) {
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
        if !file_path.ends_with(".py") {
            return Some(FrameworkExtractionResult::default());
        }
        let safe = strip_comments_for_regex(content, CommentLang::Python);
        let decorator = extract_decorator_routes(
            file_path,
            &safe,
            &DecoratorRouteOpts {
                decorator_regex: &FLASK_DECORATOR_RE,
                default_method: "GET",
                method_group: None,
                method_from_group: Some(3),
                path_group: 2,
                handler_group: None,
                find_handler: true,
                language: Language::Python,
            },
        );
        let restful = extract_flask_restful(file_path, &safe);
        Some(FrameworkExtractionResult {
            nodes: decorator.nodes.into_iter().chain(restful.nodes).collect(),
            references: decorator
                .references
                .into_iter()
                .chain(restful.references)
                .collect(),
        })
    }
}

// =============================================================================
// fastapi
// =============================================================================

/// FastAPI framework resolver (TS `fastapiResolver`).
pub struct FastapiResolver;

static FASTAPI_DEP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bfastapi\b").unwrap());

// FastAPI: @x.METHOD('/path') -> handler on the next def line. Path may be
// empty ("") for routes mounted at the router/prefix root.
static FASTAPI_DECORATOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"@(\w+)\.(get|post|put|patch|delete|options|head)\s*\(\s*['"]([^'"]*)['"]"#)
        .unwrap()
});

impl FrameworkResolver for FastapiResolver {
    fn name(&self) -> &str {
        "fastapi"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Python])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if let Some(requirements) = context.read_file("requirements.txt") {
            if FASTAPI_DEP_RE.is_match(&requirements) {
                return true;
            }
        }
        if let Some(pyproject) = context.read_file("pyproject.toml") {
            if FASTAPI_DEP_RE.is_match(&pyproject) {
                return true;
            }
        }
        for file in ["app.py", "main.py", "api.py"] {
            if let Some(content) = context.read_file(file) {
                if content.contains("FastAPI(") {
                    return true;
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
        let name = reference.reference_name.as_str();
        if name.ends_with("_router") || name == "router" {
            if let Some(result) =
                resolve_by_name_and_kind(name, VARIABLE_KINDS, ROUTER_DIRS, context)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }
        if name.starts_with("get_") || name.starts_with("Depends") {
            if let Some(result) = resolve_by_name_and_kind(name, FUNCTION_KINDS, DEP_DIRS, context)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.75,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }
        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !file_path.ends_with(".py") {
            return Some(FrameworkExtractionResult::default());
        }
        Some(extract_decorator_routes(
            file_path,
            &strip_comments_for_regex(content, CommentLang::Python),
            &DecoratorRouteOpts {
                decorator_regex: &FASTAPI_DECORATOR_RE,
                default_method: "",
                method_group: Some(2),
                method_from_group: None,
                path_group: 3,
                handler_group: None,
                find_handler: true,
                language: Language::Python,
            },
        ))
    }
}

// =============================================================================
// Decorator-route extraction (shared by flask/fastapi)
// =============================================================================

struct DecoratorRouteOpts<'a> {
    decorator_regex: &'a Regex,
    default_method: &'a str,
    method_group: Option<usize>,
    /// methods=[...] list
    method_from_group: Option<usize>,
    path_group: usize,
    handler_group: Option<usize>,
    find_handler: bool,
    language: Language,
}

static METHOD_WORD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)['"]([A-Z]+)['"]"#).unwrap());
static NEXT_DEF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n\s*(?:async\s+)?def\s+(\w+)").unwrap());

fn extract_decorator_routes(
    file_path: &str,
    content: &str,
    opts: &DecoratorRouteOpts<'_>,
) -> FrameworkExtractionResult {
    let mut nodes: Vec<Node> = Vec::new();
    let mut references: Vec<UnresolvedRef> = Vec::new();
    let now = now_millis();
    for m in opts.decorator_regex.captures_iter(content) {
        let route_path = m.get(opts.path_group).map(|g| g.as_str()).unwrap_or("");
        let mut method = opts.default_method.to_string();
        if let Some(g) = opts.method_group {
            if let Some(mg) = m.get(g).filter(|mg| !mg.as_str().is_empty()) {
                method = mg.as_str().to_uppercase();
            }
        } else if let Some(g) = opts.method_from_group {
            if let Some(mg) = m.get(g).filter(|mg| !mg.as_str().is_empty()) {
                if let Some(word) = METHOD_WORD_RE.captures(mg.as_str()) {
                    method = word[1].to_uppercase();
                }
            }
        }
        let whole = m.get(0).unwrap();
        let line = line_of(content, whole.start());
        let display_path = if route_path.is_empty() {
            "/"
        } else {
            route_path
        };
        let name = if method.is_empty() {
            display_path.to_string()
        } else {
            format!("{method} {display_path}")
        };
        let mut route_node = Node::new(
            format!("route:{file_path}:{line}:{method}:{route_path}"),
            NodeKind::Route,
            name,
            format!("{file_path}::{method}:{route_path}"),
            file_path,
            opts.language,
            line,
            line,
        );
        route_node.end_column = whole.as_str().len() as u32;
        route_node.updated_at = now;
        let route_id = route_node.id.clone();
        nodes.push(route_node);

        let handler_from_group: Option<String> = opts
            .handler_group
            .and_then(|g| m.get(g))
            .map(|h| h.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let handler_name: Option<String> = if handler_from_group.is_some() {
            handler_from_group
        } else if opts.find_handler {
            let tail = &content[whole.end()..];
            NEXT_DEF_RE.captures(tail).map(|dm| dm[1].to_string())
        } else {
            None
        };
        if let Some(handler_name) = handler_name {
            references.push(UnresolvedRef {
                from_node_id: route_id,
                reference_name: handler_name,
                reference_kind: EdgeKind::References,
                line,
                column: 0,
                file_path: file_path.to_string(),
                language: Language::Python,
                candidates: None,
                metadata: None,
            });
        }
    }
    FrameworkExtractionResult { nodes, references }
}

// =============================================================================
// Flask-RESTful
// =============================================================================

static FLASK_RESTFUL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.add\w*[Rr]esource\s*\(\s*(\w+)\s*,\s*((?:['"][^'"]+['"]\s*,?\s*)+)"#).unwrap()
});
static QUOTED_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"['"]([^'"]+)['"]"#).unwrap());

/// Flask-RESTful: `api.add_resource(ResourceClass, '/path'[, '/path2'])`
/// (and variants like redash's `add_org_resource`). The ResourceClass holds the
/// HTTP-verb methods (get/post/…), so the route references the class — its verb
/// methods resolve as the handlers via the class. Method is ANY (the class
/// decides which verbs it serves).
fn extract_flask_restful(file_path: &str, safe: &str) -> FrameworkExtractionResult {
    let mut nodes: Vec<Node> = Vec::new();
    let mut references: Vec<UnresolvedRef> = Vec::new();
    let now = now_millis();
    for m in FLASK_RESTFUL_RE.captures_iter(safe) {
        let class_name = m.get(1).unwrap().as_str();
        let paths: Vec<&str> = QUOTED_PATH_RE
            .captures_iter(m.get(2).unwrap().as_str())
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        let line = line_of(safe, m.get(0).unwrap().start());
        for route_path in paths {
            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:ANY:{route_path}"),
                NodeKind::Route,
                format!("ANY {route_path}"),
                format!("{file_path}::ANY:{route_path}"),
                file_path,
                Language::Python,
                line,
                line,
            );
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);
            references.push(UnresolvedRef {
                from_node_id: route_id,
                reference_name: class_name.to_string(),
                reference_kind: EdgeKind::References,
                line,
                column: 0,
                file_path: file_path.to_string(),
                language: Language::Python,
                candidates: None,
                metadata: None,
            });
        }
    }
    FrameworkExtractionResult { nodes, references }
}

// =============================================================================
// Directory / kind patterns + shared name lookup
// =============================================================================

const MODEL_DIRS: &[&str] = &["models", "app/models", "src/models"];
const VIEW_DIRS: &[&str] = &["views", "app/views", "src/views", "api/views"];
const FORM_DIRS: &[&str] = &["forms", "app/forms", "src/forms"];
const ROUTER_DIRS: &[&str] = &["/routers/", "/api/", "/routes/", "/endpoints/"];
const DEP_DIRS: &[&str] = &["/dependencies/", "/deps/", "/core/"];

const CLASS_KINDS: &[NodeKind] = &[NodeKind::Class];
const VIEW_KINDS: &[NodeKind] = &[NodeKind::Class, NodeKind::Function];
const VARIABLE_KINDS: &[NodeKind] = &[NodeKind::Variable];
const FUNCTION_KINDS: &[NodeKind] = &[NodeKind::Function];

/// Resolve a symbol by name using indexed queries instead of scanning all files.
fn resolve_by_name_and_kind(
    name: &str,
    kinds: &[NodeKind],
    preferred_dir_patterns: &[&str],
    context: &dyn ResolutionContext,
) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    if candidates.is_empty() {
        return None;
    }

    let kind_filtered: Vec<&Node> = candidates
        .iter()
        .filter(|n| kinds.contains(&n.kind))
        .collect();
    if kind_filtered.is_empty() {
        return None;
    }

    // Prefer candidates in framework-conventional directories
    if !preferred_dir_patterns.is_empty() {
        if let Some(preferred) = kind_filtered.iter().find(|n| {
            preferred_dir_patterns
                .iter()
                .any(|d| n.file_path.contains(d))
        }) {
            return Some(preferred.id.clone());
        }
    }

    // Fall back to any match
    Some(kind_filtered[0].id.clone())
}
