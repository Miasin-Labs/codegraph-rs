use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Language, Node, NodeKind};

/// Resolve `<typeName>::<methodName>` against indexed method nodes.
///
/// `preferred_fqn`: optional FQN that identifies WHICH class declaration
/// `type_name` refers to in the caller's file. When multiple candidates
/// share the same qualifiedName (`FooConverter::convert` in both
/// `dao/converter/` and `service/converter/`), the FQN's file-path-suffix
/// picks the right one — the disambiguation signal Java imports carry but
/// the call site doesn't (#314).
pub(in crate::resolution::name_matcher) fn resolve_method_on_type(
    type_name: &str,
    method_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    confidence: f64,
    resolved_by: ResolvedBy,
    preferred_fqn: Option<&str>,
) -> Option<ResolvedRef> {
    // Look up methods by name and match by qualifiedName ending in
    // `<typeName>::<methodName>`. This works whether the method is defined
    // in-class (`class Foo { int bar() { ... } }`) or out-of-line in a separate
    // file (`int Foo::bar() { ... }` in foo.cpp while class Foo is in foo.hpp).
    // The previous same-file approach missed the latter — the typical C++ layout.
    let method_candidates = context.get_nodes_by_name(method_name);
    let want = format!("{type_name}::{method_name}");
    let want_suffix = format!("::{want}");
    let matches: Vec<&Node> = method_candidates
        .iter()
        .filter(|m| {
            m.kind == NodeKind::Method
                && m.language == reference.language
                && (m.qualified_name == want || m.qualified_name.ends_with(&want_suffix))
        })
        .collect();
    if matches.is_empty() {
        return None;
    }

    if matches.len() > 1 {
        if let Some(fqn) = preferred_fqn {
            let ext = if reference.language == Language::Kotlin {
                ".kt"
            } else {
                ".java"
            };
            let fqn_path = format!("{}{}", fqn.replace('.', "/"), ext);
            let chosen = matches.iter().find(|m| {
                let fp = m.file_path.replace('\\', "/");
                fp.ends_with(&fqn_path) || fp.ends_with(&format!("/{fqn_path}"))
            });
            if let Some(chosen) = chosen {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: chosen.id.clone(),
                    confidence,
                    resolved_by,
                });
            }
        }
    }

    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: matches[0].id.clone(),
        confidence,
        resolved_by,
    })
}
