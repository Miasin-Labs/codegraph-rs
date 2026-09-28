//! Lookups through rustdoc's API indexes ([`crate::deps::rustdoc`]) — the
//! compiler's own account of a crate's paths, re-exports and impls — for
//! the graphs that have one; the name and layout heuristics of
//! [`super::lookup`] stay for those that do not.
//!
//! * **Paths** walk the index's public paths: a re-export of another
//!   crate's item or module continues in that crate's index, a glob
//!   re-export (`pub mod task { pub use core::task::*; }`) tries each crate
//!   it names — exactly one must answer. A path ending in a member
//!   (`Vec::with_capacity`, `Ordering::Less`) is that type's variant or
//!   associated item: one inherent item, else one trait item.
//! * **Methods** (`recv.m()` on a known type) are probed the way rustc
//!   probes them: at each auto-deref step (`String` → `str`), the type's
//!   inherent methods first, then methods of the traits it implements —
//!   impls anywhere in the toolchain, and blanket impls whose bounds the
//!   type meets — counting only traits the calling file has in scope
//!   ([`TraitScope`]); a trait method the impl does not override is the
//!   trait's own. Several candidates, or one that depends on a trait this
//!   pass cannot see, answer nothing.
//! * **Items become nodes** by where rustdoc says they are written: the
//!   node of that file, name and kind spanning that line (the shard build
//!   made sure there is one — [`crate::deps::rustdoc::reconcile`]).

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use super::lookup::Found;
use super::scope::{InScope, TraitScope};
use crate::deps::rustdoc::reconcile::matching_node;
use crate::deps::rustdoc::{ApiImpl, ApiIndex, ApiItem, ApiKind, PathStep};
use crate::resolution::external::graphs::GraphLocation;
use crate::resolution::external::open::{ForeignGraph, GraphCache};
use crate::resolution::types::ResolutionContext;
use crate::types::{EdgeKind, Node, NodeKind};

/// Most crate-to-crate re-export hops a path follows.
const MAX_HOPS: u8 = 8;
/// Most auto-deref steps a method probe takes.
const MAX_DEREF: usize = 4;
/// Most type-alias hops.
const MAX_ALIAS: usize = 4;
/// The toolchain crates, in dependency order.
const TOOLCHAIN: &[&str] = &["core", "alloc", "std"];
const DEREF: &str = "core::ops::deref::Deref";
/// The prelude modules of `std` whose traits every file has in scope.
const PRELUDES: &[&str] = &["prelude::rust_2021", "prelude::rust_2024"];
/// Most modules a prelude or glob walk visits.
const MAX_MODULES: usize = 64;

/// `CODEGRAPH_API_TRACE=1`: print every API path and method lookup and
/// what it came to (stderr).
pub(crate) fn tracing() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("CODEGRAPH_API_TRACE").is_some_and(|v| v != "0"))
}

/// An item of a graph's API index.
#[derive(Clone)]
pub(crate) struct ApiRef {
    graph: Rc<ForeignGraph>,
    api: Arc<ApiIndex>,
    item: u32,
}

impl ApiRef {
    fn item(&self) -> &ApiItem {
        &self.api.items[self.item as usize]
    }

    /// Two references to one item (an index may hold a copy of another
    /// crate's item).
    fn place(&self) -> (String, u32, String) {
        let item = self.item();
        let file = item
            .file
            .strip_prefix("@toolchain:")
            .unwrap_or(&item.file)
            .to_string();
        (file, item.line, item.name.clone())
    }
}

/// Where a path led.
enum Resolved {
    Item(ApiRef, bool),
    /// A type or trait, and what is left of the path (a member).
    Member(ApiRef, Vec<String>, bool),
    /// Into a crate without an index: its own lookup answers.
    Legacy(String, Vec<String>),
    NotFound,
}

/// The graph of `krate` and its API index, when it has one.
fn api_of(cache: &GraphCache<'_>, krate: &str) -> Option<(Rc<ForeignGraph>, Arc<ApiIndex>)> {
    let graph = cache.get(krate)?;
    let api = graph.api.clone()?;
    Some((graph, api))
}

/// The crate `krate` has an API index.
pub(crate) fn has_index(cache: &GraphCache<'_>, krate: &str) -> bool {
    api_of(cache, krate).is_some()
}

fn segments(path: &str) -> Vec<String> {
    if path.is_empty() {
        Vec::new()
    } else {
        path.split("::").map(str::to_string).collect()
    }
}

/// Where `path` (relative to `krate`'s root) leads.
fn resolve(
    cache: &GraphCache<'_>,
    krate: &str,
    path: &[String],
    hops: u8,
    reexported: bool,
) -> Resolved {
    let Some(graph) = cache.get(krate) else {
        return Resolved::NotFound;
    };
    let Some(api) = graph.api.clone() else {
        return Resolved::Legacy(krate.to_string(), path.to_vec());
    };
    match api.resolve(path) {
        PathStep::Item(item) => Resolved::Item(ApiRef { graph, api, item }, reexported),
        PathStep::Member { item, rest } => {
            Resolved::Member(ApiRef { graph, api, item }, rest, reexported)
        }
        PathStep::External { target, rest } if hops < MAX_HOPS => {
            let mut next = segments(&target.path);
            next.extend(rest);
            resolve(cache, &target.krate, &next, hops + 1, true)
        }
        PathStep::Globs { targets, rest } if hops < MAX_HOPS => {
            let mut answers: Vec<Resolved> = Vec::new();
            for target in targets {
                let mut next = segments(&target.path);
                next.extend(rest.iter().cloned());
                match resolve(cache, &target.krate, &next, hops + 1, true) {
                    Resolved::NotFound => {}
                    found => answers.push(found),
                }
            }
            match answers.len() {
                1 => answers.pop().unwrap_or(Resolved::NotFound),
                _ => Resolved::NotFound,
            }
        }
        _ => Resolved::NotFound,
    }
}

/// The item `rest` names in `krate` through the API indexes, as a node
/// admitted for a reference of `kind` — `None` when `krate` has no index
/// (the heuristics answer), `Some(None)` when the index has no such item.
pub(crate) fn lookup_path(
    cache: &GraphCache<'_>,
    krate: &str,
    rest: &[String],
    kind: EdgeKind,
) -> Option<Option<Found>> {
    if !has_index(cache, krate) || rest.is_empty() {
        return None;
    }
    let answer = lookup_indexed_path(cache, krate, rest, kind);
    if tracing() {
        eprintln!(
            "api path {krate}::{} → {:?}",
            rest.join("::"),
            answer.as_ref().map(|found| (
                found.node.qualified_name.clone(),
                found.node.file_path.clone()
            ))
        );
    }
    Some(answer)
}

fn lookup_indexed_path(
    cache: &GraphCache<'_>,
    krate: &str,
    rest: &[String],
    kind: EdgeKind,
) -> Option<Found> {
    let (item, reexported) = match resolve(cache, krate, rest, 0, false) {
        Resolved::Item(item, reexported) => (Candidate::Api(item), reexported),
        Resolved::Member(owner, rest, reexported) => (member(cache, &owner, &rest)?, reexported),
        Resolved::Legacy(krate, path) => {
            return super::lookup::lookup_path(cache, &krate, &path, kind).map(|mut found| {
                found.reexported = true;
                found
            });
        }
        Resolved::NotFound => return None,
    };
    let (graph, node) = item.node(cache)?;
    admitted(&node, kind).then_some(Found {
        graph,
        node,
        confidence: 0.95,
        reexported,
    })
}

/// The node kinds a reference of `kind` may target.
fn admitted(node: &Node, kind: EdgeKind) -> bool {
    match kind {
        EdgeKind::Implements | EdgeKind::Extends => node.kind == NodeKind::Trait,
        EdgeKind::Calls | EdgeKind::Instantiates => matches!(
            node.kind,
            NodeKind::Function
                | NodeKind::Method
                | NodeKind::Struct
                | NodeKind::EnumMember
                | NodeKind::Constant
                | NodeKind::Variable
        ),
        _ => !matches!(
            node.kind,
            NodeKind::Module | NodeKind::File | NodeKind::Import
        ),
    }
}

/// A member of the type or trait `owner`: a variant, else the one
/// inherent associated item named so, else the one trait item.
fn member(cache: &GraphCache<'_>, owner: &ApiRef, rest: &[String]) -> Option<Candidate> {
    let [name] = rest else {
        return None;
    };
    let item = owner.item();
    if item.kind == ApiKind::Trait {
        let index = *owner.api.trait_items(&item.canonical)?.get(name)?;
        return Some(Candidate::Api(ApiRef {
            item: index,
            ..owner.clone()
        }));
    }
    let ty = type_key(cache, owner)?;
    if let Some(variant) = owner
        .api
        .ty(&ty.key)
        .and_then(|found| found.variants.get(name))
    {
        return Some(Candidate::Api(ApiRef {
            item: *variant,
            ..owner.clone()
        }));
    }
    let impls = impls_of(cache, &ty);
    let mut inherent: Vec<Candidate> = impls
        .iter()
        .filter(|found| found.imp().trait_path.is_none())
        .filter_map(|found| found.member(name))
        .map(Candidate::Api)
        .collect();
    inherent.extend(incoherent(cache, &ty.key, name, false));
    if let Some(one) = one(inherent) {
        return one;
    }
    let traits: Vec<Candidate> = impls
        .iter()
        .filter(|found| found.imp().trait_path.is_some() && !found.imp().negative)
        .filter_map(|found| found.member_or_provided(cache, name))
        .map(Candidate::Api)
        .collect();
    one(traits).flatten()
}

/// What a lookup may find: an item of an index, or — for an inherent method
/// rustdoc leaves out — the node itself.
#[derive(Clone)]
enum Candidate {
    Api(ApiRef),
    Node(Rc<ForeignGraph>, Box<Node>),
}

impl Candidate {
    fn place(&self) -> (String, u32, String) {
        match self {
            Candidate::Api(item) => item.place(),
            Candidate::Node(_, node) => {
                (node.file_path.clone(), node.start_line, node.name.clone())
            }
        }
    }

    /// The node it is.
    fn node(&self, cache: &GraphCache<'_>) -> Option<(Rc<ForeignGraph>, Node)> {
        match self {
            Candidate::Api(item) => node_of(cache, item),
            Candidate::Node(graph, node) => Some((Rc::clone(graph), (**node).clone())),
        }
    }
}

/// `Some(Some(x))`: exactly one distinct item; `Some(None)`: several;
/// `None`: none.
fn one(candidates: Vec<Candidate>) -> Option<Option<Candidate>> {
    let mut places = HashSet::new();
    let mut distinct = Vec::new();
    for candidate in candidates {
        if places.insert(candidate.place()) {
            distinct.push(candidate);
        }
    }
    match distinct.len() {
        0 => None,
        1 => Some(distinct.pop()),
        _ => Some(None),
    }
}

/// A type, as the probe sees it: its key, and the generic arguments a
/// type alias gave it (`AtomicBool` = `Atomic<bool>`), which pick the
/// impls that apply.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TypeRef {
    key: String,
    args: Vec<Option<String>>,
}

impl TypeRef {
    fn bare(key: String) -> TypeRef {
        TypeRef {
            key,
            args: Vec::new(),
        }
    }

    /// The impl, written for arguments `for_args`, applies to this type.
    fn admits(&self, for_args: &[Option<String>]) -> bool {
        self.args.is_empty()
            || for_args.len() != self.args.len()
            || for_args
                .iter()
                .zip(&self.args)
                .all(|(written, ours)| match (written, ours) {
                    (Some(written), Some(ours)) => written == ours,
                    _ => true,
                })
    }
}

/// The type a type item is (a type alias followed to what it stands for).
fn type_key(cache: &GraphCache<'_>, item: &ApiRef) -> Option<TypeRef> {
    let mut ty = TypeRef::bare(item.item().canonical.clone());
    if !item.item().kind.is_type() || ty.key.is_empty() {
        return None;
    }
    for _ in 0..MAX_ALIAS {
        let home = ty.key.split("::").next().unwrap_or_default().to_string();
        let alias = api_of(cache, &home).and_then(|(_, api)| {
            api.ty(&ty.key)
                .and_then(|found| Some((found.alias_of.clone()?, found.alias_args.clone())))
        });
        match alias {
            Some((key, args)) => ty = TypeRef { key, args },
            None => break,
        }
    }
    Some(ty)
}

/// One impl of a type, in the graph whose index holds it.
struct FoundImpl {
    graph: Rc<ForeignGraph>,
    api: Arc<ApiIndex>,
    index: usize,
    /// A blanket impl whose bounds could not all be checked.
    uncertain: bool,
}

impl FoundImpl {
    fn imp(&self) -> &ApiImpl {
        &self.api.impls[self.index]
    }

    fn at(&self, item: u32) -> ApiRef {
        ApiRef {
            graph: Rc::clone(&self.graph),
            api: Arc::clone(&self.api),
            item,
        }
    }

    /// The impl's own member `name`.
    fn member(&self, name: &str) -> Option<ApiRef> {
        self.imp().items.get(name).map(|item| self.at(*item))
    }

    /// The member `name`, or the trait's own when the impl does not
    /// override a provided method.
    fn member_or_provided(&self, cache: &GraphCache<'_>, name: &str) -> Option<ApiRef> {
        if let Some(own) = self.member(name) {
            return Some(own);
        }
        let imp = self.imp();
        if !imp.provided.iter().any(|provided| provided == name) {
            return None;
        }
        trait_item(cache, imp.trait_path.as_deref()?, name)
    }
}

/// The item `name` of the trait `canonical`, from its crate's index.
fn trait_item(cache: &GraphCache<'_>, canonical: &str, name: &str) -> Option<ApiRef> {
    let home = canonical.split("::").next()?;
    let (graph, api) = api_of(cache, home)?;
    let item = *api.trait_items(canonical)?.get(name)?;
    Some(ApiRef { graph, api, item })
}

/// Every impl of the type `key` the reachable indexes hold: its own
/// crate's list (with the blanket impls rustdoc found to apply), impls of
/// it elsewhere in the toolchain, and blanket impls of the toolchain crates
/// its own crate cannot see (`alloc`'s `ToString` for `core`'s types) whose
/// bounds it meets.
fn impls_of(cache: &GraphCache<'_>, ty: &TypeRef) -> Vec<FoundImpl> {
    let key = ty.key.as_str();
    let home = if key.starts_with("prim:") {
        None
    } else {
        key.split("::").next()
    };
    let mut found = Vec::new();
    let mut seen_crates = HashSet::new();
    let mut add = |graph: Rc<ForeignGraph>, api: Arc<ApiIndex>, found: &mut Vec<FoundImpl>| {
        if !seen_crates.insert(api.krate.clone()) {
            return;
        }
        let own = api.ty(key).map(|ty| ty.impls.clone()).unwrap_or_default();
        let foreign = api.foreign_impls.get(key).cloned().unwrap_or_default();
        for index in own.into_iter().chain(foreign) {
            if !ty.admits(&api.impls[index as usize].for_args) {
                continue;
            }
            found.push(FoundImpl {
                graph: Rc::clone(&graph),
                api: Arc::clone(&api),
                index: index as usize,
                uncertain: false,
            });
        }
    };
    if let Some((graph, api)) = home.and_then(|home| api_of(cache, home)) {
        add(graph, api, &mut found);
    }
    for krate in TOOLCHAIN {
        if let Some((graph, api)) = api_of(cache, krate) {
            add(graph, api, &mut found);
        }
    }
    // Blanket impls of toolchain crates above the type's own.
    let above: &[&str] = match home {
        None => &["alloc", "std"],
        Some("core") => &["alloc", "std"],
        Some("alloc") => &["std"],
        Some(_) => &[],
    };
    let implemented: HashSet<String> = found
        .iter()
        .filter(|found| !found.imp().negative)
        .filter_map(|found| found.imp().trait_path.clone())
        .collect();
    for krate in above {
        let Some((graph, api)) = api_of(cache, krate) else {
            continue;
        };
        for index in &api.blankets {
            let imp = &api.impls[*index as usize];
            let (applies, uncertain) = match &imp.bounds {
                Some(bounds) => (
                    bounds.iter().all(|bound| implemented.contains(bound)),
                    false,
                ),
                None => (true, true),
            };
            if applies {
                found.push(FoundImpl {
                    graph: Rc::clone(&graph),
                    api: Arc::clone(&api),
                    index: *index as usize,
                    uncertain,
                });
            }
        }
    }
    found
}

/// What a receiver's type is, as far as its methods go.
enum Owner {
    Type(TypeRef),
    /// A trait (`dyn Write`, `impl Iterator`): its own methods.
    Trait(ApiRef),
}

/// The type (or trait) the `owner` path, inside `graph`'s crate, names.
fn owner_of(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    api: &Arc<ApiIndex>,
    owner: &[String],
) -> Option<Owner> {
    let of_item = |item: ApiRef| {
        if item.item().kind == ApiKind::Trait {
            Some(Owner::Trait(item))
        } else {
            type_key(cache, &item).map(Owner::Type)
        }
    };
    if let [name] = owner {
        let primitive = format!("prim:{name}");
        if TOOLCHAIN
            .iter()
            .any(|krate| api_of(cache, krate).is_some_and(|(_, api)| api.ty(&primitive).is_some()))
        {
            return Some(Owner::Type(TypeRef::bare(primitive)));
        }
    }
    match resolve(cache, &graph.krate, owner, 0, false) {
        Resolved::Item(item, _) => return of_item(item),
        Resolved::Legacy(..) | Resolved::Member(..) => return None,
        Resolved::NotFound => {}
    }
    // A type written bare inside the crate: the one of that name.
    let name = owner.last()?;
    let suffix = format!("::{name}");
    let mut named = api
        .types
        .iter()
        .filter(|(key, ty)| {
            key.ends_with(&suffix) && api.items[ty.item as usize].kind != ApiKind::Primitive
        })
        .map(|(_, ty)| ty.item);
    let first = named.next()?;
    if named.next().is_some() {
        return None;
    }
    of_item(ApiRef {
        graph: Rc::clone(graph),
        api: Arc::clone(api),
        item: first,
    })
}

/// How a method lookup's caller sees traits: its file's scope and the
/// project around it.
pub(crate) struct Caller<'a> {
    pub(crate) scope: &'a TraitScope,
    pub(crate) project: &'a dyn ResolutionContext,
    pub(crate) file: &'a str,
    /// The reference's line.
    pub(crate) line: u32,
}

impl Caller<'_> {
    /// A `use` on `line` is inside the fn the reference is in.
    fn in_same_fn(&self, line: u32) -> bool {
        let innermost = |at: u32| {
            self.project
                .scopes_enclosing_line(self.file, at)
                .into_iter()
                .find(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
                .map(|node| node.id)
        };
        let here = innermost(self.line);
        here.is_some() && here == innermost(line)
    }
}

/// `method` of the type at `owner` in `graph`'s crate, probed through the
/// API indexes — `None` when the crate has no index, `Some(None)` when no
/// single method answers.
pub(crate) fn lookup_method(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    owner: &[String],
    method: &str,
    caller: Option<&Caller<'_>>,
) -> Option<Option<(Rc<ForeignGraph>, Node)>> {
    let api = graph.api.clone()?;
    if let Resolved::Legacy(krate, path) = resolve(cache, &graph.krate, owner, 0, false) {
        let other = cache.get(&krate)?;
        return Some(super::lookup::legacy_method(cache, &other, &path, method));
    }
    let found = match owner_of(cache, graph, &api, owner) {
        Some(Owner::Type(ty)) => probe(cache, &ty, method, caller),
        // A trait's own method (not its supertraits').
        Some(Owner::Trait(item)) => owner_trait_method(&item, method),
        None => None,
    };
    let node = found.as_ref().and_then(|candidate| candidate.node(cache));
    if tracing() {
        eprintln!(
            "api method {}::{owner:?} .{method} → {:?}",
            graph.krate,
            node.as_ref().map(|(_, n)| (
                n.qualified_name.clone(),
                n.file_path.clone(),
                n.start_line
            ))
        );
    }
    Some(node)
}

/// The method `method` the trait `item` itself declares.
fn owner_trait_method(item: &ApiRef, method: &str) -> Option<Candidate> {
    let index = *item.api.trait_items(&item.item().canonical)?.get(method)?;
    let found = ApiRef {
        item: index,
        ..item.clone()
    };
    (found.item().kind == ApiKind::Method && found.item().has_self).then_some(Candidate::Api(found))
}

/// rustc's method probe over the API indexes (see the module docs).
fn probe(
    cache: &GraphCache<'_>,
    start: &TypeRef,
    method: &str,
    caller: Option<&Caller<'_>>,
) -> Option<Candidate> {
    let prelude = prelude_traits(cache);
    let unseen_traits = caller.is_none_or(|caller| other_traits_define(cache, caller, method));
    let mut ty = start.clone();
    let mut visited = HashSet::new();
    for _ in 0..=MAX_DEREF {
        if !visited.insert(ty.clone()) {
            return None;
        }
        let key = ty.key.clone();
        let impls = impls_of(cache, &ty);
        let mut inherent: Vec<Candidate> = impls
            .iter()
            .filter(|found| found.imp().trait_path.is_none())
            .filter_map(|found| found.member(method))
            .filter(|item| item.item().kind == ApiKind::Method && item.item().has_self)
            .map(Candidate::Api)
            .collect();
        inherent.extend(incoherent(cache, &key, method, true));
        if let Some(answer) = one(inherent) {
            return answer;
        }
        let mut callable: Vec<Candidate> = Vec::new();
        let mut unsure = false;
        for found in &impls {
            let imp = found.imp();
            let Some(trait_path) = imp.trait_path.as_deref() else {
                continue;
            };
            if imp.negative || imp.synthetic {
                continue;
            }
            let Some(item) = found.member_or_provided(cache, method) else {
                continue;
            };
            if item.item().kind != ApiKind::Method || !item.item().has_self {
                continue;
            }
            let in_scope = match caller {
                Some(caller) => caller
                    .scope
                    .has(&prelude, trait_path, &|line| caller.in_same_fn(line)),
                None if prelude.contains(trait_path) => InScope::Yes,
                None => InScope::Maybe,
            };
            match in_scope {
                InScope::No => continue,
                InScope::Maybe => unsure = true,
                InScope::Yes => {}
            }
            unsure |= found.uncertain;
            callable.push(Candidate::Api(item));
        }
        if tracing() {
            eprintln!(
                "  probe {key}.{method}: trait candidates {:?} unsure={unsure} unseen={unseen_traits}",
                callable.iter().map(|c| c.place()).collect::<Vec<_>>()
            );
        }
        if callable.is_empty() && !unsure {
            // A trait this pass cannot see may answer at this step.
            if unseen_traits {
                return None;
            }
            ty = TypeRef::bare(deref_target(&impls)?);
            continue;
        }
        if unsure || unseen_traits {
            return None;
        }
        return one(callable).flatten();
    }
    None
}

/// Inherent methods rustdoc's JSON leaves out: the `impl str { … }`,
/// `impl<T> [T] { … }`, `impl Error { … }` blocks `alloc` and `std` write
/// for `core`'s types (`#[rustc_allow_incoherent_impl]`), which the JSON of
/// neither crate carries. Found in the shard itself: a method of that name
/// (with `self` when `needs_self`) in the toolchain crates above the type's
/// own, whose enclosing block is an inherent `impl` naming the type, in the
/// file of the type's module.
fn incoherent(cache: &GraphCache<'_>, key: &str, method: &str, needs_self: bool) -> Vec<Candidate> {
    let memo_key = (key.to_string(), method.to_string(), needs_self);
    if let Some(known) = cache.memo.api_incoherent.borrow().get(&memo_key) {
        return known
            .iter()
            .filter_map(|(graph, node)| {
                Some(Candidate::Node(
                    cache.get_index(*graph)?,
                    Box::new(node.clone()),
                ))
            })
            .collect();
    }
    let (above, module): (&[&str], Vec<&str>) = match key.strip_prefix("prim:") {
        Some(_) => (&["alloc", "std"], Vec::new()),
        None => {
            let mut path: Vec<&str> = key.split("::").collect();
            path.pop();
            let home = if path.is_empty() { "" } else { path.remove(0) };
            match home {
                "core" => (&["alloc", "std"], path),
                "alloc" => (&["std"], path),
                _ => (&[], path),
            }
        }
    };
    let owner = crate::deps::rustdoc::owner_name(key);
    let mut found: Vec<(Rc<ForeignGraph>, Node)> = Vec::new();
    for krate in above {
        let Some(graph) = cache.get(krate).filter(|graph| graph.crate_dir == *krate) else {
            continue;
        };
        for node in graph.context.get_nodes_by_name(method) {
            if !matches!(node.kind, NodeKind::Method | NodeKind::Function)
                || !graph.in_crate(&node.file_path)
                || (!module.is_empty() && file_module(&node.file_path) != module)
            {
                continue;
            }
            if needs_self
                && !node.signature.as_deref().is_some_and(|signature| {
                    let params = signature.trim_start_matches('(').trim_start();
                    ["self", "&self", "&mut self", "mut self", "&'"]
                        .iter()
                        .any(|start| params.starts_with(start))
                })
            {
                continue;
            }
            if impl_self_type(&graph, &node).as_deref() == Some(owner.as_str()) {
                found.push((Rc::clone(&graph), node));
            }
        }
    }
    cache.memo.api_incoherent.borrow_mut().insert(
        memo_key,
        found
            .iter()
            .map(|(graph, node)| (graph.index, node.clone()))
            .collect(),
    );
    found
        .into_iter()
        .map(|(graph, node)| Candidate::Node(graph, Box::new(node)))
        .collect()
}

/// The module path of a toolchain file (`alloc/src/io/error.rs` →
/// `["io", "error"]`).
fn file_module(file: &str) -> Vec<&str> {
    let mut path: Vec<&str> = file.split('/').skip(2).collect();
    if let Some(last) = path.last_mut() {
        *last = last.trim_end_matches(".rs");
    }
    if matches!(path.last(), Some(&"mod") | Some(&"lib")) {
        path.pop();
    }
    path
}

/// The type an inherent `impl` block around `node` is for (`str`, `[T]`,
/// `Error`), read from its header; `None` in a trait impl or outside one.
fn impl_self_type(graph: &ForeignGraph, node: &Node) -> Option<String> {
    use crate::resolution::line_index::Lines;
    let source = graph.context.read_file_arc(&node.file_path)?;
    let lines = Lines::of(&source);
    let at = (node.start_line as usize).checked_sub(1)?;
    let indent = |line: &str| line.len() - line.trim_start().len();
    let own = indent(lines.get(at)?);
    let header = lines
        .span()
        .slice(0..at)
        .iter()
        .rev()
        .take(5_000)
        .find(|line| {
            !line.trim().is_empty()
                && indent(line) < own
                && !line.trim_start().starts_with("//")
                && !line.trim_start().starts_with("#")
        })?
        .trim();
    let rest = header
        .strip_prefix("unsafe ")
        .unwrap_or(header)
        .strip_prefix("impl")?;
    let rest = rest.trim_start();
    // Skip the impl's generic parameters.
    let rest = if rest.starts_with('<') {
        let mut depth = 0usize;
        let mut end = 0;
        for (index, c) in rest.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        end = index + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest.get(end..)?.trim_start()
    } else {
        rest
    };
    let ty = rest.split(['{']).next()?.split(" where").next()?.trim();
    if ty.is_empty() || ty.contains(" for ") {
        return None;
    }
    if ty.starts_with('[') {
        return Some(ty.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    let path = ty.split('<').next()?.trim();
    Some(path.rsplit("::").next().unwrap_or(path).to_string())
}

/// What `impls` say the type derefs to.
fn deref_target(impls: &[FoundImpl]) -> Option<String> {
    let mut targets = impls
        .iter()
        .filter(|found| found.imp().trait_path.as_deref() == Some(DEREF))
        .filter_map(|found| found.imp().deref_target.clone());
    let first = targets.next()?;
    targets.all(|other| other == first).then_some(first)
}

/// A trait no index describes — the project's own, or one of a crate the
/// file imports from without an index — may define `method` and be in
/// scope.
fn other_traits_define(cache: &GraphCache<'_>, caller: &Caller<'_>, method: &str) -> bool {
    let key = (caller.file.to_string(), method.to_string());
    if let Some(known) = cache.memo.api_unseen.borrow().get(&key) {
        return *known;
    }
    let project_defines = |name: Option<&str>, same_file: bool| {
        caller.project.get_nodes_by_name(method).iter().any(|node| {
            node.kind == NodeKind::Method
                && (!same_file || node.file_path == caller.file)
                && node
                    .qualified_name
                    .rsplit_once("::")
                    .is_some_and(|(owner, _)| {
                        let owner = owner.rsplit("::").next().unwrap_or(owner);
                        name.is_none_or(|name| name == owner)
                            && caller
                                .project
                                .get_nodes_by_name(owner)
                                .iter()
                                .any(|node| node.kind == NodeKind::Trait)
                    })
        })
    };
    let crate_defines = |krate: &str, name: Option<&str>| {
        let Some(graph) = cache.get(krate) else {
            return false;
        };
        graph.context.get_nodes_by_name(method).iter().any(|node| {
            node.kind == NodeKind::Method
                && node
                    .qualified_name
                    .rsplit_once("::")
                    .is_some_and(|(owner, _)| {
                        let owner = owner.rsplit("::").next().unwrap_or(owner);
                        name.is_none_or(|name| name == owner)
                            && graph
                                .context
                                .get_nodes_by_name(owner)
                                .iter()
                                .any(|node| node.kind == NodeKind::Trait)
                    })
        })
    };
    // Traits written in the file itself are in scope there.
    let mut defines = project_defines(None, true);
    if !defines {
        defines = caller.scope.opaque_imports().iter().any(|(krate, name)| {
            if krate.is_empty() {
                project_defines(name.as_deref(), false)
            } else {
                crate_defines(krate, name.as_deref())
            }
        });
    }
    if !defines && caller.scope.is_opaque() {
        // A glob of a project module: any project trait.
        defines = project_defines(None, false);
    }
    cache.memo.api_unseen.borrow_mut().insert(key, defines);
    defines
}

/// The traits of `std`'s preludes (every file has them in scope).
fn prelude_traits(cache: &GraphCache<'_>) -> Rc<HashSet<String>> {
    if let Some(known) = cache.memo.api_prelude.borrow().as_ref() {
        return Rc::clone(known);
    }
    let mut traits = HashSet::new();
    for module in PRELUDES {
        if let Some(found) = module_traits(cache, "std", &segments(module)) {
            traits.extend(found);
        }
    }
    let traits = Rc::new(traits);
    *cache.memo.api_prelude.borrow_mut() = Some(Rc::clone(&traits));
    traits
}

/// `name` is one of the names `std`'s preludes bring into every file (and
/// `std` has an index).
pub(crate) fn is_prelude_name(cache: &GraphCache<'_>, name: &str) -> bool {
    if cache.memo.api_prelude_names.borrow().is_none() {
        let mut names = std::collections::HashSet::new();
        if has_index(cache, "std") {
            for module in PRELUDES {
                visit_module(cache, "std", &segments(module), &mut |_, _, entry| {
                    names.insert(entry.to_string());
                });
            }
        }
        *cache.memo.api_prelude_names.borrow_mut() = Some(Rc::new(names));
    }
    cache
        .memo
        .api_prelude_names
        .borrow()
        .as_ref()
        .is_some_and(|names| names.contains(name))
}

/// Call `visit(crate, module path, name)` for every name the module at
/// `path` of `krate` exports (its globs followed across crates); `false`
/// when no index describes it.
fn visit_module(
    cache: &GraphCache<'_>,
    krate: &str,
    path: &[String],
    visit: &mut dyn FnMut(&str, &[String], &str),
) -> bool {
    let mut queue: Vec<(String, Vec<String>)> = vec![(krate.to_string(), path.to_vec())];
    let mut visited = HashSet::new();
    let mut known = false;
    while let Some((krate, path)) = queue.pop() {
        if visited.len() >= MAX_MODULES || !visited.insert((krate.clone(), path.clone())) {
            continue;
        }
        // The module itself may be another crate's, re-exported.
        let Some((krate, path)) = module_home(cache, &krate, &path, 0) else {
            continue;
        };
        let Some((_, api)) = api_of(cache, &krate) else {
            continue;
        };
        known = true;
        let (entries, globs) = api.module_entries(&path.join("::"));
        for (name, _) in entries {
            visit(&krate, &path, &name);
        }
        for glob in globs {
            queue.push((glob.krate, segments(&glob.path)));
        }
    }
    known
}

/// The traits the module at `path` of `krate` exports (its names and its
/// globs, followed across crates) — `None` when no index can tell.
pub(crate) fn module_traits(
    cache: &GraphCache<'_>,
    krate: &str,
    path: &[String],
) -> Option<Vec<String>> {
    let mut entries: Vec<(String, Vec<String>)> = Vec::new();
    let known = visit_module(cache, krate, path, &mut |krate, module, name| {
        let mut full = module.to_vec();
        full.push(name.to_string());
        entries.push((krate.to_string(), full));
    });
    let traits = entries
        .into_iter()
        .filter_map(|(krate, full)| trait_at(cache, &krate, &full).flatten())
        .collect();
    known.then_some(traits)
}

/// The item `name` of `std`'s prelude, admitted for a reference of `kind`.
pub(crate) fn prelude_lookup(cache: &GraphCache<'_>, name: &str, kind: EdgeKind) -> Option<Found> {
    for module in PRELUDES {
        let mut path = segments(module);
        path.push(name.to_string());
        if let Some(Some(found)) = lookup_path(cache, "std", &path, kind) {
            return Some(Found {
                reexported: false,
                ..found
            });
        }
    }
    None
}

/// Whether the module at `path` of `krate` exports `name` — `None` when no
/// index can tell.
pub(crate) fn module_exports(
    cache: &GraphCache<'_>,
    krate: &str,
    path: &[String],
    name: &str,
) -> Option<bool> {
    let (home, module) = module_home(cache, krate, path, 0)?;
    let mut full = module;
    full.push(name.to_string());
    match resolve(cache, &home, &full, 0, false) {
        Resolved::NotFound => Some(false),
        Resolved::Legacy(..) => None,
        Resolved::Item(..) | Resolved::Member(..) => Some(true),
    }
}

/// The crate and path where the module `path` of `krate` is defined
/// (following module re-exports).
fn module_home(
    cache: &GraphCache<'_>,
    krate: &str,
    path: &[String],
    hops: u8,
) -> Option<(String, Vec<String>)> {
    let (_, api) = api_of(cache, krate)?;
    if path.is_empty() {
        return Some((krate.to_string(), Vec::new()));
    }
    match api.resolve(path) {
        PathStep::Module => Some((krate.to_string(), path.to_vec())),
        PathStep::External { target, rest } if hops < MAX_HOPS => {
            let mut next = segments(&target.path);
            next.extend(rest);
            module_home(cache, &target.krate, &next, hops + 1)
        }
        _ => None,
    }
}

/// What the path names, as a trait: `Some(Some(canonical))` for a trait,
/// `Some(None)` for anything else, `None` when no index can tell.
pub(crate) fn trait_at(
    cache: &GraphCache<'_>,
    krate: &str,
    path: &[String],
) -> Option<Option<String>> {
    if !has_index(cache, krate) {
        return None;
    }
    match resolve(cache, krate, path, 0, false) {
        Resolved::Item(item, _) => Some(
            (item.item().kind == ApiKind::Trait && !item.item().canonical.is_empty())
                .then(|| item.item().canonical.clone()),
        ),
        Resolved::Legacy(..) => None,
        Resolved::Member(..) | Resolved::NotFound => Some(None),
    }
}

/// The node an index item is: in the graph that holds its file, the node
/// of its name and kind spanning its line.
fn node_of(cache: &GraphCache<'_>, item: &ApiRef) -> Option<(Rc<ForeignGraph>, Node)> {
    let key = (item.graph.index, Arc::as_ptr(&item.api) as usize, item.item);
    if let Some(known) = cache.memo.api_nodes.borrow().get(&key) {
        let (graph, node) = known.clone()?;
        return Some((cache.get_index(graph)?, node));
    }
    let found = locate(cache, item).and_then(|(graph, file)| {
        let nodes = graph
            .context
            .get_nodes_in_file_named(&file, &item.item().name);
        matching_node(item.item(), &nodes)
            .cloned()
            .map(|node| (graph, node))
    });
    cache.memo.api_nodes.borrow_mut().insert(
        key,
        found
            .as_ref()
            .map(|(graph, node)| (graph.index, node.clone())),
    );
    found
}

/// The graph holding an index item's file, and the file as that graph
/// records it.
fn locate(cache: &GraphCache<'_>, item: &ApiRef) -> Option<(Rc<ForeignGraph>, String)> {
    let file = &item.item().file;
    if file.is_empty() || item.item().line == 0 {
        return None;
    }
    if let Some(library) = file.strip_prefix("@toolchain:") {
        return toolchain_graph(cache, library).map(|graph| (graph, library.to_string()));
    }
    if let Some(absolute) = file.strip_prefix("@abs:") {
        for (index, reachable) in cache.reach().graphs().iter().enumerate() {
            if let GraphLocation::Shard { source_dir, .. } = &reachable.location {
                if let Ok(inside) = std::path::Path::new(absolute).strip_prefix(source_dir) {
                    let graph = cache.get_index(index)?;
                    return Some((graph, inside.to_string_lossy().into_owned()));
                }
            }
        }
        return None;
    }
    if item.graph.crate_dir.is_empty() {
        return Some((Rc::clone(&item.graph), file.clone()));
    }
    // A toolchain index: the file is in the crate its first segment names.
    toolchain_graph(cache, file).map(|graph| (graph, file.clone()))
}

/// The toolchain graph whose directory holds `file` (`core/src/cell.rs`).
fn toolchain_graph(cache: &GraphCache<'_>, file: &str) -> Option<Rc<ForeignGraph>> {
    let krate = file.split('/').next()?;
    let graph = cache.get(krate)?;
    (graph.crate_dir == krate).then_some(graph)
}
