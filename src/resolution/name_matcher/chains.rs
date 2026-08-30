use std::sync::LazyLock;

use regex::Regex;

use super::exact::match_by_exact_name;
use super::fuzzy::match_fuzzy;
use super::receiver::{infer_cpp_receiver_type, resolve_method_on_type};
use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Language, NodeKind};

static CALL_CHAIN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+)\(\)\.([A-Za-z_][0-9A-Za-z_]*)$").expect("valid call-chain regex")
});
static CPP_MAKE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|::)(?:make_unique|make_shared)\s*<\s*([A-Za-z_]\w*)")
        .expect("valid C++ make regex")
});

fn imported_fqn(
    type_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if !matches!(reference.language, Language::Java | Language::Kotlin) {
        return None;
    }
    context
        .get_import_mappings(&reference.file_path, reference.language)
        .into_iter()
        .find(|mapping| mapping.local_name == type_name)
        .map(|mapping| mapping.source)
}

/// Go: the declared return type of a package-qualified factory — `pkg.Factory()`.
///
/// Go package-level functions are indexed with a BARE `qualified_name`
/// (`Order`, not `service.Order`), so the `Class::method` lookup that serves
/// the dot-notation languages can never match `service::Order`. A dotted prefix
/// at a Go call site is a PACKAGE qualifier, not a receiver type (Go has no
/// `Class.staticMethod()` form), so resolve it as one: among the package-level
/// functions sharing the factory's name, prefer those declared in the
/// qualifying package's directory. When several plausible candidates disagree
/// on their return type the result is `None`, so a guess produces no edge
/// rather than a wrong one (#750).
fn lookup_go_package_func_return_type(
    pkg: &str,
    func_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let candidates: Vec<crate::types::Node> = context
        .get_nodes_by_name(func_name)
        .into_iter()
        .filter(|node| {
            node.kind == NodeKind::Function
                && node.language == Language::Go
                && node.return_type.is_some()
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // `pkg` is the name at the CALL SITE, which an alias detaches from the
    // directory (`ctrlcart "app/internal/controller/order/cart"`). Map it back
    // through the file's imports; a plain import maps to itself. A package whose
    // name differs from its directory (legal) is covered too — the import PATH
    // is what's matched, never the package clause.
    let import_path = context
        .get_import_mappings(&reference.file_path, reference.language)
        .into_iter()
        .find(|mapping| mapping.local_name == pkg)
        .map(|mapping| mapping.source)
        .unwrap_or_else(|| pkg.to_string());
    // Go requires one package per directory, so the import path's tail IS the
    // declaring directory. Match the longest tail available — the candidate's
    // full directory path — which separates same-named packages under different
    // parents.
    let by_dir: Vec<&crate::types::Node> = candidates
        .iter()
        .filter(|node| {
            let dir = go_dir_of(&node.file_path);
            !dir.is_empty() && (import_path == dir || import_path.ends_with(&format!("/{dir}")))
        })
        .collect();
    let pool: Vec<&crate::types::Node> = if by_dir.is_empty() {
        candidates.iter().collect()
    } else {
        by_dir
    };
    let types: std::collections::HashSet<&str> = pool
        .iter()
        .filter_map(|node| node.return_type.as_deref())
        .collect();
    if types.len() == 1 {
        pool[0].return_type.clone()
    } else {
        None
    }
}

/// The directory path a file lives in — the package scope for Go.
fn go_dir_of(file_path: &str) -> String {
    let normalized = file_path.replace('\\', "/");
    match normalized.rfind('/') {
        Some(idx) if idx > 0 => normalized[..idx].to_string(),
        _ => String::new(),
    }
}

fn lookup_callee_return_type(
    callee: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let mut parts: Vec<&str> = callee.split("::").filter(|part| !part.is_empty()).collect();
    let method = parts.pop().unwrap_or(callee);
    let class = (!parts.is_empty()).then(|| parts.join("::"));
    let candidates = context.get_nodes_by_name(method);
    let candidates = candidates.iter().filter(|node| {
        matches!(node.kind, NodeKind::Method | NodeKind::Function)
            && node.language == reference.language
            && node.return_type.is_some()
    });

    if let Some(class) = class {
        let wanted = format!("{class}::{method}");
        return candidates
            .filter(|node| {
                node.qualified_name == wanted
                    || node.qualified_name.ends_with(&format!("::{wanted}"))
                    || wanted.ends_with(&format!("::{}", node.qualified_name))
            })
            .find_map(|node| node.return_type.clone());
    }
    candidates
        .filter(|node| node.kind == NodeKind::Function)
        .find_map(|node| node.return_type.clone())
}

fn class_exists(
    type_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> bool {
    let simple = type_name
        .split("::")
        .filter(|part| !part.is_empty())
        .last()
        .unwrap_or(type_name);
    context.get_nodes_by_name(simple).into_iter().any(|node| {
        node.language == reference.language
            && matches!(
                node.kind,
                NodeKind::Class | NodeKind::Struct | NodeKind::Union
            )
    })
}

fn cpp_call_result_type(
    inner: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let expression = inner.trim();
    if let Some(name) = CPP_MAKE_RE
        .captures(expression)
        .and_then(|captures| captures.get(1))
    {
        return Some(name.as_str().to_string());
    }

    if let Some((receiver, method)) = expression.rsplit_once('.') {
        if !receiver.chars().any(|ch| matches!(ch, '.' | '(' | ':')) {
            let receiver_type = infer_cpp_receiver_type(receiver, reference, context)?;
            return lookup_callee_return_type(
                &format!("{receiver_type}::{method}"),
                reference,
                context,
            );
        }
    }

    if let Some(return_type) = lookup_callee_return_type(expression, reference, context) {
        return Some(return_type);
    }
    class_exists(expression, reference, context).then(|| {
        expression
            .split("::")
            .filter(|part| !part.is_empty())
            .last()
            .unwrap_or(expression)
            .to_string()
    })
}

/// Go: resolve a re-encoded chain `<inner>().<method>`.
///
/// A dotted inner (`pkg.Factory`) is a PACKAGE-qualified factory: the dotted
/// prefix is a package qualifier, not a receiver type (Go has no
/// `Class.staticMethod()` form), and package-level functions carry a BARE
/// `qualified_name` (`Order`, not `service.Order`), so the `Class::method`
/// lookup the dot-notation languages use can never match it. Resolve the
/// factory as a package-level function and VALIDATE the method on its declared
/// return type. An interface return lands on the interface's method, which the
/// dynamic-dispatch pass bridges to the implementation. A package qualifier the
/// file doesn't import, or candidates disagreeing on their return type, yields
/// no edge rather than a guess (#1640/#750).
///
/// A bare inner (`New`) is a package-level factory FUNCTION whose declared
/// return type is the receiver's type. When that return type isn't recoverable
/// (typically a package-level VARIABLE holding a function value), fall back to
/// bare-name resolution of the method so a re-encoded ref never DROPS an edge
/// the un-re-encoded bare path would have found. When `inner` IS a real factory
/// but the method is absent on its return type, the return type is recovered
/// and `resolve_method_on_type` yields no edge — the absent-method safety
/// guarantee is preserved.
fn match_go_call_chain(
    inner: &str,
    outer_method: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if let Some((receiver, factory_method)) = inner.rsplit_once('.') {
        let pkg = receiver.split('.').rfind(|part| !part.is_empty())?;
        let receiver_type =
            lookup_go_package_func_return_type(pkg, factory_method, reference, context)?;
        let preferred = imported_fqn(&receiver_type, reference, context);
        return resolve_method_on_type(
            &receiver_type,
            outer_method,
            reference,
            context,
            0.85,
            ResolvedBy::InstanceMethod,
            preferred.as_deref(),
        );
    }
    // Bare package-level factory `New().Method()`.
    if let Some(receiver_type) = lookup_callee_return_type(inner, reference, context) {
        let preferred = imported_fqn(&receiver_type, reference, context);
        return resolve_method_on_type(
            &receiver_type,
            outer_method,
            reference,
            context,
            0.85,
            ResolvedBy::InstanceMethod,
            preferred.as_deref(),
        );
    }
    // Return type not recoverable: fall back to bare-name resolution of the
    // method, but tie the match to the ORIGINAL re-encoded ref so the batched
    // resolver's cleanup can clear the stored row.
    let bare_ref = UnresolvedRef {
        reference_name: outer_method.to_string(),
        ..reference.clone()
    };
    let bare_match =
        match_by_exact_name(&bare_ref, context).or_else(|| match_fuzzy(&bare_ref, context))?;
    Some(ResolvedRef {
        original: reference.clone(),
        ..bare_match
    })
}

pub(super) fn match_call_chain(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let captures = CALL_CHAIN_RE.captures(&reference.reference_name)?;
    let inner = captures.get(1)?.as_str();
    let outer_method = captures.get(2)?.as_str();

    let receiver_type = match reference.language {
        Language::C | Language::Cpp => cpp_call_result_type(inner, reference, context),
        Language::Php | Language::Rust if inner.contains("::") => {
            let factory_class = inner.rsplit_once("::")?.0;
            lookup_callee_return_type(inner, reference, context).map(|return_type| {
                if return_type == "self" {
                    factory_class.to_string()
                } else {
                    return_type
                }
            })
        }
        Language::Go => return match_go_call_chain(inner, outer_method, reference, context),
        Language::Java
        | Language::Kotlin
        | Language::Csharp
        | Language::Swift
        | Language::Scala
        | Language::Dart
        | Language::Objc
        | Language::Pascal => {
            if let Some((receiver, factory_method)) = inner.rsplit_once('.') {
                let factory_class = receiver.split('.').rfind(|part| !part.is_empty())?;
                lookup_callee_return_type(
                    &format!("{factory_class}::{factory_method}"),
                    reference,
                    context,
                )
                .or_else(|| {
                    ((reference.language == Language::Objc
                        && factory_class
                            .chars()
                            .next()
                            .is_some_and(|ch| ch.is_ascii_uppercase()))
                        || (reference.language == Language::Pascal
                            && matches!(factory_class.chars().next(), Some('T' | 'I'))))
                    .then(|| factory_class.to_string())
                })
            } else if matches!(
                reference.language,
                Language::Kotlin
                    | Language::Swift
                    | Language::Scala
                    | Language::Dart
                    | Language::Pascal
            ) && inner
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_uppercase())
            {
                Some(inner.to_string())
            } else {
                None
            }
        }
        _ => None,
    }?;

    let preferred = imported_fqn(&receiver_type, reference, context);
    resolve_method_on_type(
        &receiver_type,
        outer_method,
        reference,
        context,
        0.85,
        ResolvedBy::InstanceMethod,
        preferred.as_deref(),
    )
}
