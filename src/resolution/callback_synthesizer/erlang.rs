//! Erlang behaviour callback dispatch synthesis.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{enclosing_fn, line_of};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, EdgeKind, Node, NodeKind};

const ERLANG_BEHAVIOUR_FANOUT_CAP: usize = 24;

static ERLANG_DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(^|[^?\w@'])([A-Z][A-Za-z0-9_@]*):([a-z][A-Za-z0-9_@]*)\(")
        .expect("valid Erlang dispatch regex")
});
static ERLANG_CALLBACK_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*-callback\s+('[^'\n]+'|[a-z][A-Za-z0-9_@]*)\s*\(")
        .expect("valid Erlang callback regex")
});

fn is_erlang_file(file: &str) -> bool {
    file.ends_with(".erl") || file.ends_with(".hrl")
}

/// Blank Erlang `%` comments and double-quoted strings while preserving byte
/// offsets and newlines. Single-quoted atoms stay visible because a quoted
/// callback name is valid syntax.
fn strip_erlang_for_regex(source: &str) -> String {
    let input = source.as_bytes();
    let mut output = input.to_vec();
    let mut index = 0usize;
    while index < input.len() {
        if input[index] == b'%' {
            while index < input.len() && input[index] != b'\n' {
                output[index] = b' ';
                index += 1;
            }
            continue;
        }
        if input[index] == b'"' {
            output[index] = b' ';
            index += 1;
            while index < input.len() {
                if input[index] == b'\n' {
                    index += 1;
                    continue;
                }
                let byte = input[index];
                output[index] = b' ';
                index += 1;
                if byte == b'\\' && index < input.len() {
                    if input[index] != b'\n' {
                        output[index] = b' ';
                    }
                    index += 1;
                } else if byte == b'"' {
                    break;
                }
            }
            continue;
        }
        if input[index] == b'\'' {
            // Quoted atoms stay visible, but comment markers inside them are
            // data rather than the start of an Erlang comment.
            index += 1;
            while index < input.len() {
                if input[index] == b'\\' {
                    index = (index + 2).min(input.len());
                    continue;
                }
                let byte = input[index];
                index += 1;
                if byte == b'\'' {
                    break;
                }
            }
            continue;
        }
        index += 1;
    }
    String::from_utf8(output).expect("blanking ASCII delimiters preserves UTF-8")
}

/// Count the arguments following `open_index`, respecting nested Erlang
/// containers, quoted values, character literals, and bit syntax.
fn erlang_arity_at(source: &str, open_index: usize) -> i32 {
    let bytes = source.as_bytes();
    if bytes.get(open_index) != Some(&b'(') {
        return -1;
    }
    let limit = bytes.len().min(open_index.saturating_add(4000));
    let mut depth = 1i32;
    let mut binary_depth = 0i32;
    let mut commas = 0i32;
    let mut saw_argument = false;
    let mut index = open_index + 1;
    while index < limit {
        let byte = bytes[index];
        if byte == b'"' || byte == b'\'' {
            let quote = byte;
            index += 1;
            while index < limit && bytes[index] != quote {
                if bytes[index] == b'\\' {
                    index += 1;
                }
                index += 1;
            }
            saw_argument = true;
            index += usize::from(index < limit);
            continue;
        }
        if byte == b'$' {
            index += 1;
            if bytes.get(index) == Some(&b'\\') {
                index += 1;
            }
            saw_argument = true;
            index += usize::from(index < limit);
            continue;
        }
        if byte == b'<' && bytes.get(index + 1) == Some(&b'<') {
            binary_depth += 1;
            saw_argument = true;
            index += 2;
            continue;
        }
        if byte == b'>' && bytes.get(index + 1) == Some(&b'>') && binary_depth > 0 {
            binary_depth -= 1;
            index += 2;
            continue;
        }
        if binary_depth == 0 {
            if matches!(byte, b'(' | b'[' | b'{') {
                depth += 1;
                saw_argument = true;
                index += 1;
                continue;
            }
            if matches!(byte, b')' | b']' | b'}') {
                depth -= 1;
                if depth == 0 {
                    return if saw_argument { commas + 1 } else { 0 };
                }
                index += 1;
                continue;
            }
            if byte == b',' && depth == 1 {
                commas += 1;
                index += 1;
                continue;
            }
        }
        if !byte.is_ascii_whitespace() {
            saw_argument = true;
        }
        index += 1;
    }
    -1
}

#[derive(Clone)]
struct DispatchSite {
    caller: Node,
    function: String,
    arity: i32,
    line: u32,
}

fn dispatch_sites(
    source: &str,
    nodes: &[Node],
    callback_names: &HashSet<String>,
) -> Vec<DispatchSite> {
    let safe = strip_erlang_for_regex(source);
    let mut sites = Vec::new();
    for capture in ERLANG_DISPATCH_RE.captures_iter(&safe) {
        let function = capture[3].to_string();
        if !callback_names.contains(&function) {
            continue;
        }
        let whole = capture.get(0).expect("whole dispatch match");
        let open_index = whole.end() - 1;
        let arity = erlang_arity_at(&safe, open_index);
        if arity < 0 {
            continue;
        }
        let line = line_of(&safe, whole.start());
        let Some(caller) = enclosing_fn(nodes, line) else {
            continue;
        };
        sites.push(DispatchSite {
            caller: caller.clone(),
            function,
            arity,
            line,
        });
    }
    sites
}

/// Link variable-module calls (`Module:callback(...)`) to exported callback
/// implementations when exactly one in-repo behaviour declares that
/// name/arity pair.
pub(super) fn erlang_behaviour_dispatch_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    let mut erlang_modules = Vec::new();
    queries.iterate_nodes_by_kind(NodeKind::Namespace, |node| {
        if node.language.as_str() == "erlang" || is_erlang_file(&node.file_path) {
            erlang_modules.push(node);
        }
        true
    })?;
    if erlang_modules.is_empty() {
        return Ok(Vec::new());
    }

    let mut module_by_file = HashMap::new();
    for module in erlang_modules {
        module_by_file
            .entry(module.file_path.clone())
            .or_insert(module);
    }

    let mut declaring_behaviours: HashMap<String, Vec<Node>> = HashMap::new();
    let mut callback_names = HashSet::new();
    for file in ctx.get_all_files() {
        if !is_erlang_file(&file) {
            continue;
        }
        let Some(behaviour) = module_by_file.get(&file) else {
            continue;
        };
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if !content.contains("-callback") {
            continue;
        }
        let safe = strip_erlang_for_regex(&content);
        for capture in ERLANG_CALLBACK_DECL_RE.captures_iter(&safe) {
            let name = capture[1].trim_matches('\'').to_string();
            let whole = capture.get(0).expect("whole callback match");
            let arity = erlang_arity_at(&safe, whole.end() - 1);
            if arity < 0 {
                continue;
            }
            let behaviours = declaring_behaviours
                .entry(format!("{name}/{arity}"))
                .or_default();
            if !behaviours
                .iter()
                .any(|candidate| candidate.id == behaviour.id)
            {
                behaviours.push(behaviour.clone());
            }
            callback_names.insert(name);
        }
    }
    if declaring_behaviours.is_empty() {
        return Ok(Vec::new());
    }

    let mut target_cache: HashMap<String, Vec<Node>> = HashMap::new();
    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for file in ctx.get_all_files() {
        if !is_erlang_file(&file) {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if !content.contains(':') {
            continue;
        }
        let nodes = ctx.get_nodes_in_file(&file);
        for site in dispatch_sites(&content, &nodes, &callback_names) {
            let key = format!("{}/{}", site.function, site.arity);
            let Some(behaviours) = declaring_behaviours.get(&key) else {
                continue;
            };
            if behaviours.len() != 1 {
                continue;
            }
            let behaviour = &behaviours[0];
            let cache_key = format!("{}#{}", behaviour.id, site.function);
            if !target_cache.contains_key(&cache_key) {
                let mut targets = Vec::new();
                for edge in
                    queries.get_incoming_edges(&behaviour.id, Some(&[EdgeKind::Implements]))?
                {
                    let Some(implementer) = queries.get_node_by_id(&edge.source)? else {
                        continue;
                    };
                    if implementer.kind != NodeKind::Namespace
                        || (implementer.language.as_str() != "erlang"
                            && !is_erlang_file(&implementer.file_path))
                    {
                        continue;
                    }
                    if let Some(function) = ctx
                        .get_nodes_in_file(&implementer.file_path)
                        .into_iter()
                        .find(|node| {
                            node.kind == NodeKind::Function
                                && node.name == site.function
                                && node.is_exported != Some(false)
                        })
                    {
                        targets.push(function);
                    }
                }
                target_cache.insert(cache_key.clone(), targets);
            }
            let targets = &target_cache[&cache_key];
            if targets.is_empty() || targets.len() > ERLANG_BEHAVIOUR_FANOUT_CAP {
                continue;
            }
            for target in targets {
                if target.id == site.caller.id
                    || !seen.insert(format!("{}>{}", site.caller.id, target.id))
                {
                    continue;
                }
                edges.push(synthesized_edge(
                    &site.caller.id,
                    &target.id,
                    Some(site.line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("erlang-behaviour")),
                        (
                            "via",
                            Value::from(format!(
                                "{}:{}/{}",
                                behaviour.name, site.function, site.arity
                            )),
                        ),
                        (
                            "registeredAt",
                            Value::from(format!("{}:{}", file, site.line)),
                        ),
                    ]),
                ));
            }
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Language;

    #[test]
    fn arity_ignores_nested_containers_strings_and_bit_syntax() {
        let source = r#"call(fun((A, B) -> ok), #{k => [1, 2]}, <<1, 2>>, "a,b")"#;
        assert_eq!(erlang_arity_at(source, source.find('(').unwrap()), 4);
        assert_eq!(erlang_arity_at("call()", 4), 0);
        assert_eq!(erlang_arity_at("call(unclosed", 4), -1);
    }

    #[test]
    fn dispatch_scan_uses_synthetic_nodes_and_callback_name_arity() {
        let source = "run(Module) ->\n  Module:init(#{a => 1}, []),\n  Module:other().\n";
        let caller = Node::new(
            "caller",
            NodeKind::Function,
            "run",
            "dispatch::run",
            "dispatch.erl",
            Language::Unknown,
            1,
            3,
        );
        let callbacks = HashSet::from(["init".to_string()]);
        let sites = dispatch_sites(source, &[caller], &callbacks);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].caller.id, "caller");
        assert_eq!(sites[0].function, "init");
        assert_eq!(sites[0].arity, 2);
        assert_eq!(sites[0].line, 2);
    }

    #[test]
    fn comments_and_strings_do_not_create_dispatch_sites() {
        let source = "% Module:init(X).\nrun() -> io:format(\"Module:init(X)\").\n";
        let caller = Node::new(
            "caller",
            NodeKind::Function,
            "run",
            "dispatch::run",
            "dispatch.erl",
            Language::Unknown,
            2,
            2,
        );
        let callbacks = HashSet::from(["init".to_string()]);
        assert!(dispatch_sites(source, &[caller], &callbacks).is_empty());
    }
}
