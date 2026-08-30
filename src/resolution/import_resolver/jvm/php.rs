use crate::resolution::name_matcher::resolve_method_on_type;
use crate::resolution::types::{
    ImportMapping,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{Language, NodeKind};

/// Resolve a PHP reference whose receiver is an imported class name or
/// alias: `Settle::getSettlesToExcel(...)` where
/// `use App\Services\SettleService as Settle;`. The extractor emits the
/// call as `Settle.getSettlesToExcel`, and the import mapping binds the
/// local name (`Settle`) to the exported class (`SettleService`).
///
/// PHP qualified names drop the namespace (a method is indexed as
/// `SettleService::getSettlesToExcel`, not `App\Services\...`), so the
/// filesystem-path import lookup used for JS/TS can't follow the FQN
/// import. Without this the receiver is ignored and the method-name-only
/// fallback silently attributes the call to whichever same-named method
/// it finds first — routinely the wrong Service/Repository layer (#1545).
///
/// Resolves the receiver through the import mapping's `localName` first
/// and constrains the method lookup to that class; when several classes
/// share the class name across namespaces, the import's source FQN
/// path breaks the tie. Falls back to `None` (so name-matching still
/// runs) only when the receiver can't be resolved.
pub(super) fn resolve_php_imported_reference(
    reference: &UnresolvedRef,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.language != Language::Php || imports.is_empty() {
        return None;
    }

    // Only static/instance calls of the form `Receiver.method` carry a
    // receiver we can disambiguate; a bare name has no receiver.
    let dot = reference.reference_name.find('.')?;
    let receiver = &reference.reference_name[..dot];
    let member = &reference.reference_name[dot + 1..];
    if receiver.is_empty() || member.is_empty() || member.contains('.') {
        return None;
    }

    for imp in imports {
        if imp.local_name != receiver {
            continue;
        }

        // The imported class name (`SettleService`), stripped of its
        // namespace. `exported_name` already holds the class basename;
        // fall back to the source FQN's tail for safety.
        let class_name = if imp.exported_name.is_empty() {
            imp.source.rsplit('\\').next().unwrap_or(&imp.source)
        } else {
            imp.exported_name.as_str()
        };

        // The source FQN's directory path disambiguates same-named
        // classes living in different namespaces (`App\Services\Foo`
        // vs `App\Repositories\Foo`). Convert `\` to `/` and match the
        // candidate file by suffix — PSR-4 root casing may differ, so a
        // suffix ending in `Services/Foo.php` still identifies the file.
        let fqn_path = format!("{}.php", imp.source.replace('\\', "/"));
        let preferred = choose_by_fqn_path(context, class_name, member, &fqn_path);
        if let Some(resolved) = preferred {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: resolved,
                confidence: 0.9,
                resolved_by: ResolvedBy::Import,
            });
        }

        // Single class name match (no cross-namespace ambiguity): bind
        // the method to `<class>::<member>` via the shared helper.
        if let Some(resolved) = resolve_method_on_type(
            class_name,
            member,
            reference,
            context,
            0.9,
            ResolvedBy::Import,
            None,
        ) {
            return Some(resolved);
        }
    }

    None
}

/// When multiple methods share `<class>::<member>` across namespaces,
/// pick the one whose file path matches the import's source FQN path.
fn choose_by_fqn_path(
    context: &dyn ResolutionContext,
    class_name: &str,
    member: &str,
    fqn_path: &str,
) -> Option<String> {
    let want = format!("{class_name}::{member}");
    let want_suffix = format!("::{want}");
    let candidates = context.get_nodes_by_name(member);
    let matches: Vec<_> = candidates
        .iter()
        .filter(|m| {
            m.kind == NodeKind::Method
                && m.language == Language::Php
                && (m.qualified_name == want || m.qualified_name.ends_with(&want_suffix))
        })
        .collect();
    if matches.len() < 2 {
        return None;
    }
    matches
        .iter()
        .find(|m| {
            let fp = m.file_path.replace('\\', "/");
            fp.ends_with(fqn_path) || fp.ends_with(&format!("/{fqn_path}"))
        })
        .map(|m| m.id.clone())
}
