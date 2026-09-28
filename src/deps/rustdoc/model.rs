//! The API index of one crate, as rustdoc describes it: every path the
//! crate's public API names and what it names, every impl (inherent,
//! trait, blanket), and where each item is written.
//!
//! Paths are relative to the crate root (`cell::RefCell` in `core`);
//! items and types are keyed by their *canonical* path — the path of
//! definition rustdoc records, crate first (`core::cell::RefCell`,
//! `core::panic::unwind_safe::UnwindSafe`) — so indexes of different
//! crates name the same item the same way. Primitive types are keyed
//! `prim:<name>` (`prim:str`; every slice is `prim:slice`, every array
//! `prim:array`, every raw pointer `prim:pointer`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What an index item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKind {
    Module,
    Struct,
    Enum,
    Union,
    Trait,
    TypeAlias,
    Primitive,
    /// A free function.
    Function,
    /// A function of an impl or a trait (with or without `self`).
    Method,
    Variant,
    Constant,
    Static,
    AssocConst,
    AssocType,
    Macro,
}

impl ApiKind {
    /// A type's kind (what a method's owner or a `Deref` target can be).
    pub fn is_type(self) -> bool {
        matches!(
            self,
            Self::Struct | Self::Enum | Self::Union | Self::TypeAlias | Self::Primitive
        )
    }
}

/// One item the index refers to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiItem {
    pub kind: ApiKind,
    pub name: String,
    /// The canonical path (`core::cell::RefCell`); a method's is its
    /// owner's plus its name (`core::cell::RefCell::borrow`,
    /// `prim:str::len`). Empty when rustdoc gives the item no path.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub canonical: String,
    /// Where it is written: relative to the graph's source root, or
    /// `@toolchain:<crate>/src/…` (the toolchain's library), or `@abs:<path>`
    /// (another crate's source); empty when rustdoc gives no span.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    #[serde(default)]
    pub line: u32,
    #[serde(default)]
    pub end_line: u32,
    /// A method's owner as the index writes qualified names (`RefCell`,
    /// `str`, the trait's name for a trait's own items).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// A method that takes `self` (callable as `recv.m()`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_self: bool,
    /// Nameable from outside the crate (`pub`, or a trait/impl member).
    #[serde(default, skip_serializing_if = "is_false")]
    pub public: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// An item of another crate, by that crate's name and a path in it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExternalPath {
    pub krate: String,
    /// Segments after the crate, `::`-joined (`task`, `cell::RefCell`).
    pub path: String,
}

/// What a path names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// An item of this crate (its index in [`ApiIndex::items`]).
    Item(u32),
    /// A module of this crate (its public items are paths of their own).
    Module,
    /// A re-export of another crate's item or module.
    External(ExternalPath),
}

/// One impl block (or a blanket impl rustdoc found to apply to a type).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiImpl {
    /// The implemented trait's canonical path; `None` for an inherent impl.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trait_path: Option<String>,
    /// The type key it is for; empty for a blanket impl over a generic.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub for_type: String,
    /// The type's generic arguments as the impl writes them: a type key
    /// where concrete (`impl Atomic<bool>`), `None` where generic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub for_args: Vec<Option<String>>,
    /// A blanket impl (`impl<T: Display> ToString for T`), listed on a type
    /// because rustdoc found it applies, or as written (then `for_type` is
    /// empty and `bounds` are what `T` must implement).
    #[serde(default, skip_serializing_if = "is_false")]
    pub blanket: bool,
    /// The traits a blanket impl's `T` must implement (canonical paths);
    /// `None` when a bound is anything but a plain trait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Vec<String>>,
    /// Members by name.
    #[serde(default)]
    pub items: BTreeMap<String, u32>,
    /// Trait methods the impl does not override (the trait's own).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provided: Vec<String>,
    /// A `Deref` impl's `Target`, as a type key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deref_target: Option<String>,
    /// `impl !Trait for T`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub negative: bool,
    /// An auto-trait impl rustdoc synthesized (`Send`, `Unpin`): no items.
    #[serde(default, skip_serializing_if = "is_false")]
    pub synthetic: bool,
}

/// A type the crate defines (or a primitive it documents impls of).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiType {
    pub item: u32,
    /// Impls of it in this crate (indexes into [`ApiIndex::impls`]).
    #[serde(default)]
    pub impls: Vec<u32>,
    /// What a type alias stands for, as a type key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<String>,
    /// The generic arguments the alias gives it (`AtomicBool` =
    /// `Atomic<bool>`), as in [`ApiImpl::for_args`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alias_args: Vec<Option<String>>,
    /// An enum's variants by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, u32>,
}

/// One crate's API index (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiIndex {
    /// [`super::API_FORMAT`] of the build that wrote it.
    pub format: u32,
    /// The rustdoc JSON `format_version` it was read from.
    pub rustdoc_format: u32,
    /// The crate's name as code names it (`core`, `serde_json`).
    pub krate: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crate_version: Option<String>,
    pub items: Vec<ApiItem>,
    /// Public paths (relative to the crate root) → what they name.
    pub paths: BTreeMap<String, Target>,
    /// Every item's path of definition (relative to the crate root), public
    /// or not — how a type written inside the crate is found.
    #[serde(default)]
    pub defined: BTreeMap<String, u32>,
    /// Glob re-exports of other crates' modules, by the module path they
    /// are in (`task` → `core::task`, `alloc::task`; `""` = crate root).
    #[serde(default)]
    pub globs: BTreeMap<String, Vec<ExternalPath>>,
    pub impls: Vec<ApiImpl>,
    /// Types defined here, and primitives, by type key.
    pub types: BTreeMap<String, ApiType>,
    /// Impls here of types defined elsewhere, by type key (`alloc`'s
    /// `impl str`, `std`'s `impl f64`).
    #[serde(default)]
    pub foreign_impls: BTreeMap<String, Vec<u32>>,
    /// Blanket impls written here over a bare generic.
    #[serde(default)]
    pub blankets: Vec<u32>,
    /// Traits defined here → their items by name.
    #[serde(default)]
    pub traits: BTreeMap<String, BTreeMap<String, u32>>,
}

/// Where a path lookup inside one index ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStep {
    /// The whole path names this item.
    Item(u32),
    /// A prefix names this type or trait; `rest` is left (a member).
    Member { item: u32, rest: Vec<String> },
    /// A prefix is another crate's re-export: continue there with `rest`
    /// appended to the external path.
    External {
        target: ExternalPath,
        rest: Vec<String>,
    },
    /// The module `rest` is looked up in glob-imports these modules.
    Globs {
        targets: Vec<ExternalPath>,
        rest: Vec<String>,
    },
    /// A module of this crate.
    Module,
    /// This crate has no such path.
    NotFound,
}

impl ApiIndex {
    pub fn item(&self, index: u32) -> Option<&ApiItem> {
        self.items.get(index as usize)
    }

    /// Where `segments` (relative to the crate root) lead in this crate.
    pub fn resolve(&self, segments: &[String]) -> PathStep {
        if segments.is_empty() {
            return PathStep::Module;
        }
        for len in (1..=segments.len()).rev() {
            let key = segments[..len].join("::");
            let rest = || segments[len..].to_vec();
            if let Some(target) = self.paths.get(&key) {
                let step = match target {
                    Target::Item(item) if len == segments.len() => PathStep::Item(*item),
                    Target::Item(item) => PathStep::Member {
                        item: *item,
                        rest: rest(),
                    },
                    Target::Module if len == segments.len() => PathStep::Module,
                    // A module of ours whose item is not one of its paths:
                    // only its globs can have it.
                    Target::Module => self.globs_at(&key, rest()),
                    Target::External(target) => PathStep::External {
                        target: target.clone(),
                        rest: rest(),
                    },
                };
                if step != PathStep::NotFound {
                    return step;
                }
                break;
            }
        }
        match self.globs_at("", segments.to_vec()) {
            PathStep::NotFound => self.resolve_defined(segments),
            step => step,
        }
    }

    /// `segments` as a path of definition (how another crate's index, and
    /// types written inside the crate, name its items — private modules
    /// and all).
    pub fn resolve_defined(&self, segments: &[String]) -> PathStep {
        for len in (1..=segments.len()).rev() {
            if let Some(item) = self.defined.get(&segments[..len].join("::")) {
                return if len == segments.len() {
                    PathStep::Item(*item)
                } else {
                    PathStep::Member {
                        item: *item,
                        rest: segments[len..].to_vec(),
                    }
                };
            }
        }
        PathStep::NotFound
    }

    fn globs_at(&self, module: &str, rest: Vec<String>) -> PathStep {
        match self.globs.get(module) {
            Some(targets) if !targets.is_empty() => PathStep::Globs {
                targets: targets.clone(),
                rest,
            },
            _ => PathStep::NotFound,
        }
    }

    /// The type (or primitive) `key` if this crate defines it.
    pub fn ty(&self, key: &str) -> Option<&ApiType> {
        self.types.get(key)
    }

    /// Impls in this crate of the type `key` (its own, or foreign ones).
    pub fn impls_of(&self, key: &str) -> Vec<&ApiImpl> {
        let own = self.types.get(key).map(|ty| ty.impls.as_slice());
        let foreign = self.foreign_impls.get(key).map(Vec::as_slice);
        own.into_iter()
            .chain(foreign)
            .flatten()
            .filter_map(|index| self.impls.get(*index as usize))
            .collect()
    }

    /// The items of the trait `key`, when this crate defines it.
    pub fn trait_items(&self, key: &str) -> Option<&BTreeMap<String, u32>> {
        self.traits.get(key)
    }

    /// The public names a module (`prelude::rust_2024`) exposes directly,
    /// with what each names, and the other crates' modules it glob-imports.
    pub fn module_entries(&self, module: &str) -> (Vec<(String, Target)>, Vec<ExternalPath>) {
        let prefix = format!("{module}::");
        let entries = self
            .paths
            .range(prefix.clone()..)
            .take_while(|(key, _)| key.starts_with(&prefix))
            .filter(|(key, _)| !key[prefix.len()..].contains("::"))
            .map(|(key, target)| (key[prefix.len()..].to_string(), target.clone()))
            .collect();
        (entries, self.globs.get(module).cloned().unwrap_or_default())
    }

    /// Keep only the items `keep` accepts (and every type, primitive and
    /// trait, which method lookup needs whether or not the graph holds
    /// them); paths, impls and traits referring to a dropped item forget it.
    pub fn retain_items(&mut self, keep: impl Fn(&ApiItem) -> bool) {
        let mut remap: Vec<Option<u32>> = Vec::with_capacity(self.items.len());
        let mut kept = Vec::new();
        for item in std::mem::take(&mut self.items) {
            let needed = item.kind.is_type() || item.kind == ApiKind::Trait || keep(&item);
            if needed {
                remap.push(Some(kept.len() as u32));
                kept.push(item);
            } else {
                remap.push(None);
            }
        }
        self.items = kept;
        let map = |index: u32| remap.get(index as usize).copied().flatten();
        self.paths.retain(|_, target| match target {
            Target::Item(index) => match map(*index) {
                Some(new) => {
                    *index = new;
                    true
                }
                None => false,
            },
            _ => true,
        });
        self.defined.retain(|_, index| match map(*index) {
            Some(new) => {
                *index = new;
                true
            }
            None => false,
        });
        for imp in &mut self.impls {
            imp.items.retain(|_, index| match map(*index) {
                Some(new) => {
                    *index = new;
                    true
                }
                None => false,
            });
        }
        for members in self.traits.values_mut() {
            members.retain(|_, index| match map(*index) {
                Some(new) => {
                    *index = new;
                    true
                }
                None => false,
            });
        }
        self.types.retain(|_, ty| match map(ty.item) {
            Some(new) => {
                ty.item = new;
                ty.variants.retain(|_, index| match map(*index) {
                    Some(new) => {
                        *index = new;
                        true
                    }
                    None => false,
                });
                true
            }
            None => false,
        });
    }

    /// Every method name the crate's impls and traits define.
    pub fn method_names(&self) -> impl Iterator<Item = &str> {
        self.items
            .iter()
            .filter(|item| item.kind == ApiKind::Method)
            .map(|item| item.name.as_str())
    }
}
