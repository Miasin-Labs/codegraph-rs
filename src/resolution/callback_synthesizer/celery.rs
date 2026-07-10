//! Python Celery task-dispatch synthesis.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{enclosing_fn, line_of};
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Node, NodeKind};

static CELERY_DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([A-Za-z_]\w*)\s*\.\s*(?:delay|apply_async)\s*\(")
        .expect("valid Celery dispatch regex")
});
static CELERY_TASK_DECORATOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@\s*(?:[A-Za-z_][\w.]*\.)?(?:shared_task|task)\b")
        .expect("valid Celery decorator regex")
});
static PREVIOUS_PY_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:async\s+def|def|class)\b").expect("valid Python declaration regex")
});

const CELERY_FANOUT_CAP: usize = 80;
const CELERY_DECORATOR_LOOKBACK: usize = 12;

fn is_celery_task(
    ctx: &dyn ResolutionContext,
    node: &Node,
    cache: &mut HashMap<String, bool>,
) -> bool {
    if let Some(cached) = cache.get(&node.id) {
        return *cached;
    }

    let mut matched = false;
    if node.kind == NodeKind::Function && node.file_path.ends_with(".py") {
        if let Some(content) = ctx.read_file(&node.file_path) {
            let lines: Vec<&str> = content.split('\n').collect();
            let last = node.start_line.saturating_sub(2) as usize;
            let stop =
                node.start_line
                    .saturating_sub(1 + CELERY_DECORATOR_LOOKBACK as u32) as usize;
            if last < lines.len() && last >= stop {
                for index in (stop..=last).rev() {
                    let line = lines.get(index).copied().unwrap_or_default().trim();
                    if PREVIOUS_PY_DECL_RE.is_match(line) {
                        break;
                    }
                    if CELERY_TASK_DECORATOR_RE.is_match(line) {
                        matched = true;
                        break;
                    }
                }
            }
        }
    }
    cache.insert(node.id.clone(), matched);
    matched
}

fn resolve_task(
    ctx: &dyn ResolutionContext,
    name: &str,
    dispatch_file: &str,
    cache: &mut HashMap<String, bool>,
) -> Option<Node> {
    let candidates: Vec<Node> = ctx
        .get_nodes_by_name(name)
        .into_iter()
        .filter(|node| node.kind == NodeKind::Function && is_celery_task(ctx, node, cache))
        .collect();
    match candidates.as_slice() {
        [] => None,
        [only] => Some(only.clone()),
        many => many
            .iter()
            .find(|candidate| candidate.file_path == dispatch_file)
            .cloned(),
    }
}

/// Link `.delay(...)` and `.apply_async(...)` sites to decorated Celery tasks.
pub(super) fn celery_dispatch_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut task_cache = HashMap::new();
    let mut seen = HashSet::new();
    let mut edges = Vec::new();

    for file in ctx.get_all_files() {
        if !file.ends_with(".py") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if !content.contains(".delay(") && !content.contains(".apply_async(") {
            continue;
        }

        let safe = strip_comments_for_regex(&content, CommentLang::Python);
        let nodes = ctx.get_nodes_in_file(&file);
        let mut added = 0usize;
        for captures in CELERY_DISPATCH_RE.captures_iter(&safe) {
            if added >= CELERY_FANOUT_CAP {
                break;
            }
            let Some(whole) = captures.get(0) else {
                continue;
            };
            let Some(name) = captures.get(1).map(|capture| capture.as_str()) else {
                continue;
            };
            let line = line_of(&safe, whole.start());
            let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                continue;
            };
            let Some(target) = resolve_task(ctx, name, &file, &mut task_cache) else {
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
                    ("synthesizedBy", Value::from("celery-dispatch")),
                    ("via", Value::from(name)),
                    ("registeredAt", Value::from(format!("{file}:{line}"))),
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
    use crate::types::{Language, NodeKind};

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

    fn node(id: &str, name: &str, file: &str, start: u32, end: u32) -> Node {
        Node::new(
            id,
            NodeKind::Function,
            name,
            format!("{file}::{name}"),
            file,
            Language::Python,
            start,
            end,
        )
    }

    #[test]
    fn links_dispatch_to_decorated_task_and_ignores_comments() {
        let fixture = Fixture {
            files: HashMap::from([
                (
                    "tasks.py".into(),
                    "@shared_task\ndef process(ids):\n    return ids\n\ndef ordinary():\n    pass\n"
                        .into(),
                ),
                (
                    "views.py".into(),
                    "def view():\n    # ordinary.delay()\n    process.delay([1])\n".into(),
                ),
            ]),
            nodes: vec![
                node("task", "process", "tasks.py", 2, 3),
                node("ordinary", "ordinary", "tasks.py", 5, 6),
                node("view", "view", "views.py", 1, 3),
            ],
        };

        let edges = celery_dispatch_edges(&fixture);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "view");
        assert_eq!(edges[0].target, "task");
        assert_eq!(edges[0].line, Some(3));
        assert_eq!(
            edges[0].metadata.as_ref().unwrap()["synthesizedBy"],
            "celery-dispatch"
        );
    }

    #[test]
    fn previous_declaration_stops_decorator_inheritance() {
        let fixture = Fixture {
            files: HashMap::from([
                (
                    "tasks.py".into(),
                    "@shared_task\ndef first():\n    pass\n\ndef second():\n    pass\n".into(),
                ),
                (
                    "views.py".into(),
                    "def view():\n    second.apply_async()\n".into(),
                ),
            ]),
            nodes: vec![
                node("first", "first", "tasks.py", 2, 3),
                node("second", "second", "tasks.py", 5, 6),
                node("view", "view", "views.py", 1, 2),
            ],
        };
        assert!(celery_dispatch_edges(&fixture).is_empty());
    }
}
