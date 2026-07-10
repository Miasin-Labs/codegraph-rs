//! Razor `@using` and cascading `_Imports.razor` type resolution.

use std::collections::{HashMap, HashSet};

use regex::Regex;

use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::Language;

fn collect_usings(source: &str, out: &mut HashSet<String>) {
    let pattern = Regex::new(r"(?m)^\s*@using\s+(?:static\s+)?([A-Za-z_][\w.]*)")
        .expect("valid Razor using regex");
    for capture in pattern.captures_iter(source) {
        if let Some(namespace) = capture.get(1) {
            out.insert(namespace.as_str().to_string());
        }
    }
}

pub(super) fn match_via_using(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.language != Language::Razor
        || reference.reference_name.contains('.')
        || reference.reference_name.contains(':')
    {
        return None;
    }

    let mut usings = HashSet::new();
    if let Some(source) = context.read_file(&reference.file_path) {
        collect_usings(&source, &mut usings);
    }

    let normalized = reference.file_path.replace('\\', "/");
    let mut directory = normalized
        .rsplit_once('/')
        .map_or(String::new(), |(dir, _)| dir.to_string());
    loop {
        let imports_path = if directory.is_empty() {
            "_Imports.razor".to_string()
        } else {
            format!("{directory}/_Imports.razor")
        };
        if imports_path != normalized {
            if let Some(source) = context.read_file(&imports_path) {
                collect_usings(&source, &mut usings);
            }
        }
        if directory.is_empty() {
            break;
        }
        directory = directory
            .rsplit_once('/')
            .map_or(String::new(), |(parent, _)| parent.to_string());
    }

    let mut found = HashMap::new();
    for namespace in usings {
        let qualified = format!("{namespace}::{}", reference.reference_name);
        for node in context.get_nodes_by_qualified_name(&qualified) {
            found.insert(node.id.clone(), node);
        }
    }
    if found.len() != 1 {
        return None;
    }
    let node = found.into_values().next()?;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: node.id,
        confidence: 0.9,
        resolved_by: ResolvedBy::Import,
    })
}
