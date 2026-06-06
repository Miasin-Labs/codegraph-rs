//! Vue / Nuxt Framework Resolver
//!
//! Handles Vue component references, compiler macros (defineProps, etc.),
//! Nuxt auto-imports, and Nuxt file-based routing patterns.
//!
//! Ported from `src/resolution/frameworks/vue.ts`.

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

/// Vue 3 compiler macros — compiler-provided, not user code
static VUE_COMPILER_MACROS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "defineProps",
        "defineEmits",
        "defineExpose",
        "defineOptions",
        "defineSlots",
        "defineModel",
        "withDefaults",
    ]
    .into_iter()
    .collect()
});

/// Nuxt auto-imported composables and utilities
static NUXT_AUTO_IMPORTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // Routing
        "useRoute",
        "useRouter",
        "navigateTo",
        "abortNavigation",
        // Data fetching
        "useFetch",
        "useAsyncData",
        "useLazyFetch",
        "useLazyAsyncData",
        "refreshNuxtData",
        // State
        "useState",
        "clearNuxtState",
        // Head
        "useHead",
        "useSeoMeta",
        "useServerSeoMeta",
        // Runtime
        "useRuntimeConfig",
        "useAppConfig",
        "useNuxtApp",
        // Cookies
        "useCookie",
        // Error
        "useError",
        "createError",
        "showError",
        "clearError",
        // Page/layout
        "definePageMeta",
        "defineNuxtConfig",
        "defineNuxtPlugin",
        "defineNuxtRouteMiddleware",
        // Request
        "useRequestHeaders",
        "useRequestEvent",
        "useRequestFetch",
        "useRequestURL",
    ]
    .into_iter()
    .collect()
});

/// Nuxt virtual module prefixes (auto-import namespaces)
const NUXT_VIRTUAL_MODULES: [&str; 5] = ["#imports", "#components", "#app", "#build", "#head"];

/// `vueResolver` — unit struct implementing [`FrameworkResolver`].
/// (No `languages` in the TS object → applies to all languages.)
pub struct VueResolver;

impl FrameworkResolver for VueResolver {
    fn name(&self) -> &str {
        "vue"
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for vue or nuxt in package.json
        if let Some(package_json) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&package_json) {
                let deps = merged_deps(&pkg);
                if ["vue", "nuxt", "@nuxt/kit"]
                    .iter()
                    .any(|k| dep_truthy(&deps, k))
                {
                    return true;
                }
            }
        }

        // Check for .vue files in project
        context.get_all_files().iter().any(|f| f.ends_with(".vue"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // Pattern 1: Vue compiler macros (defineProps, defineEmits, etc.)
        if VUE_COMPILER_MACROS.contains(reference.reference_name.as_str()) {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: reference.from_node_id.clone(),
                confidence: 1.0,
                resolved_by: ResolvedBy::Framework,
            });
        }

        // Pattern 2: Nuxt auto-imported composables
        if NUXT_AUTO_IMPORTS.contains(reference.reference_name.as_str()) {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: reference.from_node_id.clone(),
                confidence: 1.0,
                resolved_by: ResolvedBy::Framework,
            });
        }

        // Pattern 3: Nuxt virtual module imports (#imports, #components, etc.)
        if reference.reference_kind == EdgeKind::Imports
            && reference.reference_name.starts_with('#')
            && NUXT_VIRTUAL_MODULES
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

        // Pattern 4: @ alias imports (@/components/Foo -> src/components/Foo)
        if reference.reference_kind == EdgeKind::Imports
            && reference.reference_name.starts_with("@/")
        {
            let alias_path = reference.reference_name.replacen("@/", "src/", 1);
            for ext in [
                "",
                ".ts",
                ".js",
                ".vue",
                "/index.ts",
                "/index.js",
                "/index.vue",
            ] {
                let full_path = format!("{alias_path}{ext}");
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

        // Pattern 5: ~ alias imports (~/components/Foo -> src/components/Foo, Nuxt convention)
        if reference.reference_kind == EdgeKind::Imports
            && reference.reference_name.starts_with("~/")
        {
            let alias_path = reference.reference_name.replacen("~/", "src/", 1);
            for ext in [
                "",
                ".ts",
                ".js",
                ".vue",
                "/index.ts",
                "/index.js",
                "/index.vue",
            ] {
                let full_path = format!("{alias_path}{ext}");
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

        // Pattern 6: Component references (PascalCase) — resolve to .vue files
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
        static EXT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.[^/.]+$").unwrap());

        let mut nodes: Vec<Node> = Vec::new();
        let now = now_ms();

        // Normalize to forward slashes
        let normalized = file_path.replace('\\', "/");

        // Detect Nuxt page routes (pages/ directory)
        let pages_index = normalized.find("/pages/");
        if let Some(pages_index) = pages_index {
            if normalized.ends_with(".vue") {
                let route_path =
                    file_path_to_nuxt_route(&normalized, pages_index + "/pages/".len());
                if let Some(route_path) = route_path {
                    let mut node = Node::new(
                        format!("route:{file_path}:{route_path}:1"),
                        NodeKind::Route,
                        &route_path,
                        format!("{file_path}::route:{route_path}"),
                        file_path,
                        Language::Vue,
                        1,
                        1,
                    );
                    node.updated_at = now;
                    nodes.push(node);
                }
            }
        }

        // Detect Nuxt API routes (server/api/ directory)
        let api_index = normalized.find("/server/api/");
        if let Some(api_index) = api_index {
            let after_api = &normalized[api_index + "/server/api/".len()..];
            let route_name = EXT_RE.replace(after_api, ""); // Remove extension
            let route_name = route_name.strip_suffix("/index").unwrap_or(&route_name); // index -> parent path
            let api_route = format!("/api/{route_name}");

            let mut node = Node::new(
                format!("route:{file_path}:{api_route}:1"),
                NodeKind::Route,
                &api_route,
                format!("{file_path}::route:{api_route}"),
                file_path,
                if normalized.ends_with(".vue") {
                    Language::Vue
                } else {
                    Language::Typescript
                },
                1,
                1,
            );
            node.updated_at = now;
            nodes.push(node);
        }

        // Detect Nuxt middleware (middleware/ directory)
        let middleware_index = normalized.find("/middleware/");
        if let Some(middleware_index) = middleware_index {
            let after_middleware = &normalized[middleware_index + "/middleware/".len()..];
            let middleware_name = EXT_RE.replace(after_middleware, "");

            let mut node = Node::new(
                format!("middleware:{file_path}:{middleware_name}:1"),
                NodeKind::Function,
                middleware_name.as_ref(),
                format!("{file_path}::middleware:{middleware_name}"),
                file_path,
                if normalized.ends_with(".vue") {
                    Language::Vue
                } else {
                    Language::Typescript
                },
                1,
                1,
            );
            node.updated_at = now;
            nodes.push(node);
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

/// Check if string is PascalCase
fn is_pascal_case(s: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z][a-zA-Z0-9]*$").unwrap());
    RE.is_match(s)
}

/// Resolve a Vue component reference to its .vue file
fn resolve_component(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let all_files = context.get_all_files();
    let vue_files: Vec<&String> = all_files.iter().filter(|f| f.ends_with(".vue")).collect();

    // Check for exact name match (Button -> Button.vue)
    for file in &vue_files {
        let file_name = file.rsplit(['/', '\\']).next().unwrap_or("");
        let component_name = file_name.strip_suffix(".vue").unwrap_or(file_name);
        if component_name == name {
            let nodes = context.get_nodes_in_file(file);
            let component = nodes
                .iter()
                .find(|n| n.kind == NodeKind::Component && n.name == name);
            if let Some(component) = component {
                return Some(component.id.clone());
            }
        }
    }

    // Check same directory first for better specificity
    let from_dir = match from_file.rfind('/') {
        Some(i) => &from_file[..i],
        None => "",
    };
    for file in &vue_files {
        if file.starts_with(from_dir) {
            let file_name = file.rsplit(['/', '\\']).next().unwrap_or("");
            let component_name = file_name.strip_suffix(".vue").unwrap_or(file_name);
            if component_name == name {
                let nodes = context.get_nodes_in_file(file);
                let component = nodes.iter().find(|n| n.kind == NodeKind::Component);
                if let Some(component) = component {
                    return Some(component.id.clone());
                }
            }
        }
    }

    None
}

/// Convert a file path to a Nuxt route path
fn file_path_to_nuxt_route(normalized: &str, after_pages_start: usize) -> Option<String> {
    static REST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\.\.\.([^\]]+)\]").unwrap());
    static OPTIONAL_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[{2}([^\]]+)\]{2}").unwrap());
    static PARAM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]").unwrap());

    let after_pages = &normalized[after_pages_start..];

    // Remove the .vue extension
    let without_ext = after_pages.strip_suffix(".vue").unwrap_or(after_pages);

    // Remove /index suffix (index.vue -> parent route)
    let without_index = without_ext.strip_suffix("/index").unwrap_or(without_ext);

    // Convert Nuxt param syntax [param] to :param
    let replaced = REST_RE.replace_all(without_index, "*${1}"); // [...slug] -> *slug (catch-all)
    let replaced = OPTIONAL_RE.replace_all(&replaced, ":${1}?"); // [[optional]] -> :optional?
    let replaced = PARAM_RE.replace_all(&replaced, ":${1}"); // [param] -> :param
    let route = format!("/{replaced}");

    if route == "/" {
        return Some("/".to_string());
    }
    // Remove trailing slash
    Some(route.strip_suffix('/').unwrap_or(&route).to_string())
}
