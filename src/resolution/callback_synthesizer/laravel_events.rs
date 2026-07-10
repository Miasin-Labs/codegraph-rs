//! Laravel event-to-listener synthesis.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{enclosing_fn, line_of};
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Node, NodeKind};

static LARAVEL_DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bevent\s*\(\s*new\s+\\?([A-Za-z_][\w\\]*)").expect("valid Laravel dispatch regex")
});
static LISTEN_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$listen\s*=\s*\[").expect("valid Laravel listen declaration regex")
});
static LISTEN_ENTRY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:([A-Za-z_\\][\w\\]*)::class|'([^']+)'|"([^"]+)")\s*=>\s*\[([^\]]*)\]"#)
        .expect("valid Laravel listen entry regex")
});
static LISTEN_CLASS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:([A-Za-z_\\][\w\\]*)::class|'([^']+)'|"([^"]+)")"#)
        .expect("valid Laravel listener class regex")
});
static HANDLE_EVENT_TYPES_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"function\s+handle\s*\(\s*(?:\.\.\.\s*)?(\??[A-Za-z_\\][\w\\|]*)\s+&?\s*(?:\.\.\.\s*)?\$",
    )
    .expect("valid Laravel handle signature regex")
});
static PHP_CLASS_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z]\w*$").expect("valid PHP class-name regex"));

const LARAVEL_FANOUT_CAP: usize = 200;

fn php_simple_name(reference: &str) -> String {
    reference
        .trim_start_matches('\\')
        .rsplit('\\')
        .next()
        .unwrap_or_default()
        .rsplit("::")
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn laravel_handle_event_types(declaration: &str) -> Vec<String> {
    let Some(types) = HANDLE_EVENT_TYPES_RE
        .captures(declaration)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().trim_start_matches('?'))
    else {
        return Vec::new();
    };
    types
        .split('|')
        .map(php_simple_name)
        .filter(|name| PHP_CLASS_NAME_RE.is_match(name))
        .collect()
}

fn php_array_body(source: &str, open_index: usize) -> Option<&str> {
    let mut depth = 0usize;
    for (offset, byte) in source.as_bytes()[open_index..].iter().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return source.get(open_index + 1..open_index + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn handle_of(ctx: &dyn ResolutionContext, class: &Node) -> Option<Node> {
    ctx.get_nodes_in_file(&class.file_path)
        .into_iter()
        .find(|node| {
            node.kind == NodeKind::Method
                && node.name == "handle"
                && node.start_line >= class.start_line
                && node.start_line <= class.end_line
        })
}

fn add_listener(
    listeners: &mut HashMap<String, HashMap<String, Node>>,
    event: String,
    handle: Node,
) {
    listeners
        .entry(event)
        .or_default()
        .insert(handle.id.clone(), handle);
}

/// Link `event(new X(...))` sites to Laravel listener `handle` methods for X.
pub(super) fn laravel_event_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut listeners: HashMap<String, HashMap<String, Node>> = HashMap::new();

    for file in ctx.get_all_files() {
        if !file.ends_with(".php") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };

        if content.contains("function handle") {
            let lines: Vec<&str> = content.split('\n').collect();
            for node in ctx.get_nodes_in_file(&file) {
                if node.kind != NodeKind::Method || node.name != "handle" {
                    continue;
                }
                let start = node.start_line.saturating_sub(1) as usize;
                let end = (node.start_line as usize + 2).min(lines.len());
                let declaration = lines[start..end].join("\n");
                for event in laravel_handle_event_types(&declaration) {
                    add_listener(&mut listeners, event, node.clone());
                }
            }
        }

        if content.contains("$listen") {
            let safe = strip_comments_for_regex(&content, CommentLang::Php);
            if let Some(declaration) = LISTEN_DECL_RE.find(&safe) {
                if let Some(relative_open) = safe[declaration.start()..].find('[') {
                    let open = declaration.start() + relative_open;
                    if let Some(body) = php_array_body(&safe, open) {
                        for entry in LISTEN_ENTRY_RE.captures_iter(body) {
                            let event = php_simple_name(
                                entry
                                    .get(1)
                                    .or_else(|| entry.get(2))
                                    .or_else(|| entry.get(3))
                                    .map(|capture| capture.as_str())
                                    .unwrap_or_default(),
                            );
                            let Some(listener_body) = entry.get(4).map(|capture| capture.as_str())
                            else {
                                continue;
                            };
                            for listener_match in LISTEN_CLASS_RE.captures_iter(listener_body) {
                                let listener_name = php_simple_name(
                                    listener_match
                                        .get(1)
                                        .or_else(|| listener_match.get(2))
                                        .or_else(|| listener_match.get(3))
                                        .map(|capture| capture.as_str())
                                        .unwrap_or_default(),
                                );
                                let listener = ctx
                                    .get_nodes_by_name(&listener_name)
                                    .into_iter()
                                    .find(|node| {
                                        node.kind == NodeKind::Class
                                            && handle_of(ctx, node).is_some()
                                    });
                                if let Some(class) = listener {
                                    if let Some(handle) = handle_of(ctx, &class) {
                                        add_listener(&mut listeners, event.clone(), handle);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if listeners.is_empty() {
        return Vec::new();
    }

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for file in ctx.get_all_files() {
        if !file.ends_with(".php") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if !content.contains("event(") {
            continue;
        }

        let safe = strip_comments_for_regex(&content, CommentLang::Php);
        let nodes = ctx.get_nodes_in_file(&file);
        let mut added = 0usize;
        for captures in LARAVEL_DISPATCH_RE.captures_iter(&safe) {
            if added >= LARAVEL_FANOUT_CAP {
                break;
            }
            let Some(whole) = captures.get(0) else {
                continue;
            };
            let Some(event) = captures
                .get(1)
                .map(|capture| php_simple_name(capture.as_str()))
            else {
                continue;
            };
            let Some(targets) = listeners.get(&event) else {
                continue;
            };
            let line = line_of(&safe, whole.start());
            let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                continue;
            };
            for target in targets.values() {
                if added >= LARAVEL_FANOUT_CAP {
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
                        ("synthesizedBy", Value::from("laravel-event")),
                        ("via", Value::from(event.as_str())),
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

    fn node(id: &str, kind: NodeKind, name: &str, file: &str, start: u32, end: u32) -> Node {
        Node::new(
            id,
            kind,
            name,
            format!("{file}::{name}"),
            file,
            Language::Php,
            start,
            end,
        )
    }

    #[test]
    fn parses_typed_and_union_listener_parameters() {
        assert_eq!(
            laravel_handle_event_types(
                "public function handle(?App\\Events\\Started|Stopped $event): void"
            ),
            vec!["Started", "Stopped"]
        );
        assert!(laravel_handle_event_types("function handle(string $value)").is_empty());
    }

    #[test]
    fn links_dispatch_to_auto_discovered_typed_listener() {
        let fixture = Fixture {
            files: HashMap::from([
                (
                    "Listener.php".into(),
                    "<?php\nclass UpdatePlayback {\n  public function handle(PlaybackStarted $event): void {}\n}\n"
                        .into(),
                ),
                (
                    "Service.php".into(),
                    "<?php\nfunction play() {\n  event(new PlaybackStarted());\n}\n".into(),
                ),
            ]),
            nodes: vec![
                node("listener-class", NodeKind::Class, "UpdatePlayback", "Listener.php", 2, 4),
                node("handle", NodeKind::Method, "handle", "Listener.php", 3, 3),
                node("play", NodeKind::Function, "play", "Service.php", 2, 4),
            ],
        };

        let edges = laravel_event_edges(&fixture);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "play");
        assert_eq!(edges[0].target, "handle");
        assert_eq!(
            edges[0].metadata.as_ref().unwrap()["via"],
            "PlaybackStarted"
        );
    }

    #[test]
    fn listen_map_links_untyped_listener() {
        let fixture = Fixture {
            files: HashMap::from([
                (
                    "Provider.php".into(),
                    "<?php\nprotected $listen = [\n  PlaybackStarted::class => [UpdatePlayback::class],\n];\n"
                        .into(),
                ),
                (
                    "Listener.php".into(),
                    "<?php\nclass UpdatePlayback {\n  public function handle($event): void {}\n}\n"
                        .into(),
                ),
                (
                    "Service.php".into(),
                    "<?php\nfunction play() { event(new PlaybackStarted()); }\n".into(),
                ),
            ]),
            nodes: vec![
                node("listener-class", NodeKind::Class, "UpdatePlayback", "Listener.php", 2, 4),
                node("handle", NodeKind::Method, "handle", "Listener.php", 3, 3),
                node("play", NodeKind::Function, "play", "Service.php", 2, 2),
            ],
        };
        assert_eq!(laravel_event_edges(&fixture).len(), 1);
    }
}
