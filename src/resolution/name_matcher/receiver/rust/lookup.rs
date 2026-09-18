//! Project lookups behind Rust receiver inference: what a type path names,
//! what a callee's signature returns, and which of several same-named
//! definitions a file means.

use std::collections::HashSet;
use std::sync::Arc;

use super::bindings::assignment;
use super::crates::project_crate_dir;
use super::types::{Named, named_type, signature_return};
use crate::resolution::name_matcher::rust_path::crate_key;
use crate::resolution::name_matcher::{UseBinding, UseLeaf};
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{Language, Node, NodeKind};

/// How many `use`/alias hops a type path is followed through.
const MAX_HOPS: u8 = 6;

/// A type a Rust file names, pinned down as far as the file allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::resolution::name_matcher) struct RustType {
    /// The type's own name (`CodeGraph`).
    pub(in crate::resolution::name_matcher) name: String,
    home: Home,
}

/// Where a type is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Home {
    /// Only the name is known (a type in scope without a `use` naming it),
    /// with the modules the file glob-imports (`use crate::fixture::*`).
    Anywhere { globs: Vec<String> },
    /// A crate of the project, and the module its path names, if any.
    Project {
        scope: CrateScope,
        module: Option<String>,
    },
    /// A crate outside the project (`std`, `tree_sitter`), or a slice,
    /// tuple, or pointer: no project method runs on it.
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CrateScope {
    /// The crate with this key (`crate::`, `super::`, a local module).
    Key(String),
    /// Another crate of the workspace, by its source directory.
    Dir(String),
}

impl CrateScope {
    fn contains(&self, file_path: &str) -> bool {
        match self {
            CrateScope::Key(key) => crate_key(file_path) == *key,
            CrateScope::Dir(dir) => file_path.starts_with(dir.as_str()),
        }
    }
}

/// How near a definition is to what a reference names; smaller is nearer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    outside_home: bool,
    outside_module: bool,
    other_file: bool,
    outside_globs: bool,
    other_crate: bool,
    /// Directories the definition's file does not share with the
    /// reference's (integration-test files are one-file crates, so this
    /// separates `tests/a/fixture.rs` from `tests/b/fixture.rs`).
    distance: usize,
}

impl RustType {
    fn anywhere(name: &str) -> Self {
        RustType {
            name: name.to_string(),
            home: Home::Anywhere { globs: Vec::new() },
        }
    }

    pub(super) fn external(name: &str) -> Self {
        RustType {
            name: name.to_string(),
            home: Home::External,
        }
    }

    /// The project defines this type (not merely a same-named one).
    pub(in crate::resolution::name_matcher) fn is_project_type(
        &self,
        context: &dyn ResolutionContext,
    ) -> bool {
        self.home != Home::External && is_rust_project_type(&self.name, context)
    }

    /// The method `Self::method` the project defines, best match first. An
    /// external type's methods are project methods only when no project
    /// type shares its name (an extension trait implemented for `Vec`).
    pub(in crate::resolution::name_matcher) fn method<'a>(
        &self,
        method: &str,
        nodes: &'a [Node],
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<&'a Node> {
        if self.home == Home::External && is_rust_project_type(&self.name, context) {
            return None;
        }
        methods_of(self, nodes, method, reference).first().copied()
    }

    /// The field `Self::field` the project declares, best match first.
    pub(super) fn field<'a>(
        &self,
        field: &str,
        nodes: &'a [Node],
        reference: &UnresolvedRef,
    ) -> Option<&'a Node> {
        let exact = format!("{}::{field}", self.name);
        let suffix = format!("::{exact}");
        let mut fields: Vec<&Node> = nodes
            .iter()
            .filter(|node| {
                node.language == Language::Rust
                    && node.kind == NodeKind::Field
                    && (node.qualified_name == exact || node.qualified_name.ends_with(&suffix))
            })
            .collect();
        self.sort_by_home(&mut fields, reference);
        fields.first().copied()
    }

    /// Sort `nodes` (definitions named like this type or its members) by
    /// how well they match its home, then by nearness to `reference`.
    fn sort_by_home(&self, nodes: &mut [&Node], reference: &UnresolvedRef) {
        let here = crate_key(&reference.file_path);
        nodes.sort_by_cached_key(|node| self.rank(node, &reference.file_path, &here));
    }

    fn rank(&self, node: &Node, file: &str, here: &str) -> Rank {
        let path = node.file_path.as_str();
        let (in_home, in_module, in_globs) = match &self.home {
            Home::Project { scope, module } => (
                scope.contains(path),
                module
                    .as_deref()
                    .is_some_and(|module| file_is_module(path, module)),
                false,
            ),
            Home::Anywhere { globs } => (
                true,
                false,
                globs.iter().any(|module| file_is_module(path, module)),
            ),
            Home::External => (true, false, false),
        };
        let shared = path
            .split('/')
            .zip(file.split('/'))
            .take_while(|(a, b)| a == b)
            .count();
        Rank {
            outside_home: !in_home,
            outside_module: !in_module,
            other_file: path != file,
            outside_globs: !in_globs,
            other_crate: crate_key(path) != here,
            distance: path.split('/').count().saturating_sub(shared),
        }
    }

    /// The nearest of `nodes` (all tied for nearest).
    fn nearest<'a>(&self, mut nodes: Vec<&'a Node>, reference: &UnresolvedRef) -> Vec<&'a Node> {
        let here = crate_key(&reference.file_path);
        let Some(best) = nodes
            .iter()
            .map(|node| self.rank(node, &reference.file_path, &here))
            .min()
        else {
            return nodes;
        };
        nodes.retain(|node| self.rank(node, &reference.file_path, &here) == best);
        nodes
    }
}

fn is_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Struct | NodeKind::Enum | NodeKind::Union | NodeKind::Trait | NodeKind::TypeAlias
    )
}

/// `name` is a struct, enum, union, trait, or type alias the project defines.
fn is_rust_project_type(name: &str, context: &dyn ResolutionContext) -> bool {
    context
        .get_nodes_by_name(name)
        .iter()
        .any(|node| node.language == Language::Rust && is_type_kind(node.kind))
}

/// `file_path` holds module `module` (`…/module.rs` or `…/module/mod.rs`).
pub(in crate::resolution::name_matcher) fn file_is_module(file_path: &str, module: &str) -> bool {
    let mut parts = file_path.rsplit('/');
    let file = parts.next().unwrap_or_default();
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    stem == module || (stem == "mod" && parts.next() == Some(module))
}

/// What a written type means in `file`.
pub(super) fn resolve_named(
    named: Named<'_>,
    self_ty: Option<&RustType>,
    file: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<RustType> {
    match named {
        Named::SelfType => self_ty.cloned(),
        Named::Structural(name) => Some(RustType::external(name)),
        Named::Path(path) => Some(resolve_type(path, file, reference, context)),
    }
}

/// What the type path `path`, written in `file`, names: `use` declarations
/// and type aliases followed, the crate its first segment names recorded.
pub(in crate::resolution::name_matcher) fn resolve_type(
    path: &str,
    file: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> RustType {
    let mut path = path.to_string();
    let mut file = file.to_string();
    for _ in 0..MAX_HOPS {
        let resolved = resolve_path(&path, &file, context);
        match resolved {
            Resolved::Type(ty) => match alias_target(&ty, reference, context) {
                Some((target, alias_file)) => {
                    path = target;
                    file = alias_file;
                }
                None => return ty,
            },
            Resolved::Rewritten(rewritten) => path = rewritten,
        }
    }
    RustType::anywhere(path.rsplit("::").next().unwrap_or_default())
}

enum Resolved {
    Type(RustType),
    /// The path with a `use`d first segment replaced by what it imports.
    Rewritten(String),
}

fn resolve_path(path: &str, file: &str, context: &dyn ResolutionContext) -> Resolved {
    let segments: Vec<&str> = path.split("::").filter(|part| !part.is_empty()).collect();
    let name = segments.last().copied().unwrap_or_default();
    let Some((&root, rest)) = segments.split_first() else {
        return Resolved::Type(RustType::anywhere(name));
    };
    if let Some(mut full) = use_path(root, file, context) {
        // `use a::b::Name [as root];` then `root::…`: continue from `a::b::Name`.
        full.extend(rest.iter().map(|part| part.to_string()));
        if full != segments {
            return Resolved::Rewritten(full.join("::"));
        }
    }
    if rest.is_empty() {
        return Resolved::Type(RustType {
            name: name.to_string(),
            home: Home::Anywhere {
                globs: glob_modules(file, context),
            },
        });
    }
    let module = (segments.len() >= 3).then(|| segments[segments.len() - 2].to_string());
    let home = match root {
        "crate" | "$crate" | "self" | "super" => Home::Project {
            scope: CrateScope::Key(crate_key(file)),
            module,
        },
        "Self" => Home::Anywhere { globs: Vec::new() },
        _ => match project_crate_dir(root, context) {
            Some(dir) => Home::Project {
                scope: CrateScope::Dir(dir),
                module,
            },
            None if is_local_module(root, file, context) => Home::Project {
                scope: CrateScope::Key(crate_key(file)),
                module: module.or_else(|| Some(root.to_string())),
            },
            None => Home::External,
        },
    };
    Resolved::Type(RustType {
        name: name.to_string(),
        home,
    })
}

/// The modules `file` glob-imports: `use crate::fixture::*` -> `fixture`.
fn glob_modules(file: &str, context: &dyn ResolutionContext) -> Vec<String> {
    context
        .get_rust_use_leaves(file)
        .iter()
        .filter(|found| found.leaf.binding == UseBinding::Glob)
        .filter_map(|found| found.leaf.path.last().cloned())
        .filter(|module| !matches!(module.as_str(), "crate" | "self" | "super"))
        .collect()
}

/// The full path a `use` in `file` binds `name` to (`use a::b as name`).
fn use_path(name: &str, file: &str, context: &dyn ResolutionContext) -> Option<Vec<String>> {
    let binding = UseBinding::Name(name.to_string());
    let declared = context
        .get_rust_use_leaves(file)
        .iter()
        .map(|found| &found.leaf)
        .find(|leaf| leaf.binding == binding)
        .cloned();
    declared
        .or_else(|| {
            fn_local_uses(file, context)
                .iter()
                .find(|leaf| leaf.binding == binding)
                .cloned()
        })
        .map(|leaf| leaf.path)
}

/// `use` declarations written inside fn bodies, which the index keeps no
/// Import node for.
pub(in crate::resolution::name_matcher) fn fn_local_uses(
    file: &str,
    context: &dyn ResolutionContext,
) -> Arc<[UseLeaf]> {
    context.get_rust_fn_local_uses(file)
}

/// `root` is a module of `file`'s crate (a 2018-edition relative path).
fn is_local_module(root: &str, file: &str, context: &dyn ResolutionContext) -> bool {
    let here = crate_key(file);
    context.get_nodes_by_name(root).iter().any(|node| {
        node.language == Language::Rust
            && node.kind == NodeKind::Module
            && crate_key(&node.file_path) == here
    })
}

/// When the definition `ty` names is a type alias: the aliased type as
/// written, and the file it is written in.
fn alias_target(
    ty: &RustType,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<(String, String)> {
    if ty.home == Home::External {
        return None;
    }
    let nodes = context.get_nodes_by_name(&ty.name);
    let mut types: Vec<&Node> = nodes
        .iter()
        .filter(|node| node.language == Language::Rust && is_type_kind(node.kind))
        .collect();
    ty.sort_by_home(&mut types, reference);
    let alias = types
        .first()
        .filter(|node| node.kind == NodeKind::TypeAlias)?;
    let target = aliased_text(alias, context)?;
    match named_type(&target)? {
        Named::Path(path) => Some((path.to_string(), alias.file_path.clone())),
        Named::SelfType | Named::Structural(_) => None,
    }
}

/// The text right of `=` in `type Name<..> = Type;`.
fn aliased_text(alias: &Node, context: &dyn ResolutionContext) -> Option<String> {
    let source = context.read_file_arc(&alias.file_path)?;
    let start = alias.start_line.saturating_sub(1) as usize;
    let length = (alias.end_line.max(alias.start_line) as usize).saturating_sub(start);
    let declaration = source
        .lines()
        .skip(start)
        .take(length.max(1))
        .collect::<Vec<_>>()
        .join(" ");
    let equals = assignment(&declaration)?;
    Some(
        declaration[equals + 1..]
            .split(';')
            .next()?
            .trim()
            .to_string(),
    )
}

/// Methods named `method` on the type `ty` (`Ty::method`), best match first.
fn methods_of<'a>(
    ty: &RustType,
    nodes: &'a [Node],
    method: &str,
    reference: &UnresolvedRef,
) -> Vec<&'a Node> {
    let exact = format!("{}::{method}", ty.name);
    let suffix = format!("::{exact}");
    let mut methods: Vec<&Node> = nodes
        .iter()
        .filter(|node| {
            node.language == Language::Rust
                && node.kind == NodeKind::Method
                && (node.qualified_name == exact || node.qualified_name.ends_with(&suffix))
        })
        .collect();
    ty.sort_by_home(&mut methods, reference);
    methods
}

/// The return type written on the associated fn `owner::name`, with the
/// file it is written in.
pub(super) fn assoc_fn_return(
    owner: &RustType,
    name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<(String, String)> {
    let nodes = context.get_nodes_by_name(name);
    let candidates = methods_of(owner, &nodes, name, reference);
    agreed_return(owner.nearest(candidates, reference), Some(&owner.name))
}

/// The return type written on the free fn at `path` (`load`, `graph::load`)
/// called from the reference's file, with the file it is written in.
pub(super) fn free_fn_return(
    path: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<(String, String)> {
    // A path or `use` pins the fn's crate and module like a type's.
    let home = resolve_type(path, &reference.file_path, reference, context);
    if home.home == Home::External {
        return None;
    }
    let nodes = context.get_nodes_by_name(&home.name);
    let candidates: Vec<&Node> = nodes
        .iter()
        .filter(|node| node.language == Language::Rust && node.kind == NodeKind::Function)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    agreed_return(home.nearest(candidates, reference), None)
}

/// The return type the candidates agree on (`Self` and `owner` alike), and
/// the first one's file.
fn agreed_return(candidates: Vec<&Node>, owner: Option<&str>) -> Option<(String, String)> {
    let returns: Option<Vec<&str>> = candidates
        .iter()
        .map(|node| node.signature.as_deref().and_then(signature_return))
        .collect();
    let returns = returns?;
    let first = *returns.first()?;
    let same = |text: &str| match owner {
        Some(owner) if text == "Self" => owner.to_string(),
        _ => text.to_string(),
    };
    let distinct: HashSet<String> = returns.iter().map(|text| same(text)).collect();
    let file = candidates.first()?.file_path.clone();
    (distinct.len() == 1).then(|| (first.to_string(), file))
}
