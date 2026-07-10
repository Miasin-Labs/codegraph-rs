//! Spring application-event synthesis.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{enclosing_fn, line_of};
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Node, NodeKind};

static SPRING_PUBLISH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.publishEvent\s*\(\s*new\s+([A-Z][A-Za-z0-9_]*)")
        .expect("valid Spring publish regex")
});
static SPRING_LISTENER_ANNO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@(?:EventListener|TransactionalEventListener)\b")
        .expect("valid Spring listener regex")
});
static SPRING_ANNO_TYPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@(?:EventListener|TransactionalEventListener)\s*\(\s*([A-Z][A-Za-z0-9_]*)\.class")
        .expect("valid Spring annotation type regex")
});
static SPRING_APP_LISTENER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bApplicationListener\s*<").expect("valid Spring interface regex")
});

const SPRING_FANOUT_CAP: usize = 80;

fn spring_first_param_type(signature: Option<&str>) -> Option<String> {
    let signature = signature?;
    let open = signature.find('(')?;
    let rest = &signature[open + 1..];
    let inner = rest.split_once(')').map_or(rest, |(inner, _)| inner).trim();
    if inner.is_empty() {
        return None;
    }
    let first = inner.split(',').next()?.trim();
    let tokens: Vec<&str> = first
        .split_whitespace()
        .filter(|token| *token != "final" && !token.starts_with('@'))
        .collect();
    if tokens.len() < 2 {
        return None;
    }
    let candidate = tokens[tokens.len() - 2]
        .split('<')
        .next()
        .unwrap_or_default();
    let mut chars = candidate.chars();
    if !chars.next().is_some_and(|first| first.is_ascii_uppercase())
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some(candidate.to_string())
}

fn listener_annotation_head(lines: &[&str], node: &Node) -> String {
    let start = node.start_line.saturating_sub(1) as usize;
    let end = (node.start_line as usize + 7).min(lines.len());
    let mut annotations = Vec::new();
    for line in lines.iter().take(end).skip(start) {
        let trimmed = line.trim();
        if !trimmed.starts_with('@') {
            break;
        }
        annotations.push(trimmed);
    }
    annotations.join("\n")
}

/// Link `publishEvent(new X(...))` sites to Spring listeners for event type X.
pub(super) fn spring_event_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut listeners: HashMap<String, Vec<Node>> = HashMap::new();
    let mut publisher_files = Vec::new();

    for file in ctx.get_all_files() {
        if !file.ends_with(".java") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if content.contains(".publishEvent(") {
            publisher_files.push(file.clone());
        }
        let has_annotation =
            content.contains("@EventListener") || content.contains("@TransactionalEventListener");
        let has_app_listener = SPRING_APP_LISTENER_RE.is_match(&content);
        if !has_annotation && !has_app_listener {
            continue;
        }

        let lines: Vec<&str> = content.split('\n').collect();
        for node in ctx.get_nodes_in_file(&file) {
            if node.kind != NodeKind::Method {
                continue;
            }
            let head = listener_annotation_head(&lines, &node);
            let annotated = has_annotation && SPRING_LISTENER_ANNO_RE.is_match(&head);
            let application_listener = has_app_listener && node.name == "onApplicationEvent";
            if !annotated && !application_listener {
                continue;
            }

            let event_type = spring_first_param_type(node.signature.as_deref()).or_else(|| {
                annotated.then(|| {
                    SPRING_ANNO_TYPE_RE
                        .captures(&head)
                        .and_then(|captures| captures.get(1))
                        .map(|capture| capture.as_str().to_string())
                })?
            });
            let Some(event_type) = event_type else {
                continue;
            };
            listeners.entry(event_type).or_default().push(node);
        }
    }
    if listeners.is_empty() {
        return Vec::new();
    }

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for file in publisher_files {
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if !content.contains(".publishEvent(") {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Java);
        let nodes = ctx.get_nodes_in_file(&file);
        let mut added = 0usize;
        for captures in SPRING_PUBLISH_RE.captures_iter(&safe) {
            if added >= SPRING_FANOUT_CAP {
                break;
            }
            let Some(whole) = captures.get(0) else {
                continue;
            };
            let Some(event_type) = captures.get(1).map(|capture| capture.as_str()) else {
                continue;
            };
            let Some(targets) = listeners.get(event_type) else {
                continue;
            };
            let line = line_of(&safe, whole.start());
            let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                continue;
            };
            for target in targets {
                if added >= SPRING_FANOUT_CAP {
                    break;
                }
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
                        ("synthesizedBy", Value::from("spring-event")),
                        ("via", Value::from(event_type)),
                        ("registeredAt", Value::from(format!("{file}:{line}"))),
                    ]),
                ));
                added += 1;
            }
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

    fn method(
        id: &str,
        name: &str,
        file: &str,
        start: u32,
        end: u32,
        signature: Option<&str>,
    ) -> Node {
        let mut node = Node::new(
            id,
            NodeKind::Method,
            name,
            format!("{file}::{name}"),
            file,
            Language::Java,
            start,
            end,
        );
        node.signature = signature.map(str::to_string);
        node
    }

    #[test]
    fn extracts_first_java_parameter_type() {
        assert_eq!(
            spring_first_param_type(Some("void (final PasswordChangedEvent event)")),
            Some("PasswordChangedEvent".into())
        );
        assert_eq!(spring_first_param_type(Some("void (int count)")), None);
        assert_eq!(spring_first_param_type(Some("void ()")), None);
    }

    #[test]
    fn links_publisher_to_annotated_listener() {
        let fixture = Fixture {
            files: HashMap::from([
                (
                    "Listener.java".into(),
                    "class Listener {\n  @EventListener\n  void changed(PasswordChangedEvent event) {}\n}\n"
                        .into(),
                ),
                (
                    "Publisher.java".into(),
                    "class Publisher {\n  void change() {\n    events.publishEvent(new PasswordChangedEvent());\n  }\n}\n"
                        .into(),
                ),
            ]),
            nodes: vec![
                method(
                    "listener",
                    "changed",
                    "Listener.java",
                    2,
                    3,
                    Some("void (PasswordChangedEvent event)"),
                ),
                method("publisher", "change", "Publisher.java", 2, 4, None),
            ],
        };

        let edges = spring_event_edges(&fixture);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "publisher");
        assert_eq!(edges[0].target, "listener");
        assert_eq!(edges[0].line, Some(3));
        assert_eq!(
            edges[0].metadata.as_ref().unwrap()["via"],
            "PasswordChangedEvent"
        );
    }

    #[test]
    fn annotation_value_supplies_type_for_no_arg_listener() {
        let fixture = Fixture {
            files: HashMap::from([
                (
                    "Listener.java".into(),
                    "class Listener {\n  @EventListener(OrderPlaced.class)\n  void changed() {}\n}\n"
                        .into(),
                ),
                (
                    "Publisher.java".into(),
                    "class Publisher {\n  void publish() { events.publishEvent(new OrderPlaced()); }\n}\n"
                        .into(),
                ),
            ]),
            nodes: vec![
                method("listener", "changed", "Listener.java", 2, 3, None),
                method("publisher", "publish", "Publisher.java", 2, 2, None),
            ],
        };
        assert_eq!(spring_event_edges(&fixture).len(), 1);
    }
}
