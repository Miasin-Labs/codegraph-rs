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

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use super::layout::{ModuleLocation, inline_modules, module_files, module_location};
use super::use_tree::{UseBinding, UseLeaf, UseVisibility};
use crate::resolution::name_matcher::receiver::project_crate_dir;
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
    /// A path's leading segments (`m` in `m::f`): a module only.
    Module,
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
        match (self, kind) {
            (Self::Module, kind) => kind == NodeKind::Module,
            (_, NodeKind::Import | NodeKind::File) => false,
            // A call runs a value: never a module, nor a macro (`m!()`).
            (Self::Value, NodeKind::Module | NodeKind::Macro) => false,
            _ => true,
        }
    }

    fn admits_binding(self, binding: &UseBinding) -> bool {
        !matches!(binding, UseBinding::Module(_)) || self != Self::Value
    }
}

/// What a name resolves to in a module.
#[derive(Debug, Clone)]
pub(super) enum Resolution {
    Found(Box<Node>),
    /// Several distinct items could be meant (`cfg` alternatives: `#[cfg(test)]
    /// use mocks::f; #[cfg(not(test))] use real::f;`), or none is known
    /// when the lookup could not finish.
    Ambiguous(Vec<Node>),
    /// Only a `use` of another crate's item binds the name (`use
    /// std::path::Path;`, directly or through a re-export).
    External,
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
            return Resolution::Ambiguous(Vec::new());
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
        let mut external = false;
        for item in self.items(module, name) {
            if visible_item(&item) {
                targets.add(item);
            }
        }
        let uses = self.uses(module, name);
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
            if is_external_root(self.context, module, &leaf.path) {
                external = true;
                continue;
            }
            let Some(source) = use_source(self.context, module, path) else {
                continue;
            };
            match self.resolve(&source, original, None, depth + 1) {
                Resolution::Found(node) => targets.add(*node),
                Resolution::Ambiguous(nodes) if nodes.is_empty() => {
                    return Resolution::Ambiguous(nodes);
                }
                Resolution::Ambiguous(nodes) => targets.add_all(nodes),
                Resolution::External => external = true,
                Resolution::NotFound => {}
            }
        }
        if !targets.is_empty() {
            return targets.finish();
        }
        if external {
            return Resolution::External;
        }

        // Globs only fill in names nothing above binds.
        for leaf in &uses {
            if leaf.binding != UseBinding::Glob || !visible_leaf(leaf) {
                continue;
            }
            if is_external_root(self.context, module, &leaf.path) {
                continue;
            }
            let Some(source) = use_source(self.context, module, &leaf.path) else {
                continue;
            };
            match self.resolve(&source, name, Some(module), depth + 1) {
                Resolution::Found(node) => targets.add(*node),
                Resolution::Ambiguous(nodes) if nodes.is_empty() => {
                    return Resolution::Ambiguous(nodes);
                }
                Resolution::Ambiguous(nodes) => targets.add_all(nodes),
                Resolution::External => external = true,
                Resolution::NotFound => {}
            }
        }
        match targets.finish() {
            Resolution::NotFound if external => Resolution::External,
            resolution => resolution,
        }
    }

    /// Free items named `name` defined directly in `module` — in one of its
    /// files, or in an inline `mod` block that spells out the rest of it.
    fn items(&mut self, module: &ModuleLocation, name: &str) -> Vec<Node> {
        let (context, namespace) = (self.context, self.namespace);
        let nodes = self.named.entry(name.to_string()).or_insert_with(|| {
            // A module lookup asks the index for modules alone: `std` or
            // `fmt` name thousands of imports it must not copy.
            let named = match namespace {
                Namespace::Module => context.get_nodes_by_name_and_kind(name, NodeKind::Module),
                _ => context.get_nodes_by_name(name),
            };
            named
                .into_iter()
                .filter(|node| node.language == Language::Rust)
                .collect()
        });
        let found: Vec<Node> = nodes
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
            .collect();
        if found.is_empty() && namespace == Namespace::Module {
            // `mod m;` written inside a macro call (`cfg_rt! { mod m; }`)
            // leaves no Module node; the module's indexed file stands in.
            return module
                .walk(&[name])
                .and_then(|child| module_file(context, &child))
                .into_iter()
                .collect();
        }
        found
    }

    /// Use leaves declared directly in `module`: at the top level of its
    /// files, or inside inline `mod` blocks of an ancestor's file.
    /// Only the leaves a lookup of `name` reads: those binding it, and
    /// globs (a module declares many `use`s; cloning them all per lookup
    /// made every lookup linear in them).
    fn uses(&self, module: &ModuleLocation, name: &str) -> Vec<UseLeaf> {
        let wanted =
            |leaf: &UseLeaf| leaf.binding == UseBinding::Glob || leaf.bound_name() == Some(name);
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
                        .filter(|entry| entry.inline_modules == inline && wanted(&entry.leaf))
                        .map(|entry| entry.leaf.clone()),
                );
                if inline.is_empty() {
                    leaves.extend(
                        macro_level_uses(self.context, &file)
                            .iter()
                            .filter(|leaf| wanted(leaf))
                            .cloned(),
                    );
                }
            }
        }
        leaves
    }
}

/// The `use` declarations a file writes at its top level inside a macro
/// call (`cfg_rt! { mod builder; pub use self::builder::Builder; }`),
/// which the index keeps no Import node for: its indented `use` lines no
/// fn, type or inline `mod` encloses. Read once per file.
pub(super) fn macro_level_uses(context: &dyn ResolutionContext, file: &str) -> Arc<Vec<UseLeaf>> {
    thread_local! {
        /// Per file, with the source it was read from: a lookup visits the
        /// files of every module on its path, more than a few-file memo
        /// holds, and the scope test per `use` must run once per file.
        static USES: RefCell<HashMap<String, (Arc<str>, Arc<Vec<UseLeaf>>)>> =
            RefCell::new(HashMap::new());
    }
    let locals = context.get_rust_fn_local_uses(file);
    if locals.is_empty() {
        return Arc::default();
    }
    let Some(source) = context.read_file_arc(file) else {
        return Arc::default();
    };
    if let Some(uses) = USES.with(|memo| {
        memo.borrow()
            .get(file)
            .filter(|(seen, _)| Arc::ptr_eq(seen, &source))
            .map(|(_, uses)| Arc::clone(uses))
    }) {
        return uses;
    }
    let uses: Arc<Vec<UseLeaf>> = Arc::new(
        locals
            .iter()
            .filter(|local| {
                !context
                    .scopes_enclosing_line(file, local.line)
                    .iter()
                    .any(|scope| scope.language == Language::Rust)
            })
            .map(|local| local.leaf.clone())
            .collect(),
    );
    USES.with(|memo| {
        memo.borrow_mut()
            .insert(file.to_string(), (source, Arc::clone(&uses)))
    });
    uses
}

/// The module a `use` path's leading segments (`path`, written in `module`)
/// name: a crate of the workspace by its name (`codegraph::types`), else
/// as a 2018-edition path walks from `module`.
pub(super) fn use_source<S: AsRef<str>>(
    context: &dyn ResolutionContext,
    module: &ModuleLocation,
    path: &[S],
) -> Option<ModuleLocation> {
    if let Some((root, rest)) = path.split_first() {
        let root = root.as_ref();
        if !is_path_keyword(root) && !is_local_module(context, module, root) {
            if let Some(dir) = project_crate_dir(root, context) {
                let crate_root = ModuleLocation {
                    crate_key: dir.trim_end_matches('/').to_string(),
                    module: Vec::new(),
                };
                return crate_root.walk(rest);
            }
        }
    }
    module.walk(path)
}

/// Whether the `use` path `path`, written in `module`, starts at a crate
/// outside the project (`std`, `axum`): not `crate`/`self`/`super`, not a
/// crate of the workspace, a module of this crate, or a project type
/// (`use Kind::Leaf`).
pub(super) fn is_external_root<S: AsRef<str>>(
    context: &dyn ResolutionContext,
    module: &ModuleLocation,
    path: &[S],
) -> bool {
    let [root, _, ..] = path else {
        return false;
    };
    let root = root.as_ref();
    !is_path_keyword(root)
        && project_crate_dir(root, context).is_none()
        && !is_local_module(context, module, root)
        && !names_rust_type(context, root)
}

/// `name` is a Rust struct, enum, union, trait or type alias of the
/// project. Asked by kind: `std` or `fmt` name thousands of imports a
/// by-name lookup would copy.
pub(super) fn names_rust_type(context: &dyn ResolutionContext, name: &str) -> bool {
    [
        NodeKind::Struct,
        NodeKind::Enum,
        NodeKind::Union,
        NodeKind::Trait,
        NodeKind::TypeAlias,
    ]
    .into_iter()
    .any(|kind| {
        context
            .get_nodes_by_name_and_kind(name, kind)
            .iter()
            .any(|node| node.language == Language::Rust)
    })
}

fn is_path_keyword(segment: &str) -> bool {
    matches!(segment, "crate" | "$crate" | "self" | "super" | "Self")
}

/// `name` is a module of `module`'s crate: a `mod` the index has, or the
/// indexed file of a child module.
fn is_local_module(context: &dyn ResolutionContext, module: &ModuleLocation, name: &str) -> bool {
    context
        .get_nodes_by_name_and_kind(name, NodeKind::Module)
        .iter()
        .any(|node| {
            node.language == Language::Rust
                && module_location(&node.file_path).crate_key == module.crate_key
        })
        || module.walk(&[name]).is_some_and(|child| {
            module_file(context, &child).is_some()
                || module_files(&child)
                    .iter()
                    .any(|file| !context.get_rust_use_leaves(file).is_empty())
        })
}

/// The File node of an indexed file holding `module`'s items (`m.rs` or
/// `m/mod.rs`) — asked of the index, never the disk.
pub(super) fn module_file(
    context: &dyn ResolutionContext,
    module: &ModuleLocation,
) -> Option<Node> {
    module_files(module).iter().find_map(|file| {
        context
            .get_nodes_by_qualified_name(file)
            .into_iter()
            .find(|node| node.kind == NodeKind::File && node.file_path == *file)
    })
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

    fn add_all(&mut self, nodes: Vec<Node>) {
        for node in nodes {
            self.add(node);
        }
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn finish(mut self) -> Resolution {
        match self.0.len() {
            0 => Resolution::NotFound,
            1 => Resolution::Found(Box::new(self.0.remove(0))),
            _ => Resolution::Ambiguous(self.0),
        }
    }
}
