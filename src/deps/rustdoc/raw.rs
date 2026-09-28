//! The part of rustdoc's JSON output ([`rustdoc_json_types`], format
//! [`super::SUPPORTED_FORMAT_VERSIONS`]) the API index reads — nothing
//! else is kept. Unknown fields are skipped without allocating, and every
//! item kind or type shape this reader does not need becomes `Other`, so a
//! crate of 87 MB (`core`) parses into a few tens of MB.
//!
//! [`rustdoc_json_types`]: https://github.com/rust-lang/rust/tree/master/src/rustdoc-json-types

use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, Visitor};

/// An item or path id (numeric in the supported formats).
pub(crate) type Id = u32;

/// One crate's rustdoc JSON.
#[derive(Debug, Deserialize)]
pub(crate) struct RawCrate {
    pub(crate) root: Id,
    #[serde(default)]
    pub(crate) crate_version: Option<String>,
    pub(crate) index: HashMap<Id, RawItem>,
    #[serde(default)]
    pub(crate) paths: HashMap<Id, RawSummary>,
    #[serde(default)]
    pub(crate) external_crates: HashMap<u32, RawExternalCrate>,
    pub(crate) format_version: u32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawExternalCrate {
    pub(crate) name: String,
}

/// `paths[id]`: an item's crate and its path of definition
/// (`["core", "cell", "RefCell"]`), for local and external items alike.
#[derive(Debug, Deserialize)]
pub(crate) struct RawSummary {
    pub(crate) crate_id: u32,
    pub(crate) path: Vec<String>,
    /// `module`, `struct`, `macro`, `proc_derive` …
    #[serde(default)]
    pub(crate) kind: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawItem {
    pub(crate) crate_id: u32,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) span: Option<RawSpan>,
    pub(crate) visibility: RawVisibility,
    pub(crate) inner: Inner,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawSpan {
    pub(crate) filename: String,
    pub(crate) begin: (u32, u32),
    pub(crate) end: (u32, u32),
}

/// `"public"`, `"default"` (trait items, impl members, variants),
/// `"crate"`, or `{"restricted": …}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawVisibility {
    Public,
    Default,
    Restricted,
}

impl<'de> Deserialize<'de> for RawVisibility {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawVisibility;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a visibility")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<RawVisibility, E> {
                Ok(match value {
                    "public" => RawVisibility::Public,
                    "default" => RawVisibility::Default,
                    _ => RawVisibility::Restricted,
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawVisibility, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(RawVisibility::Restricted)
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// An item's kind and the fields of it this reader uses.
#[derive(Debug)]
pub(crate) enum Inner {
    Module(RawModule),
    Use(RawUse),
    Impl(Box<RawImpl>),
    Function(RawFunction),
    Struct(RawImpls),
    Enum(RawEnum),
    Union(RawImpls),
    Trait(RawTrait),
    Primitive(RawPrimitive),
    TypeAlias(RawTypeAlias),
    AssocType(RawAssocType),
    Variant,
    Constant,
    Static,
    AssocConst,
    Macro,
    Other,
}

impl<'de> Deserialize<'de> for Inner {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Inner;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an item kind")
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Inner, E> {
                Ok(Inner::Other)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Inner, A::Error> {
                let Some(kind) = map.next_key::<String>()? else {
                    return Ok(Inner::Other);
                };
                let inner = match kind.as_str() {
                    "module" => Inner::Module(map.next_value()?),
                    "use" => Inner::Use(map.next_value()?),
                    "impl" => Inner::Impl(Box::new(map.next_value()?)),
                    "function" => Inner::Function(map.next_value()?),
                    "struct" => Inner::Struct(map.next_value()?),
                    "union" => Inner::Union(map.next_value()?),
                    "enum" => Inner::Enum(map.next_value()?),
                    "trait" => Inner::Trait(map.next_value()?),
                    "primitive" => Inner::Primitive(map.next_value()?),
                    "type_alias" => Inner::TypeAlias(map.next_value()?),
                    "assoc_type" => Inner::AssocType(map.next_value()?),
                    other => {
                        map.next_value::<IgnoredAny>()?;
                        match other {
                            "variant" => Inner::Variant,
                            "constant" => Inner::Constant,
                            "static" => Inner::Static,
                            "assoc_const" => Inner::AssocConst,
                            "macro" | "proc_macro" => Inner::Macro,
                            _ => Inner::Other,
                        }
                    }
                };
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(inner)
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawModule {
    #[serde(default)]
    pub(crate) items: Vec<Id>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawUse {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) id: Option<Id>,
    pub(crate) is_glob: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawImpl {
    #[serde(default)]
    pub(crate) generics: RawGenerics,
    #[serde(default)]
    pub(crate) provided_trait_methods: Vec<String>,
    #[serde(default, rename = "trait")]
    pub(crate) trait_: Option<RawPath>,
    #[serde(rename = "for")]
    pub(crate) for_: Ty,
    #[serde(default)]
    pub(crate) items: Vec<Id>,
    #[serde(default)]
    pub(crate) is_negative: bool,
    #[serde(default)]
    pub(crate) is_synthetic: bool,
    #[serde(default)]
    pub(crate) blanket_impl: Option<Ty>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawGenerics {
    #[serde(default)]
    pub(crate) params: Vec<RawGenericParam>,
    #[serde(default)]
    pub(crate) where_predicates: Vec<RawWherePredicate>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawGenericParam {
    pub(crate) name: String,
    pub(crate) kind: RawParamKind,
}

/// A generic parameter: only a type parameter's bounds are read.
#[derive(Debug)]
pub(crate) enum RawParamKind {
    Type(Vec<RawBound>),
    Other,
}

impl<'de> Deserialize<'de> for RawParamKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct TypeParam {
            #[serde(default)]
            bounds: Vec<RawBound>,
        }
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawParamKind;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a generic parameter kind")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawParamKind, A::Error> {
                let mut kind = RawParamKind::Other;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "type" {
                        kind = RawParamKind::Type(map.next_value::<TypeParam>()?.bounds);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(kind)
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// `where T: A + B` — only predicates on a bare generic are read.
#[derive(Debug)]
pub(crate) enum RawWherePredicate {
    Bound { ty: Ty, bounds: Vec<RawBound> },
    Other,
}

impl<'de> Deserialize<'de> for RawWherePredicate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Bound {
            #[serde(rename = "type")]
            ty: Ty,
            #[serde(default)]
            bounds: Vec<RawBound>,
        }
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawWherePredicate;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a where predicate")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<RawWherePredicate, A::Error> {
                let mut predicate = RawWherePredicate::Other;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "bound_predicate" {
                        let bound: Bound = map.next_value()?;
                        predicate = RawWherePredicate::Bound {
                            ty: bound.ty,
                            bounds: bound.bounds,
                        };
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(predicate)
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// A bound: a trait (and whether it is `?Trait`), or anything else.
#[derive(Debug)]
pub(crate) enum RawBound {
    Trait { path: RawPath, maybe: bool },
    Other,
}

impl<'de> Deserialize<'de> for RawBound {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct TraitBound {
            #[serde(rename = "trait")]
            trait_: RawPath,
            #[serde(default)]
            modifier: String,
        }
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawBound;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a bound")
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<RawBound, E> {
                Ok(RawBound::Other)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawBound, A::Error> {
                let mut bound = RawBound::Other;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "trait_bound" {
                        let found: TraitBound = map.next_value()?;
                        bound = RawBound::Trait {
                            path: found.trait_,
                            maybe: found.modifier == "maybe",
                        };
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(bound)
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// A path to an item (`{"path": "fmt::Display", "id": 314, "args": …}`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct RawPath {
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) id: Option<Id>,
    /// Its generic type arguments (`Atomic<bool>`: `[bool]`), lifetimes and
    /// consts left out.
    #[serde(default)]
    pub(crate) args: RawArgs,
}

/// `{"angle_bracketed": {"args": [{"type": …}, {"lifetime": …}], …}}`:
/// the type arguments, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RawArgs(pub(crate) Vec<Ty>);

impl<'de> Deserialize<'de> for RawArgs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Angle {
            #[serde(default)]
            args: Vec<Arg>,
        }
        enum Arg {
            Type(Ty),
            Other,
        }
        impl<'de> Deserialize<'de> for Arg {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct V;
                impl<'de> Visitor<'de> for V {
                    type Value = Arg;
                    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                        f.write_str("a generic argument")
                    }
                    fn visit_str<E: de::Error>(self, _: &str) -> Result<Arg, E> {
                        Ok(Arg::Other)
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Arg, A::Error> {
                        let mut arg = Arg::Other;
                        while let Some(key) = map.next_key::<String>()? {
                            if key == "type" {
                                arg = Arg::Type(map.next_value()?);
                            } else {
                                map.next_value::<IgnoredAny>()?;
                            }
                        }
                        Ok(arg)
                    }
                }
                deserializer.deserialize_any(V)
            }
        }
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawArgs;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("generic arguments")
            }
            fn visit_unit<E: de::Error>(self) -> Result<RawArgs, E> {
                Ok(RawArgs::default())
            }
            fn visit_none<E: de::Error>(self) -> Result<RawArgs, E> {
                Ok(RawArgs::default())
            }
            fn visit_some<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<RawArgs, D::Error> {
                deserializer.deserialize_any(self)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawArgs, A::Error> {
                let mut args = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if key == "angle_bracketed" {
                        let angle: Angle = map.next_value()?;
                        args = angle
                            .args
                            .into_iter()
                            .filter_map(|arg| match arg {
                                Arg::Type(ty) => Some(ty),
                                Arg::Other => None,
                            })
                            .collect();
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(RawArgs(args))
            }
        }
        deserializer.deserialize_option(V)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawFunction {
    pub(crate) sig: RawSig,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSig {
    #[serde(default)]
    pub(crate) inputs: Vec<(String, IgnoredAny)>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawImpls {
    #[serde(default)]
    pub(crate) impls: Vec<Id>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawEnum {
    #[serde(default)]
    pub(crate) variants: Vec<Id>,
    #[serde(default)]
    pub(crate) impls: Vec<Id>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTrait {
    #[serde(default)]
    pub(crate) items: Vec<Id>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPrimitive {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) impls: Vec<Id>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTypeAlias {
    #[serde(rename = "type")]
    pub(crate) ty: Ty,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawAssocType {
    #[serde(default, rename = "type")]
    pub(crate) ty: Option<Ty>,
}

/// A type, as far as method lookup cares: a path to an item, a primitive,
/// a generic parameter, a reference to one of those, a slice or an array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ty {
    Path(RawPath),
    Primitive(String),
    Generic(String),
    Ref(Box<Ty>),
    Slice,
    RawPointer,
    Array,
    Other,
}

impl<'de> Deserialize<'de> for Ty {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Borrowed {
            #[serde(rename = "type")]
            ty: Ty,
        }
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ty;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a type")
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Ty, E> {
                Ok(Ty::Other)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ty, A::Error> {
                let Some(kind) = map.next_key::<String>()? else {
                    return Ok(Ty::Other);
                };
                let ty = match kind.as_str() {
                    "resolved_path" => Ty::Path(map.next_value()?),
                    "primitive" => Ty::Primitive(map.next_value()?),
                    "generic" => Ty::Generic(map.next_value()?),
                    "borrowed_ref" => Ty::Ref(Box::new(map.next_value::<Borrowed>()?.ty)),
                    other => {
                        map.next_value::<IgnoredAny>()?;
                        match other {
                            "slice" => Ty::Slice,
                            "raw_pointer" => Ty::RawPointer,
                            "array" => Ty::Array,
                            _ => Ty::Other,
                        }
                    }
                };
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(ty)
            }
        }
        deserializer.deserialize_any(V)
    }
}
