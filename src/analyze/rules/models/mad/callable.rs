//! The callable a MaD row names, per CodeQL's per-language spelling:
//!
//! - Java/C#/Go/C++ tuples: package (namespace), type, subtypes, name,
//!   signature (its parameter count is the arity), `ext`;
//! - Rust canonical paths: `std::env::var`, `<std::process::Command>::new`,
//!   `<alloc::vec::Vec as core::convert::From>::from`,
//!   `<_ as core::iter::Iterator>::map` (a trait's method: any implementor);
//! - Python/JS API-graph paths: a root type (`os`, `zipfile.ZipFile` —
//!   instances, `zipfile.ZipFile!` — the class, `asyncpg.~Connection` — a
//!   named type) followed by `Member[x]`, `Call`, `Subclass`,
//!   `WithArity[n]`… up to the position.

use super::access::Token;

/// A callable as models name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callable {
    pub namespace: String,
    pub type_name: String,
    pub subtypes: bool,
    pub name: String,
    pub arity: Option<u8>,
}

/// `java.util.Map<K,V>` → `java.util.Map`: generic arguments dropped.
fn strip_generics(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Parameters in a signature `(String,Properties)`, `()`; `None` for no
/// signature.
pub fn arity(signature: &str) -> Option<u8> {
    let signature = signature.trim();
    let inner = signature.strip_prefix('(')?.strip_suffix(')')?;
    let inner = strip_generics(inner);
    if inner.trim().is_empty() {
        return Some(0);
    }
    Some(inner.split(',').count().min(u8::MAX as usize) as u8)
}

/// A Java/C#/Go/C++ tuple's callable.
pub fn java_like(
    package: &str,
    type_name: &str,
    subtypes: &str,
    name: &str,
    signature: &str,
    ext: &str,
) -> Result<Callable, String> {
    if !ext.trim().is_empty() {
        return Err(format!("`ext` qualifier `{}`", ext.trim()));
    }
    let name = name.trim();
    if name.is_empty() {
        return Err("no method name (a whole-type model)".into());
    }
    let bare = strip_generics(type_name);
    // `Map$Entry` (Java), `Outer::Inner` (C++): the simple name.
    let simple = bare
        .rsplit(['$', '.'])
        .next()
        .unwrap_or(&bare)
        .rsplit("::")
        .next()
        .unwrap_or_default()
        .to_string();
    Ok(Callable {
        namespace: package.trim().to_string(),
        type_name: simple,
        subtypes: matches!(subtypes.trim(), "true" | "True" | "TRUE"),
        name: strip_generics(name),
        arity: arity(signature),
    })
}

/// Split `a::b::c` at `::` outside `<…>`.
fn path_segments(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let mut chars = path.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '<' => {
                depth += 1;
                current.push(ch);
            }
            '>' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ':' if depth == 0 && chars.peek() == Some(&':') => {
                chars.next();
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    out.push(current);
    out.into_iter()
        .map(|s| strip_generics(&s))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether a Rust type as written is a plain path (`std::fs::File`), not
/// a reference, slice, tuple, pointer, trait object or generic parameter.
fn is_type_path(text: &str) -> bool {
    let segments = path_segments(text);
    let Some(last) = segments.last() else {
        return false;
    };
    // A lone single-letter or `_` is a generic parameter.
    if segments.len() == 1 && (last == "_" || last.len() == 1) {
        return false;
    }
    text.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '<' | '>' | ',' | ' '))
        && !text.starts_with("dyn ")
        && !text.starts_with("impl ")
}

/// A Rust canonical path's callable.
pub fn rust(path: &str) -> Result<Callable, String> {
    let path = path.trim();
    if let Some(rest) = path.strip_prefix('<') {
        // `<Self [as Trait]>::name`: find the `>` closing the first `<`.
        let mut depth = 1usize;
        let mut close = None;
        for (i, ch) in rest.char_indices() {
            match ch {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close.ok_or("unbalanced `<`")?;
        let inner = &rest[..close];
        let name = rest[close + 1..]
            .strip_prefix("::")
            .ok_or("no method after the type")?
            .trim()
            .to_string();
        if name.is_empty() || name.contains("::") {
            return Err(format!("method `{name}` is not a name"));
        }
        let (self_ty, trait_path) = match split_as(inner) {
            Some((self_ty, trait_path)) => (self_ty, Some(trait_path)),
            None => (inner, None),
        };
        let self_ty = self_ty
            .trim()
            .trim_start_matches('&')
            .trim_start_matches("mut ")
            .trim();
        if is_type_path(self_ty) {
            let mut segments = path_segments(self_ty);
            let type_name = segments.pop().unwrap_or_default();
            return Ok(Callable {
                namespace: segments.join("::"),
                type_name,
                subtypes: false,
                name,
                arity: None,
            });
        }
        if let Some(trait_path) = trait_path {
            let mut segments = path_segments(trait_path.trim());
            let type_name = segments.pop().ok_or("empty trait path")?;
            return Ok(Callable {
                namespace: segments.join("::"),
                type_name,
                subtypes: true,
                name,
                arity: None,
            });
        }
        return Err("impl for a slice, pointer or other non-path type".into());
    }
    let mut segments = path_segments(path);
    let name = segments.pop().ok_or("empty path")?;
    if segments.is_empty() {
        return Err(format!("`{name}` has no crate"));
    }
    Ok(Callable {
        namespace: segments.join("::"),
        type_name: String::new(),
        subtypes: false,
        name,
        arity: None,
    })
}

/// `A as B` split at the top-level ` as `.
fn split_as(inner: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let bytes = inner.as_bytes();
    for i in 0..bytes.len() {
        match bytes[i] {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b' ' if depth == 0 && inner[i..].starts_with(" as ") => {
                return Some((&inner[..i], &inner[i + 4..]));
            }
            _ => {}
        }
    }
    None
}

/// An API-graph root type: module path, type name, whether it names the
/// class itself (`!`) rather than its instances.
struct Root {
    module: Vec<String>,
    type_name: Option<String>,
    class_object: bool,
}

fn api_root(root: &str) -> Root {
    let root = root.trim();
    let class_object = root.ends_with('!');
    let root = root.trim_end_matches('!');
    if let Some((module, synthetic)) = root.rsplit_once(".~") {
        return Root {
            module: vec![module.to_string()],
            type_name: Some(synthetic.to_string()),
            class_object,
        };
    }
    if let Some(synthetic) = root.strip_prefix('~') {
        return Root {
            module: Vec::new(),
            type_name: Some(synthetic.to_string()),
            class_object,
        };
    }
    // `zipfile.ZipFile`: a capitalized last segment is a class; a scoped
    // npm package (`@x/y`) is one module.
    let (module, last) = match root.rsplit_once('.') {
        Some((module, last)) if !root.starts_with('@') || module.contains('/') => {
            (module, Some(last))
        }
        _ => (root, None),
    };
    match last {
        Some(last) if class_object || last.chars().next().is_some_and(char::is_uppercase) => Root {
            module: vec![module.to_string()],
            type_name: Some(last.to_string()),
            class_object,
        },
        Some(last) => Root {
            module: vec![module.to_string(), last.to_string()],
            type_name: None,
            class_object: false,
        },
        None => Root {
            module: vec![root.to_string()],
            type_name: None,
            class_object: false,
        },
    }
}

/// The callables an API-graph `root` + callable `tokens` name (a
/// `Member[a,b]` names two). `Err` when the path reaches the callable
/// through something codegraph cannot follow.
pub fn api_graph(root: &str, tokens: &[Token]) -> Result<Vec<Callable>, String> {
    let root = api_root(root);
    // Each alternative: (module path, members after it).
    let mut alternatives: Vec<Vec<String>> = vec![Vec::new()];
    let mut subtypes = false;
    let mut arity: Option<u8> = None;
    let mut called = false;
    for token in tokens {
        match token.name {
            "Member" | "Method" => {
                if called {
                    return Err("reached through another call's result (API-graph chain)".into());
                }
                let names: Vec<&str> = token
                    .arg
                    .unwrap_or_default()
                    .split(',')
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .collect();
                if names.is_empty() {
                    return Err("empty `Member[]`".into());
                }
                alternatives = alternatives
                    .into_iter()
                    .flat_map(|path| {
                        names.iter().map(move |name| {
                            let mut path = path.clone();
                            path.push(name.to_string());
                            path
                        })
                    })
                    .collect();
            }
            "Subclass" => subtypes = true,
            "Instance" | "Awaited" => {}
            "Call" => called = true,
            "WithArity" => {
                arity = token
                    .arg
                    .and_then(|a| a.split(',').next())
                    .and_then(|a| a.trim().parse().ok());
            }
            "ReturnValue" | "Argument" | "Parameter" => {
                return Err("reached through another call's result (API-graph chain)".into());
            }
            "AnyMember" => return Err("any member of a value".into()),
            "WithStringArgument" => {
                return Err("selected by a string argument's value".into());
            }
            other => return Err(format!("callable path token `{other}`")),
        }
    }
    let mut out = Vec::new();
    for members in alternatives {
        let callable = match (&root.type_name, members.split_last()) {
            // `X!` + `Call`: the class is constructed.
            (Some(type_name), None) if root.class_object && called => Callable {
                namespace: root.module.join("."),
                type_name: type_name.clone(),
                subtypes,
                name: type_name.clone(),
                arity,
            },
            (Some(type_name), Some((name, []))) => Callable {
                namespace: root.module.join("."),
                type_name: type_name.clone(),
                subtypes,
                name: name.clone(),
                arity,
            },
            (Some(_), Some(_)) => {
                return Err("a member of a member of a type (API-graph chain)".into());
            }
            (None, Some((name, rest))) => {
                let mut namespace = root.module.clone();
                namespace.extend(rest.iter().cloned());
                Callable {
                    namespace: namespace.join("."),
                    type_name: String::new(),
                    subtypes,
                    name: name.clone(),
                    arity,
                }
            }
            // A module that is itself a function (`require('open')(url)`).
            (None, None) if called || !root.module.is_empty() => Callable {
                namespace: root.module.join("."),
                type_name: String::new(),
                subtypes,
                name: String::new(),
                arity,
            },
            (_, None) => return Err("a type that is itself called".into()),
        };
        out.push(callable);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::access::tokens;
    use super::*;

    #[test]
    fn java_tuples() {
        let c = java_like("java.sql", "Statement", "True", "executeQuery", "", "").unwrap();
        assert_eq!(c.type_name, "Statement");
        assert!(c.subtypes);
        assert_eq!(c.arity, None);
        let c = java_like("java.util", "Map$Entry", "true", "getValue", "()", "").unwrap();
        assert_eq!(c.type_name, "Entry");
        assert_eq!(c.arity, Some(0));
        let c = java_like(
            "java.sql",
            "DriverManager",
            "False",
            "getConnection",
            "(String,String,String)",
            "",
        )
        .unwrap();
        assert_eq!(c.arity, Some(3));
        assert!(!c.subtypes);
        assert_eq!(arity("(Map<String,Object>,int)"), Some(2));
        assert!(java_like("a", "B", "true", "c", "", "Annotated").is_err());
    }

    #[test]
    fn rust_paths() {
        let c = rust("std::env::var").unwrap();
        assert_eq!((c.namespace.as_str(), c.name.as_str()), ("std::env", "var"));
        assert!(c.type_name.is_empty());
        let c = rust("<std::process::Command>::new").unwrap();
        assert_eq!(c.namespace, "std::process");
        assert_eq!(c.type_name, "Command");
        assert_eq!(c.name, "new");
        let c = rust("<alloc::vec::Vec as core::convert::From>::from").unwrap();
        assert_eq!(c.type_name, "Vec");
        assert!(!c.subtypes);
        let c = rust("<_ as core::iter::traits::iterator::Iterator>::map").unwrap();
        assert_eq!(c.type_name, "Iterator");
        assert_eq!(c.namespace, "core::iter::traits::iterator");
        assert!(c.subtypes);
        assert!(rust("<& as reqwest::into_url::IntoUrlSealed>::as_str").is_ok());
        assert!(rust("<[_]>::len").is_err());
    }

    #[test]
    fn api_graph_paths() {
        let c = api_graph("os", &tokens("Member[getenv]")).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].namespace.as_str(), c[0].name.as_str()),
            ("os", "getenv")
        );
        let c = api_graph("urllib", &tokens("Member[parse].Member[quote,unquote]")).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].namespace, "urllib.parse");
        assert_eq!(c[1].name, "unquote");
        let c = api_graph("zipfile.ZipFile", &tokens("Member[extractall]")).unwrap();
        assert_eq!(c[0].type_name, "ZipFile");
        assert_eq!(c[0].namespace, "zipfile");
        let c = api_graph("email.header.Header!", &tokens("Subclass.Call")).unwrap();
        assert_eq!(c[0].name, "Header");
        assert!(c[0].subtypes);
        let c = api_graph("asyncpg.~Connection", &tokens("Member[execute]")).unwrap();
        assert_eq!(c[0].type_name, "Connection");
        assert!(
            api_graph(
                "readline",
                &tokens("Member[createInterface].ReturnValue.Member[question]")
            )
            .is_err()
        );
    }
}
