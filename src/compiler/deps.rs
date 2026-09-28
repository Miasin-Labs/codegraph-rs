//! Dependency symbols onto the dependency shards (and linked projects).
//!
//! rust-analyzer names a dependency item by package, version and its path
//! of definition: `rust-analyzer cargo serde_json 1.0.150 value/impl#[Value]as_str().`
//! is the method `as_str` of an `impl Value` block in module `value` of
//! serde_json 1.0.150. The shard of exactly that version holds the item as
//! a node named `as_str`, qualified `Value::as_str`, in a file whose module
//! path is `value` (`src/value.rs`, `src/value/mod.rs`) or an outer module
//! of it (an inline `mod`). Exactly one such node, or nothing.
//!
//! Graphs are opened read-only through the external pass's
//! [`GraphCache`] (bounded, least recently used closed first); lookups are
//! memoized per symbol.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::scip::SymbolId;
use super::symbol::{Suffix, Symbol};
use crate::db::ExternalGraphKind;
use crate::deps::{DepKey, Ecosystem};
use crate::resolution::external::graphs::Reach;
use crate::resolution::external::open::{ForeignGraph, GraphCache};
use crate::resolution::types::ResolutionContext;
use crate::types::{Language, Node, NodeKind};

/// Where a dependency symbol led.
pub(crate) enum DependencyTarget {
    Found {
        graph: Rc<ForeignGraph>,
        node: Box<Node>,
    },
    /// No readable shard (or linked index) for that package version.
    NoGraph,
    /// The graph holds no single item for it.
    NotFound,
    /// A module, a field or anything else the graph does not link to.
    NotLinked,
}

pub(crate) struct DependencyTargets<'r> {
    cache: GraphCache<'r>,
    /// Graph index by `crates/<name>-<version>` key, and linked projects by
    /// crate name.
    by_key: HashMap<String, usize>,
    by_crate: HashMap<String, usize>,
    memo: RefCell<HashMap<SymbolId, Option<(usize, Node)>>>,
}

impl<'r> DependencyTargets<'r> {
    pub(crate) fn new(reach: &'r Reach, max_open: usize) -> Self {
        let mut by_key = HashMap::new();
        let mut by_crate = HashMap::new();
        for (index, graph) in reach.graphs().iter().enumerate() {
            match graph.kind {
                ExternalGraphKind::Dependency => {
                    by_key.insert(graph.key.clone(), index);
                }
                ExternalGraphKind::Project => {
                    by_crate.insert(graph.krate.clone(), index);
                }
            }
        }
        DependencyTargets {
            cache: GraphCache::new(reach, max_open),
            by_key,
            by_crate,
            memo: RefCell::new(HashMap::new()),
        }
    }

    pub(crate) fn graphs(&self) -> usize {
        self.by_key.len() + self.by_crate.len()
    }

    pub(crate) fn opens(&self) -> crate::resolution::external::OpenStats {
        self.cache.stats()
    }

    fn graph_index(&self, symbol: &Symbol) -> Option<usize> {
        let key = format!(
            "{}/{}",
            Ecosystem::Crates.as_str(),
            DepKey::new(Ecosystem::Crates, &symbol.package, &symbol.version).dir_name()
        );
        self.by_key
            .get(&key)
            .or_else(|| self.by_crate.get(&symbol.package.replace('-', "_")))
            .or_else(|| self.by_crate.get(&symbol.package))
            .copied()
    }

    /// The node `symbol` (interned as `id`) names in its graph.
    pub(crate) fn lookup(&self, id: SymbolId, symbol: &Symbol) -> DependencyTarget {
        let item = symbol.item();
        if matches!(
            item.suffix,
            Suffix::Namespace | Suffix::Parameter | Suffix::TypeParameter | Suffix::Meta
        ) || (item.suffix == Suffix::Term && item.owner.is_some() && !item.in_impl)
        {
            return DependencyTarget::NotLinked;
        }
        let Some(index) = self.graph_index(symbol) else {
            return DependencyTarget::NoGraph;
        };
        if let Some(known) = self.memo.borrow().get(&id) {
            return match known {
                Some((graph, node)) => match self.cache.get_index(*graph) {
                    Some(graph) => DependencyTarget::Found {
                        graph,
                        node: Box::new(node.clone()),
                    },
                    None => DependencyTarget::NoGraph,
                },
                None => DependencyTarget::NotFound,
            };
        }
        let Some(graph) = self.cache.get_index(index) else {
            return DependencyTarget::NoGraph;
        };
        let found = find_item(&graph, symbol);
        self.memo
            .borrow_mut()
            .insert(id, found.clone().map(|node| (index, node)));
        match found {
            Some(node) => DependencyTarget::Found {
                graph,
                node: Box::new(node),
            },
            None => DependencyTarget::NotFound,
        }
    }
}

/// The one node of `graph` the symbol names.
fn find_item(graph: &ForeignGraph, symbol: &Symbol) -> Option<Node> {
    let item = symbol.item();
    let kinds: &[NodeKind] = match (item.suffix, item.owner) {
        (Suffix::Method, Some(_)) => &[NodeKind::Method, NodeKind::Function],
        (Suffix::Method, None) => &[NodeKind::Function],
        (Suffix::Type, Some(_)) if !item.in_impl => &[NodeKind::EnumMember],
        (Suffix::Type, _) => &[
            NodeKind::Struct,
            NodeKind::Enum,
            NodeKind::Trait,
            NodeKind::Union,
            NodeKind::TypeAlias,
        ],
        (Suffix::Term, Some(_)) => &[NodeKind::Constant, NodeKind::EnumMember],
        (Suffix::Term, None) => &[NodeKind::Constant, NodeKind::Variable],
        (Suffix::Macro, _) => &[NodeKind::Macro],
        _ => return None,
    };
    let qualified = match item.owner {
        Some(owner) => format!("{owner}::{}", item.name),
        None => item.name.to_string(),
    };
    let suffix = format!("::{qualified}");
    let candidates: Vec<Node> = graph
        .context
        .get_nodes_by_name(item.name)
        .into_iter()
        .filter(|node| {
            node.language == Language::Rust
                && kinds.contains(&node.kind)
                && (node.qualified_name == qualified || node.qualified_name.ends_with(&suffix))
                && graph.in_crate(&node.file_path)
        })
        .collect();
    if candidates.len() <= 1 {
        return candidates.into_iter().next();
    }
    // Several: keep those whose file's module path is the symbol's, or the
    // closest outer module of it (an inline `mod`).
    let modules = symbol.modules();
    let base = source_base(graph);
    let depth = |node: &Node| module_match(&base, &node.file_path, &modules);
    let best = candidates.iter().filter_map(depth).max()?;
    let mut matching = candidates
        .into_iter()
        .filter(|node| depth(node) == Some(best));
    let first = matching.next()?;
    // `cfg` twins (one item per platform) are one item.
    let one_place = matching.all(|other| {
        other.qualified_name == first.qualified_name && other.file_path == first.file_path
    });
    one_place.then_some(first)
}

/// The directory the crate's module tree starts in (`src` for `src/lib.rs`).
fn source_base(graph: &ForeignGraph) -> String {
    match graph.lib_root.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    }
}

/// How many of `modules` the file's own module path covers, when the file
/// is `modules`' file or that of an outer module.
fn module_match(base: &str, file: &str, modules: &[&str]) -> Option<usize> {
    let relative = if base.is_empty() {
        file
    } else {
        file.strip_prefix(base)?.strip_prefix('/')?
    };
    let stem = relative.strip_suffix(".rs")?;
    let mut path: Vec<&str> = stem.split('/').collect();
    if matches!(path.last(), Some(&"mod") | Some(&"lib") | Some(&"main")) {
        path.pop();
    }
    (path.len() <= modules.len() && path.iter().zip(modules).all(|(a, b)| a == b))
        .then_some(path.len())
}

#[cfg(test)]
mod tests {
    use super::module_match;

    #[test]
    fn module_paths_of_files() {
        let modules = ["value", "de"];
        assert_eq!(module_match("src", "src/value/de.rs", &modules), Some(2));
        assert_eq!(module_match("src", "src/value/mod.rs", &modules), Some(1));
        assert_eq!(module_match("src", "src/lib.rs", &modules), Some(0));
        assert_eq!(module_match("src", "src/map.rs", &modules), None);
        assert_eq!(module_match("src", "benches/x.rs", &modules), None);
        assert_eq!(module_match("", "value.rs", &modules), Some(1));
    }
}
