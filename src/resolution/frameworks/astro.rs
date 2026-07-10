//! Astro framework resolver.

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

const ASTRO_VIRTUAL_MODULES: &[&str] = &[
    "astro:content",
    "astro:assets",
    "astro:actions",
    "astro:env",
    "astro:i18n",
    "astro:middleware",
    "astro:transitions",
    "astro:components",
    "astro:schema",
];

static PASCAL_CASE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][a-zA-Z0-9]*$").expect("valid Astro component regex"));
static CONFIG_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.config\.[a-z]+$").expect("valid Astro config regex"));

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Default)]
pub struct AstroResolver;

impl FrameworkResolver for AstroResolver {
    fn name(&self) -> &str {
        "astro"
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if let Some(package_json) = context.read_file("package.json") {
            if let Ok(package) = serde_json::from_str::<serde_json::Value>(&package_json) {
                let has_astro = ["dependencies", "devDependencies"]
                    .iter()
                    .filter_map(|key| package.get(key).and_then(serde_json::Value::as_object))
                    .any(|dependencies| dependencies.contains_key("astro"));
                if has_astro {
                    return true;
                }
            }
        }

        context
            .get_all_files()
            .iter()
            .any(|file| file.ends_with(".astro"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        if reference.reference_name == "Astro" || reference.reference_name.starts_with("Astro.") {
            return Some(framework_resolution(
                reference,
                &reference.from_node_id,
                1.0,
            ));
        }

        if reference.reference_kind == EdgeKind::Imports
            && reference.reference_name.starts_with("astro:")
            && ASTRO_VIRTUAL_MODULES
                .iter()
                .any(|prefix| reference.reference_name.starts_with(prefix))
        {
            return Some(framework_resolution(
                reference,
                &reference.from_node_id,
                1.0,
            ));
        }

        if PASCAL_CASE_RE.is_match(&reference.reference_name)
            && matches!(
                reference.reference_kind,
                EdgeKind::References | EdgeKind::Calls
            )
        {
            let target =
                resolve_component(&reference.reference_name, &reference.file_path, context)?;
            return Some(framework_resolution(reference, &target, 0.8));
        }

        None
    }

    fn extract(&self, file_path: &str, _content: &str) -> Option<FrameworkExtractionResult> {
        let normalized = file_path.replace('\\', "/");
        let Some(after_pages) = path_after_pages(&normalized) else {
            return Some(FrameworkExtractionResult::default());
        };

        if !has_astro_route_extension(&normalized) {
            return Some(FrameworkExtractionResult::default());
        }

        let base = after_pages.rsplit('/').next().unwrap_or_default();
        if after_pages
            .split('/')
            .any(|segment| segment.starts_with('_'))
            || CONFIG_FILE_RE.is_match(base)
        {
            return Some(FrameworkExtractionResult::default());
        }

        let route_path = file_path_to_astro_route(after_pages);
        let mut route = Node::new(
            format!("route:{file_path}:{route_path}:1"),
            NodeKind::Route,
            route_path.clone(),
            format!("{file_path}::route:{route_path}"),
            file_path,
            if normalized.ends_with(".astro") {
                Language::Astro
            } else {
                Language::Typescript
            },
            1,
            1,
        );
        route.start_column = 0;
        route.end_column = 0;
        route.updated_at = now_ms();

        Some(FrameworkExtractionResult {
            nodes: vec![route],
            references: Vec::new(),
        })
    }
}

fn framework_resolution(reference: &UnresolvedRef, target: &str, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        original: reference.clone(),
        target_node_id: target.to_string(),
        confidence,
        resolved_by: ResolvedBy::Framework,
    }
}

fn resolve_component(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let components: Vec<Node> = context
        .get_nodes_by_name(name)
        .into_iter()
        .filter(|node| node.kind == NodeKind::Component)
        .collect();

    let from_dir = from_file.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    if let Some(component) = components
        .iter()
        .find(|component| component.file_path.starts_with(from_dir))
    {
        return Some(component.id.clone());
    }

    (components.len() == 1).then(|| components[0].id.clone())
}

fn path_after_pages(path: &str) -> Option<&str> {
    path.strip_prefix("src/pages/")
        .or_else(|| path.split_once("/src/pages/").map(|(_, suffix)| suffix))
}

fn has_astro_route_extension(path: &str) -> bool {
    [".astro", ".ts", ".js", ".mjs"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

fn file_path_to_astro_route(after_pages: &str) -> String {
    let without_extension = [".astro", ".ts", ".js", ".mjs"]
        .iter()
        .find_map(|extension| after_pages.strip_suffix(extension))
        .unwrap_or(after_pages);
    let without_index = without_extension
        .strip_suffix("/index")
        .or_else(|| (without_extension == "index").then_some(""))
        .unwrap_or(without_extension)
        .trim_end_matches('/');

    static REST_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\[\.\.\.([^\]]+)\]").expect("valid Astro rest parameter regex")
    });
    static PARAM_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[([^\]]+)\]").expect("valid Astro parameter regex"));

    let route = REST_PARAM_RE.replace_all(without_index, "*$1");
    let route = PARAM_RE.replace_all(&route, ":$1");
    let route = format!("/{route}");
    if route == "/" {
        route
    } else {
        route.trim_end_matches('/').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_astro_page_paths_to_routes() {
        assert_eq!(file_path_to_astro_route("index.astro"), "/");
        assert_eq!(file_path_to_astro_route("blog/[slug].astro"), "/blog/:slug");
        assert_eq!(
            file_path_to_astro_route("docs/[...path].astro"),
            "/docs/*path"
        );
        assert_eq!(file_path_to_astro_route("api/posts.ts"), "/api/posts");
    }

    #[test]
    fn extracts_file_based_routes_and_skips_private_pages() {
        let resolver = AstroResolver;
        let result = resolver
            .extract("apps/site/src/pages/blog/[slug].astro", "")
            .expect("extract hook is implemented");
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "/blog/:slug");
        assert_eq!(result.nodes[0].language, Language::Astro);

        let private = resolver
            .extract("src/pages/_partials/card.astro", "")
            .expect("extract hook is implemented");
        assert!(private.nodes.is_empty());
    }
}
