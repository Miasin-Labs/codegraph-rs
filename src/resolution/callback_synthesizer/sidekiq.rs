use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{enclosing_fn, line_of};
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Node, NodeKind};

static SIDEKIQ_DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Z][A-Za-z0-9_]*(?:::[A-Z][A-Za-z0-9_]*)*)\s*\.\s*perform_(?:async|in|at)\b")
        .expect("valid Sidekiq dispatch regex")
});
static SIDEKIQ_WORKER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\binclude\s+Sidekiq::(?:Job|Worker)\b").expect("valid Sidekiq worker regex")
});

const SIDEKIQ_FANOUT_CAP: usize = 80;

fn class_source(content: &str, class: &Node) -> String {
    let start = class.start_line.saturating_sub(1) as usize;
    let count = class
        .end_line
        .saturating_sub(class.start_line)
        .saturating_add(1) as usize;
    content
        .lines()
        .skip(start)
        .take(count)
        .collect::<Vec<_>>()
        .join("\n")
}

fn perform_of(
    ctx: &dyn ResolutionContext,
    class: &Node,
    cache: &mut HashMap<String, Option<Node>>,
) -> Option<Node> {
    if let Some(cached) = cache.get(&class.id) {
        return cached.clone();
    }

    let perform = ctx.read_file(&class.file_path).and_then(|content| {
        if !SIDEKIQ_WORKER_RE.is_match(&class_source(&content, class)) {
            return None;
        }
        ctx.get_nodes_in_file(&class.file_path)
            .into_iter()
            .find(|node| {
                node.kind == NodeKind::Method
                    && node.name == "perform"
                    && node.start_line >= class.start_line
                    && node.start_line <= class.end_line
            })
    });
    cache.insert(class.id.clone(), perform.clone());
    perform
}

fn resolve_worker_perform(
    ctx: &dyn ResolutionContext,
    worker_ref: &str,
    cache: &mut HashMap<String, Option<Node>>,
) -> Option<Node> {
    if worker_ref.contains("::") {
        for class in ctx.get_nodes_by_qualified_name(worker_ref) {
            if class.kind == NodeKind::Class {
                if let Some(perform) = perform_of(ctx, &class, cache) {
                    return Some(perform);
                }
            }
        }
    }

    let simple_name = worker_ref.rsplit("::").next().unwrap_or(worker_ref);
    let mut workers = ctx
        .get_nodes_by_name(simple_name)
        .into_iter()
        .filter(|node| node.kind == NodeKind::Class)
        .filter_map(|class| perform_of(ctx, &class, cache));
    let worker = workers.next()?;
    if workers.next().is_some() {
        return None;
    }
    Some(worker)
}

/// Bridge Ruby Sidekiq enqueue sites to the selected worker's `perform` method.
pub(super) fn sidekiq_dispatch_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut perform_cache: HashMap<String, Option<Node>> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut edges = Vec::new();

    for file in ctx.get_all_files() {
        if !file.ends_with(".rb") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if !content.contains(".perform_async")
            && !content.contains(".perform_in")
            && !content.contains(".perform_at")
        {
            continue;
        }

        let safe = strip_comments_for_regex(&content, CommentLang::Ruby);
        let nodes_in_file = ctx.get_nodes_in_file(&file);
        let mut added = 0usize;
        for captures in SIDEKIQ_DISPATCH_RE.captures_iter(&safe) {
            if added >= SIDEKIQ_FANOUT_CAP {
                break;
            }
            let Some(whole) = captures.get(0) else {
                continue;
            };
            let Some(worker_ref) = captures.get(1).map(|m| m.as_str()) else {
                continue;
            };
            let line = line_of(&safe, whole.start());
            let Some(dispatcher) = enclosing_fn(&nodes_in_file, line) else {
                continue;
            };
            let Some(target) = resolve_worker_perform(ctx, worker_ref, &mut perform_cache) else {
                continue;
            };
            if target.id == dispatcher.id {
                continue;
            }
            let key = format!("{}>{}", dispatcher.id, target.id);
            if !seen.insert(key) {
                continue;
            }

            edges.push(synthesized_edge(
                &dispatcher.id,
                &target.id,
                Some(line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("sidekiq-dispatch")),
                    ("via", Value::from(worker_ref)),
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
    use std::collections::HashMap;

    use super::*;
    use crate::resolution::types::ImportMapping;
    use crate::types::{Language, NodeKind};

    struct Ctx {
        files: HashMap<String, String>,
        nodes: Vec<Node>,
    }

    impl ResolutionContext for Ctx {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.file_path == file_path)
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

        fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.qualified_name == qualified_name)
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

        fn file_exists(&self, file_path: &str) -> bool {
            self.files.contains_key(file_path)
        }

        fn read_file(&self, file_path: &str) -> Option<String> {
            self.files.get(file_path).cloned()
        }

        fn get_project_root(&self) -> &str {
            ""
        }

        fn get_all_files(&self) -> Vec<String> {
            self.files.keys().cloned().collect()
        }

        fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.name.to_lowercase() == lower_name)
                .cloned()
                .collect()
        }

        fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn node(
        id: &str,
        kind: NodeKind,
        name: &str,
        qualified_name: &str,
        file_path: &str,
        start_line: u32,
        end_line: u32,
    ) -> Node {
        Node::new(
            id,
            kind,
            name,
            qualified_name,
            file_path,
            Language::Ruby,
            start_line,
            end_line,
        )
    }

    fn base_ctx(dispatch: &str) -> Ctx {
        Ctx {
            files: HashMap::from([
                (
                    "app/workers/destroy_user_worker.rb".to_string(),
                    "class DestroyUserWorker\n  include Sidekiq::Worker\n  def perform(user_id)\n  end\nend\n"
                        .to_string(),
                ),
                ("app/services/users.rb".to_string(), dispatch.to_string()),
            ]),
            nodes: vec![
                node(
                    "class:worker",
                    NodeKind::Class,
                    "DestroyUserWorker",
                    "DestroyUserWorker",
                    "app/workers/destroy_user_worker.rb",
                    1,
                    5,
                ),
                node(
                    "method:perform",
                    NodeKind::Method,
                    "perform",
                    "DestroyUserWorker.perform",
                    "app/workers/destroy_user_worker.rb",
                    3,
                    4,
                ),
                node(
                    "method:destroy",
                    NodeKind::Method,
                    "destroy",
                    "Users.destroy",
                    "app/services/users.rb",
                    1,
                    3,
                ),
            ],
        }
    }

    #[test]
    fn links_enqueue_site_to_worker_perform() {
        let ctx = base_ctx("def destroy(user)\n  DestroyUserWorker.perform_async(user.id)\nend\n");
        let edges = sidekiq_dispatch_edges(&ctx);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "method:destroy");
        assert_eq!(edges[0].target, "method:perform");
        assert_eq!(edges[0].line, Some(2));
        assert_eq!(
            edges[0]
                .metadata
                .as_ref()
                .and_then(|m| m.get("synthesizedBy")),
            Some(&Value::from("sidekiq-dispatch"))
        );
    }

    #[test]
    fn ignores_comments_and_non_sidekiq_workers() {
        let mut ctx =
            base_ctx("def destroy(user)\n  # DestroyUserWorker.perform_async(user.id)\nend\n");
        assert!(sidekiq_dispatch_edges(&ctx).is_empty());

        ctx.files.insert(
            "app/services/users.rb".to_string(),
            "def destroy(user)\n  DestroyUserWorker.perform_async(user.id)\nend\n".to_string(),
        );
        ctx.files.insert(
            "app/workers/destroy_user_worker.rb".to_string(),
            "class DestroyUserWorker\n  def perform(user_id)\n  end\nend\n".to_string(),
        );
        assert!(sidekiq_dispatch_edges(&ctx).is_empty());
    }

    #[test]
    fn rejects_ambiguous_unqualified_worker_names() {
        let mut ctx =
            base_ctx("def destroy(user)\n  DestroyUserWorker.perform_in(5, user.id)\nend\n");
        ctx.files.insert(
            "app/workers/admin/destroy_user_worker.rb".to_string(),
            "class Admin::DestroyUserWorker\n  include Sidekiq::Job\n  def perform(user_id)\n  end\nend\n"
                .to_string(),
        );
        ctx.nodes.extend([
            node(
                "class:admin-worker",
                NodeKind::Class,
                "DestroyUserWorker",
                "Admin::DestroyUserWorker",
                "app/workers/admin/destroy_user_worker.rb",
                1,
                5,
            ),
            node(
                "method:admin-perform",
                NodeKind::Method,
                "perform",
                "Admin::DestroyUserWorker.perform",
                "app/workers/admin/destroy_user_worker.rb",
                3,
                4,
            ),
        ]);
        assert!(sidekiq_dispatch_edges(&ctx).is_empty());

        ctx.files.insert(
            "app/services/users.rb".to_string(),
            "def destroy(user)\n  Admin::DestroyUserWorker.perform_at(10, user.id)\nend\n"
                .to_string(),
        );
        let edges = sidekiq_dispatch_edges(&ctx);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, "method:admin-perform");
    }
}
