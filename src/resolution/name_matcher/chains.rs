use std::sync::LazyLock;

use regex::Regex;

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
            && matches!(node.kind, NodeKind::Class | NodeKind::Struct)
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
        Language::Java
        | Language::Kotlin
        | Language::Csharp
        | Language::Swift
        | Language::Go
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
            } else if reference.language == Language::Go {
                lookup_callee_return_type(inner, reference, context)
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
