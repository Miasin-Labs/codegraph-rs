//! SCIP symbol strings, as rust-analyzer writes them.
//!
//! `<scheme> <manager> <package> <version> <descriptor>+`, e.g.
//! `rust-analyzer cargo reqwest 0.12.28 async_impl/client/impl#[ClientBuilder]no_proxy().`
//! (a space inside a field is written twice). Descriptors are
//! `name/` (namespace: a module), `name#` (type), `name.` (term: a field,
//! const, static, variant), `name(disambiguator).` (method or function),
//! `[name]` (type parameter), `(name)` (parameter), `name:` (meta) and
//! `name!` (macro). A name that is not a plain identifier is back-quoted
//! (`` `Mul<Self>` ``, a doubled back-quote for a literal one).
//!
//! rust-analyzer spells an impl block `impl#[SelfType]` or
//! `impl#[SelfType][Trait]`, and its items hang under it:
//! `ops/arith/impl#[f64][`Mul<Self>`]mul().`.

/// How a descriptor ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suffix {
    Namespace,
    Type,
    Term,
    Method,
    TypeParameter,
    Parameter,
    Meta,
    Macro,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub name: String,
    pub suffix: Suffix,
    /// Byte offset of the descriptor in the symbol text.
    pub at: usize,
}

/// A parsed global symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub scheme: String,
    pub manager: String,
    pub package: String,
    pub version: String,
    pub descriptors: Vec<Descriptor>,
}

/// The standard library crates rust-analyzer names with a URL as version.
const STD_CRATES: [&str; 5] = ["std", "core", "alloc", "proc_macro", "test"];

impl Symbol {
    /// Parse a global symbol; `None` for a local (`local 3`) or anything
    /// malformed.
    pub fn parse(text: &str) -> Option<Symbol> {
        if text.starts_with("local ") {
            return None;
        }
        let mut rest = text;
        let scheme = take_field(&mut rest)?;
        let manager = take_field(&mut rest)?;
        let package = take_field(&mut rest)?;
        let version = take_field(&mut rest)?;
        let descriptors = parse_descriptors(rest, text.len() - rest.len())?;
        if descriptors.is_empty() {
            return None;
        }
        Some(Symbol {
            scheme,
            manager,
            package,
            version,
            descriptors,
        })
    }

    /// The standard library (`std`, `core`, `alloc`, …), whose version
    /// rust-analyzer writes as the source URL.
    pub fn is_std(&self) -> bool {
        STD_CRATES.contains(&self.package.as_str()) || self.version.starts_with("https://")
    }

    /// The module path the item sits in (the leading namespaces), without
    /// a crate root marker.
    pub fn modules(&self) -> Vec<&str> {
        self.descriptors
            .iter()
            .take_while(|descriptor| descriptor.suffix == Suffix::Namespace)
            .map(|descriptor| descriptor.name.as_str())
            .filter(|name| *name != "crate")
            .collect()
    }

    /// The item the symbol names, read from its descriptors.
    pub fn item(&self) -> Item<'_> {
        let rest: Vec<&Descriptor> = self
            .descriptors
            .iter()
            .skip_while(|descriptor| descriptor.suffix == Suffix::Namespace)
            .collect();
        let Some((last, before)) = rest.split_last() else {
            // A module itself (`other/`).
            let name = self
                .descriptors
                .last()
                .map_or("", |descriptor| descriptor.name.as_str());
            return Item {
                name,
                suffix: Suffix::Namespace,
                owner: None,
                trait_name: None,
                in_impl: false,
            };
        };
        let mut owner = None;
        let mut trait_name = None;
        let mut in_impl = false;
        let mut i = 0;
        while i < before.len() {
            let descriptor = before[i];
            if descriptor.suffix == Suffix::Type && descriptor.name == "impl" {
                in_impl = true;
                let params: Vec<&str> = before[i + 1..]
                    .iter()
                    .take_while(|descriptor| descriptor.suffix == Suffix::TypeParameter)
                    .map(|descriptor| descriptor.name.as_str())
                    .collect();
                owner = params.first().map(|name| type_head(name));
                trait_name = params.get(1).map(|name| type_head(name));
                i += 1 + params.len();
                continue;
            }
            if descriptor.suffix == Suffix::Type {
                owner = Some(descriptor.name.as_str());
                trait_name = None;
                in_impl = false;
            }
            i += 1;
        }
        Item {
            name: last.name.as_str(),
            suffix: last.suffix,
            owner,
            trait_name,
            in_impl,
        }
    }

    /// The symbol of the item a member belongs to: `Shape#area().` →
    /// `Shape#`. Used to find the trait of a trait method.
    pub fn text_without_last(text: &str) -> Option<&str> {
        let symbol = Symbol::parse(text)?;
        let last = symbol.descriptors.last()?;
        text.get(..last.at)
    }
}

/// What a symbol names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item<'a> {
    /// The item's own name (`no_proxy`, `ClientBuilder`, `json`).
    pub name: &'a str,
    pub suffix: Suffix,
    /// The type it belongs to (`ClientBuilder` for a method of its impl;
    /// the trait for a trait's method; the enum for a variant).
    pub owner: Option<&'a str>,
    /// The trait of the impl it is in (`impl#[Sq][Shape]area().` → `Shape`).
    pub trait_name: Option<&'a str>,
    /// Declared in an `impl` block.
    pub in_impl: bool,
}

/// `Vec<T, A>` → `Vec`, `&'a str` → `str`, `a::B<T>` → `B`; a slice,
/// array or tuple type (`[T]`) is returned unchanged.
pub fn type_head(name: &str) -> &str {
    let mut text = name.trim();
    loop {
        if let Some(rest) = text.strip_prefix('&') {
            text = rest.trim_start();
        } else if let Some(rest) = text.strip_prefix('\'') {
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            text = rest[end..].trim_start();
        } else if let Some(rest) = text
            .strip_prefix("mut ")
            .or_else(|| text.strip_prefix("dyn "))
        {
            text = rest.trim_start();
        } else {
            break;
        }
    }
    if text.starts_with(['[', '(']) {
        return text;
    }
    let head = text[..text.find('<').unwrap_or(text.len())].trim();
    head.rsplit("::").next().unwrap_or(head)
}

fn is_simple(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '+' | '-' | '$')
}

/// One space-separated package field (`  ` is a literal space).
fn take_field(rest: &mut &str) -> Option<String> {
    let bytes = rest.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b' ' {
            if bytes.get(i + 1) == Some(&b' ') {
                out.push(' ');
                i += 2;
                continue;
            }
            *rest = &rest[i + 1..];
            return Some(out);
        }
        let c = rest[i..].chars().next()?;
        out.push(c);
        i += c.len_utf8();
    }
    None
}

fn parse_descriptors(mut text: &str, base: usize) -> Option<Vec<Descriptor>> {
    let total = base + text.len();
    let mut out = Vec::new();
    while !text.is_empty() {
        let at = total - text.len();
        if let Some(rest) = text.strip_prefix('[') {
            let (name, rest) = read_name(rest)?;
            text = rest.strip_prefix(']')?;
            out.push(Descriptor {
                name,
                suffix: Suffix::TypeParameter,
                at,
            });
            continue;
        }
        if let Some(rest) = text.strip_prefix('(') {
            let (name, rest) = read_name(rest)?;
            text = rest.strip_prefix(')')?;
            out.push(Descriptor {
                name,
                suffix: Suffix::Parameter,
                at,
            });
            continue;
        }
        let (name, rest) = read_name(text)?;
        let mut chars = rest.chars();
        let suffix = match chars.next()? {
            '/' => Suffix::Namespace,
            '#' => Suffix::Type,
            '.' => Suffix::Term,
            ':' => Suffix::Meta,
            '!' => Suffix::Macro,
            '(' => {
                // `name(disambiguator).`
                let close = rest.find(')')?;
                text = rest[close + 1..].strip_prefix('.')?;
                out.push(Descriptor {
                    name,
                    suffix: Suffix::Method,
                    at,
                });
                continue;
            }
            _ => return None,
        };
        text = chars.as_str();
        out.push(Descriptor { name, suffix, at });
    }
    Some(out)
}

/// A simple identifier or a back-quoted name.
fn read_name(text: &str) -> Option<(String, &str)> {
    if let Some(mut rest) = text.strip_prefix('`') {
        let mut name = String::new();
        loop {
            let at = rest.find('`')?;
            name.push_str(&rest[..at]);
            rest = &rest[at + 1..];
            if let Some(after) = rest.strip_prefix('`') {
                name.push('`');
                rest = after;
            } else {
                return Some((name, rest));
            }
        }
    }
    let end = text.find(|c: char| !is_simple(c)).unwrap_or(text.len());
    if end == 0 {
        return None;
    }
    Some((text[..end].to_string(), &text[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_and_descriptors() {
        let symbol = Symbol::parse(
            "rust-analyzer cargo reqwest 0.12.28 async_impl/client/impl#[ClientBuilder]no_proxy().",
        )
        .unwrap();
        assert_eq!(symbol.package, "reqwest");
        assert_eq!(symbol.version, "0.12.28");
        assert_eq!(symbol.modules(), vec!["async_impl", "client"]);
        let item = symbol.item();
        assert_eq!(item.name, "no_proxy");
        assert_eq!(item.suffix, Suffix::Method);
        assert_eq!(item.owner, Some("ClientBuilder"));
        assert_eq!(item.trait_name, None);
        assert!(item.in_impl);
    }

    #[test]
    fn reads_trait_impls_and_backquoted_names() {
        let symbol = Symbol::parse(
            "rust-analyzer cargo core https://github.com/rust-lang/rust/library/core ops/arith/impl#[f64][`Mul<Self>`]mul().",
        )
        .unwrap();
        assert!(symbol.is_std());
        let item = symbol.item();
        assert_eq!(item.owner, Some("f64"));
        assert_eq!(item.trait_name, Some("Mul"));
        assert_eq!(item.name, "mul");

        let vec_len = Symbol::parse(
            "rust-analyzer cargo alloc https://github.com/rust-lang/rust/library/alloc vec/impl#[`Vec<T, A>`]len().",
        )
        .unwrap();
        assert_eq!(vec_len.item().owner, Some("Vec"));
    }

    #[test]
    fn items_of_every_shape() {
        let item = |text: &str| {
            let symbol = Symbol::parse(text).unwrap();
            let item = symbol.item();
            (
                item.name.to_string(),
                item.suffix,
                item.owner.map(str::to_string),
            )
        };
        assert_eq!(
            item("rust-analyzer cargo fx 0.1.0 Shape#area()."),
            ("area".into(), Suffix::Method, Some("Shape".into()))
        );
        assert_eq!(
            item("rust-analyzer cargo fx 0.1.0 Circle#r."),
            ("r".into(), Suffix::Term, Some("Circle".into()))
        );
        assert_eq!(
            item("rust-analyzer cargo fx 0.1.0 Token#Ident#"),
            ("Ident".into(), Suffix::Type, Some("Token".into()))
        );
        assert_eq!(
            item("rust-analyzer cargo fx 0.1.0 make_fn!"),
            ("make_fn".into(), Suffix::Macro, None)
        );
        assert_eq!(
            item("rust-analyzer cargo fx 0.1.0 other/"),
            ("other".into(), Suffix::Namespace, None)
        );
        assert_eq!(
            item("rust-analyzer cargo toml 1.1.6+spec-1.1.0 de/from_str()."),
            ("from_str".into(), Suffix::Method, None)
        );
    }

    #[test]
    fn locals_and_garbage_do_not_parse() {
        assert!(Symbol::parse("local 3").is_none());
        assert!(Symbol::parse("rust-analyzer cargo").is_none());
        assert!(Symbol::parse("rust-analyzer cargo fx 0.1.0 ").is_none());
        assert!(Symbol::parse("rust-analyzer cargo fx 0.1.0 a*b").is_none());
    }

    #[test]
    fn member_symbols_strip_to_their_owner() {
        assert_eq!(
            Symbol::text_without_last("rust-analyzer cargo fx 0.1.0 Shape#area()."),
            Some("rust-analyzer cargo fx 0.1.0 Shape#")
        );
    }

    #[test]
    fn type_heads_drop_generics_and_references() {
        assert_eq!(type_head("Vec<T, A>"), "Vec");
        assert_eq!(type_head("&'a str"), "str");
        assert_eq!(type_head("&mut Foo<T>"), "Foo");
        assert_eq!(type_head("Shape"), "Shape");
        assert_eq!(type_head("[T]"), "[T]");
    }
}
