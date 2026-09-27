//! The Rust ecosystem: cargo-fuzz targets.
//!
//! [`RustContext::callable`] turns an indexed function into what a
//! `fuzz_target!` can call — its public path, each parameter's
//! [`types::ArgForm`], the receiver's constructor for a method — and
//! [`harness`] writes the cargo-fuzz project around it.

pub mod api;
pub mod existing;
pub mod harness;
pub mod types;

use std::collections::HashMap;

use api::{CrateInfo, OwnerType, RustApi, visibility_is_pub};
use types::{ArgForm, RawParam, classify, generic_bounds, return_type, split_params};

use super::model::{Harnessable, InputShape, ParamInfo, ReceiverInfo};
use super::sites::FnSyntax;
use crate::analyze::bugs::FnSpan;

/// A parameter the harness fills.
#[derive(Debug, Clone)]
pub struct HarnessParam {
    pub name: String,
    pub ty: String,
    pub form: ArgForm,
}

/// How the harness drains a returned iterator (its work runs in `next`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drain {
    None,
    /// `for _item in call {}`.
    Iterator,
    /// `if let Ok(items) = call { for _item in items {} }`.
    IteratorInResult,
    /// `if let Some(items) = call { for _item in items {} }`.
    IteratorInOption,
}

/// How a constructor's result is unwrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    Plain,
    Result,
    Option,
}

/// A way to build a method's receiver.
#[derive(Debug, Clone)]
pub struct Constructor {
    /// The call, without arguments (`blurhash::Decoder::new`).
    pub call: String,
    /// For the report (`Decoder::new`, `Default`).
    pub label: String,
    pub params: Vec<HarnessParam>,
    pub wrap: Wrap,
}

/// How the harness calls the target.
#[derive(Debug, Clone)]
pub enum CallForm {
    /// A free or associated function: the full path (`krate::a::f`,
    /// `krate::Type::parse`).
    Path(String),
    /// A method on a constructed receiver.
    Method {
        name: String,
        owner_path: String,
        constructor: Option<Constructor>,
    },
}

/// A function as a fuzz target can call it.
#[derive(Debug, Clone)]
pub struct RustCallable {
    pub span: FnSpan,
    pub krate: CrateInfo,
    pub call: CallForm,
    pub owner: Option<String>,
    pub params: Vec<HarnessParam>,
    /// Explicit generic arguments (`::<_, krate::Value>`), when the return
    /// type alone fixes a parameter.
    pub turbofish: Option<String>,
    pub trait_import: Option<String>,
    pub is_async: bool,
    pub is_unsafe: bool,
    /// The result is a lazy iterator the harness has to drain.
    pub drain: Drain,
    /// What the author has to supply, in words.
    pub todos: Vec<String>,
}

impl RustCallable {
    /// Every value the harness has to build: the constructor's, then the
    /// target's.
    pub fn all_params(&self) -> Vec<&HarnessParam> {
        let constructor = match &self.call {
            CallForm::Method {
                constructor: Some(constructor),
                ..
            } => constructor.params.iter().collect(),
            _ => Vec::new(),
        };
        constructor.into_iter().chain(self.params.iter()).collect()
    }

    pub fn harnessable(&self) -> Harnessable {
        if self.todos.is_empty() {
            Harnessable::Complete
        } else {
            Harnessable::NeedsInput
        }
    }

    /// The public path as a person would write it (`blurhash::decode`).
    pub fn display_path(&self) -> String {
        match &self.call {
            CallForm::Path(path) => path.clone(),
            CallForm::Method {
                name, owner_path, ..
            } => format!("{owner_path}::{name}"),
        }
    }

    pub fn param_infos(&self) -> Vec<ParamInfo> {
        self.params
            .iter()
            .map(|p| ParamInfo {
                name: p.name.clone(),
                ty: p.ty.clone(),
                shape: p.form.shape(),
            })
            .collect()
    }

    /// Weight for the receiver: a method needs a value to call it on.
    pub fn receiver_weight(&self) -> f64 {
        match &self.call {
            CallForm::Path(_) => 1.0,
            CallForm::Method {
                constructor: Some(constructor),
                ..
            } => {
                if constructor
                    .params
                    .iter()
                    .any(|p| p.form.shape() == InputShape::Opaque)
                {
                    0.5
                } else {
                    0.9
                }
            }
            CallForm::Method {
                constructor: None, ..
            } => 0.35,
        }
    }

    pub fn receiver_info(&self) -> Option<ReceiverInfo> {
        match &self.call {
            CallForm::Path(_) => None,
            CallForm::Method { constructor, .. } => Some(ReceiverInfo {
                owner: self.owner.clone().unwrap_or_default(),
                constructor: constructor.as_ref().map(|c| c.label.clone()),
            }),
        }
    }
}

/// Everything [`RustContext::callable`] reads: the API, and every indexed
/// function with its syntax facts (constructors are looked up there).
pub struct RustContext<'a> {
    pub api: &'a RustApi,
    pub functions: &'a [FnSpan],
    pub syntax: &'a HashMap<String, FnSyntax>,
    /// `Owner` → indices into `functions` of its associated functions.
    by_owner: HashMap<&'a str, Vec<usize>>,
}

impl<'a> RustContext<'a> {
    pub fn new(
        api: &'a RustApi,
        functions: &'a [FnSpan],
        syntax: &'a HashMap<String, FnSyntax>,
    ) -> Self {
        let mut by_owner: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, span) in functions.iter().enumerate() {
            if let Some((owner, _)) = span.qualified_name.rsplit_once("::") {
                by_owner.entry(owner).or_default().push(index);
            }
        }
        Self {
            api,
            functions,
            syntax,
            by_owner,
        }
    }

    /// `span` as a fuzz target calls it, or `None` when no outside crate can
    /// (private, in a binary, a trait's own method, a test).
    pub fn callable(&self, span: &FnSpan) -> Option<RustCallable> {
        if span.is_test || !span.file.ends_with(".rs") {
            return None;
        }
        let krate = self.api.crate_of(&span.file)?.clone();
        let syntax = self.syntax.get(&span.id).cloned().unwrap_or_default();
        let signature = span.signature.clone().unwrap_or_default();
        let raw = split_params(&signature);
        let receiver = raw.iter().find_map(|p| match p {
            RawParam::SelfParam(kind) => Some(*kind),
            RawParam::Named { .. } => None,
        });
        let bounds = generic_bounds(&syntax.generics);
        let mut params = harness_params(&raw, &bounds);
        self.fill_defaults(&krate, &mut params);
        let mut todos = Vec::new();
        for param in params.iter().filter(|p| p.form == ArgForm::Opaque) {
            todos.push(format!("build `{}: {}`", param.name, param.ty));
        }
        let returns = return_type(&signature).unwrap_or_default();

        let owner_name = (span.kind == "method")
            .then(|| span.qualified_name.rsplit_once("::").map(|(o, _)| o))
            .flatten()
            .map(|owner| owner.rsplit("::").next().unwrap_or(owner).to_string());
        let lib = &krate.lib_name;
        let mut trait_import = None;
        let call = match &owner_name {
            None => {
                let path = self.api.function_path(&krate, span, &syntax)?;
                CallForm::Path(format!("{lib}::{}", path.join("::")))
            }
            Some(owner_name) => {
                let owner = self.api.owner_type(&krate, owner_name, &span.file)?;
                if owner.kind == "trait" {
                    return None;
                }
                let owner_path = format!("{lib}::{}", owner.path.as_ref()?.join("::"));
                match &syntax.impl_trait {
                    Some(implemented) => {
                        trait_import = self.trait_import(&krate, implemented, &mut todos);
                    }
                    None if !visibility_is_pub(&syntax.visibility) => return None,
                    None => {}
                }
                if owner.generic {
                    todos.push(format!(
                        "`{}` is generic: name its type parameters where the harness builds it",
                        owner.name
                    ));
                }
                match receiver {
                    None => CallForm::Path(format!("{owner_path}::{}", span.name)),
                    Some(_) => {
                        let constructor = self.constructor(&krate, &owner, &owner_path, span);
                        if constructor.is_none() {
                            todos.push(format!("construct the `{}` receiver", owner.name));
                        }
                        CallForm::Method {
                            name: span.name.clone(),
                            owner_path,
                            constructor,
                        }
                    }
                }
            }
        };
        if let CallForm::Method {
            constructor: Some(constructor),
            ..
        } = &call
        {
            for param in constructor
                .params
                .iter()
                .filter(|p| p.form == ArgForm::Opaque)
            {
                todos.push(format!(
                    "build the constructor's `{}: {}`",
                    param.name, param.ty
                ));
            }
        }
        let is_async = syntax.modifiers.split_whitespace().any(|m| m == "async");
        let is_unsafe = syntax.modifiers.split_whitespace().any(|m| m == "unsafe");
        if is_async {
            todos.push("drive the returned future with an executor".to_string());
        }
        if is_unsafe {
            todos.push("uphold the function's safety contract before calling it".to_string());
        }
        let turbofish = turbofish(
            &raw,
            &bounds,
            &params,
            &returns,
            &syntax.generics,
            &mut todos,
            || {
                self.api
                    .public_paths(&krate, &[], "Value", true)
                    .into_iter()
                    .next()
                    .filter(|_| self.crate_defines_type(&krate, "Value"))
                    .map(|path| format!("{lib}::{}", path.join("::")))
            },
        );
        Some(RustCallable {
            span: span.clone(),
            krate,
            call,
            owner: owner_name,
            params,
            turbofish,
            trait_import,
            is_async,
            is_unsafe,
            drain: self.drain(&returns, &mut todos),
            todos,
        })
    }

    fn crate_defines_type(&self, krate: &CrateInfo, name: &str) -> bool {
        self.api
            .owner_type(krate, name, &krate.lib_root)
            .is_some_and(|owner| owner.path.is_some() && owner.kind != "trait")
    }

    /// The `use` a trait-impl method needs in scope (`None` when it is
    /// unknown — a TODO says so).
    fn trait_import(
        &self,
        krate: &CrateInfo,
        implemented: &str,
        todos: &mut Vec<String>,
    ) -> Option<String> {
        let bare = implemented.split('<').next().unwrap_or(implemented).trim();
        let last = bare.rsplit("::").next().unwrap_or(bare);
        if let Some((_, path)) = STD_TRAITS.iter().find(|(name, _)| *name == last) {
            return Some((*path).to_string());
        }
        if let Some(path) = self
            .api
            .owner_type(krate, last, &krate.lib_root)
            .filter(|owner| owner.kind == "trait")
            .and_then(|owner| owner.path)
        {
            return Some(format!("{}::{}", krate.lib_name, path.join("::")));
        }
        if bare.contains("::") {
            return Some(bare.to_string());
        }
        todos.push(format!("bring the trait `{bare}` into scope"));
        None
    }

    /// The project iterator type `returns` names (`HstoreEntries` in
    /// `Result<HstoreEntries<'_>, E>`) and its `next` method: the work a
    /// lazy result defers.
    pub fn lazy_next(&self, returns: &str) -> Option<(usize, Wrap)> {
        let wrap = if returns.starts_with("Option<") {
            Wrap::Option
        } else if returns
            .split('<')
            .next()
            .is_some_and(|head| head.ends_with("Result"))
        {
            Wrap::Result
        } else {
            Wrap::Plain
        };
        let inner = match wrap {
            Wrap::Plain => returns,
            _ => returns.split_once('<').map_or(returns, |(_, rest)| rest),
        };
        let name = inner
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
            .next()?
            .rsplit("::")
            .next()?;
        let next = self
            .by_owner
            .get(name)?
            .iter()
            .copied()
            .find(|&i| self.functions[i].name == "next")?;
        Some((next, wrap))
    }

    fn drain(&self, returns: &str, todos: &mut Vec<String>) -> Drain {
        if [
            "impl Iterator",
            "impl DoubleEndedIterator",
            "impl ExactSizeIterator",
        ]
        .iter()
        .any(|prefix| returns.starts_with(prefix))
        {
            return Drain::Iterator;
        }
        let Some((next, wrap)) = self.lazy_next(returns) else {
            return Drain::None;
        };
        let implemented = self
            .syntax
            .get(&self.functions[next].id)
            .and_then(|syntax| syntax.impl_trait.clone())
            .unwrap_or_default();
        if implemented.split('<').next().map(str::trim) != Some("Iterator") {
            todos.push(format!(
                "drain the returned `{}` (its work runs in `next`)",
                self.functions[next]
                    .qualified_name
                    .rsplit_once("::")
                    .map_or("", |(o, _)| o)
            ));
            return Drain::None;
        }
        match wrap {
            Wrap::Plain => Drain::Iterator,
            Wrap::Result => Drain::IteratorInResult,
            Wrap::Option => Drain::IteratorInOption,
        }
    }

    /// Opaque parameters of a project type that has a `Default` (derived,
    /// or an `impl Default` — an indexed `Type::default`) get one.
    fn fill_defaults(&self, krate: &CrateInfo, params: &mut [HarnessParam]) {
        for param in params.iter_mut().filter(|p| p.form == ArgForm::Opaque) {
            let Some((reference, name)) = types::named_type(&param.ty) else {
                continue;
            };
            let has_impl = self.by_owner.get(name.as_str()).is_some_and(|functions| {
                functions
                    .iter()
                    .any(|&i| self.functions[i].name == "default")
            });
            let derives = || {
                self.api
                    .owner_type(krate, &name, &krate.lib_root)
                    .is_some_and(|owner| owner.derives_default && owner.path.is_some())
            };
            if has_impl || derives() {
                param.form = ArgForm::Default { reference };
            }
        }
    }

    /// The best constructor of `owner`: all-fuzzable parameters first, then
    /// `new`, `default`, `from_*`/`parse*`…, then fewest parameters.
    fn constructor(
        &self,
        krate: &CrateInfo,
        owner: &OwnerType,
        owner_path: &str,
        target: &FnSpan,
    ) -> Option<Constructor> {
        let mut best: Option<((bool, u8, usize, String), Constructor)> = None;
        for &index in self.by_owner.get(owner.name.as_str()).into_iter().flatten() {
            let span = &self.functions[index];
            if span.id == target.id
                || span.is_test
                || self
                    .api
                    .crate_of(&span.file)
                    .is_none_or(|k| k.dir != krate.dir)
            {
                continue;
            }
            let syntax = self.syntax.get(&span.id).cloned().unwrap_or_default();
            if syntax.impl_trait.is_none() && !visibility_is_pub(&syntax.visibility) {
                continue;
            }
            if syntax.modifiers.contains("async") || syntax.modifiers.contains("unsafe") {
                continue;
            }
            let signature = span.signature.clone().unwrap_or_default();
            let raw = split_params(&signature);
            if raw.iter().any(|p| matches!(p, RawParam::SelfParam(_))) {
                continue;
            }
            let Some(wrap) =
                return_type(&signature).and_then(|returns| constructs(&returns, &owner.name))
            else {
                continue;
            };
            let bounds = generic_bounds(&syntax.generics);
            let mut params = harness_params(&raw, &bounds);
            self.fill_defaults(krate, &mut params);
            let opaque = params.iter().any(|p| p.form == ArgForm::Opaque);
            let name_rank = match span.name.as_str() {
                "new" => 0,
                "default" => 1,
                name if ["from_", "parse", "decode", "with_", "read"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix)) =>
                {
                    2
                }
                _ => 3,
            };
            let key = (opaque, name_rank, params.len(), span.name.clone());
            if best.as_ref().is_some_and(|(best_key, _)| *best_key <= key) {
                continue;
            }
            best = Some((
                key,
                Constructor {
                    call: format!("{owner_path}::{}", span.name),
                    label: format!("{}::{}", owner.name, span.name),
                    params: params
                        .into_iter()
                        .map(|p| HarnessParam {
                            name: format!("new_{}", p.name),
                            ..p
                        })
                        .collect(),
                    wrap,
                },
            ));
        }
        let best = best.map(|(_, constructor)| constructor);
        if owner.derives_default
            && best
                .as_ref()
                .is_none_or(|c| c.params.iter().any(|p| p.form == ArgForm::Opaque))
        {
            return Some(Constructor {
                call: format!("<{owner_path} as Default>::default"),
                label: "Default".to_string(),
                params: Vec::new(),
                wrap: Wrap::Plain,
            });
        }
        best
    }
}

/// Std traits whose methods need a `use` to be called by path.
const STD_TRAITS: &[(&str, &str)] = &[
    ("FromStr", "std::str::FromStr"),
    ("Read", "std::io::Read"),
    ("BufRead", "std::io::BufRead"),
    ("Write", "std::io::Write"),
    ("Seek", "std::io::Seek"),
    ("Index", "std::ops::Index"),
    ("IndexMut", "std::ops::IndexMut"),
    ("From", "std::convert::From"),
    ("TryFrom", "std::convert::TryFrom"),
    ("Iterator", "std::iter::Iterator"),
    ("Default", "std::default::Default"),
];

/// Each named parameter with its form. Type parameters any `u8` satisfies
/// (`T: Copy`) are instantiated as `u8`, so `&[T]` is fuzzed as bytes.
fn harness_params(raw: &[RawParam], bounds: &HashMap<String, String>) -> Vec<HarnessParam> {
    let as_u8 = types::u8_params(bounds);
    raw.iter()
        .filter_map(|param| match param {
            RawParam::SelfParam(_) => None,
            RawParam::Named { name, ty } => Some((name, ty)),
        })
        .enumerate()
        .map(|(index, (name, ty))| {
            let concrete = as_u8
                .iter()
                .fold(ty.clone(), |ty, param| types::substitute(&ty, param, "u8"));
            HarnessParam {
                name: field_name(name, index),
                ty: ty.clone(),
                form: classify(&concrete, bounds),
            }
        })
        .collect()
}

/// A parameter's name as a struct field (`arg2` for `_` and patterns).
fn field_name(name: &str, index: usize) -> String {
    let valid = !name.is_empty()
        && name != "_"
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    if valid {
        name.trim_start_matches('_').to_string()
    } else {
        format!("arg{index}")
    }
}

/// Whether `returns` hands back the owner (`Self`, `Owner`, `Result<Self,
/// E>`, `Option<Owner<T>>`) and how it is wrapped.
fn constructs(returns: &str, owner: &str) -> Option<Wrap> {
    let names_owner = |ty: &str| {
        let ty = ty.trim();
        ty == "Self" || ty == owner || ty.starts_with(&format!("{owner}<"))
    };
    if names_owner(returns) {
        return Some(Wrap::Plain);
    }
    let (head, args) = returns.split_once('<')?;
    let args = args.strip_suffix('>')?;
    let first = types::split_top_level(args, ',').first().copied()?;
    if !names_owner(first) {
        return None;
    }
    let head = head.rsplit("::").next().unwrap_or(head);
    if head.ends_with("Result") {
        Some(Wrap::Result)
    } else if head == "Option" {
        Some(Wrap::Option)
    } else {
        None
    }
}

/// Explicit generic arguments when a type parameter appears in no
/// parameter the harness passes (typically `fn from_str<T:
/// Deserialize>(s: &str) -> Result<T>`): `_` for inferred ones, the
/// crate's own `Value` for a `Deserialize` bound, a TODO otherwise.
fn turbofish(
    raw: &[RawParam],
    bounds: &HashMap<String, String>,
    params: &[HarnessParam],
    returns: &str,
    generics: &str,
    todos: &mut Vec<String>,
    crate_value: impl Fn() -> Option<String>,
) -> Option<String> {
    let order = type_param_order(generics);
    if order.is_empty() {
        return None;
    }
    let mentioned = |name: &str, text: &str| {
        text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| word == name)
    };
    let in_params = |name: &str| {
        raw.iter().any(|p| match p {
            RawParam::Named { ty, .. } => mentioned(name, ty),
            RawParam::SelfParam(_) => false,
        })
    };
    let free: Vec<&String> = order.iter().filter(|name| !in_params(name)).collect();
    if free.is_empty() {
        return None;
    }
    let impl_params = params
        .iter()
        .any(|p| p.ty.trim_start().starts_with("impl "));
    let mut args = Vec::new();
    for name in &order {
        if !free.contains(&name) {
            args.push("_".to_string());
            continue;
        }
        let bound = bounds.get(name).cloned().unwrap_or_default();
        let chosen = if bound.contains("Deserialize") {
            crate_value()
        } else {
            None
        };
        match chosen {
            Some(ty) => args.push(ty),
            None => {
                if mentioned(name, returns) || !bound.is_empty() {
                    todos.push(format!(
                        "pick a concrete type for `{name}`{}",
                        if bound.is_empty() {
                            String::new()
                        } else {
                            format!(" (bound: {bound})")
                        }
                    ));
                }
                args.push(format!("() /* TODO: {name} */"));
            }
        }
    }
    if impl_params {
        todos.push("`impl Trait` parameters forbid explicit generic arguments".to_string());
        return None;
    }
    Some(format!("::<{}>", args.join(", ")))
}

/// Type parameter names in declaration order (lifetimes and consts out).
fn type_param_order(generics: &str) -> Vec<String> {
    let text = types::normalize(generics);
    let Some(list) = text.strip_prefix('<') else {
        return Vec::new();
    };
    // Up to the angle bracket closing the list.
    let mut depth = 1i32;
    let mut end = list.len();
    for (i, ch) in list.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    types::split_top_level(&list[..end], ',')
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.starts_with('\'') && !p.starts_with("const "))
        .map(|p| {
            p.split(|c: char| c == ':' || c == '=' || c.is_whitespace())
                .next()
                .unwrap_or(p)
                .to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_are_recognized_by_return_type() {
        assert_eq!(constructs("Self", "Decoder"), Some(Wrap::Plain));
        assert_eq!(constructs("Decoder<'a>", "Decoder"), Some(Wrap::Plain));
        assert_eq!(
            constructs("Result<Self, Error>", "Decoder"),
            Some(Wrap::Result)
        );
        assert_eq!(
            constructs("io::Result<Decoder>", "Decoder"),
            Some(Wrap::Result)
        );
        assert_eq!(constructs("Option<Self>", "Decoder"), Some(Wrap::Option));
        assert_eq!(constructs("Vec<Self>", "Decoder"), None);
        assert_eq!(constructs("u32", "Decoder"), None);
    }

    #[test]
    fn free_type_parameters_get_a_turbofish() {
        let raw = split_params("(s: &str) -> Result<T>");
        let bounds = generic_bounds("<T: DeserializeOwned>");
        let params = harness_params(&raw, &bounds);
        let mut todos = Vec::new();
        let fish = turbofish(
            &raw,
            &bounds,
            &params,
            "Result<T>",
            "<T: DeserializeOwned>",
            &mut todos,
            || Some("serde_yaml::Value".to_string()),
        );
        assert_eq!(fish.as_deref(), Some("::<serde_yaml::Value>"));
        assert!(todos.is_empty());

        let raw = split_params("(r: R) -> Result<T>");
        let bounds = generic_bounds("<R: Read, T: Decode>");
        let params = harness_params(&raw, &bounds);
        let fish = turbofish(
            &raw,
            &bounds,
            &params,
            "Result<T>",
            "<R: Read, T: Decode>",
            &mut todos,
            || None,
        );
        assert_eq!(fish.as_deref(), Some("::<_, () /* TODO: T */>"));
        assert_eq!(todos.len(), 1, "{todos:?}");

        // Every parameter is inferred: nothing to add.
        let raw = split_params("(r: R) -> u8");
        let bounds = generic_bounds("<R: Read>");
        let params = harness_params(&raw, &bounds);
        assert_eq!(
            turbofish(
                &raw,
                &bounds,
                &params,
                "u8",
                "<R: Read>",
                &mut todos,
                || None
            ),
            None
        );
    }

    #[test]
    fn pattern_parameters_get_field_names() {
        assert_eq!(field_name("data", 0), "data");
        assert_eq!(field_name("_", 1), "arg1");
        assert_eq!(field_name("(a, b)", 2), "arg2");
        assert_eq!(field_name("_unused", 3), "unused");
    }
}
