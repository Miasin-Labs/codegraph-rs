//! Name lookup inside a module, following `use` re-exports.
//!
//! Modelled on rustc_resolve's in-module lookup
//! (`resolve_ident_in_local_module_non_globs_unadjusted`, then
//! `resolve_ident_in_module_globs_unadjusted`): a module's own items and its
//! single imports bind a name first; glob imports are consulted only when
//! neither does, and two globs naming different items leave the name
//! ambiguous. rustc resolves every import eagerly to a fixpoint; this walks
//! only the chain the queried name needs, guarded like rustc's
//! `cycle_detection` against re-export cycles.

use std::collections::HashMap;

use super::layout::{ModuleLocation, inline_modules, module_files, module_location};
use super::use_tree::{UseBinding, UseLeaf, UseVisibility};
use crate::resolution::types::ResolutionContext;
use crate::types::{EdgeKind, Language, Node, NodeKind, Visibility};

/// How many import hops one lookup may follow. Real re-export chains are two
/// or three hops; the cap only bounds pathological inputs.
const MAX_DEPTH: usize = 8;

/// The namespace a reference looks a name up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Namespace {
    /// A call targets a value: never a module, import, or file.
    Value,
    /// Anything but an import or file.
    Any,
}

impl Namespace {
    pub(super) fn of(kind: EdgeKind) -> Self {
        if kind == EdgeKind::Calls {
            Self::Value
        } else {
            Self::Any
        }
    }

    pub(super) fn admits(self, kind: NodeKind) -> bool {
        match kind {
            NodeKind::Import | NodeKind::File => false,
            NodeKind::Module => self == Self::Any,
            _ => true,
        }
    }

    fn admits_binding(self, binding: &UseBinding) -> bool {
        !matches!(binding, UseBinding::Module(_)) || self == Self::Any
    }
}

/// What a name resolves to in a module.
#[derive(Debug, Clone)]
pub(super) enum Resolution {
    Found(Box<Node>),
    /// Several distinct items (or an unfinished lookup) could be meant.
    Ambiguous,
    NotFound,
}

/// Resolve `name` in `module`, following single and glob imports.
pub(super) fn resolve_in_module(
    context: &dyn ResolutionContext,
    module: &ModuleLocation,
    name: &str,
    namespace: Namespace,
) -> Resolution {
    ModuleTree {
        context,
        namespace,
        lookups: HashMap::new(),
        named: HashMap::new(),
    }
    .resolve(module, name, None, 0)
}

/// One lookup: `name` in `module`, as seen by a glob importer (if any).
type LookupKey = (ModuleLocation, String, Option<ModuleLocation>);

struct ModuleTree<'c> {
    context: &'c dyn ResolutionContext,
    namespace: Namespace,
    /// Finished lookups, and `None` for ones still on the stack — reaching
    /// one of those again is a cycle, which binds nothing.
    lookups: HashMap<LookupKey, Option<Resolution>>,
    /// Rust nodes per name, fetched once per top-level query.
    named: HashMap<String, Vec<Node>>,
}

impl ModuleTree<'_> {
    /// `name` in `module`. A glob `importer` sees only the bindings visible to
    /// it; a single import (or the original query) sees them all.
    fn resolve(
        &mut self,
        module: &ModuleLocation,
        name: &str,
        importer: Option<&ModuleLocation>,
        depth: usize,
    ) -> Resolution {
        if depth > MAX_DEPTH {
            // Evidence we did not finish reading cannot pick a target.
            return Resolution::Ambiguous;
        }
        let key = (module.clone(), name.to_string(), importer.cloned());
        match self.lookups.get(&key) {
            Some(Some(done)) => return done.clone(),
            Some(None) => return Resolution::NotFound,
            None => {}
        }
        self.lookups.insert(key.clone(), None);
        let resolution =
            crate::ensure_sufficient_stack(|| self.lookup(module, name, importer, depth));
        self.lookups.insert(key, Some(resolution.clone()));
        resolution
    }

    fn lookup(
        &mut self,
        module: &ModuleLocation,
        name: &str,
        importer: Option<&ModuleLocation>,
        depth: usize,
    ) -> Resolution {
        let visible_item = |node: &Node| {
            importer.is_none_or(|from| {
                node.visibility != Some(Visibility::Private) || from.is_within(module)
            })
        };
        let visible_leaf =
            |leaf: &UseLeaf| importer.is_none_or(|from| leaf_visible(&leaf.vis, module, from));

        let mut targets = Targets::default();
        for item in self.items(module, name) {
            if visible_item(&item) {
                targets.add(item);
            }
        }
        let uses = self.uses(module);
        for leaf in &uses {
            let binds = leaf.bound_name() == Some(name)
                && self.namespace.admits_binding(&leaf.binding)
                && visible_leaf(leaf);
            if !binds {
                continue;
            }
            let Some((original, path)) = leaf.path.split_last() else {
                continue;
            };
            let Some(source) = module.walk(path) else {
                continue;
            };
            match self.resolve(&source, original, None, depth + 1) {
                Resolution::Found(node) => targets.add(*node),
                Resolution::Ambiguous => return Resolution::Ambiguous,
                Resolution::NotFound => {}
            }
        }
        if !targets.is_empty() {
            return targets.finish();
        }

        // Globs only fill in names nothing above binds.
        for leaf in &uses {
            if leaf.binding != UseBinding::Glob || !visible_leaf(leaf) {
                continue;
            }
            let Some(source) = module.walk(&leaf.path) else {
                continue;
            };
            match self.resolve(&source, name, Some(module), depth + 1) {
                Resolution::Found(node) => targets.add(*node),
                Resolution::Ambiguous => return Resolution::Ambiguous,
                Resolution::NotFound => {}
            }
        }
        targets.finish()
    }

    /// Free items named `name` defined directly in `module` — in one of its
    /// files, or in an inline `mod` block that spells out the rest of it.
    fn items(&mut self, module: &ModuleLocation, name: &str) -> Vec<Node> {
        let (context, namespace) = (self.context, self.namespace);
        let nodes = self.named.entry(name.to_string()).or_insert_with(|| {
            context
                .get_nodes_by_name(name)
                .into_iter()
                .filter(|node| node.language == Language::Rust)
                .collect()
        });
        nodes
            .iter()
            .filter(|node| namespace.admits(node.kind))
            .filter(|node| {
                let inline = inline_modules(&node.qualified_name);
                // A free item's qualified name is its inline modules plus
                // its own name; anything longer is an associated item.
                if node.qualified_name.split("::").count() != inline.len() + 1 {
                    return false;
                }
                let mut location = module_location(&node.file_path);
                location.module.extend(inline);
                location == *module
            })
            .cloned()
            .collect()
    }

    /// Use leaves declared directly in `module`: at the top level of its
    /// files, or inside inline `mod` blocks of an ancestor's file.
    fn uses(&self, module: &ModuleLocation) -> Vec<UseLeaf> {
        let mut leaves = Vec::new();
        for split in 0..=module.module.len() {
            let (file_module, inline) = module.module.split_at(split);
            let file_location = ModuleLocation {
                crate_key: module.crate_key.clone(),
                module: file_module.to_vec(),
            };
            for file in module_files(&file_location) {
                let declared = self.context.get_rust_use_leaves(&file);
                leaves.extend(
                    declared
                        .iter()
                        .filter(|entry| entry.inline_modules == inline)
                        .map(|entry| entry.leaf.clone()),
                );
            }
        }
        leaves
    }
}

/// Whether a use leaf declared in `owner` is visible from `from`.
fn leaf_visible(vis: &UseVisibility, owner: &ModuleLocation, from: &ModuleLocation) -> bool {
    match vis {
        UseVisibility::Public | UseVisibility::Crate => true,
        UseVisibility::Private => from.is_within(owner),
        UseVisibility::Super => owner.parent().is_some_and(|scope| from.is_within(&scope)),
        UseVisibility::Restricted(path) => {
            owner.walk(path).is_some_and(|scope| from.is_within(&scope))
        }
    }
}

/// Distinct targets found so far. Definitions that differ only by `cfg`
/// (same file, same qualified name) count once, as in `unique`.
#[derive(Default)]
struct Targets(Vec<Node>);

impl Targets {
    fn add(&mut self, node: Node) {
        let seen = self.0.iter().any(|known| {
            known.file_path == node.file_path && known.qualified_name == node.qualified_name
        });
        if !seen {
            self.0.push(node);
        }
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn finish(mut self) -> Resolution {
        match self.0.len() {
            0 => Resolution::NotFound,
            1 => Resolution::Found(Box::new(self.0.remove(0))),
            _ => Resolution::Ambiguous,
        }
    }
}
