//! Go Framework Resolver
//!
//! Handles Gin, Echo, Fiber, Chi, and standard library patterns.
//!
//! Ported from `src/resolution/frameworks/go.ts`.

use std::sync::LazyLock;

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

// Directory patterns for framework resolution
const HANDLER_DIRS: &[&str] = &[
    "handler",
    "handlers",
    "api",
    "routes",
    "controller",
    "controllers",
];
const SERVICE_DIRS: &[&str] = &["service", "services", "repository", "store", "pkg"];
const MIDDLEWARE_DIRS: &[&str] = &["middleware", "middlewares"];
const MODEL_DIRS: &[&str] = &["model", "models", "entity", "entities", "domain", "pkg"];
const SERVICE_KINDS: &[NodeKind] = &[NodeKind::Struct, NodeKind::Interface];

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Number of the line containing byte offset `idx` (1-based) — TS
/// `safe.slice(0, idx).split('\n').length`.
fn line_at(safe: &str, idx: usize) -> u32 {
    (safe[..idx].matches('\n').count() + 1) as u32
}

/// Extract the last identifier from an expression like `pkg.Sub.handler` or `handler`.
fn extract_go_tail_ident(expr: &str) -> Option<String> {
    static TAIL_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$").unwrap());
    // TS: expr.trim().replace(/\s+/g, '').replace(/\(\)$/, '')
    let cleaned: String = expr.trim().chars().filter(|c| !c.is_whitespace()).collect();
    let cleaned = cleaned.strip_suffix("()").unwrap_or(&cleaned);
    TAIL_RE
        .captures(cleaned)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Resolve a symbol by name using indexed queries instead of scanning all files.
/// Uses get_nodes_by_name (O(log n) indexed lookup) instead of iterating every file.
fn resolve_by_name_and_kind(
    name: &str,
    kind: Option<NodeKind>,
    preferred_dirs: &[&str],
    context: &dyn ResolutionContext,
    kinds: Option<&[NodeKind]>,
) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    if candidates.is_empty() {
        return None;
    }

    // Filter by kind
    let kind_filtered: Vec<&Node> = candidates
        .iter()
        .filter(|n| {
            if let Some(kinds) = kinds {
                return kinds.contains(&n.kind);
            }
            if let Some(kind) = kind {
                return n.kind == kind;
            }
            true
        })
        .collect();

    if kind_filtered.is_empty() {
        return None;
    }

    // Prefer candidates in framework-conventional directories
    let preferred: Vec<&&Node> = kind_filtered
        .iter()
        .filter(|n| {
            preferred_dirs
                .iter()
                .any(|d| n.file_path.contains(&format!("/{d}/")))
        })
        .collect();

    if let Some(first) = preferred.first() {
        return Some(first.id.clone());
    }

    // Fall back to any match
    Some(kind_filtered[0].id.clone())
}

/// TS `goResolver` (name: `"go"`).
#[derive(Debug, Default)]
pub struct GoResolver;

impl FrameworkResolver for GoResolver {
    fn name(&self) -> &str {
        "go"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Go])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for go.mod file (Go modules)
        if let Some(go_mod) = context.read_file("go.mod") {
            // TS truthiness: empty string is falsy.
            if !go_mod.is_empty() {
                return true;
            }
        }

        // Check for .go files
        context.get_all_files().iter().any(|f| f.ends_with(".go"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        static PASCAL_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[A-Z][a-zA-Z]+$").unwrap());

        // Pattern 1: Handler references
        if reference.reference_name.ends_with("Handler")
            || reference.reference_name.starts_with("Handle")
        {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                Some(NodeKind::Function),
                HANDLER_DIRS,
                context,
                None,
            ) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 2: Service/Repository references
        if reference.reference_name.ends_with("Service")
            || reference.reference_name.ends_with("Repository")
            || reference.reference_name.ends_with("Store")
        {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                None,
                SERVICE_DIRS,
                context,
                Some(SERVICE_KINDS),
            ) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 3: Middleware references
        if reference.reference_name.ends_with("Middleware")
            || reference.reference_name.starts_with("Auth")
            || reference.reference_name.starts_with("Log")
        {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                Some(NodeKind::Function),
                MIDDLEWARE_DIRS,
                context,
                None,
            ) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.75,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 4: Model/Entity references (typically PascalCase structs)
        if PASCAL_RE.is_match(&reference.reference_name) {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                Some(NodeKind::Struct),
                MODEL_DIRS,
                context,
                None,
            ) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.7,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !file_path.ends_with(".go") {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_millis();
        let safe = strip_comments_for_regex(content, CommentLang::Go);

        // <anyVar>.METHOD("/path", handler) — Gin (GET/POST/...), Chi (Get/Post/...),
        // net/http (HandleFunc/Handle). The receiver is ANY identifier, not just
        // router|r|mux|app|e: real apps route on GROUP vars (`v1.GET`, `PublicGroup.GET`,
        // `userRouter.POST`), which the fixed name list missed (gin-vue-admin: 4 routes
        // for 625 files). The verb + string-path + handler-arg gates keep it route-specific.
        static ROUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r#"\b\w+\.(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Get|Post|Put|Patch|Delete|Handle|HandleFunc)\s*\(\s*"([^"]+)"\s*,\s*([^)]+)\)"#,
            )
            .unwrap()
        });

        for caps in ROUTE_RE.captures_iter(&safe) {
            let whole = caps.get(0).unwrap();
            let raw_method = caps.get(1).unwrap().as_str();
            let route_path = caps.get(2).unwrap().as_str();
            let handler_expr = caps.get(3).unwrap().as_str();
            let line = line_at(&safe, whole.start());
            let method = if raw_method == "Handle" || raw_method == "HandleFunc" {
                "ANY".to_string()
            } else {
                raw_method.to_uppercase()
            };

            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{method}:{route_path}"),
                NodeKind::Route,
                format!("{method} {route_path}"),
                format!("{file_path}::route:{route_path}"),
                file_path,
                Language::Go,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_node_id = route_node.id.clone();
            nodes.push(route_node);

            if let Some(handler_name) = extract_go_tail_ident(handler_expr) {
                references.push(UnresolvedRef {
                    from_node_id: route_node_id,
                    reference_name: handler_name,
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: Language::Go,
                    candidates: None,
                });
            }
        }

        Some(FrameworkExtractionResult { nodes, references })
    }
}
