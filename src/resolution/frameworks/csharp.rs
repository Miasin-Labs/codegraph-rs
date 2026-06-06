//! C# Framework Resolver
//!
//! Handles ASP.NET Core, ASP.NET MVC, and common C# patterns.
//! Ported from `src/resolution/frameworks/csharp.ts`.

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

/// TS `safe.slice(i, i + 600)` — bounded lookahead window (byte-based,
/// clamped to a char boundary).
fn slice_bounded(s: &str, start: usize, max_len: usize) -> &str {
    let mut end = (start + max_len).min(s.len());
    while end > start && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[start..end]
}

static ENTRYPOINT_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:Controller|Program|Startup)\.cs$").unwrap());
static ASPNET_ATTR_SIGNAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(?:ApiController|Route|Http(?:Get|Post|Put|Patch|Delete))\b").unwrap()
});
static MODEL_NAME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z][a-zA-Z]+$").unwrap());

// Class-level [Route("api/[controller]")] prefix — joined onto each action.
static CLASS_ROUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\[Route\s*\(\s*"([^"]+)"[^)]*\)\]\s*(?:\[[^\]]*\]\s*)*(?:public\s+|sealed\s+|abstract\s+|partial\s+)*class\b"#,
    )
    .unwrap()
});

// [HttpGet], [HttpGet("path")], [HttpPost("path", Name="x")] — BARE or with a
// path. (The old regex required a string, so bare attributes — with the route
// on the class [Route] — were missed; eShopOnWeb was 24 bare / 2 string.)
static HTTP_ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\[(HttpGet|HttpPost|HttpPut|HttpPatch|HttpDelete)(?:\s*\(\s*"([^"]+)"[^)]*\))?\s*\]"#,
    )
    .unwrap()
});

// Next method declaration (skip stacked attributes; C# puts the return type
// before the name).
static METHOD_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:public|private|protected|internal)\s+[\w<>,\s\[\]?.]+?\s+(\w+)\s*\(").unwrap()
});

// Minimal APIs: app.MapGet("/path", handler)
static MINIMAL_API_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.Map(Get|Post|Put|Patch|Delete)\s*\(\s*"([^"]+)"\s*,\s*([^,)]+)"#).unwrap()
});

static TAIL_IDENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$").unwrap());
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// ASP.NET framework resolver (TS `aspnetResolver`).
pub struct AspnetResolver;

impl FrameworkResolver for AspnetResolver {
    fn name(&self) -> &str {
        "aspnet"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Csharp])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for .csproj files with ASP.NET references
        let all_files = context.get_all_files();
        for file in &all_files {
            if file.ends_with(".csproj") {
                if let Some(content) = context.read_file(file) {
                    if content.contains("Microsoft.AspNetCore")
                        || content.contains("Microsoft.NET.Sdk.Web")
                        || content.contains("System.Web.Mvc")
                    {
                        return true;
                    }
                }
            }
        }

        // Check for Program.cs with WebApplication
        if let Some(program_cs) = context.read_file("Program.cs") {
            if program_cs.contains("WebApplication")
                || program_cs.contains("CreateHostBuilder")
                || program_cs.contains("UseStartup")
            {
                return true;
            }
        }

        // Check for Startup.cs (ASP.NET Core signature)
        if context.file_exists("Startup.cs") {
            return true;
        }

        // ASP.NET signatures in controller/entrypoint SOURCE — covers feature-folder
        // apps with no `/Controllers/` dir and a subdir `Program.cs` that the
        // root-only checks above miss (e.g. realworld: Features/*/FooController.cs).
        // `.csproj` often isn't in the indexed source set, so source-scan is the
        // reliable signal.
        for file in &all_files {
            if !ENTRYPOINT_FILE_RE.is_match(file) {
                continue;
            }
            if let Some(c) = context.read_file(file) {
                if ASPNET_ATTR_SIGNAL_RE.is_match(&c)
                    || c.contains("ControllerBase")
                    || c.contains(": Controller")
                    || c.contains("MapControllers")
                    || c.contains("WebApplication")
                    || c.contains("Microsoft.AspNetCore")
                {
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

        // Pattern 1: Controller references
        if name.ends_with("Controller") {
            if let Some(result) =
                resolve_by_name_and_kind(name, CLASS_KINDS, CONTROLLER_DIRS, context)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 2: Service references (dependency injection)
        if name.ends_with("Service") || (name.starts_with('I') && name.len() > 1) {
            if let Some(result) =
                resolve_by_name_and_kind(name, SERVICE_KINDS, SERVICE_DIRS, context)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 3: Repository references
        if name.ends_with("Repository") {
            if let Some(result) = resolve_by_name_and_kind(name, SERVICE_KINDS, REPO_DIRS, context)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 4: Model/Entity references
        if MODEL_NAME_RE.is_match(name) {
            if let Some(result) = resolve_by_name_and_kind(name, CLASS_KINDS, MODEL_DIRS, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.7,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 5: ViewModel references
        if name.ends_with("ViewModel") || name.ends_with("Dto") {
            if let Some(result) =
                resolve_by_name_and_kind(name, CLASS_KINDS, VIEWMODEL_DIRS, context)
            {
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
        if !file_path.ends_with(".cs") {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_millis();
        let safe = strip_comments_for_regex(content, CommentLang::Csharp);

        // Class-level [Route("api/[controller]")] prefix — joined onto each action.
        let mut class_prefix = String::new();
        if let Some(cls) = CLASS_ROUTE_RE.captures(&safe) {
            class_prefix = cls[1].to_string();
        }

        for m in HTTP_ATTR_RE.captures_iter(&safe) {
            let verb = m.get(1).unwrap().as_str();
            let method = verb.strip_prefix("Http").unwrap_or(verb).to_uppercase();
            let route_path =
                join_cs_path(&class_prefix, m.get(2).map(|g| g.as_str()).unwrap_or(""));
            let whole = m.get(0).unwrap();
            let line = line_of(&safe, whole.start());

            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{method}:{route_path}"),
                NodeKind::Route,
                format!("{method} {route_path}"),
                format!("{file_path}::route:{route_path}"),
                file_path,
                Language::Csharp,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);

            // Next method declaration (skip stacked attributes; C# puts the return type
            // before the name). Bounded so we don't grab a far one.
            let tail = slice_bounded(&safe, whole.end(), 600);
            if let Some(method_match) = METHOD_DECL_RE.captures(tail) {
                references.push(UnresolvedRef {
                    from_node_id: route_id,
                    reference_name: method_match[1].to_string(),
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: Language::Csharp,
                    candidates: None,
                });
            }
        }

        // Minimal APIs: app.MapGet("/path", handler)
        for m in MINIMAL_API_RE.captures_iter(&safe) {
            let verb = m.get(1).unwrap().as_str();
            let route_path = m.get(2).unwrap().as_str();
            let handler_expr = m.get(3).unwrap().as_str();
            let method = verb.to_uppercase();
            let whole = m.get(0).unwrap();
            let line = line_of(&safe, whole.start());

            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{method}:{route_path}"),
                NodeKind::Route,
                format!("{method} {route_path}"),
                format!("{file_path}::route:{route_path}"),
                file_path,
                Language::Csharp,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);

            if let Some(handler_name) = extract_csharp_tail_ident(handler_expr) {
                references.push(UnresolvedRef {
                    from_node_id: route_id,
                    reference_name: handler_name,
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: Language::Csharp,
                    candidates: None,
                });
            }
        }

        Some(FrameworkExtractionResult { nodes, references })
    }
}

/// Join a class-level [Route] prefix and an action's path into one normalized `/path`.
fn join_cs_path(prefix: &str, sub: &str) -> String {
    let parts: Vec<&str> = [prefix, sub]
        .iter()
        .map(|p| p.trim_matches('/'))
        .filter(|p| !p.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

/// Extract last identifier from an expression like `MyService.Handler` or `Handler`.
fn extract_csharp_tail_ident(expr: &str) -> Option<String> {
    let cleaned = WHITESPACE_RE.replace_all(expr.trim(), "");
    TAIL_IDENT_RE.captures(&cleaned).map(|m| m[1].to_string())
}

// Directory patterns
const CONTROLLER_DIRS: &[&str] = &["/Controllers/"];
const SERVICE_DIRS: &[&str] = &["/Services/", "/Service/", "/Application/"];
const REPO_DIRS: &[&str] = &[
    "/Repositories/",
    "/Repository/",
    "/Data/",
    "/Infrastructure/",
];
const MODEL_DIRS: &[&str] = &["/Models/", "/Model/", "/Entities/", "/Entity/", "/Domain/"];
const VIEWMODEL_DIRS: &[&str] = &["/ViewModels/", "/ViewModel/", "/DTOs/", "/Dto/"];

const CLASS_KINDS: &[NodeKind] = &[NodeKind::Class];
const SERVICE_KINDS: &[NodeKind] = &[NodeKind::Class, NodeKind::Interface];

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
    if let Some(preferred) = kind_filtered.iter().find(|n| {
        preferred_dir_patterns
            .iter()
            .any(|d| n.file_path.contains(d))
    }) {
        return Some(preferred.id.clone());
    }

    // Fall back to any match
    Some(kind_filtered[0].id.clone())
}
