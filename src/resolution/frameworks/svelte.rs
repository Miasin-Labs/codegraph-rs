//! Svelte / SvelteKit Framework Resolver
//!
//! Handles Svelte component references, Svelte 5 runes,
//! store auto-subscriptions, and SvelteKit route/module patterns.
//!
//! Ported from `src/resolution/frameworks/svelte.ts`.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

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

/// Svelte 5 runes — compiler-provided, not user code
static SVELTE_RUNES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "$state",
        "$state.raw",
        "$state.snapshot",
        "$derived",
        "$derived.by",
        "$effect",
        "$effect.pre",
        "$effect.root",
        "$effect.tracking",
        "$props",
        "$bindable",
        "$inspect",
        "$host",
    ]
    .into_iter()
    .collect()
});

/// SvelteKit framework-provided module prefixes
const SVELTEKIT_MODULE_PREFIXES: [&str; 9] = [
    "$app/navigation",
    "$app/stores",
    "$app/environment",
    "$app/forms",
    "$app/paths",
    "$env/static/private",
    "$env/static/public",
    "$env/dynamic/private",
    "$env/dynamic/public",
];

/// `svelteResolver` — unit struct implementing [`FrameworkResolver`].
pub struct SvelteResolver;

const SVELTE_LANGUAGES: [Language; 1] = [Language::Svelte];

impl FrameworkResolver for SvelteResolver {
    fn name(&self) -> &str {
        "svelte"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&SVELTE_LANGUAGES)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for svelte or @sveltejs/kit in package.json
        if let Some(package_json) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&package_json) {
                let deps = merged_deps(&pkg);
                if ["svelte", "@sveltejs/kit"]
                    .iter()
                    .any(|k| dep_truthy(&deps, k))
                {
                    return true;
                }
            }
        }

        // Check for .svelte files in project
        context
            .get_all_files()
            .iter()
            .any(|f| f.ends_with(".svelte"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // Pattern 1: Svelte runes ($state, $derived, $effect, etc.)
        if is_rune_reference(&reference.reference_name) {
            // Runes are compiler-provided — return a high-confidence "framework" resolution
            // so CodeGraph doesn't waste time searching for user-defined symbols.
            // We use the fromNodeId as targetNodeId since runes don't have real targets.
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: reference.from_node_id.clone(),
                confidence: 1.0,
                resolved_by: ResolvedBy::Framework,
            });
        }

        // Pattern 2: Store auto-subscriptions ($storeName)
        if reference.reference_name.starts_with('$') && !reference.reference_name.starts_with("$$")
        {
            let store_name = &reference.reference_name[1..];
            let candidates = context.get_nodes_by_name(store_name);
            let store_node = candidates
                .iter()
                .find(|n| n.kind == NodeKind::Variable || n.kind == NodeKind::Constant);
            if let Some(store_node) = store_node {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: store_node.id.clone(),
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 3: SvelteKit module imports ($app/*, $env/*, $lib/*)
        if reference.reference_kind == EdgeKind::Imports
            && reference.reference_name.starts_with('$')
        {
            // $lib/* resolves to src/lib/* — try to find the target file
            if reference.reference_name.starts_with("$lib/") {
                let lib_path = reference.reference_name.replacen("$lib/", "src/lib/", 1);
                // Try common extensions
                for ext in ["", ".ts", ".js", ".svelte", "/index.ts", "/index.js"] {
                    let full_path = format!("{lib_path}{ext}");
                    if context.file_exists(&full_path) {
                        let nodes = context.get_nodes_in_file(&full_path);
                        if let Some(first) = nodes.first() {
                            return Some(ResolvedRef {
                                original: reference.clone(),
                                target_node_id: first.id.clone(),
                                confidence: 0.9,
                                resolved_by: ResolvedBy::Framework,
                            });
                        }
                    }
                }
            }

            // $app/* and $env/* are framework-provided
            if SVELTEKIT_MODULE_PREFIXES
                .iter()
                .any(|prefix| reference.reference_name.starts_with(prefix))
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: reference.from_node_id.clone(),
                    confidence: 1.0,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 4: Component references (PascalCase) — resolve to .svelte files
        if is_pascal_case(&reference.reference_name) && reference.reference_kind == EdgeKind::Calls
        {
            if let Some(result) =
                resolve_component(&reference.reference_name, &reference.file_path, context)
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

    fn extract(&self, file_path: &str, _content: &str) -> Option<FrameworkExtractionResult> {
        let mut nodes: Vec<Node> = Vec::new();
        let now = now_ms();

        // Detect SvelteKit route files
        let file_name = file_path.rsplit(['/', '\\']).next().unwrap_or("");
        let route_match = get_sveltekit_route_info(file_name);

        if route_match.is_some() {
            // Extract route path from directory structure
            // e.g., src/routes/blog/[slug]/+page.svelte -> /blog/:slug
            let route_path = file_path_to_sveltekit_route(file_path);

            if let Some(route_path) = route_path {
                let mut node = Node::new(
                    format!("route:{file_path}:{route_path}:1"),
                    NodeKind::Route,
                    &route_path,
                    format!("{file_path}::route:{route_path}"),
                    file_path,
                    if file_path.ends_with(".svelte") {
                        Language::Svelte
                    } else {
                        Language::Typescript
                    },
                    1,
                    1,
                );
                node.updated_at = now;
                nodes.push(node);
            }
        }

        Some(FrameworkExtractionResult {
            nodes,
            references: Vec::new(),
        })
    }
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

/// JS truthiness check on a dependency entry.
fn dep_truthy(deps: &serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    match deps.get(key) {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// Check if a reference name is a Svelte rune
fn is_rune_reference(name: &str) -> bool {
    // Direct match (e.g. $state, $derived)
    if SVELTE_RUNES.contains(name) {
        return true;
    }

    // Rune method calls come through as the base rune name
    // e.g. $state.raw -> the call is to "$state" with ".raw" accessed as property
    // Check if it's a base rune that has sub-methods
    if name == "$state" || name == "$derived" || name == "$effect" {
        return true;
    }

    false
}

/// Check if string is PascalCase
fn is_pascal_case(s: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z][a-zA-Z0-9]*$").unwrap());
    RE.is_match(s)
}

/// Resolve a Svelte component reference using name-based lookup
fn resolve_component(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    // Look for component nodes by name
    let candidates = context.get_nodes_by_name(name);
    let components: Vec<&Node> = candidates
        .iter()
        .filter(|n| n.kind == NodeKind::Component)
        .collect();

    if components.is_empty() {
        return None;
    }

    // Prefer same directory
    let from_dir = match from_file.rfind('/') {
        Some(i) => &from_file[..i],
        None => "",
    };
    if let Some(first) = components
        .iter()
        .find(|n| n.file_path.starts_with(from_dir))
    {
        return Some(first.id.clone());
    }

    Some(components[0].id.clone())
}

/// SvelteKit route file patterns — check if filename is a SvelteKit route file
fn get_sveltekit_route_info(file_name: &str) -> Option<&'static str> {
    match file_name {
        "+page.svelte" => Some("page"),
        "+page.ts" => Some("page-load"),
        "+page.js" => Some("page-load"),
        "+page.server.ts" => Some("page-server-load"),
        "+page.server.js" => Some("page-server-load"),
        "+layout.svelte" => Some("layout"),
        "+layout.ts" => Some("layout-load"),
        "+layout.js" => Some("layout-load"),
        "+layout.server.ts" => Some("layout-server-load"),
        "+layout.server.js" => Some("layout-server-load"),
        "+server.ts" => Some("api-endpoint"),
        "+server.js" => Some("api-endpoint"),
        "+error.svelte" => Some("error-page"),
        _ => None,
    }
}

/// Convert a file path to a SvelteKit route path
fn file_path_to_sveltekit_route(file_path: &str) -> Option<String> {
    static REST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\.\.\.([^\]]+)\]").unwrap());
    static OPTIONAL_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[{2}([^\]]+)\]{2}").unwrap());
    static PARAM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]").unwrap());

    // Normalize to forward slashes
    let normalized = file_path.replace('\\', "/");

    // Find the routes directory
    let routes_index = normalized.find("/routes/")?;

    // Extract the path after routes/
    let after_routes = &normalized[routes_index + "/routes/".len()..];

    // Remove the file name
    let dir_path = match after_routes.rfind('/') {
        Some(last_slash) => &after_routes[..last_slash],
        None => "",
    };

    // Convert SvelteKit param syntax [param] to :param
    let replaced = REST_RE.replace_all(dir_path, "*${1}"); // [...rest] -> *rest
    let replaced = OPTIONAL_RE.replace_all(&replaced, ":${1}?"); // [[optional]] -> :optional?
    let replaced = PARAM_RE.replace_all(&replaced, ":${1}"); // [param] -> :param
    let route = format!("/{replaced}");

    if route == "/" {
        return Some("/".to_string());
    }
    // Remove trailing slash
    Some(route.strip_suffix('/').unwrap_or(&route).to_string())
}
