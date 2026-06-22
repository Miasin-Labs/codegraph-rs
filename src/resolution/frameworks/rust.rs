//! Rust Framework Resolver
//!
//! Handles Actix-web, Rocket, Axum, and common Rust patterns.
//!
//! Ported from `src/resolution/frameworks/rust.ts`. The TS per-context
//! `WeakMap` cache for the cargo workspace crate map becomes a
//! per-instance `Mutex` cache keyed by project root (object identity
//! doesn't exist for `&dyn ResolutionContext`); see
//! `notes/frameworks-systems.md`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use super::cargo_workspace::get_cargo_workspace_crate_map;
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

// Directory patterns
const HANDLER_DIRS: &[&str] = &[
    "/handlers/",
    "/handler/",
    "/api/",
    "/routes/",
    "/controllers/",
];
const SERVICE_DIRS: &[&str] = &["/services/", "/service/", "/repository/", "/domain/"];
const MODEL_DIRS: &[&str] = &[
    "/models/",
    "/model/",
    "/entities/",
    "/entity/",
    "/domain/",
    "/types/",
];

const FUNCTION_KINDS: &[NodeKind] = &[NodeKind::Function];
const SERVICE_KINDS: &[NodeKind] = &[NodeKind::Struct, NodeKind::Trait];
const STRUCT_KINDS: &[NodeKind] = &[NodeKind::Struct];

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

/// Index of the ')' that matches the '(' at `open_idx`, or `None` if unbalanced.
fn find_matching_paren(s: &str, open_idx: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth: i64 = 0;
    for (i, &b) in bytes.iter().enumerate().skip(open_idx) {
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

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
    let preferred: Vec<&&Node> = kind_filtered
        .iter()
        .filter(|n| {
            preferred_dir_patterns
                .iter()
                .any(|d| n.file_path.contains(d))
        })
        .collect();

    if let Some(first) = preferred.first() {
        return Some(first.id.clone());
    }

    // Fall back to any match
    Some(kind_filtered[0].id.clone())
}

struct ModuleResolution {
    target_id: String,
    from_workspace: bool,
}

/// TS `rustResolver` (name: `"rust"`).
///
/// Holds the cached cargo-workspace crate map (TS used a module-level
/// `WeakMap<ResolutionContext, Map>`; here it's per-instance, keyed by
/// project root so multiple projects sharing a process don't bleed maps).
#[derive(Debug, Default)]
pub struct RustResolver {
    workspace_cache: Mutex<HashMap<String, HashMap<String, String>>>,
}

impl RustResolver {
    pub fn new() -> Self {
        Self::default()
    }

    fn cached_workspace_crate_path(
        &self,
        name: &str,
        context: &dyn ResolutionContext,
    ) -> Option<String> {
        let root = context.get_project_root().to_string();
        let mut cache = self
            .workspace_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let map = cache
            .entry(root)
            .or_insert_with(|| get_cargo_workspace_crate_map(context));
        map.get(name).cloned()
    }

    fn resolve_module(
        &self,
        name: &str,
        context: &dyn ResolutionContext,
    ) -> Option<ModuleResolution> {
        // Rust modules can be either mod.rs in a directory or name.rs
        let local_paths = [format!("src/{name}.rs"), format!("src/{name}/mod.rs")];

        let crate_path = self.cached_workspace_crate_path(name, context);
        let workspace_paths: Vec<String> = match &crate_path {
            Some(p) => vec![format!("{p}/src/lib.rs"), format!("{p}/src/main.rs")],
            None => Vec::new(),
        };

        let candidates: Vec<(String, bool)> = local_paths
            .iter()
            .map(|p| (p.clone(), false))
            .chain(workspace_paths.iter().map(|p| (p.clone(), true)))
            .collect();

        for (mod_path, from_workspace) in candidates {
            if !context.file_exists(&mod_path) {
                continue;
            }
            let nodes = context.get_nodes_in_file(&mod_path);
            if let Some(mod_node) = nodes.iter().find(|n| n.kind == NodeKind::Module) {
                return Some(ModuleResolution {
                    target_id: mod_node.id.clone(),
                    from_workspace,
                });
            }
            if let Some(first) = nodes.first() {
                return Some(ModuleResolution {
                    target_id: first.id.clone(),
                    from_workspace,
                });
            }
        }

        None
    }
}

impl FrameworkResolver for RustResolver {
    fn name(&self) -> &str {
        "rust"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Rust])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for Cargo.toml (Rust project signature)
        context.file_exists("Cargo.toml")
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        static PASCAL_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[A-Z][a-zA-Z]+$").unwrap());
        static MODULE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z_]+$").unwrap());

        // Pattern 1: Handler references
        if reference.reference_name.ends_with("_handler")
            || reference.reference_name.starts_with("handle_")
        {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                FUNCTION_KINDS,
                HANDLER_DIRS,
                context,
            ) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 2: Service/Repository trait implementations
        if reference.reference_name.ends_with("Service")
            || reference.reference_name.ends_with("Repository")
        {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                SERVICE_KINDS,
                SERVICE_DIRS,
                context,
            ) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 3: Struct references (PascalCase)
        if PASCAL_RE.is_match(&reference.reference_name) {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                STRUCT_KINDS,
                MODEL_DIRS,
                context,
            ) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.7,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 4: Module references
        if MODULE_RE.is_match(&reference.reference_name) {
            if let Some(result) = self.resolve_module(&reference.reference_name, context) {
                // Workspace-manifest hits are an exact crate-name -> crate-root
                // mapping straight from Cargo.toml, so we trust them above
                // name-matcher self-file matches (which otherwise win at 0.7
                // because every file containing `use foo::...` has its own
                // import node named `foo`).
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result.target_id,
                    confidence: if result.from_workspace { 0.95 } else { 0.6 },
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !file_path.ends_with(".rs") {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_millis();
        let safe = strip_comments_for_regex(content, CommentLang::Rust);

        // Actix-web / Rocket attribute: #[get("/path")] fn handler(..)
        // Capture the method, path, and the fn identifier that follows.
        static ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r#"#\[(get|post|put|patch|delete|head|options)\s*\(\s*["']([^"']+)["'][^\]]*\)\]"#,
            )
            .unwrap()
        });
        static FN_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\n\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)").unwrap());

        for caps in ATTR_RE.captures_iter(&safe) {
            let whole = caps.get(0).unwrap();
            let method = caps.get(1).unwrap().as_str();
            let route_path = caps.get(2).unwrap().as_str();
            let line = line_at(&safe, whole.start());
            let upper = method.to_uppercase();

            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{upper}:{route_path}"),
                NodeKind::Route,
                format!("{upper} {route_path}"),
                format!("{file_path}::route:{route_path}"),
                file_path,
                Language::Rust,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_node_id = route_node.id.clone();
            nodes.push(route_node);

            let tail = &safe[whole.end()..];
            if let Some(fn_caps) = FN_RE.captures(tail) {
                references.push(UnresolvedRef {
                    from_node_id: route_node_id,
                    reference_name: fn_caps.get(1).unwrap().as_str().to_string(),
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: Language::Rust,
                    candidates: None,
                });
            }
        }

        // Axum: .route("/path", get(h1).post(h2)…) — balanced-paren scan the route
        // call, then emit one route node per chained method. Handlers may be
        // namespaced (`get(module::handler)`, `get(self::list)`); take the last
        // path segment so the ref names the fn, not the module.
        static ROUTE_OPEN_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\.route\s*\(").unwrap());
        static PATH_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"^\s*"([^"]+)"\s*,"#).unwrap());
        static METHOD_HANDLER_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"\b(get|post|put|patch|delete|head|options|trace)\s*\(\s*([A-Za-z_][\w:]*)")
                .unwrap()
        });

        for m in ROUTE_OPEN_RE.find_iter(&safe) {
            let Some(open_idx) = safe[m.start()..].find('(').map(|i| i + m.start()) else {
                continue;
            };
            let Some(close_idx) = find_matching_paren(&safe, open_idx) else {
                continue;
            };

            let args = &safe[open_idx + 1..close_idx];
            let Some(path_caps) = PATH_RE.captures(args) else {
                continue;
            };
            let route_path = path_caps.get(1).unwrap().as_str();
            let line = line_at(&safe, m.start());

            let method_body = &args[path_caps.get(0).unwrap().end()..];
            for mh in METHOD_HANDLER_RE.captures_iter(method_body) {
                let upper = mh.get(1).unwrap().as_str().to_uppercase();
                let Some(handler) = mh
                    .get(2)
                    .unwrap()
                    .as_str()
                    .split("::")
                    .filter(|s| !s.is_empty())
                    .last()
                else {
                    continue;
                };

                let mut route_node = Node::new(
                    format!("route:{file_path}:{line}:{upper}:{route_path}"),
                    NodeKind::Route,
                    format!("{upper} {route_path}"),
                    format!("{file_path}::route:{route_path}"),
                    file_path,
                    Language::Rust,
                    line,
                    line,
                );
                route_node.updated_at = now;
                let route_node_id = route_node.id.clone();
                nodes.push(route_node);

                references.push(UnresolvedRef {
                    from_node_id: route_node_id,
                    reference_name: handler.to_string(),
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: Language::Rust,
                    candidates: None,
                });
            }
        }

        // Actix-web builder API (the dominant actix routing style; attribute macros
        // are handled above). The handler lives in `.to(handler)`, not `get(handler)`.
        let push_actix_route = |nodes: &mut Vec<Node>,
                                references: &mut Vec<UnresolvedRef>,
                                route_path: &str,
                                method: &str,
                                handler_expr: &str,
                                line: u32| {
            let Some(handler) = handler_expr.split("::").filter(|s| !s.is_empty()).last() else {
                return;
            };
            let upper = method.to_uppercase();
            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{upper}:{route_path}"),
                NodeKind::Route,
                format!("{upper} {route_path}"),
                format!("{file_path}::route:{route_path}"),
                file_path,
                Language::Rust,
                line,
                line,
            );
            route_node.updated_at = now;
            let route_node_id = route_node.id.clone();
            nodes.push(route_node);
            references.push(UnresolvedRef {
                from_node_id: route_node_id,
                reference_name: handler.to_string(),
                reference_kind: EdgeKind::References,
                line,
                column: 0,
                file_path: file_path.to_string(),
                language: Language::Rust,
                candidates: None,
            });
        };

        // web::resource("/path") { .route(web::METHOD().to(h)) | .to(h) } — possibly chained.
        static RESOURCE_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"web::resource\s*\(\s*"([^"]+)"\s*\)"#).unwrap());
        static METHOD_TO_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"web::(get|post|put|patch|delete|head)\s*\(\s*\)\s*\.to\s*\(\s*([A-Za-z_][\w:]*)",
            )
            .unwrap()
        });
        static DIRECT_TO_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^\s*\.to\s*\(\s*([A-Za-z_][\w:]*)").unwrap());

        for caps in RESOURCE_RE.captures_iter(&safe) {
            let whole = caps.get(0).unwrap();
            let route_path = caps.get(1).unwrap().as_str();
            let start_line = line_at(&safe, whole.start());
            let after = whole.end();
            // Bound the resource's method chain at the next resource() to avoid bleed.
            let next_res = safe[after..].find("web::resource").map(|i| i + after);
            let mut end = std::cmp::min(after + 500, next_res.unwrap_or(safe.len()));
            // (Rust deviation: clamp to a char boundary — TS sliced UTF-16 units.)
            while !safe.is_char_boundary(end) {
                end -= 1;
            }
            let chain = &safe[after..end];

            let mut found = false;
            for m2 in METHOD_TO_RE.captures_iter(chain) {
                let m_line =
                    start_line + chain[..m2.get(0).unwrap().start()].matches('\n').count() as u32;
                push_actix_route(
                    &mut nodes,
                    &mut references,
                    route_path,
                    m2.get(1).unwrap().as_str(),
                    m2.get(2).unwrap().as_str(),
                    m_line,
                );
                found = true;
            }
            // Direct `.resource("/x").to(handler)` (all methods) when no explicit verb route.
            if !found {
                if let Some(direct) = DIRECT_TO_RE.captures(chain) {
                    push_actix_route(
                        &mut nodes,
                        &mut references,
                        route_path,
                        "ANY",
                        direct.get(1).unwrap().as_str(),
                        start_line,
                    );
                }
            }
        }

        // App-level: .route("/path", web::METHOD().to(handler)).
        static APP_ROUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r#"\.route\s*\(\s*"([^"]+)"\s*,\s*web::(get|post|put|patch|delete|head)\s*\(\s*\)\s*\.to\s*\(\s*([A-Za-z_][\w:]*)"#,
            )
            .unwrap()
        });
        for caps in APP_ROUTE_RE.captures_iter(&safe) {
            let line = line_at(&safe, caps.get(0).unwrap().start());
            push_actix_route(
                &mut nodes,
                &mut references,
                caps.get(1).unwrap().as_str(),
                caps.get(2).unwrap().as_str(),
                caps.get(3).unwrap().as_str(),
                line,
            );
        }

        Some(FrameworkExtractionResult { nodes, references })
    }
}
