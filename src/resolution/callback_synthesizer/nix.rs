//! Nix module option declaration-to-write synthesis.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use super::edges::edge_meta;
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::types::{Edge, EdgeKind, Node, NodeKind, Provenance};

const SUBMODULE_SENTINEL: &str = "\0submodule";

#[derive(Clone)]
struct OptionRecord {
    id: String,
    file_path: String,
    start_line: u32,
    end_line: u32,
    segments: Vec<String>,
}

fn nix_leading_plain_segments(name: &str) -> Vec<String> {
    let bytes = name.as_bytes();
    let mut segments = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            let start = index;
            index += 1;
            while index < bytes.len() && bytes[index] != b'"' {
                if bytes[index] == b'\\' {
                    index += 1;
                }
                index += 1;
            }
            if index >= bytes.len() {
                return segments;
            }
            index += 1;
            let token = &name[start..index];
            if token.contains("${") {
                return segments;
            }
            segments.push(token.to_string());
            if index == bytes.len() {
                break;
            }
            if bytes[index] != b'.' {
                return segments;
            }
            index += 1;
            continue;
        }

        let start = index;
        while index < bytes.len() && bytes[index] != b'.' {
            if bytes[index] == b'"' || (bytes[index] == b'$' && bytes.get(index + 1) == Some(&b'{'))
            {
                return segments;
            }
            index += 1;
        }
        let segment = &name[start..index];
        let mut chars = segment.chars();
        let valid = chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '\'' | '-'));
        if !valid {
            return segments;
        }
        segments.push(segment.to_string());
        if index < bytes.len() {
            index += 1;
        }
    }
    segments
}

fn record_from_node(node: Node) -> Option<OptionRecord> {
    if node.language.as_str() != "nix" && !node.file_path.ends_with(".nix") {
        return None;
    }
    let segments = nix_leading_plain_segments(&node.name);
    if segments.is_empty() {
        return None;
    }
    Some(OptionRecord {
        id: node.id,
        file_path: node.file_path,
        start_line: node.start_line,
        end_line: node.end_line,
        segments,
    })
}

fn option_edge(source: &OptionRecord, target: &OptionRecord, path: String) -> Edge {
    Edge {
        source: source.id.clone(),
        target: target.id.clone(),
        kind: EdgeKind::References,
        metadata: Some(edge_meta(vec![
            ("synthesizedBy", Value::from("nix-option-path")),
            ("optionPath", Value::from(path)),
            (
                "registeredAt",
                Value::from(format!("{}:{}", target.file_path, target.start_line)),
            ),
        ])),
        line: Some(source.start_line),
        column: None,
        provenance: Some(Provenance::Heuristic),
    }
}

fn nix_option_edges_from_nodes(nodes: impl IntoIterator<Item = Node>) -> Vec<Edge> {
    let mut by_file: BTreeMap<String, Vec<OptionRecord>> = BTreeMap::new();
    for record in nodes.into_iter().filter_map(record_from_node) {
        by_file
            .entry(record.file_path.clone())
            .or_default()
            .push(record);
    }

    let mut declarations: HashMap<String, Vec<OptionRecord>> = HashMap::new();
    let mut writes = Vec::new();
    let register = |path: &[String],
                    record: &OptionRecord,
                    declarations: &mut HashMap<String, Vec<OptionRecord>>| {
        if path.len() < 2 || path.iter().any(|segment| segment == SUBMODULE_SENTINEL) {
            return;
        }
        declarations
            .entry(path.join("."))
            .or_default()
            .push(record.clone());
    };

    for records in by_file.values_mut() {
        records.sort_by(|left, right| {
            left.start_line
                .cmp(&right.start_line)
                .then_with(|| right.end_line.cmp(&left.end_line))
        });
        let mut stack: Vec<(u32, u32, Vec<String>)> = Vec::new();
        for record in records.iter() {
            while stack
                .last()
                .is_some_and(|(_, end, _)| *end < record.start_line)
            {
                stack.pop();
            }
            let enclosing = stack.last().and_then(|(start, end, prefix)| {
                let strictly_contained = record.start_line >= *start
                    && record.end_line <= *end
                    && !(record.start_line == *start && record.end_line == *end);
                strictly_contained.then(|| prefix.clone())
            });

            if record
                .segments
                .first()
                .is_some_and(|segment| segment == "options")
            {
                let own_path = record.segments[1..].to_vec();
                let prefix = if enclosing.is_some() {
                    vec![SUBMODULE_SENTINEL.to_string()]
                } else {
                    own_path
                };
                register(&prefix, record, &mut declarations);
                stack.push((record.start_line, record.end_line, prefix));
                continue;
            }
            if let Some(mut prefix) = enclosing {
                prefix.extend(record.segments.iter().cloned());
                register(&prefix, record, &mut declarations);
                stack.push((record.start_line, record.end_line, prefix));
                continue;
            }
            if record.segments.len() >= 2 {
                writes.push(record.clone());
            }
        }
    }

    let mut edges = Vec::new();
    for write in writes {
        let segments = if write
            .segments
            .first()
            .is_some_and(|segment| segment == "config")
        {
            &write.segments[1..]
        } else {
            &write.segments[..]
        };
        if segments.len() < 2 {
            continue;
        }
        for length in (2..=segments.len().min(6)).rev() {
            let path = segments[..length].join(".");
            let Some(candidates) = declarations.get(&path) else {
                continue;
            };
            let files = candidates
                .iter()
                .map(|candidate| candidate.file_path.as_str())
                .collect::<HashSet<_>>();
            if files.len() == 1 {
                let target = &candidates[0];
                if target.id != write.id {
                    edges.push(option_edge(&write, target, path));
                }
            }
            break;
        }
    }
    edges
}

/// Link Nix module configuration writes to their longest unambiguous static
/// option declaration prefix.
pub(super) fn nix_option_path_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut nodes = Vec::new();
    for kind in [NodeKind::Variable, NodeKind::Function] {
        queries.iterate_nodes_by_kind(kind, |node| {
            if node.language.as_str() == "nix" || node.file_path.ends_with(".nix") {
                nodes.push(node);
            }
            true
        })?;
    }
    Ok(nix_option_edges_from_nodes(nodes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Language;

    fn binding(id: &str, name: &str, file: &str, start: u32, end: u32) -> Node {
        Node::new(
            id,
            NodeKind::Variable,
            name,
            format!("{file}::{name}"),
            file,
            Language::Unknown,
            start,
            end,
        )
    }

    #[test]
    fn static_segments_preserve_quoted_names_and_stop_at_interpolation() {
        assert_eq!(
            nix_leading_plain_segments(r#"NSGlobalDomain."com.apple.mouse.tapBehavior""#),
            vec!["NSGlobalDomain", r#""com.apple.mouse.tapBehavior""#]
        );
        assert_eq!(
            nix_leading_plain_segments("services.${name}.enable"),
            vec!["services"]
        );
    }

    #[test]
    fn longest_unambiguous_option_prefix_links_a_config_write() {
        let nodes = vec![
            binding("decl", "options.services.nginx.enable", "module.nix", 1, 5),
            binding("write", "config.services.nginx.enable", "host.nix", 1, 1),
        ];
        let edges = nix_option_edges_from_nodes(nodes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "write");
        assert_eq!(edges[0].target, "decl");
        assert_eq!(edges[0].kind, EdgeKind::References);
    }

    #[test]
    fn duplicate_declaration_files_make_the_longest_match_ambiguous() {
        let nodes = vec![
            binding("decl-a", "options.services.nginx", "a.nix", 1, 2),
            binding("decl-b", "options.services.nginx", "b.nix", 1, 2),
            binding("write", "services.nginx.enable", "host.nix", 1, 1),
        ];
        assert!(nix_option_edges_from_nodes(nodes).is_empty());
    }

    #[test]
    fn nested_bindings_under_a_bare_options_block_compose_paths() {
        let nodes = vec![
            binding("root", "options", "module.nix", 1, 10),
            binding("decl", "services.nginx.enable", "module.nix", 2, 3),
            binding("write", "services.nginx.enable", "host.nix", 1, 1),
        ];
        let edges = nix_option_edges_from_nodes(nodes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, "decl");
    }
}
