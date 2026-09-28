//! Building an [`ApiIndex`] from a crate's rustdoc JSON.
//!
//! * **Public paths**: the module tree walked from the crate root over
//!   `pub` items — modules, items, and `pub use` re-exports (a re-export of
//!   one of the crate's own modules is walked under its new name; a glob of
//!   one of its own modules or enums is flattened; another crate's item or
//!   module becomes an [`ExternalPath`], a glob of another crate's module a
//!   [`ApiIndex::globs`] entry).
//! * **Paths of definition**: rustdoc's `paths` table, for every local item.
//! * **Impls**: each type's impl list (inherent, trait, the blanket impls
//!   rustdoc found to apply, auto traits), impls of foreign types, and
//!   blanket impls as written (`impl<T: Display> ToString for T`).
//! * **Traits**: their items by name.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::API_FORMAT;
use super::model::{ApiImpl, ApiIndex, ApiItem, ApiKind, ApiType, ExternalPath, Target};
use super::raw::{Id, Inner, RawBound, RawCrate, RawImpl, RawVisibility, RawWherePredicate, Ty};

/// How deep the module walk goes (re-exports can nest modules in cycles).
const MAX_MODULE_DEPTH: usize = 16;

/// Build the index of the crate code calls `krate` from its rustdoc JSON;
/// `file` maps a span's file name to how the index records it (see
/// [`ApiItem::file`]), `None` dropping the span.
pub(crate) fn build_index(
    raw: &RawCrate,
    krate: &str,
    file: &dyn Fn(&str) -> Option<String>,
) -> ApiIndex {
    let mut builder = Builder {
        raw,
        krate,
        file,
        index: ApiIndex {
            rustdoc_format: raw.format_version,
            crate_version: raw.crate_version.clone(),
            ..empty(krate)
        },
        item_of: HashMap::new(),
        impl_of: HashMap::new(),
    };
    builder.walk_module(raw.root, "", 0, &mut HashSet::new(), true);
    builder.definitions();
    builder.types_and_traits();
    builder.loose_impls();
    builder.index
}

struct Builder<'a> {
    raw: &'a RawCrate,
    krate: &'a str,
    file: &'a dyn Fn(&str) -> Option<String>,
    index: ApiIndex,
    item_of: HashMap<Id, u32>,
    impl_of: HashMap<Id, Option<u32>>,
}

impl Builder<'_> {
    /// The canonical path of the item `id` (`core::cell::RefCell`).
    fn canonical(&self, id: Id) -> Option<String> {
        let summary = self.raw.paths.get(&id)?;
        let krate = if summary.crate_id == 0 {
            self.krate.to_string()
        } else {
            self.raw
                .external_crates
                .get(&summary.crate_id)?
                .name
                .clone()
        };
        let mut path = vec![krate];
        path.extend(summary.path.iter().skip(1).cloned());
        Some(path.join("::"))
    }

    /// The type key of `ty` (see the module docs of [`super::model`]).
    fn type_key(&self, ty: &Ty) -> Option<String> {
        match ty {
            Ty::Path(path) => self.canonical(path.id?),
            Ty::Primitive(name) => Some(format!("prim:{name}")),
            Ty::Slice => Some("prim:slice".to_string()),
            Ty::Array => Some("prim:array".to_string()),
            Ty::RawPointer => Some("prim:pointer".to_string()),
            Ty::Ref(_) | Ty::Generic(_) | Ty::Other => None,
        }
    }

    /// The generic arguments of a path type, each a type key when concrete.
    fn type_args(&self, ty: &Ty) -> Vec<Option<String>> {
        match ty {
            Ty::Path(path) => path.args.0.iter().map(|arg| self.type_key(arg)).collect(),
            _ => Vec::new(),
        }
    }

    /// The item `id` in the index, added on first use.
    fn item(&mut self, id: Id, kind: ApiKind, owner: Option<&str>, public: bool) -> Option<u32> {
        if let Some(known) = self.item_of.get(&id) {
            // An impl's or trait's function first met by its path is a
            // method all the same.
            let entry = &mut self.index.items[*known as usize];
            if kind == ApiKind::Method && entry.kind == ApiKind::Function {
                entry.kind = ApiKind::Method;
                entry.canonical.clear();
                if entry.owner.is_none() {
                    entry.owner = owner.map(str::to_string);
                }
                entry.public |= public;
            }
            return Some(*known);
        }
        let raw = self.raw.index.get(&id)?;
        let name = raw.name.clone()?;
        let canonical = match (kind, owner) {
            (ApiKind::Method | ApiKind::AssocConst | ApiKind::AssocType, Some(_)) => String::new(),
            (ApiKind::Variant, _) | (_, None) => self.canonical(id).unwrap_or_default(),
            _ => String::new(),
        };
        let (file, line, end_line) = match &raw.span {
            Some(span) => match (self.file)(&span.filename) {
                Some(file) => (file, span.begin.0, span.end.0),
                None => (String::new(), 0, 0),
            },
            None => (String::new(), 0, 0),
        };
        let has_self = match &raw.inner {
            Inner::Function(function) => function
                .sig
                .inputs
                .first()
                .is_some_and(|(name, _)| name == "self"),
            _ => false,
        };
        let index = self.index.items.len() as u32;
        self.index.items.push(ApiItem {
            kind,
            name,
            canonical,
            file,
            line,
            end_line,
            owner: owner.map(str::to_string),
            has_self,
            public: public || raw.visibility == RawVisibility::Public,
        });
        self.item_of.insert(id, index);
        Some(index)
    }

    fn set_canonical(&mut self, item: u32, canonical: String) {
        if let Some(entry) = self.index.items.get_mut(item as usize) {
            if entry.canonical.is_empty() {
                entry.canonical = canonical;
            }
        }
    }

    /// The kind of a named, path-addressable item.
    fn kind_of(inner: &Inner) -> Option<ApiKind> {
        Some(match inner {
            Inner::Module(_) => ApiKind::Module,
            Inner::Struct(_) => ApiKind::Struct,
            Inner::Enum(_) => ApiKind::Enum,
            Inner::Union(_) => ApiKind::Union,
            Inner::Trait(_) => ApiKind::Trait,
            Inner::TypeAlias(_) => ApiKind::TypeAlias,
            Inner::Primitive(_) => ApiKind::Primitive,
            Inner::Function(_) => ApiKind::Function,
            Inner::Variant => ApiKind::Variant,
            Inner::Constant => ApiKind::Constant,
            Inner::Static => ApiKind::Static,
            Inner::Macro => ApiKind::Macro,
            _ => return None,
        })
    }

    /// What the item `id` (a `use` target) is, as a path target.
    fn target(&mut self, id: Id) -> Option<Target> {
        match self.raw.index.get(&id) {
            Some(raw) if raw.crate_id == 0 => match &raw.inner {
                Inner::Module(_) => Some(Target::Module),
                inner => {
                    // Macros live in a namespace of their own (`Debug` is a
                    // trait and a derive): they are not paths here.
                    let kind = Self::kind_of(inner).filter(|kind| *kind != ApiKind::Macro)?;
                    self.item(id, kind, None, true).map(Target::Item)
                }
            },
            _ => {
                let summary = self.raw.paths.get(&id)?;
                if summary.crate_id == 0
                    || summary.kind == "macro"
                    || summary.kind.starts_with("proc_")
                {
                    return None;
                }
                let krate = self
                    .raw
                    .external_crates
                    .get(&summary.crate_id)?
                    .name
                    .clone();
                Some(Target::External(ExternalPath {
                    krate,
                    path: summary
                        .path
                        .iter()
                        .skip(1)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("::"),
                }))
            }
        }
    }

    fn add_path(&mut self, path: String, target: Target, explicit: bool) {
        if path.is_empty() {
            return;
        }
        if explicit {
            self.index.paths.insert(path, target);
        } else {
            self.index.paths.entry(path).or_insert(target);
        }
    }

    /// Walk the module `id` as `prefix` (`""` = the crate root): its public
    /// items are paths. Names a module declares win over glob-imported ones
    /// (`explicit` is false inside a glob's walk).
    fn walk_module(
        &mut self,
        id: Id,
        prefix: &str,
        depth: usize,
        seen: &mut HashSet<(Id, String)>,
        explicit: bool,
    ) {
        if depth > MAX_MODULE_DEPTH || !seen.insert((id, prefix.to_string())) {
            return;
        }
        let Some(Inner::Module(module)) = self.raw.index.get(&id).map(|item| &item.inner) else {
            return;
        };
        let join = |name: &str| {
            if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}::{name}")
            }
        };
        let children = module.items.clone();
        let mut globs = Vec::new();
        for child in children {
            let Some(raw) = self.raw.index.get(&child) else {
                continue;
            };
            if raw.visibility != RawVisibility::Public {
                continue;
            }
            match &raw.inner {
                Inner::Use(used) if used.is_glob => globs.extend(used.id),
                Inner::Use(used) => {
                    let Some(target_id) = used.id else {
                        continue;
                    };
                    let path = join(&used.name);
                    let Some(target) = self.target(target_id) else {
                        continue;
                    };
                    self.add_path(path.clone(), target.clone(), explicit);
                    if target == Target::Module {
                        self.walk_module(target_id, &path, depth + 1, seen, explicit);
                    }
                }
                Inner::Module(_) => {
                    let Some(name) = raw.name.clone() else {
                        continue;
                    };
                    let path = join(&name);
                    self.add_path(path.clone(), Target::Module, explicit);
                    self.walk_module(child, &path, depth + 1, seen, explicit);
                }
                inner => {
                    let (Some(kind), Some(name)) = (Self::kind_of(inner), raw.name.clone()) else {
                        continue;
                    };
                    // A primitive's docs item (`std::str`) is no path: the
                    // module of that name is.
                    if matches!(kind, ApiKind::Macro | ApiKind::Primitive) {
                        continue;
                    }
                    if let Some(item) = self.item(child, kind, None, true) {
                        self.add_path(join(&name), Target::Item(item), explicit);
                    }
                }
            }
        }
        for glob in globs {
            match self.raw.index.get(&glob) {
                Some(raw) if raw.crate_id == 0 => match &raw.inner {
                    // One of this crate's modules: its names, flattened.
                    Inner::Module(_) => self.walk_module(glob, prefix, depth + 1, seen, false),
                    Inner::Enum(enumeration) => {
                        let owner = raw.name.clone();
                        for variant in enumeration.variants.clone() {
                            let name = self.raw.index.get(&variant).and_then(|v| v.name.clone());
                            if let (Some(name), Some(item)) = (
                                name,
                                self.item(variant, ApiKind::Variant, owner.as_deref(), true),
                            ) {
                                self.add_path(join(&name), Target::Item(item), false);
                            }
                        }
                    }
                    _ => {}
                },
                _ => {
                    if let Some(Target::External(target)) = self.target(glob) {
                        let entry = self.index.globs.entry(prefix.to_string()).or_default();
                        if !entry.contains(&target) {
                            entry.push(target);
                        }
                    }
                }
            }
        }
    }

    /// Every local item's path of definition.
    fn definitions(&mut self) {
        let mut ids: Vec<Id> = self
            .raw
            .paths
            .iter()
            .filter(|(id, summary)| summary.crate_id == 0 && self.raw.index.contains_key(id))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            let Some(raw) = self.raw.index.get(&id) else {
                continue;
            };
            let Some(kind) = Self::kind_of(&raw.inner) else {
                continue;
            };
            if matches!(kind, ApiKind::Module | ApiKind::Macro) {
                continue;
            }
            let Some(summary) = self.raw.paths.get(&id) else {
                continue;
            };
            let path = summary
                .path
                .iter()
                .skip(1)
                .cloned()
                .collect::<Vec<_>>()
                .join("::");
            if path.is_empty() {
                continue;
            }
            if let Some(item) = self.item(id, kind, None, false) {
                self.index.defined.entry(path).or_insert(item);
            }
        }
    }

    /// Types (with their impls and variants) and traits (with their items).
    fn types_and_traits(&mut self) {
        let mut ids: Vec<Id> = self
            .raw
            .index
            .iter()
            .filter(|(_, raw)| raw.crate_id == 0)
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            let Some(raw) = self.raw.index.get(&id) else {
                continue;
            };
            let (key, kind, impls, variants, alias) = match &raw.inner {
                Inner::Struct(s) => (
                    self.canonical(id),
                    ApiKind::Struct,
                    s.impls.clone(),
                    vec![],
                    None,
                ),
                Inner::Union(u) => (
                    self.canonical(id),
                    ApiKind::Union,
                    u.impls.clone(),
                    vec![],
                    None,
                ),
                Inner::Enum(e) => (
                    self.canonical(id),
                    ApiKind::Enum,
                    e.impls.clone(),
                    e.variants.clone(),
                    None,
                ),
                Inner::Primitive(p) => (
                    Some(format!("prim:{}", p.name)),
                    ApiKind::Primitive,
                    p.impls.clone(),
                    vec![],
                    None,
                ),
                Inner::TypeAlias(alias) => (
                    self.canonical(id),
                    ApiKind::TypeAlias,
                    vec![],
                    vec![],
                    self.type_key(&alias.ty)
                        .map(|key| (key, self.type_args(&alias.ty))),
                ),
                Inner::Trait(t) => {
                    if let Some(key) = self.canonical(id) {
                        let name = raw.name.clone().unwrap_or_default();
                        let mut members = BTreeMap::new();
                        for member in t.items.clone() {
                            let kind = match self.raw.index.get(&member).map(|m| &m.inner) {
                                Some(Inner::Function(_)) => ApiKind::Method,
                                Some(Inner::AssocConst) => ApiKind::AssocConst,
                                Some(Inner::AssocType(_)) => ApiKind::AssocType,
                                _ => continue,
                            };
                            if let Some(item) = self.item(member, kind, Some(&name), true) {
                                self.set_canonical(
                                    item,
                                    format!("{key}::{}", self.index.items[item as usize].name),
                                );
                                members.insert(self.index.items[item as usize].name.clone(), item);
                            }
                        }
                        self.item(id, ApiKind::Trait, None, false);
                        self.index.traits.insert(key, members);
                    }
                    continue;
                }
                _ => continue,
            };
            let Some(key) = key else {
                continue;
            };
            let Some(item) = self.item(id, kind, None, false) else {
                continue;
            };
            if kind == ApiKind::Primitive {
                self.set_canonical(item, key.clone());
            }
            let owner = raw.name.clone().unwrap_or_default();
            let mut variant_items = BTreeMap::new();
            for variant in variants {
                if let Some(v) = self.item(variant, ApiKind::Variant, Some(&owner), true) {
                    variant_items.insert(self.index.items[v as usize].name.clone(), v);
                }
            }
            let mut impl_indexes = Vec::new();
            for impl_id in impls {
                if let Some(index) = self.impl_block(impl_id, Some(&key)) {
                    impl_indexes.push(index);
                }
            }
            let entry = self.index.types.entry(key).or_insert(ApiType {
                item,
                impls: Vec::new(),
                alias_of: None,
                alias_args: Vec::new(),
                variants: BTreeMap::new(),
            });
            entry.impls.extend(impl_indexes);
            if let (None, Some((of, args))) = (&entry.alias_of, alias) {
                entry.alias_of = Some(of);
                entry.alias_args = args;
            }
            entry.variants.extend(variant_items);
        }
    }

    /// Impls written in this crate that no local type lists: impls of
    /// foreign types and blanket impls as written.
    fn loose_impls(&mut self) {
        let mut ids: Vec<Id> = self
            .raw
            .index
            .iter()
            .filter(|(_, raw)| raw.crate_id == 0 && matches!(raw.inner, Inner::Impl(_)))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            if self.impl_of.contains_key(&id) {
                continue;
            }
            let Some(Inner::Impl(raw)) = self.raw.index.get(&id).map(|item| &item.inner) else {
                continue;
            };
            if let Ty::Generic(_) = raw.for_ {
                if let Some(index) = self.impl_block(id, None) {
                    self.index.blankets.push(index);
                }
                continue;
            }
            let Some(key) = self.type_key(&raw.for_) else {
                continue;
            };
            if self.index.types.contains_key(&key) {
                // A local type whose own list lacks it (rare): attach there.
                if let Some(index) = self.impl_block(id, Some(&key)) {
                    if let Some(ty) = self.index.types.get_mut(&key) {
                        ty.impls.push(index);
                    }
                }
                continue;
            }
            if let Some(index) = self.impl_block(id, Some(&key)) {
                self.index.foreign_impls.entry(key).or_default().push(index);
            }
        }
    }

    /// The impl `id` as an [`ApiImpl`] (built once).
    fn impl_block(&mut self, id: Id, for_key: Option<&str>) -> Option<u32> {
        if let Some(known) = self.impl_of.get(&id) {
            return *known;
        }
        self.impl_of.insert(id, None);
        let raw_item = self.raw.index.get(&id)?;
        let Inner::Impl(raw) = &raw_item.inner else {
            return None;
        };
        let raw: &RawImpl = raw;
        let trait_path = match &raw.trait_ {
            Some(path) => Some(self.canonical(path.id?)?),
            None => None,
        };
        let generic = match &raw.for_ {
            Ty::Generic(name) => Some(name.clone()),
            _ => None,
        };
        let for_type = match (&generic, for_key) {
            (Some(_), _) => String::new(),
            (None, Some(key)) => key.to_string(),
            (None, None) => self.type_key(&raw.for_)?,
        };
        let bounds = generic.as_ref().and_then(|name| self.bounds_of(raw, name));
        let owner = match &generic {
            Some(name) => name.clone(),
            None => owner_name(&for_type),
        };
        let mut items = BTreeMap::new();
        let mut deref_target = None;
        let is_deref = trait_path
            .as_deref()
            .is_some_and(|path| path.ends_with("::Deref"));
        for member in raw.items.clone() {
            let Some(member_raw) = self.raw.index.get(&member) else {
                continue;
            };
            let kind = match &member_raw.inner {
                Inner::Function(_) => ApiKind::Method,
                Inner::AssocConst => ApiKind::AssocConst,
                Inner::AssocType(assoc) => {
                    if is_deref && member_raw.name.as_deref() == Some("Target") {
                        deref_target = assoc.ty.as_ref().and_then(|ty| self.type_key(ty));
                    }
                    ApiKind::AssocType
                }
                _ => continue,
            };
            let public = trait_path.is_some();
            if let Some(item) = self.item(member, kind, Some(&owner), public) {
                if !for_type.is_empty() {
                    let name = self.index.items[item as usize].name.clone();
                    self.set_canonical(item, format!("{for_type}::{name}"));
                }
                items.insert(self.index.items[item as usize].name.clone(), item);
            }
        }
        let index = self.index.impls.len() as u32;
        self.index.impls.push(ApiImpl {
            trait_path,
            for_type,
            for_args: if generic.is_some() {
                Vec::new()
            } else {
                self.type_args(&raw.for_)
            },
            blanket: generic.is_some() || raw.blanket_impl.is_some(),
            bounds,
            items,
            provided: raw.provided_trait_methods.clone(),
            deref_target,
            negative: raw.is_negative,
            synthetic: raw.is_synthetic,
        });
        self.impl_of.insert(id, Some(index));
        Some(index)
    }

    /// The traits a blanket impl's generic `name` must implement, or `None`
    /// when a bound is not a plain trait this crate can name.
    fn bounds_of(&self, raw: &RawImpl, name: &str) -> Option<Vec<String>> {
        let mut bounds = Vec::new();
        let mut add = |bound: &RawBound| -> Option<()> {
            if let RawBound::Trait { path, maybe } = bound {
                if !maybe {
                    bounds.push(self.canonical(path.id?)?);
                }
            }
            Some(())
        };
        for param in &raw.generics.params {
            if param.name == name {
                if let super::raw::RawParamKind::Type(param_bounds) = &param.kind {
                    for bound in param_bounds {
                        add(bound)?;
                    }
                }
            }
        }
        for predicate in &raw.generics.where_predicates {
            if let RawWherePredicate::Bound {
                ty,
                bounds: predicate_bounds,
            } = predicate
            {
                if *ty == Ty::Generic(name.to_string()) {
                    for bound in predicate_bounds {
                        add(bound)?;
                    }
                }
            }
        }
        Some(bounds)
    }
}

fn empty(krate: &str) -> ApiIndex {
    ApiIndex {
        format: API_FORMAT,
        rustdoc_format: 0,
        krate: krate.to_string(),
        crate_version: None,
        items: Vec::new(),
        paths: BTreeMap::new(),
        defined: BTreeMap::new(),
        globs: BTreeMap::new(),
        impls: Vec::new(),
        types: BTreeMap::new(),
        foreign_impls: BTreeMap::new(),
        blankets: Vec::new(),
        traits: BTreeMap::new(),
    }
}

/// How the index writes a type's name as a method's owner (`RefCell`,
/// `str`).
pub(crate) fn owner_name(type_key: &str) -> String {
    match type_key.strip_prefix("prim:") {
        Some(primitive) => match primitive {
            "slice" => "[T]".to_string(),
            "array" => "[T; N]".to_string(),
            other => other.to_string(),
        },
        None => type_key.rsplit("::").next().unwrap_or(type_key).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{ApiIndex, ApiKind, ExternalPath, PathStep};
    use super::super::{ReadError, parse_crate};
    use super::build_index;

    /// The checked-in fixture (`tests/rustdoc_fixture/`): `widgets`
    /// re-exports `parts::gear::Gear` (a private module) at its root and as
    /// `prelude::Cog`, and `pub mod task { pub use widgets_core::task::*; }`;
    /// `widgets_core` has a trait with a provided method and a blanket impl.
    fn fixture(krate: &str) -> ApiIndex {
        let text = match krate {
            "widgets" => include_str!("../../../tests/rustdoc_fixture/json/widgets.json"),
            _ => include_str!("../../../tests/rustdoc_fixture/json/widgets_core.json"),
        };
        let raw = parse_crate(text.as_bytes()).unwrap();
        build_index(&raw, krate, &|file| Some(file.to_string()))
    }

    fn path(segments: &str) -> Vec<String> {
        segments.split("::").map(str::to_string).collect()
    }

    #[test]
    fn re_export_chains_and_inline_glob_modules() {
        let widgets = fixture("widgets");
        // `pub use parts::gear::Gear` and `prelude::Cog` name one item.
        let PathStep::Item(gear) = widgets.resolve(&path("Gear")) else {
            panic!("Gear: {:?}", widgets.resolve(&path("Gear")));
        };
        assert_eq!(widgets.resolve(&path("prelude::Cog")), PathStep::Item(gear));
        let item = widgets.item(gear).unwrap();
        assert_eq!(item.canonical, "widgets::parts::gear::Gear");
        assert_eq!((item.file.as_str(), item.line), ("src/parts/gear.rs", 3));
        // Its path of definition, private module and all.
        assert_eq!(widgets.defined.get("parts::gear::Gear"), Some(&gear));
        // A member of a re-exported type.
        assert_eq!(
            widgets.resolve(&path("prelude::Cog::new")),
            PathStep::Member {
                item: gear,
                rest: vec!["new".to_string()]
            }
        );
        // Another crate's item, re-exported by name.
        assert_eq!(
            widgets.resolve(&path("prelude::Named")),
            PathStep::External {
                target: ExternalPath {
                    krate: "widgets_core".into(),
                    path: "Named".into()
                },
                rest: vec![]
            }
        );
        // `pub mod task { pub use widgets_core::task::*; }`.
        assert_eq!(
            widgets.resolve(&path("task::Poll::Ready")),
            PathStep::Globs {
                targets: vec![ExternalPath {
                    krate: "widgets_core".into(),
                    path: "task".into()
                }],
                rest: path("Poll::Ready")
            }
        );
        // A path of definition (how other crates' indexes name the item).
        assert_eq!(
            widgets.resolve(&path("parts::gear::Gear")),
            PathStep::Item(gear)
        );
        assert_eq!(
            widgets.resolve(&path("parts::gear::Cog")),
            PathStep::NotFound
        );
    }

    #[test]
    fn impls_traits_and_blanket_impls() {
        let widgets = fixture("widgets");
        let gear = widgets.ty("widgets::parts::gear::Gear").unwrap();
        let impls: Vec<_> = gear
            .impls
            .iter()
            .map(|index| &widgets.impls[*index as usize])
            .collect();
        let inherent: Vec<&str> = impls
            .iter()
            .filter(|imp| imp.trait_path.is_none())
            .flat_map(|imp| imp.items.keys().map(String::as_str))
            .collect();
        assert_eq!(inherent, ["new", "teeth"]);
        let teeth = impls
            .iter()
            .find_map(|imp| imp.items.get("teeth"))
            .and_then(|item| widgets.item(*item))
            .unwrap();
        assert!(teeth.kind == ApiKind::Method && teeth.has_self);
        assert_eq!(teeth.canonical, "widgets::parts::gear::Gear::teeth");
        let named = impls
            .iter()
            .find(|imp| imp.trait_path.as_deref() == Some("widgets_core::Named"))
            .unwrap();
        assert!(named.items.contains_key("name") && named.provided == ["greeting"]);
        // rustdoc lists the blanket impl that applies to `Gear`.
        let shout = impls
            .iter()
            .find(|imp| imp.trait_path.as_deref() == Some("widgets_core::Shout"))
            .unwrap();
        assert!(shout.blanket && shout.items.contains_key("shout"));

        let core = fixture("widgets_core");
        // The blanket impl as written: `T: Named` (`?Sized` is no bound).
        let blanket = &core.impls[core.blankets[0] as usize];
        assert_eq!(blanket.trait_path.as_deref(), Some("widgets_core::Shout"));
        assert_eq!(
            blanket.bounds.as_deref(),
            Some(&["widgets_core::Named".to_string()][..])
        );
        let named = core.trait_items("widgets_core::Named").unwrap();
        assert!(named.contains_key("name") && named.contains_key("greeting"));
        let poll = core.ty("widgets_core::task::Poll").unwrap();
        assert!(poll.variants.contains_key("Ready") && poll.variants.contains_key("Pending"));
        let (entries, globs) = core.module_entries("task");
        assert!(entries.iter().any(|(name, _)| name == "Poll") && globs.is_empty());
    }

    #[test]
    fn unknown_formats_are_refused() {
        let text = include_str!("../../../tests/rustdoc_fixture/json/widgets.json");
        let other = text.replace("\"format_version\":61}", "\"format_version\":9999}");
        assert_eq!(
            parse_crate(other.as_bytes()).unwrap_err(),
            ReadError::UnsupportedFormat(9999)
        );
        assert_eq!(
            parse_crate(b"{\"index\":{}}").unwrap_err(),
            ReadError::NotRustdocJson
        );
    }
}
