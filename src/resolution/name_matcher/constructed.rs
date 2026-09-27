//! A method called on an object constructed in place: `new X(…).m()`,
//! `(new X()).m()` (Java, Kotlin, C#, JS/TS, PHP). The receiver's type is
//! the class named after `new`, so the call runs `X::m` — the one method
//! `m` of the project class `X`, preferring the caller's own file (inner
//! classes of the same name in many files) and then the class the file
//! imports. Several candidates left, or none, resolve to nothing: never a
//! guess.

use std::sync::LazyLock;

use regex::Regex;

use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Language, Node, NodeKind};

/// `(new a.b.X<T>(args)).m` → (`a.b.X<T>`, `m`).
static NEW_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\(*\s*new\s+([A-Za-z_$][\w$.]*(?:<.*>)?)\s*\(.*\)\s*\)*\s*\.\s*([A-Za-z_$][\w$]*)$",
    )
    .expect("valid new-call regex")
});

fn applies(language: Language) -> bool {
    matches!(
        language,
        Language::Java
            | Language::Kotlin
            | Language::Csharp
            | Language::Javascript
            | Language::Typescript
            | Language::Tsx
            | Language::Jsx
            | Language::Php
    )
}

/// The simple name of a written type (`a.b.X<T>` → `X`).
fn simple(type_text: &str) -> &str {
    let bare = type_text.split('<').next().unwrap_or(type_text).trim();
    bare.rsplit(['.', '\\']).next().unwrap_or(bare)
}

pub(super) fn match_constructed_call(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if !applies(reference.language) {
        return None;
    }
    let captures = NEW_CALL_RE.captures(&reference.reference_name)?;
    let class = simple(captures.get(1)?.as_str());
    let method = captures.get(2)?.as_str();
    if class.is_empty() {
        return None;
    }
    let suffix = format!("::{class}::{method}");
    let exact = format!("{class}::{method}");
    let candidates: Vec<Node> = context
        .get_nodes_by_name(method)
        .into_iter()
        .filter(|node| {
            node.kind == NodeKind::Method
                && node.language == reference.language
                && (node.qualified_name == exact || node.qualified_name.ends_with(&suffix))
        })
        .collect();
    let chosen: Vec<&Node> = match candidates.len() {
        0 => return None,
        1 => candidates.iter().collect(),
        _ => {
            let same_file: Vec<&Node> = candidates
                .iter()
                .filter(|node| node.file_path == reference.file_path)
                .collect();
            if same_file.is_empty() {
                let imported = imported_class_path(class, reference, context);
                candidates
                    .iter()
                    .filter(|node| {
                        imported.as_deref().is_some_and(|path| {
                            let file = node.file_path.replace('\\', "/");
                            file.ends_with(path)
                        })
                    })
                    .collect()
            } else {
                same_file
            }
        }
    };
    // Overloads of one class are one method to the call graph; distinct
    // classes of the same name are a guess.
    let first = chosen.first()?;
    let owner = |node: &Node| {
        node.qualified_name
            .rsplit_once("::")
            .map(|(owner, _)| owner.to_string())
    };
    if chosen.iter().any(|node| owner(node) != owner(first)) {
        return None;
    }
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: first.id.clone(),
        confidence: 0.9,
        resolved_by: ResolvedBy::InstanceMethod,
    })
}

/// The file path a Java/Kotlin import of `class` points at
/// (`a/b/X.java`).
fn imported_class_path(
    class: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let ext = match reference.language {
        Language::Java => ".java",
        Language::Kotlin => ".kt",
        _ => return None,
    };
    context
        .get_import_mappings(&reference.file_path, reference.language)
        .into_iter()
        .find(|mapping| mapping.local_name == class)
        .map(|mapping| format!("{}{ext}", mapping.source.replace('.', "/")))
}

#[cfg(test)]
mod tests {
    use super::NEW_CALL_RE;

    #[test]
    fn the_constructed_class_and_method_are_read() {
        for (text, class, method) in [
            ("(new CWE89_51b()).badSink", "CWE89_51b", "badSink"),
            ("new Test().doSomething", "Test", "doSomething"),
            (
                "new a.b.Thing<String>(x, y(z)).run",
                "a.b.Thing<String>",
                "run",
            ),
        ] {
            let captures = NEW_CALL_RE.captures(text).expect(text);
            assert_eq!(&captures[1], class);
            assert_eq!(&captures[2], method);
        }
        assert!(NEW_CALL_RE.captures("thing.doSomething").is_none());
        assert!(NEW_CALL_RE.captures("newThing().run").is_none());
    }
}
