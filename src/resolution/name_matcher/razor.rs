//! Razor `@using` and cascading `_Imports.razor` type resolution.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use regex::Regex;

use crate::resolution::line_index::SourceMemo;
use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::Language;

static USING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*@using\s+(?:static\s+)?([A-Za-z_][\w.]*)")
        .expect("valid Razor using regex")
});

/// The `@using` namespaces of `source`, parsed once per file: this runs per
/// reference, for the file and each `_Imports.razor` above it.
fn usings_of(source: &Arc<str>) -> Arc<Vec<String>> {
    thread_local! {
        static USINGS: RefCell<SourceMemo<Vec<String>>> = const { RefCell::new(SourceMemo::new()) };
    }
    USINGS.with(|memo| {
        memo.borrow_mut().get_or_insert_with(source, |text| {
            USING
                .captures_iter(text)
                .filter_map(|capture| capture.get(1))
                .map(|namespace| namespace.as_str().to_string())
                .collect()
        })
    })
}

fn collect_usings(source: &Arc<str>, out: &mut HashSet<String>) {
    out.extend(usings_of(source).iter().cloned());
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
    if let Some(source) = context.read_file_arc(&reference.file_path) {
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
            if let Some(source) = context.read_file_arc(&imports_path) {
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
