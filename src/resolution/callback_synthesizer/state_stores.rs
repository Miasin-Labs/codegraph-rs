//! Dynamic state-management dispatch synthesis.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{count_newlines, enclosing_fn, node_source};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Language, Node, NodeKind};

const THUNK_FANOUT_CAP: usize = 24;
const PINIA_FANOUT_CAP: usize = 80;
const VUEX_FANOUT_CAP: usize = 120;
const RTK_GENERATED_HOOK_SIGNATURE: &str = "= RTK Query generated hook";

static THUNK_DECL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"create(?:Async)?Thunk").expect("valid regex"));
static THUNK_DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)dispatch\s*\(\s*([A-Za-z_][0-9A-Za-z_]*)\s*[(),]").expect("valid regex")
});
static RTK_HOOK_DERIVE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^use([A-Z][0-9A-Za-z]*?)(?:Query|Mutation)$").expect("valid regex")
});
static PINIA_FACTORY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)(?:export\s+)?const\s+([0-9A-Za-z_]+)\s*=\s*defineStore\s*\(")
        .expect("valid regex")
});
static PINIA_BIND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)const\s+([0-9A-Za-z_]+)\s*=\s*(?:await\s+)?([0-9A-Za-z_]+)\s*\(")
        .expect("valid regex")
});
static PINIA_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([0-9A-Za-z_]+)\s*\.\s*([0-9A-Za-z_]+)\s*\(").expect("valid regex")
});
static VUEX_DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?-u:\b)(?:dispatch|commit)\s*\(\s*["']([A-Za-z][0-9A-Za-z_/]*)["']"#)
        .expect("valid regex")
});
static VUEX_STORE_SIGNAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?-u:\b)(?:defineStore|createStore|Vuex|mutations|actions|getters|namespaced)(?-u:\b)",
    )
    .expect("valid regex")
});

fn js_comment_language(language: Language) -> CommentLang {
    if matches!(language, Language::Javascript | Language::Jsx) {
        CommentLang::Javascript
    } else {
        CommentLang::Typescript
    }
}

fn source_comment_language(file: &str) -> CommentLang {
    if file.ends_with(".js")
        || file.ends_with(".jsx")
        || file.ends_with(".mjs")
        || file.ends_with(".cjs")
    {
        CommentLang::Javascript
    } else {
        CommentLang::Typescript
    }
}

fn is_consumer_file(file: &str) -> bool {
    [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".vue"]
        .iter()
        .any(|ext| file.ends_with(ext))
}

/// Bridge `dispatch(nextThunk(...))` calls from a thunk constant to the dispatched thunk.
pub(super) fn redux_thunk_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let mut seen = HashSet::new();

    queries.iterate_nodes_by_kind(NodeKind::Constant, |node| {
        let is_thunk = node
            .signature
            .as_deref()
            .is_some_and(|signature| THUNK_DECL_RE.is_match(signature));
        if !is_thunk {
            return true;
        }
        let Some(source) = node_source(ctx, &node) else {
            return true;
        };
        let safe = strip_comments_for_regex(&source, js_comment_language(node.language));
        let mut added = 0usize;
        for capture in THUNK_DISPATCH_RE.captures_iter(&safe) {
            if added >= THUNK_FANOUT_CAP {
                break;
            }
            let matched = capture.get(0).expect("full dispatch match");
            let name = capture.get(1).expect("dispatch name").as_str();
            if name == node.name {
                continue;
            }
            let candidates: Vec<Node> = ctx
                .get_nodes_by_name(name)
                .into_iter()
                .filter(|candidate| {
                    matches!(
                        candidate.kind,
                        NodeKind::Constant | NodeKind::Function | NodeKind::Method
                    )
                })
                .collect();
            let target = candidates
                .iter()
                .find(|candidate| {
                    candidate
                        .signature
                        .as_deref()
                        .is_some_and(|signature| THUNK_DECL_RE.is_match(signature))
                })
                .or_else(|| {
                    candidates
                        .iter()
                        .find(|candidate| candidate.kind == NodeKind::Constant)
                })
                .or_else(|| {
                    candidates
                        .iter()
                        .find(|candidate| candidate.file_path == node.file_path)
                })
                .or_else(|| candidates.first());
            let Some(target) = target else { continue };
            if target.id == node.id || !seen.insert(format!("{}>{}", node.id, target.id)) {
                continue;
            }
            let line = node.start_line + count_newlines(&safe[..matched.start()]);
            edges.push(synthesized_edge(
                &node.id,
                &target.id,
                Some(line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("redux-thunk")),
                    ("via", Value::from(name)),
                    (
                        "registeredAt",
                        Value::from(format!("{}:{}", node.file_path, line)),
                    ),
                ]),
            ));
            added += 1;
        }
        true
    })?;
    Ok(edges)
}

/// Derive an RTK endpoint key from `use[Lazy]Endpoint(Query|Mutation)`.
pub(super) fn rtk_endpoint_name_from_hook(hook: &str) -> Option<String> {
    let captures = RTK_HOOK_DERIVE_RE.captures(hook)?;
    let mut middle = captures.get(1)?.as_str();
    if let Some(without_lazy) = middle.strip_prefix("Lazy") {
        middle = without_lazy;
    }
    let mut chars = middle.chars();
    let first = chars.next()?;
    Some(first.to_lowercase().collect::<String>() + chars.as_str())
}

/// Link an extracted RTK generated-hook sentinel node to its same-file endpoint.
pub(super) fn rtk_query_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    queries.iterate_nodes_by_kind(NodeKind::Function, |hook| {
        if hook.signature.as_deref() != Some(RTK_GENERATED_HOOK_SIGNATURE) {
            return true;
        }
        let Some(endpoint_name) = rtk_endpoint_name_from_hook(&hook.name) else {
            return true;
        };
        let target = ctx
            .get_nodes_by_name(&endpoint_name)
            .into_iter()
            .find(|candidate| {
                candidate.kind == NodeKind::Function && candidate.file_path == hook.file_path
            });
        let Some(target) = target else { return true };
        if target.id == hook.id || !seen.insert(format!("{}>{}", hook.id, target.id)) {
            return true;
        }
        edges.push(synthesized_edge(
            &hook.id,
            &target.id,
            Some(hook.start_line),
            edge_meta(vec![
                ("synthesizedBy", Value::from("rtk-query")),
                ("via", Value::from(endpoint_name.as_str())),
                (
                    "registeredAt",
                    Value::from(format!("{}:{}", hook.file_path, hook.start_line)),
                ),
            ]),
        ));
        true
    })?;
    Ok(edges)
}

/// Bridge `const store = useXStore(); store.action()` to the Pinia action node.
pub(super) fn pinia_store_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let files = ctx.get_all_files();
    let mut factory_file: HashMap<String, String> = HashMap::new();

    for file in &files {
        if !is_consumer_file(file) {
            continue;
        }
        let Some(content) = ctx.read_file(file) else {
            continue;
        };
        if !content.contains("defineStore") {
            continue;
        }
        for capture in PINIA_FACTORY_RE.captures_iter(&content) {
            factory_file.insert(
                capture.get(1).expect("factory name").as_str().to_string(),
                file.clone(),
            );
        }
    }
    if factory_file.is_empty() {
        return Vec::new();
    }

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for file in &files {
        if !is_consumer_file(file) {
            continue;
        }
        let Some(content) = ctx.read_file(file) else {
            continue;
        };
        if !content.contains("Store") {
            continue;
        }
        let safe = strip_comments_for_regex(&content, source_comment_language(file));
        let mut var_store: HashMap<String, String> = HashMap::new();
        for capture in PINIA_BIND_RE.captures_iter(&safe) {
            let factory = capture.get(2).expect("factory binding").as_str();
            if let Some(store_file) = factory_file.get(factory) {
                var_store.insert(
                    capture.get(1).expect("store variable").as_str().to_string(),
                    store_file.clone(),
                );
            }
        }
        if var_store.is_empty() {
            continue;
        }

        let nodes_in_file = ctx.get_nodes_in_file(file);
        let fallback = nodes_in_file
            .iter()
            .find(|node| node.kind == NodeKind::Component);
        let mut added = 0usize;
        for capture in PINIA_CALL_RE.captures_iter(&safe) {
            if added >= PINIA_FANOUT_CAP {
                break;
            }
            let matched = capture.get(0).expect("store method call");
            let store_var = capture.get(1).expect("store variable").as_str();
            let Some(store_file) = var_store.get(store_var) else {
                continue;
            };
            let method = capture.get(2).expect("store method").as_str();
            let line = count_newlines(&safe[..matched.start()]) + 1;
            let Some(dispatcher) = enclosing_fn(&nodes_in_file, line).or(fallback) else {
                continue;
            };
            let target = ctx.get_nodes_by_name(method).into_iter().find(|candidate| {
                candidate.kind == NodeKind::Function && candidate.file_path == *store_file
            });
            let Some(target) = target else { continue };
            if target.id == dispatcher.id
                || !seen.insert(format!("{}>{}", dispatcher.id, target.id))
            {
                continue;
            }
            edges.push(synthesized_edge(
                &dispatcher.id,
                &target.id,
                Some(line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("pinia-store")),
                    ("via", Value::from(method)),
                    ("registeredAt", Value::from(format!("{}:{}", file, line))),
                ]),
            ));
            added += 1;
        }
    }
    edges
}

fn path_has_segment(file_path: &str, segment: &str) -> bool {
    let normalized = file_path.replace('\\', "/");
    let normalized = format!("/{normalized}");
    normalized.contains(&format!("/{segment}/")) || normalized.contains(&format!("/{segment}."))
}

fn is_vue_store_file(
    ctx: &dyn ResolutionContext,
    cache: &mut HashMap<String, bool>,
    file: &str,
) -> bool {
    if let Some(cached) = cache.get(file) {
        return *cached;
    }
    let is_store = ctx.read_file(file).is_some_and(|content| {
        let distinct: HashSet<&str> = VUEX_STORE_SIGNAL
            .find_iter(&content)
            .map(|matched| matched.as_str())
            .collect();
        distinct.len() >= 2
    });
    cache.insert(file.to_string(), is_store);
    is_store
}

fn resolve_vuex_target(
    ctx: &dyn ResolutionContext,
    cache: &mut HashMap<String, bool>,
    key: &str,
    dispatch_file: &str,
) -> Option<Node> {
    let segments: Vec<&str> = key.split('/').collect();
    let action = *segments.last()?;
    let mut candidates = Vec::new();
    for candidate in ctx.get_nodes_by_name(action) {
        if candidate.kind == NodeKind::Function
            && is_vue_store_file(ctx, cache, &candidate.file_path)
        {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        return None;
    }
    if segments.len() > 1 {
        let namespace = segments[segments.len() - 2];
        return candidates
            .iter()
            .find(|candidate| path_has_segment(&candidate.file_path, namespace))
            .cloned()
            .or_else(|| (candidates.len() == 1).then(|| candidates[0].clone()));
    }
    candidates
        .iter()
        .find(|candidate| candidate.file_path == dispatch_file)
        .cloned()
        .or_else(|| (candidates.len() == 1).then(|| candidates[0].clone()))
}

/// Bridge Vuex `dispatch('namespace/action')` / `commit('mutation')` keys.
pub(super) fn vuex_dispatch_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut store_file_cache = HashMap::new();
    let mut edges = Vec::new();
    let mut seen = HashSet::new();

    for file in ctx.get_all_files() {
        if !is_consumer_file(&file) {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if !content.contains("dispatch(") && !content.contains("commit(") {
            continue;
        }
        let safe = strip_comments_for_regex(&content, source_comment_language(&file));
        let nodes_in_file = ctx.get_nodes_in_file(&file);
        let fallback = nodes_in_file
            .iter()
            .find(|node| node.kind == NodeKind::Component);
        let mut added = 0usize;
        for capture in VUEX_DISPATCH_RE.captures_iter(&safe) {
            if added >= VUEX_FANOUT_CAP {
                break;
            }
            let matched = capture.get(0).expect("vuex dispatch");
            let key = capture.get(1).expect("vuex key").as_str();
            let line = count_newlines(&safe[..matched.start()]) + 1;
            let Some(dispatcher) = enclosing_fn(&nodes_in_file, line).or(fallback) else {
                continue;
            };
            let Some(target) = resolve_vuex_target(ctx, &mut store_file_cache, key, &file) else {
                continue;
            };
            if target.id == dispatcher.id
                || !seen.insert(format!("{}>{}", dispatcher.id, target.id))
            {
                continue;
            }
            edges.push(synthesized_edge(
                &dispatcher.id,
                &target.id,
                Some(line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("vuex-dispatch")),
                    ("via", Value::from(key)),
                    ("registeredAt", Value::from(format!("{}:{}", file, line))),
                ]),
            ));
            added += 1;
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolution::types::ImportMapping;

    struct Fixture {
        files: HashMap<String, String>,
        nodes: Vec<Node>,
    }

    impl ResolutionContext for Fixture {
        fn get_nodes_in_file(&self, path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.file_path == path)
                .cloned()
                .collect()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.qualified_name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.kind == kind)
                .cloned()
                .collect()
        }
        fn file_exists(&self, path: &str) -> bool {
            self.files.contains_key(path)
        }
        fn read_file(&self, path: &str) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn get_project_root(&self) -> &str {
            "/project"
        }
        fn get_all_files(&self) -> Vec<String> {
            self.files.keys().cloned().collect()
        }
        fn get_nodes_by_lower_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.name.to_lowercase() == name)
                .cloned()
                .collect()
        }
        fn get_import_mappings(&self, _: &str, _: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn node(id: &str, kind: NodeKind, name: &str, file: &str, start: u32, end: u32) -> Node {
        Node::new(
            id,
            kind,
            name,
            format!("{file}::{name}"),
            file,
            Language::Typescript,
            start,
            end,
        )
    }

    #[test]
    fn derives_rtk_endpoint_names() {
        assert_eq!(
            rtk_endpoint_name_from_hook("useGetRecordsQuery").as_deref(),
            Some("getRecords")
        );
        assert_eq!(
            rtk_endpoint_name_from_hook("useLazyGetRecordsQuery").as_deref(),
            Some("getRecords")
        );
        assert_eq!(
            rtk_endpoint_name_from_hook("useUpdateRecordMutation").as_deref(),
            Some("updateRecord")
        );
        assert!(rtk_endpoint_name_from_hook("useRecords").is_none());
    }

    #[test]
    fn links_pinia_store_method_to_store_action() {
        let fixture = Fixture {
            files: HashMap::from([
                (
                    "stores/user.ts".into(),
                    "export const useUserStore = defineStore('user', { actions: {} })".into(),
                ),
                (
                    "views/login.ts".into(),
                    "function submit() {\n  const user = useUserStore()\n  user.login()\n}".into(),
                ),
            ]),
            nodes: vec![
                node(
                    "submit",
                    NodeKind::Function,
                    "submit",
                    "views/login.ts",
                    1,
                    4,
                ),
                node("login", NodeKind::Function, "login", "stores/user.ts", 1, 1),
            ],
        };
        let edges = pinia_store_edges(&fixture);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (edges[0].source.as_str(), edges[0].target.as_str()),
            ("submit", "login")
        );
    }

    #[test]
    fn resolves_namespaced_vuex_dispatch_to_store_file() {
        let fixture = Fixture {
            files: HashMap::from([
                (
                    "stores/user.ts".into(),
                    "export default createStore({ actions: { login() {} }, mutations: {} })".into(),
                ),
                (
                    "views/login.ts".into(),
                    "function submit() {\n  dispatch('user/login')\n}".into(),
                ),
            ]),
            nodes: vec![
                node(
                    "submit",
                    NodeKind::Function,
                    "submit",
                    "views/login.ts",
                    1,
                    3,
                ),
                node("login", NodeKind::Function, "login", "stores/user.ts", 1, 1),
            ],
        };
        let edges = vuex_dispatch_edges(&fixture);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (edges[0].source.as_str(), edges[0].target.as_str()),
            ("submit", "login")
        );
    }

    #[test]
    fn path_segment_matching_is_component_bounded() {
        assert!(path_has_segment("src/stores/user.ts", "user"));
        assert!(path_has_segment("src/user/actions.ts", "user"));
        assert!(!path_has_segment("src/stores/superuser.ts", "user"));
    }
}
