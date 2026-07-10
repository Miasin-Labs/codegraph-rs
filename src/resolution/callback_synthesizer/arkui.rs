//! HarmonyOS ArkUI dynamic-edge synthesis.

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{enclosing_fn, line_of, node_source};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, EdgeKind, Node, NodeKind};

const MAX_CALLBACKS_PER_CHANNEL: usize = 40;
const ARKUI_EMITTER_FANOUT_CAP: usize = 8;
const ARKUI_ARRAY_MUTATORS: &str = "push|pop|shift|unshift|splice|sort|reverse|fill";

const ARKUI_REACTIVE_DECORATORS: &[&str] = &[
    "State",
    "Prop",
    "Link",
    "Provide",
    "Consume",
    "StorageLink",
    "StorageProp",
    "LocalStorageLink",
    "LocalStorageProp",
    "ObjectLink",
    "Local",
    "Provider",
    "Consumer",
];

static ARKUI_EMITTER_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)(?-u:\b)emitter\s*\.\s*(emit|on|once)\s*\(\s*([A-Za-z_$][\w$.]*|\{[^)]{0,120}(?-u:\b)eventId\s*:\s*[^,}]+[^)]*\})",
    )
    .expect("valid ArkUI emitter regex")
});
static ARKUI_EVENT_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)eventId\s*:\s*([\w$.]+)").expect("valid eventId regex"));
static ARKUI_ROUTER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)(?-u:\b)router\s*\.\s*(?:pushUrl|replaceUrl)\s*\(\s*\{[^)]{0,200}(?-u:\b)url\s*:\s*['\"]([\w\-./]+)['\"]"#,
    )
    .expect("valid ArkUI router regex")
});

fn is_arkts(node: &Node) -> bool {
    node.language.as_str() == "arkts" || node.file_path.ends_with(".ets")
}

fn has_decorator(node: &Node, wanted: &str) -> bool {
    node.decorators.as_ref().is_some_and(|decorators| {
        decorators
            .iter()
            .any(|decorator| decorator.trim_start_matches('@') == wanted)
    })
}

fn child_nodes(queries: &QueryBuilder, parent_id: &str) -> Result<Vec<Node>> {
    let mut children = Vec::new();
    for edge in queries.get_outgoing_edges(parent_id, Some(&[EdgeKind::Contains]), None)? {
        if let Some(node) = queries.get_node_by_id(&edge.target)? {
            children.push(node);
        }
    }
    Ok(children)
}

fn contains_reactive_mutation(source: &str, property_names: &[String]) -> bool {
    if property_names.is_empty() {
        return false;
    }
    let properties = property_names
        .iter()
        .map(|property| regex::escape(property))
        .collect::<Vec<_>>()
        .join("|");
    let pattern = format!(
        r"this\.(?:{properties})\s*(=|\+\+|--|[+\-*/%&|^]=|\.(?:{ARKUI_ARRAY_MUTATORS})\s*\()"
    );
    let mutation_re = Regex::new(&pattern).expect("escaped property names produce valid regex");
    let found = mutation_re.captures_iter(source).any(|capture| {
        let operator = capture.get(1).expect("operator capture");
        operator.as_str() != "=" || source.as_bytes().get(operator.end()).copied() != Some(b'=')
    });
    found
}

/// Link a method that mutates a reactive ArkUI property to its component's
/// `build` method. Reading state alone deliberately creates no edge.
pub(super) fn arkui_state_build_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    let mut structs = Vec::new();
    queries.iterate_nodes_by_kind(NodeKind::Struct, |node| {
        if is_arkts(&node) {
            structs.push(node);
        }
        true
    })?;

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for structure in structs {
        let children = child_nodes(queries, &structure.id)?;
        let Some(build) = children
            .iter()
            .find(|node| node.kind == NodeKind::Method && node.name == "build")
        else {
            continue;
        };
        let reactive_properties = children
            .iter()
            .filter(|node| {
                node.kind == NodeKind::Property
                    && ARKUI_REACTIVE_DECORATORS
                        .iter()
                        .any(|decorator| has_decorator(node, decorator))
            })
            .map(|node| node.name.clone())
            .collect::<Vec<_>>();
        if reactive_properties.is_empty() {
            continue;
        }

        let mut added = 0usize;
        for method in &children {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            if method.kind != NodeKind::Method || method.id == build.id {
                continue;
            }
            let Some(source) = node_source(ctx, method) else {
                continue;
            };
            let safe = strip_comments_for_regex(&source, CommentLang::Typescript);
            if !contains_reactive_mutation(&safe, &reactive_properties) {
                continue;
            }
            if !seen.insert(format!("{}>{}", method.id, build.id)) {
                continue;
            }
            edges.push(synthesized_edge(
                &method.id,
                &build.id,
                Some(method.start_line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("arkui-state")),
                    ("via", Value::from("state assignment")),
                    (
                        "registeredAt",
                        Value::from(format!("{}:{}", build.file_path, build.start_line)),
                    ),
                ]),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

#[derive(Clone)]
struct EmitterSite {
    node_id: String,
    file: String,
    line: u32,
}

fn workspace_module_dirs(ctx: &dyn ResolutionContext) -> Vec<String> {
    let mut dirs = ctx
        .get_workspace_packages()
        .map(|workspace| workspace.by_name.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.len()));
    dirs.dedup();
    dirs
}

fn module_scope<'a>(file: &str, module_dirs: &'a [String]) -> &'a str {
    module_dirs
        .iter()
        .find(|dir| file == dir.as_str() || file.starts_with(&format!("{dir}/")))
        .map(String::as_str)
        .unwrap_or("")
}

fn emitter_key(safe: &str, argument: &str, file: &str, module_dirs: &[String]) -> Option<String> {
    let token = if argument.starts_with('{') {
        ARKUI_EVENT_ID_RE
            .captures(argument)
            .and_then(|capture| capture.get(1))
            .map(|capture| capture.as_str().to_string())?
    } else {
        argument.to_string()
    };
    if token.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(format!("num:{file}:{token}"));
    }
    if token.contains('.') {
        return Some(format!("name:{}:{token}", module_scope(file, module_dirs)));
    }

    let escaped = regex::escape(&token);
    let declaration_re = Regex::new(&format!(
        r"(?-u:\b){escaped}(?-u:\b)\s*(?::[^=\n]+)?=\s*(?:new\s+[\w$.]+\(\s*([^)\n]+?)\s*\)|([\w$.]+))"
    ))
    .ok()?;
    let declaration = declaration_re.captures(safe)?;
    let inner = declaration
        .get(1)
        .or_else(|| declaration.get(2))?
        .as_str()
        .trim();
    if inner.bytes().all(|byte| byte.is_ascii_digit()) {
        Some(format!("num:{file}:{inner}"))
    } else if inner
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.'))
    {
        Some(format!("name:{}:{inner}", module_scope(file, module_dirs)))
    } else {
        None
    }
}

/// Bridge `emitter.emit(key)` to `emitter.on/once(key, ...)`, with numeric
/// keys scoped to a file and named keys scoped to a workspace package.
pub(super) fn arkui_emitter_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let module_dirs = workspace_module_dirs(ctx);
    let mut emitters: BTreeMap<String, Vec<EmitterSite>> = BTreeMap::new();
    let mut handlers: BTreeMap<String, Vec<EmitterSite>> = BTreeMap::new();

    for file in ctx.get_all_files() {
        if !file.ends_with(".ets") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if !content.contains("emitter.") {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Typescript);
        let nodes = ctx
            .get_nodes_in_file(&file)
            .into_iter()
            .filter(|node| matches!(node.kind, NodeKind::Method | NodeKind::Function))
            .collect::<Vec<_>>();
        for capture in ARKUI_EMITTER_CALL_RE.captures_iter(&safe) {
            let verb = &capture[1];
            let argument = capture[2].trim();
            let Some(key) = emitter_key(&safe, argument, &file, &module_dirs) else {
                continue;
            };
            let whole = capture.get(0).expect("whole emitter match");
            let line = line_of(&safe, whole.start());
            let Some(enclosing) = enclosing_fn(&nodes, line) else {
                continue;
            };
            let site = EmitterSite {
                node_id: enclosing.id.clone(),
                file: file.clone(),
                line,
            };
            if verb == "emit" {
                emitters.entry(key).or_default().push(site);
            } else {
                handlers.entry(key).or_default().push(site);
            }
        }
    }

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for (key, emitter_sites) in emitters {
        let Some(handler_sites) = handlers.get(&key) else {
            continue;
        };
        if emitter_sites.len() > ARKUI_EMITTER_FANOUT_CAP
            || handler_sites.len() > ARKUI_EMITTER_FANOUT_CAP
        {
            continue;
        }
        let event = key.rsplit(':').next().unwrap_or(&key);
        for emitter in &emitter_sites {
            for handler in handler_sites {
                if emitter.node_id == handler.node_id
                    || !seen.insert(format!("{}>{}", emitter.node_id, handler.node_id))
                {
                    continue;
                }
                edges.push(synthesized_edge(
                    &emitter.node_id,
                    &handler.node_id,
                    Some(emitter.line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("arkui-emitter")),
                        ("event", Value::from(event)),
                        (
                            "registeredAt",
                            Value::from(format!("{}:{}", handler.file, handler.line)),
                        ),
                    ]),
                ));
            }
        }
    }
    edges
}

/// Bridge literal `router.pushUrl`/`replaceUrl` destinations to the unique
/// `@Entry` struct in the matching ArkUI page file.
pub(super) fn arkui_router_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let all_files = ctx.get_all_files();
    let module_dirs = workspace_module_dirs(ctx);
    let mut edges = Vec::new();
    let mut seen = HashSet::new();

    for file in &all_files {
        if !file.ends_with(".ets") {
            continue;
        }
        let Some(content) = ctx.read_file(file) else {
            continue;
        };
        if !content.contains("router.") {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Typescript);
        let nodes = ctx
            .get_nodes_in_file(file)
            .into_iter()
            .filter(|node| matches!(node.kind, NodeKind::Method | NodeKind::Function))
            .collect::<Vec<_>>();
        for capture in ARKUI_ROUTER_RE.captures_iter(&safe) {
            let url = &capture[1];
            let whole = capture.get(0).expect("whole router match");
            let line = line_of(&safe, whole.start());
            let Some(enclosing) = enclosing_fn(&nodes, line) else {
                continue;
            };
            let suffix = format!("/src/main/ets/{url}.ets");
            let mut candidates = all_files
                .iter()
                .filter(|candidate| candidate.ends_with(&suffix))
                .collect::<Vec<_>>();
            if candidates.len() > 1 {
                let scope = module_scope(file, &module_dirs);
                let same_module = candidates
                    .iter()
                    .copied()
                    .filter(|candidate| module_scope(candidate, &module_dirs) == scope)
                    .collect::<Vec<_>>();
                if !same_module.is_empty() {
                    candidates = same_module;
                }
            }
            if candidates.len() != 1 {
                continue;
            }
            let page_file = candidates[0];
            let Some(page) = ctx
                .get_nodes_in_file(page_file)
                .into_iter()
                .find(|node| node.kind == NodeKind::Struct && has_decorator(node, "Entry"))
            else {
                continue;
            };
            if !seen.insert(format!("{}>{}", enclosing.id, page.id)) {
                continue;
            }
            edges.push(synthesized_edge(
                &enclosing.id,
                &page.id,
                Some(line),
                edge_meta(vec![
                    ("synthesizedBy", Value::from("arkui-route")),
                    ("event", Value::from(url)),
                    (
                        "registeredAt",
                        Value::from(format!("{}:{}", page_file, page.start_line)),
                    ),
                ]),
            ));
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactive_mutation_gate_rejects_reads_and_equality() {
        let properties = vec!["count".to_string(), "todos".to_string()];
        assert!(contains_reactive_mutation("this.count += 1", &properties));
        assert!(contains_reactive_mutation(
            "this.todos.push(item)",
            &properties
        ));
        assert!(!contains_reactive_mutation(
            "return this.count",
            &properties
        ));
        assert!(!contains_reactive_mutation(
            "this.count == other",
            &properties
        ));
        assert!(!contains_reactive_mutation(
            "this.count === other",
            &properties
        ));
    }

    #[test]
    fn emitter_keys_obey_numeric_and_named_scopes() {
        let modules = vec!["features/cart".to_string()];
        assert_eq!(
            emitter_key("", "7", "features/cart/a.ets", &modules).as_deref(),
            Some("num:features/cart/a.ets:7")
        );
        assert_eq!(
            emitter_key("", "Events.Added", "features/cart/a.ets", &modules).as_deref(),
            Some("name:features/cart:Events.Added")
        );
        assert_eq!(
            emitter_key(
                "const changed = new EventsId(42);",
                "changed",
                "features/cart/a.ets",
                &modules,
            )
            .as_deref(),
            Some("num:features/cart/a.ets:42")
        );
    }
}
