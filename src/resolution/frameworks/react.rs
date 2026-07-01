//! React Framework Resolver
//!
//! Handles React and Next.js patterns.
//! Ported from `src/resolution/frameworks/react.ts`.

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

/// 1-based line number of the byte offset `index`.
fn line_at(s: &str, index: usize) -> u32 {
    (s[..index].matches('\n').count() + 1) as u32
}

/// `content[start..start+window]` clamped to length and char boundaries
/// (TS `String.slice` windows; byte-based here, see notes/frameworks-js.md).
fn window_after(content: &str, start: usize, window: usize) -> &str {
    let mut end = (start + window).min(content.len());
    while end > start && !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[start..end]
}

/// `reactResolver` — unit struct implementing [`FrameworkResolver`].
pub struct ReactResolver;

const REACT_LANGUAGES: [Language; 2] = [Language::Javascript, Language::Typescript];

impl FrameworkResolver for ReactResolver {
    fn name(&self) -> &str {
        "react"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&REACT_LANGUAGES)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for React in package.json
        if let Some(package_json) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&package_json) {
                let deps = merged_deps(&pkg);
                if ["react", "next", "react-native"]
                    .iter()
                    .any(|k| dep_truthy(&deps, k))
                {
                    return true;
                }
            }
        }

        // Check for .jsx/.tsx files
        context
            .get_all_files()
            .iter()
            .any(|f| f.ends_with(".jsx") || f.ends_with(".tsx"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // Pattern 1: Component references (PascalCase)
        if is_pascal_case(&reference.reference_name) && !is_built_in_type(&reference.reference_name)
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

        // Pattern 2: Hook references (use*)
        if reference.reference_name.starts_with("use") && reference.reference_name.len() > 3 {
            if let Some(result) = resolve_hook(&reference.reference_name, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 3: Context references
        if reference.reference_name.ends_with("Context")
            || reference.reference_name.ends_with("Provider")
        {
            if let Some(result) = resolve_context(&reference.reference_name, context) {
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
        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_ms();

        // Extract component definitions
        // function Component() or const Component = () =>
        static COMPONENT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
            [
                // Function components
                r"(?:export\s+)?function\s+([A-Z][a-zA-Z0-9]*)\s*\(",
                // Arrow function components
                r"(?:export\s+)?(?:const|let)\s+([A-Z][a-zA-Z0-9]*)\s*=\s*(?:\([^)]*\)|[a-zA-Z_][a-zA-Z0-9_]*)\s*=>",
                // forwardRef components
                r"(?:export\s+)?(?:const|let)\s+([A-Z][a-zA-Z0-9]*)\s*=\s*(?:React\.)?forwardRef",
                // memo components
                r"(?:export\s+)?(?:const|let)\s+([A-Z][a-zA-Z0-9]*)\s*=\s*(?:React\.)?memo",
            ]
            .iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
        });

        for pattern in COMPONENT_PATTERNS.iter() {
            for m in pattern.captures_iter(content) {
                let full_match = m.get(0).unwrap();
                let name = &m[1];
                let line = line_at(content, full_match.start());

                // Check if it returns JSX (rough heuristic)
                let after_match = window_after(content, full_match.end(), 500);
                let has_jsx = after_match.contains('<')
                    && (after_match.contains("/>") || after_match.contains("</"));

                if has_jsx {
                    let mut node = Node::new(
                        format!("component:{file_path}:{name}:{line}"),
                        NodeKind::Component,
                        name,
                        format!("{file_path}::{name}"),
                        file_path,
                        if file_path.ends_with(".tsx") {
                            Language::Tsx
                        } else {
                            Language::Jsx
                        },
                        line,
                        line,
                    );
                    node.end_column = full_match.as_str().len() as u32;
                    node.is_exported = Some(full_match.as_str().contains("export"));
                    node.updated_at = now;
                    nodes.push(node);
                }
            }
        }

        // Extract custom hooks
        static HOOK_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?:export\s+)?(?:function|const|let)\s+(use[A-Z][a-zA-Z0-9]*)\s*[=(]")
                .unwrap()
        });
        for m in HOOK_PATTERN.captures_iter(content) {
            let full_match = m.get(0).unwrap();
            let name = &m[1];
            let line = line_at(content, full_match.start());

            let mut node = Node::new(
                format!("hook:{file_path}:{name}:{line}"),
                NodeKind::Function,
                name,
                format!("{file_path}::{name}"),
                file_path,
                if file_path.ends_with(".ts") || file_path.ends_with(".tsx") {
                    Language::Typescript
                } else {
                    Language::Javascript
                },
                line,
                line,
            );
            node.end_column = full_match.as_str().len() as u32;
            node.is_exported = Some(full_match.as_str().contains("export"));
            node.updated_at = now;
            nodes.push(node);
        }

        // React Router: <Route path="/x" component={Comp}/> (v5) or
        // <Route path="/x" element={<Comp/>}/> (v6). Attributes appear in any order,
        // and element={...} contains a nested `>`, so scan a window after each
        // <Route rather than trying to match the whole (possibly multi-line) tag.
        static ROUTE_TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<Route\b").unwrap());
        static ROUTE_PATH_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"\bpath\s*=\s*["']([^"']+)["']"#).unwrap());
        static ROUTE_COMPONENT_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\bcomponent\s*=\s*\{\s*([A-Z][A-Za-z0-9_]*)").unwrap());
        static ROUTE_ELEMENT_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\belement\s*=\s*\{\s*<\s*([A-Z][A-Za-z0-9_]*)").unwrap());
        let jsx_lang = if file_path.ends_with(".tsx") {
            Language::Tsx
        } else {
            Language::Jsx
        };
        for route_match in ROUTE_TAG_RE.find_iter(content) {
            let window = window_after(content, route_match.start(), 400);
            let Some(path_match) = ROUTE_PATH_RE.captures(window) else {
                continue; // index/layout routes without a path
            };
            let route_path = &path_match[1];
            let comp_match = ROUTE_COMPONENT_RE
                .captures(window)
                .or_else(|| ROUTE_ELEMENT_RE.captures(window));
            let line = line_at(content, route_match.start());
            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{route_path}"),
                NodeKind::Route,
                route_path,
                format!("{file_path}::route:{route_path}"),
                file_path,
                jsx_lang,
                line,
                line,
            );
            route_node.updated_at = now;
            let route_node_id = route_node.id.clone();
            nodes.push(route_node);
            if let Some(comp_match) = comp_match {
                references.push(UnresolvedRef {
                    from_node_id: route_node_id,
                    reference_name: comp_match[1].to_string(),
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: jsx_lang,
                    candidates: None,
                    metadata: None,
                });
            }
        }

        // React Router data-router (v6.4+): createBrowserRouter([{ path, element }]).
        // Only scan files that use the data-router API, then pull each route object's
        // `path` + `element={<Comp/>}` / `Component: Comp` (a forward window confirms
        // it's a route object, not a stray `path:` field).
        static DATA_ROUTER_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"\b(?:createBrowserRouter|createHashRouter|createMemoryRouter|createRoutesFromElements)\b",
            )
            .unwrap()
        });
        static OBJ_PATH_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"\bpath\s*:\s*['"]([^'"]*)['"]"#).unwrap());
        static OBJ_ELEMENT_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\belement\s*:\s*<\s*([A-Z][A-Za-z0-9_]*)").unwrap());
        static OBJ_COMPONENT_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\bComponent\s*:\s*([A-Z][A-Za-z0-9_]*)").unwrap());
        if DATA_ROUTER_RE.is_match(content) {
            for om in OBJ_PATH_RE.captures_iter(content) {
                let whole = om.get(0).unwrap();
                let win = window_after(content, whole.start(), 300);
                let comp_match = OBJ_ELEMENT_RE
                    .captures(win)
                    .or_else(|| OBJ_COMPONENT_RE.captures(win));
                let Some(comp_match) = comp_match else {
                    continue; // require a component → it's a real route object
                };
                let route_path = if om[1].is_empty() { "/" } else { &om[1] };
                let line = line_at(content, whole.start());
                let mut route_node = Node::new(
                    format!("route:{file_path}:{line}:{route_path}"),
                    NodeKind::Route,
                    route_path,
                    format!("{file_path}::route:{route_path}"),
                    file_path,
                    jsx_lang,
                    line,
                    line,
                );
                route_node.updated_at = now;
                let route_node_id = route_node.id.clone();
                nodes.push(route_node);
                references.push(UnresolvedRef {
                    from_node_id: route_node_id,
                    reference_name: comp_match[1].to_string(),
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: jsx_lang,
                    candidates: None,
                    metadata: None,
                });
            }
        }

        // Extract Next.js pages/routes (pages directory convention)
        if file_path.contains("pages/") || file_path.contains("app/") {
            // Default export in pages becomes a route
            if content.contains("export default") {
                if let Some(route_path) = file_path_to_route(file_path) {
                    let idx = content.find("export default").unwrap();
                    let line_num = line_at(content, idx);

                    let mut node = Node::new(
                        format!("route:{file_path}:{route_path}:{line_num}"),
                        NodeKind::Route,
                        &route_path,
                        format!("{file_path}::route:{route_path}"),
                        file_path,
                        if file_path.ends_with(".tsx") {
                            Language::Tsx
                        } else if file_path.ends_with(".ts") {
                            Language::Typescript
                        } else {
                            Language::Javascript
                        },
                        line_num,
                        line_num,
                    );
                    node.updated_at = now;
                    nodes.push(node);
                }
            }
        }

        Some(FrameworkExtractionResult { nodes, references })
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

/// Check if name is a built-in type
fn is_built_in_type(name: &str) -> bool {
    BUILT_IN_TYPES.contains(name)
}

static BUILT_IN_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "Array",
        "Boolean",
        "Date",
        "Error",
        "Function",
        "JSON",
        "Math",
        "Number",
        "Object",
        "Promise",
        "RegExp",
        "String",
        "Symbol",
        "Map",
        "Set",
        "WeakMap",
        "WeakSet",
        "React",
        "Component",
        "Fragment",
        "Suspense",
        "StrictMode",
    ]
    .into_iter()
    .collect()
});

const COMPONENT_KINDS: [NodeKind; 3] = [NodeKind::Component, NodeKind::Function, NodeKind::Class];

/// Resolve a component reference using name-based lookup
fn resolve_component(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    if candidates.is_empty() {
        return None;
    }

    let components: Vec<&Node> = candidates
        .iter()
        .filter(|n| COMPONENT_KINDS.contains(&n.kind))
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

    // Prefer component directories
    const COMPONENT_DIRS: [&str; 7] = [
        "/components/",
        "/src/components/",
        "/app/components/",
        "/pages/",
        "/src/pages/",
        "/views/",
        "/src/views/",
    ];
    if let Some(first) = components
        .iter()
        .find(|n| COMPONENT_DIRS.iter().any(|d| n.file_path.contains(d)))
    {
        return Some(first.id.clone());
    }

    Some(components[0].id.clone())
}

/// Resolve a custom hook reference using name-based lookup
fn resolve_hook(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    if candidates.is_empty() {
        return None;
    }

    let hooks: Vec<&Node> = candidates
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("use"))
        .collect();
    if hooks.is_empty() {
        return None;
    }

    // Prefer hooks directories
    const HOOK_DIRS: [&str; 4] = ["/hooks/", "/src/hooks/", "/lib/hooks/", "/utils/hooks/"];
    if let Some(first) = hooks
        .iter()
        .find(|n| HOOK_DIRS.iter().any(|d| n.file_path.contains(d)))
    {
        return Some(first.id.clone());
    }

    Some(hooks[0].id.clone())
}

/// Resolve a context reference using name-based lookup
fn resolve_context(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    static SUFFIX_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"Context$|Provider$").unwrap());
    let candidates = context.get_nodes_by_name(name);
    if candidates.is_empty() {
        // Try without Context/Provider suffix
        let base_name = SUFFIX_RE.replace(name, "").into_owned();
        if base_name != name {
            let base_candidates = context.get_nodes_by_name(&base_name);
            if let Some(first) = base_candidates.first() {
                return Some(first.id.clone());
            }
        }
        return None;
    }

    // Prefer context directories
    const CONTEXT_DIRS: [&str; 6] = [
        "/context/",
        "/contexts/",
        "/src/context/",
        "/src/contexts/",
        "/providers/",
        "/src/providers/",
    ];
    if let Some(first) = candidates
        .iter()
        .find(|n| CONTEXT_DIRS.iter().any(|d| n.file_path.contains(d)))
    {
        return Some(first.id.clone());
    }

    Some(candidates[0].id.clone())
}

/// Convert file path to Next.js route
fn file_path_to_route(file_path: &str) -> Option<String> {
    // pages/index.tsx -> /
    // pages/about.tsx -> /about
    // pages/blog/[slug].tsx -> /blog/:slug
    // app/page.tsx -> /
    // app/about/page.tsx -> /about

    // Only real page-component files are routes. Exclude non-page extensions
    // (.mjs/.json/.cjs), config files (next.config.ts, vite.config.ts…), and
    // Next.js special files (_app/_document). This also stops a `*.config.mjs`
    // with `export default` in a dir like `nextjs-pages/` from being a "route".
    static PAGE_EXT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.(tsx?|jsx?)$").unwrap());
    static CONFIG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.config\.[a-z]+$").unwrap());
    static PAGES_SEG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|/)pages/").unwrap());
    static APP_SEG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|/)app/").unwrap());
    static PAGES_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^.*pages/").unwrap());
    static APP_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^.*app/").unwrap());
    static INDEX_FILE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"/index\.(tsx?|jsx?)$").unwrap());
    static PAGE_FILE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"/page\.(tsx?|jsx?)$").unwrap());
    static PARAM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]").unwrap());

    let base = file_path.split('/').next_back().unwrap_or("");
    if !PAGE_EXT_RE.is_match(base) {
        return None;
    }
    if base.starts_with('_') || CONFIG_RE.is_match(base) {
        return None;
    }

    // Match pages/ and app/ as PATH SEGMENTS (not a substring — `nextjs-pages/`
    // must not count as a `pages/` router dir).
    if PAGES_SEG_RE.is_match(file_path) {
        let route = PAGES_PREFIX_RE.replace(file_path, "/");
        let route = INDEX_FILE_RE.replace(&route, "");
        let route = PAGE_EXT_RE.replace(&route, "");
        let mut route = PARAM_RE.replace_all(&route, ":${1}").into_owned();

        if route.is_empty() {
            route = "/".to_string();
        }
        return Some(route);
    }

    if APP_SEG_RE.is_match(file_path) {
        // App router - only page.tsx files are routes
        if !file_path.contains("page.") {
            return None;
        }

        let route = APP_PREFIX_RE.replace(file_path, "/");
        let route = PAGE_FILE_RE.replace(&route, "");
        let mut route = PARAM_RE.replace_all(&route, ":${1}").into_owned();

        if route.is_empty() {
            route = "/".to_string();
        }
        return Some(route);
    }

    None
}
