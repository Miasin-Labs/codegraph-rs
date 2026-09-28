//! Rust path resolution: `crate::`, `self::`, `super::`, and `Self::`.
//!
//! Rust nodes are not stored under module paths. A free function's qualified
//! name is its bare name (`check`) and a method's is `Type::method`; the
//! module lives only in the file path. So `crate::graph::cancel::check` or
//! `Self::fresh_label` can never string-match a node, and every such call was
//! left unresolved — 485 of them in codegraph's own index, invisible to
//! `callers` and `impact`.
//!
//! This strategy maps the path onto the crate/module layout instead
//! ([`layout`]): the referencing file fixes the crate and current module,
//! `crate`/`self`/`super` pick the target module, and a candidate matches when
//! its own file sits in that module (and, for `Type::item`, its qualified name
//! is `Type::item`). When the item lives elsewhere, the module's `pub use`
//! re-exports are followed to it ([`module_tree`], reading declarations
//! parsed by [`use_tree`]). `Self` becomes the enclosing impl type of the
//! referencing method.

mod bare;
mod layout;
mod module_tree;
mod use_tree;

pub(in crate::resolution::name_matcher) use bare::{BareBinding, bare_binding};
use layout::{ModuleLocation, declared_module, inline_modules, item_location, module_location};
use module_tree::{
    Namespace,
    Resolution,
    module_file,
    names_rust_type,
    resolve_in_module,
    use_source,
};
pub use use_tree::{
    LocalUse,
    RustUse,
    UseBinding,
    UseLeaf,
    UseVisibility,
    parse_use_leaves,
    rust_fn_local_uses,
    rust_use_leaves,
};

use crate::resolution::name_matcher::receiver::project_crate_dir;
use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{EdgeKind, Language, Node, NodeKind, Visibility};

/// The module path of a `.rs` file inside its crate (see
/// [`layout::module_location`]).
pub(in crate::resolution::name_matcher) fn module_of(file_path: &str) -> Vec<String> {
    module_location(file_path).module
}

/// The crate a `.rs` file belongs to (see [`layout::module_location`]).
pub(in crate::resolution::name_matcher) fn crate_key(file_path: &str) -> String {
    module_location(file_path).crate_key
}

/// Split a path, dropping generic arguments (`Vec::<u8>::new` -> `Vec::new`).
fn segments(path: &str) -> Vec<&str> {
    path.split("::")
        .map(str::trim)
        .filter(|segment| !segment.is_empty() && !segment.starts_with('<'))
        .collect()
}

fn resolved(reference: &UnresolvedRef, target: &Node, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        original: reference.clone(),
        target_node_id: target.id.clone(),
        confidence,
        resolved_by: ResolvedBy::QualifiedName,
    }
}

/// Pick one candidate, or none when the evidence is ambiguous. Definitions
/// that differ only by `cfg` (same file, same qualified name — e.g. unix and
/// windows variants) are one item, represented by the first.
fn unique<'a>(candidates: &'a [&'a Node]) -> Option<&'a Node> {
    let (first, rest) = candidates.split_first()?;
    rest.iter()
        .all(|node| {
            node.file_path == first.file_path && node.qualified_name == first.qualified_name
        })
        .then_some(*first)
}

fn starts_uppercase(segment: &str) -> bool {
    segment.chars().next().is_some_and(char::is_uppercase)
}

/// `Self::item` -> `<enclosing impl type>::item`.
fn resolve_self(
    reference: &UnresolvedRef,
    rest: &[&str],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let from = context.get_node_by_id(&reference.from_node_id)?;
    let (owner, _) = from.qualified_name.rsplit_once("::")?;
    let target = format!("{owner}::{}", rest.join("::"));
    let candidates = context.get_nodes_by_qualified_name(&target);
    let rust: Vec<&Node> = candidates
        .iter()
        .filter(|node| node.language == Language::Rust)
        .collect();
    // An owner name can repeat across crates; prefer the referencing crate.
    let here = module_location(&reference.file_path).crate_key;
    let same_crate: Vec<&Node> = rust
        .iter()
        .copied()
        .filter(|node| module_location(&node.file_path).crate_key == here)
        .collect();
    if let Some(node) = unique(&same_crate).or_else(|| unique(&rust)) {
        // `Self::Output` in `impl Future for T` names the trait's associated
        // type, which the impl's `type Output = …;` only defines: the trait
        // is where rustc's path leads.
        if node.kind == NodeKind::TypeAlias && !owner_is_trait(owner, node, context) {
            return None;
        }
        return Some(resolved(reference, node, 0.95));
    }
    // In a trait impl the method is keyed by the trait (`Default::default`),
    // not the implementing type, so the owner above can be the trait. Fall
    // back to the one associated item of that name in the same file.
    let [item] = rest else {
        return None;
    };
    let suffix = format!("::{item}");
    let local: Vec<Node> = context
        .get_nodes_in_file_named(&reference.file_path, item)
        .into_iter()
        .filter(|node| {
            node.qualified_name.ends_with(&suffix)
                && node.qualified_name != format!("{owner}::{item}")
                && node.kind != NodeKind::TypeAlias
        })
        .collect();
    let local_refs: Vec<&Node> = local.iter().collect();
    unique(&local_refs).map(|node| resolved(reference, node, 0.8))
}

/// `owner` (the `Self` of the referencing item) is the trait declaring
/// `item`: a reference inside the trait's own definition.
fn owner_is_trait(owner: &str, item: &Node, context: &dyn ResolutionContext) -> bool {
    let name = owner.rsplit("::").next().unwrap_or(owner);
    context
        .get_nodes_in_file_named(&item.file_path, name)
        .iter()
        .any(|node| node.kind == NodeKind::Trait && node.language == Language::Rust)
}

/// The module a reference is written in: its file's module plus any inline
/// `mod` blocks around the referencing item — `super::f` inside `mod tests
/// { … }` in lib.rs means the crate root, not "above the crate root".
///
/// The referencing item's qualified name spells the inline modules around
/// it, except a method's (`Type::m` wherever its `impl` sits): the
/// innermost `mod` block enclosing the reference's line says so then.
fn caller_module(reference: &UnresolvedRef, context: &dyn ResolutionContext) -> ModuleLocation {
    let mut here = module_location(&reference.file_path);
    // (A node a framework synthesized — `src/lib.rs::route:/x` — names no
    // module: only identifier segments count, that name a `mod` of the file.)
    let from_item: Vec<String> = context
        .get_node_by_id(&reference.from_node_id)
        .map(|from| inline_modules(&from.qualified_name))
        .unwrap_or_default()
        .into_iter()
        .take_while(|segment| {
            segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                // A fn nested in `outer` is `outer::walk`, a method of
                // `impl Index for usize` is `usize::index_into`: only a
                // `mod` of the file is a module.
                && context
                    .get_nodes_in_file_named(&reference.file_path, segment)
                    .iter()
                    .any(|node| node.kind == NodeKind::Module)
        })
        .collect();
    let from_scope = context
        .scopes_enclosing_line(&reference.file_path, reference.line)
        .into_iter()
        .find(|scope| {
            scope.kind == NodeKind::Module
                && scope.language == Language::Rust
                && scope.end_line > scope.start_line
        })
        .map(|module| inline_modules(&format!("{}::item", module.qualified_name)))
        .unwrap_or_default();
    here.module.extend(if from_scope.len() > from_item.len() {
        from_scope
    } else {
        from_item
    });
    here
}

/// `crate::`/`self::`/`super::` paths, resolved against the module layout.
fn resolve_module_path(
    reference: &UnresolvedRef,
    path: &[&str],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (&item, module_path) = path.split_last()?;
    let caller = caller_module(reference, context);
    // `self`/`super` start from the current module; `crate` ignores where
    // it starts.
    let from_root = module_path.first() == Some(&"crate");
    let start = if from_root {
        module_location(&reference.file_path)
    } else {
        caller.clone()
    };
    // The module the path names — or, for `Type::item`, the type's "module".
    let target = start.walk(module_path)?;
    let path = ModulePath {
        caller,
        target,
        owner: owner_segment(module_path),
        item,
        guess: if from_root {
            Guess::Project
        } else {
            Guess::SameCrate
        },
    };
    resolve_in_target(reference, &path, context)
}

/// `m::f`, `m::n::f`, `m::Type::new`: a path whose first segment is a
/// module in scope where it is written — a child `mod m` of the current
/// module (in a file or inline), or a module a `use` brings in (`use
/// crate::a::m;`, `use a::m as n;`, a glob, a re-export). Anything else
/// (`std::mem::take`, `serde_json::from_str`, `u32::from`, a module of
/// the same name elsewhere) is left alone.
fn resolve_scoped_path(
    reference: &UnresolvedRef,
    parts: &[&str],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (&head, rest) = parts.split_first()?;
    // Types (`Type::new`) have their own rules; a module is snake_case.
    if !head.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
        return None;
    }
    let (&item, inner) = rest.split_last()?;
    if inner.iter().any(|segment| is_path_keyword(segment)) {
        return None;
    }
    let caller = caller_module(reference, context);
    let base = module_in_scope(reference, head, &caller, context)?;
    let target = base.walk(inner)?;
    let path = ModulePath {
        caller,
        target,
        owner: owner_segment(inner),
        item,
        // The path names a module the index has, so an item missing there
        // is generated or another crate's: never guessed at.
        guess: Guess::Nothing,
    };
    resolve_in_target(reference, &path, context)
}

fn is_path_keyword(segment: &str) -> bool {
    matches!(segment, "crate" | "$crate" | "self" | "super" | "Self")
}

/// The project module `name` denotes where `caller` is, as rustc resolves
/// a path's first segment: a `use` in the fn holding the reference (a
/// block's items shadow the module's), else the module's own `mod` items
/// and single imports, then its globs, following re-exports.
fn module_in_scope(
    reference: &UnresolvedRef,
    name: &str,
    caller: &ModuleLocation,
    context: &dyn ResolutionContext,
) -> Option<ModuleLocation> {
    let binds = |leaf: &UseLeaf| {
        leaf.bound_name() == Some(name) && !matches!(leaf.binding, UseBinding::Glob)
    };
    let fn_local: Vec<LocalUse> = context
        .get_rust_fn_local_uses(&reference.file_path)
        .iter()
        .filter(|local| binds(&local.leaf))
        .cloned()
        .collect();
    if !fn_local.is_empty() {
        let in_scope = fn_uses_in_scope(reference, &fn_local, context);
        if !in_scope.is_empty() {
            // Every such `use` must name the same project module.
            let mut modules = in_scope
                .iter()
                .map(|leaf| imported_module(context, caller, &leaf.path));
            let first = modules.next()??;
            return modules
                .all(|module| module.as_ref() == Some(&first))
                .then_some(first);
        }
    }

    // The `use`s binding `name` in the caller's module: the file's, at the
    // caller's inline `mod` depth.
    let file_depth = module_location(&reference.file_path).module.len();
    let inline = caller.module.get(file_depth..).unwrap_or_default();
    let imported = context
        .get_rust_use_leaves(&reference.file_path)
        .iter()
        .any(|found| {
            found.inline_modules == inline
                && (binds(&found.leaf) || found.leaf.binding == UseBinding::Glob)
        });
    // The test most paths fail cheaply: nothing can bind the name
    // (`std::…`, `u32::…`) — no `use` or glob, no module of that name in
    // this crate, no child module file.
    let named_module = || {
        context
            .get_nodes_by_name_and_kind(name, NodeKind::Module)
            .iter()
            .any(|node| {
                node.language == Language::Rust
                    && module_location(&node.file_path).crate_key == caller.crate_key
            })
    };
    let child_file = || {
        caller
            .walk(&[name])
            .and_then(|child| module_file(context, &child))
    };
    if !imported && !named_module() && child_file().is_none() {
        return None;
    }

    match resolve_in_module(context, caller, name, Namespace::Module) {
        Resolution::Found(node) => Some(declared_module(&node)),
        Resolution::Ambiguous(_) | Resolution::External | Resolution::NotFound => None,
    }
}

/// The fn-local `use`s among `uses` that bind where `reference` is: those
/// written inside a fn that also holds the reference (nested fn items see
/// their enclosing block's items).
fn fn_uses_in_scope(
    reference: &UnresolvedRef,
    uses: &[LocalUse],
    context: &dyn ResolutionContext,
) -> Vec<UseLeaf> {
    let fns: Vec<(u32, u32)> = context
        .scopes_enclosing_line(&reference.file_path, reference.line)
        .into_iter()
        .filter(|scope| {
            scope.language == Language::Rust
                && matches!(scope.kind, NodeKind::Function | NodeKind::Method)
        })
        .map(|scope| (scope.start_line, scope.end_line.max(scope.start_line)))
        .collect();
    uses.iter()
        .filter(|local| {
            fns.iter()
                .any(|&(start, end)| start <= local.line && local.line <= end)
        })
        .map(|local| local.leaf.clone())
        .collect()
}

/// The project module the `use` path `path`, written in `from`, imports
/// (re-exports followed).
fn imported_module(
    context: &dyn ResolutionContext,
    from: &ModuleLocation,
    path: &[String],
) -> Option<ModuleLocation> {
    let (last, parent) = path.split_last()?;
    let source = use_source(context, from, parent)?;
    match resolve_in_module(context, &source, last, Namespace::Module) {
        Resolution::Found(node) => Some(declared_module(&node)),
        Resolution::Ambiguous(_) | Resolution::External | Resolution::NotFound => None,
    }
}

/// How far the last-resort shape match of [`resolve_in_target`] reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Guess {
    /// No guess.
    Nothing,
    /// The one item of the path's shape in the target's crate.
    SameCrate,
    /// That, else the one such item project-wide (a `crate::` re-export
    /// from another workspace crate).
    Project,
}

/// A module path taken apart: the item it names, where it is looked up,
/// and who asks.
struct ModulePath<'p> {
    /// The module the reference is written in.
    caller: ModuleLocation,
    /// The module the path names — or, for `Type::item`, the type's
    /// "module".
    target: ModuleLocation,
    /// `Type` in `…::Type::item`.
    owner: Option<&'p str>,
    item: &'p str,
    guess: Guess,
}

/// The last module segment of a path, when it names a type.
fn owner_segment<'p>(module_path: &[&'p str]) -> Option<&'p str> {
    module_path
        .last()
        .copied()
        .filter(|owner| starts_uppercase(owner))
}

/// Whether the free item `node` of `module` can be named from `caller`: a
/// private item only inside its module and the modules nested in it.
fn visible_from(node: &Node, module: &ModuleLocation, caller: &ModuleLocation) -> bool {
    node.visibility != Some(Visibility::Private) || caller.is_within(module)
}

/// The item a module path names, found in the module it names.
fn resolve_in_target(
    reference: &UnresolvedRef,
    path: &ModulePath<'_>,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let ModulePath {
        caller,
        target,
        owner,
        item,
        guess,
    } = path;
    let (owner, item) = (*owner, *item);

    // Namespace filter: a call targets a value, so modules, imports, and file
    // nodes sharing the name are not candidates (they made `crate::…::tools`
    // look ambiguous next to the `tools` fn).
    let namespace = Namespace::of(reference.reference_kind);
    let candidates = context.get_nodes_by_name(item);
    let rust: Vec<&Node> = candidates
        .iter()
        .filter(|node| node.language == Language::Rust && namespace.admits(node.kind))
        .collect();

    // `module::…::item` for a free item, or `module::…::Type::item` for an
    // associated item — in the module's files or an inline `mod` block.
    // Try the free reading first, then the associated one. A private free
    // item is invisible outside its module, so a path reaching it from
    // there names something else (a generated item, a re-export).
    let free: Vec<&Node> = rust
        .iter()
        .copied()
        .filter(|node| {
            let (location, local) = item_location(node);
            local == item && location == *target && visible_from(node, target, caller)
        })
        .collect();
    if let Some(node) = unique(&free) {
        return Some(resolved(reference, node, 0.95));
    }
    if let (Some(owner), Some(owner_module)) = (owner, target.parent()) {
        let wanted = format!("{owner}::{item}");
        let associated: Vec<&Node> = rust
            .iter()
            .copied()
            .filter(|node| {
                let (location, local) = item_location(node);
                local == wanted && location == owner_module
            })
            .collect();
        if let Some(node) = unique(&associated) {
            return Some(resolved(reference, node, 0.95));
        }
    }

    // Re-exports (`pub use`) move items away from the module their path
    // names: follow the module's use declarations to the definition. An
    // ambiguous re-export stops here rather than guessing below.
    if owner.is_none() {
        match resolve_in_module(context, target, item, namespace) {
            Resolution::Found(node) => {
                // A re-export cannot widen a private item's reach.
                let (home, _) = item_location(&node);
                return visible_from(&node, &home, caller)
                    .then(|| resolved(reference, &node, 0.95));
            }
            // A re-export of another crate's item names nothing here.
            Resolution::Ambiguous(_) | Resolution::External => return None,
            Resolution::NotFound => {}
        }
    }

    // Last resort, for re-exports the index cannot follow (another crate,
    // a re-exported type's methods): accept the one same-crate item of the
    // right shape; for `crate::…`, also the one such item project-wide (a
    // re-export from another workspace crate). Never guess among several.
    if *guess == Guess::Nothing {
        return None;
    }
    let shape: Vec<&Node> = rust
        .iter()
        .copied()
        .filter(|node| match owner {
            Some(owner) => node.qualified_name == format!("{owner}::{item}"),
            None => node.qualified_name == item,
        })
        .collect();
    let same_crate: Vec<&Node> = shape
        .iter()
        .copied()
        .filter(|node| module_location(&node.file_path).crate_key == target.crate_key)
        .collect();
    if let Some(node) = unique(&same_crate) {
        return Some(resolved(reference, node, 0.8));
    }
    // Re-exports mostly lift an item out of a child module (`pub use
    // self::builder::Builder;` inside a `cfg_rt! { … }` the index cannot
    // read): the one such item in the module the path names or a child.
    let module = if owner.is_some() {
        target.parent()
    } else {
        Some(target.clone())
    };
    if let Some(module) = module {
        let below: Vec<&Node> = same_crate
            .iter()
            .copied()
            .filter(|node| item_location(node).0.is_within_child_of(&module))
            .collect();
        if let Some(node) = unique(&below) {
            return Some(resolved(reference, node, 0.8));
        }
    }
    if *guess == Guess::Project && same_crate.is_empty() {
        if let Some(node) = unique(&shape) {
            return Some(resolved(reference, node, 0.7));
        }
    }
    None
}

/// The one project type a type path written at the top level of `file`
/// names (`Sender`, `crate::loom::sync::Mutex`), found as rustc would:
/// the module's items, `use`s, re-exports and globs followed. `None` when
/// the module tree cannot say, or the path names something else.
pub(in crate::resolution::name_matcher) fn type_definition(
    path: &str,
    file: &str,
    context: &dyn ResolutionContext,
) -> Option<Node> {
    item_definition(path, file, EdgeKind::References, context).filter(|node| {
        matches!(
            node.kind,
            NodeKind::Struct
                | NodeKind::Enum
                | NodeKind::Union
                | NodeKind::Trait
                | NodeKind::TypeAlias
        )
    })
}

/// The one free fn a call path written at the top level of `file` names
/// (`mpsc::channel`, `load`), found like [`type_definition`]'s type.
pub(in crate::resolution::name_matcher) fn fn_definition(
    path: &str,
    file: &str,
    context: &dyn ResolutionContext,
) -> Option<Node> {
    item_definition(path, file, EdgeKind::Calls, context)
        .filter(|node| node.kind == NodeKind::Function)
}

fn item_definition(
    path: &str,
    file: &str,
    kind: EdgeKind,
    context: &dyn ResolutionContext,
) -> Option<Node> {
    let reference = UnresolvedRef {
        from_node_id: String::new(),
        reference_name: path.to_string(),
        reference_kind: kind,
        line: 0,
        column: 0,
        file_path: file.to_string(),
        language: Language::Rust,
        candidates: None,
        metadata: None,
    };
    if path.contains("::") {
        let found = match_rust_path(&reference, context)?;
        context.get_node_by_id(&found.target_node_id)
    } else {
        match bare_binding(&reference, context) {
            BareBinding::Item { node, .. } => Some(*node),
            _ => None,
        }
    }
}

/// Resolve a Rust path whose head is `crate`, `self`, `super`, `Self`, or a
/// module in scope where the path is written.
pub fn match_rust_path(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.language != Language::Rust || !reference.reference_name.contains("::") {
        return None;
    }
    let parts = segments(&reference.reference_name);
    let (&head, rest) = parts.split_first()?;
    if rest.is_empty() {
        return None;
    }
    match head {
        "Self" => resolve_self(reference, rest, context),
        "crate" | "self" | "super" => resolve_module_path(reference, &parts, context),
        _ => resolve_scoped_path(reference, &parts, context)
            .or_else(|| resolve_crate_path(reference, &parts, context))
            .or_else(|| resolve_projection(reference, &parts, context)),
    }
}

/// `codegraph::types::Node`, `tokio::sync::Mutex::new`: a path whose first
/// segment names a crate of the workspace (no module in scope does),
/// resolved from that crate's root like a `crate::` path of it.
fn resolve_crate_path(
    reference: &UnresolvedRef,
    parts: &[&str],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (&head, rest) = parts.split_first()?;
    let (&item, module_path) = rest.split_last()?;
    if module_path.iter().any(|segment| is_path_keyword(segment)) {
        return None;
    }
    let dir = project_crate_dir(head, context)?;
    let root = ModuleLocation {
        crate_key: dir.trim_end_matches('/').to_string(),
        module: Vec::new(),
    };
    let path = ModulePath {
        caller: caller_module(reference, context),
        target: root.walk(module_path)?,
        owner: owner_segment(module_path),
        item,
        guess: Guess::SameCrate,
    };
    resolve_in_target(reference, &path, context)
}

/// `T::Value` in a type, `T` a generic parameter bounded by a project trait
/// (`T: RequestConfigValue`): the associated type the trait declares, when
/// exactly one project trait declares one of that name. A `T` the project
/// defines as a type is left to the other rules.
fn resolve_projection(
    reference: &UnresolvedRef,
    parts: &[&str],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let [param, item] = parts else {
        return None;
    };
    if reference.reference_kind == EdgeKind::Calls
        || !starts_uppercase(param)
        || !starts_uppercase(item)
        || names_rust_type(context, param)
    {
        return None;
    }
    let declared: Vec<Node> = context
        .get_nodes_by_name_and_kind(item, NodeKind::TypeAlias)
        .into_iter()
        .filter(|node| {
            node.language == Language::Rust
                && node
                    .qualified_name
                    .rsplit_once("::")
                    .is_some_and(|(owner, _)| owner_is_trait(owner, node, context))
        })
        .collect();
    let declared: Vec<&Node> = declared.iter().collect();
    unique(&declared).map(|node| resolved(reference, node, 0.8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_generic_arguments_from_segments() {
        assert_eq!(segments("Vec::<u8>::new"), ["Vec", "new"]);
        assert_eq!(segments("crate::a::b"), ["crate", "a", "b"]);
    }
}
