use std::collections::HashSet;

use super::super::paths::resolve_import_path;
use super::python::find_python_module_file;
use super::re_exports::{WantedSymbol, find_exported_symbol};
use crate::resolution::name_matcher::{
    infer_receiver_type_from_declaration,
    resolve_method_on_type,
};
use crate::resolution::types::{
    ImportMapping,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{EdgeKind, Language, NodeKind};

pub(super) fn resolve_path_import_reference(
    reference: &UnresolvedRef,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // Check if the reference name matches any import
    for imp in imports {
        if imp.local_name == reference.reference_name
            || reference
                .reference_name
                .starts_with(&format!("{}.", imp.local_name))
        {
            let mut resolved_path = resolve_import_path(
                &imp.source,
                &reference.file_path,
                reference.language,
                context,
            );

            // Python ABSOLUTE dotted source (`from scripts.lire_fec import
            // summarize as fec_summary`, source `scripts.lire_fec`):
            // `resolve_import_path` only maps RELATIVE dotted paths (`.mod`,
            // `..pkg.mod`) for Python and returns None here, so the whole
            // block used to no-op and drop the call edge for a directly-called
            // (non-member, non-namespace) name imported through an absolute
            // module. `resolve_python_receiver` / `resolve_module_import_to_file`
            // already carry this fallback for the qualified-member and
            // whole-module cases (#578); this loop needs it too for a bare
            // aliased-function call (#1518).
            if resolved_path.is_none() && reference.language == Language::Python {
                resolved_path = find_python_module_file(&imp.source, reference, context);
            }

            let Some(resolved_path) = resolved_path else {
                continue;
            };

            let exported_name = if imp.is_default {
                "default".to_string()
            } else {
                imp.exported_name.clone()
            };
            let member_name = if imp.is_namespace {
                Some(
                    reference
                        .reference_name
                        .replacen(&format!("{}.", imp.local_name), "", 1),
                )
            } else {
                None
            };

            let want = WantedSymbol {
                is_default: imp.is_default,
                is_namespace: imp.is_namespace,
                exported_name,
                member_name,
            };
            let mut visited: HashSet<String> = HashSet::new();
            let target_node = find_exported_symbol(
                &resolved_path,
                &want,
                reference.language,
                context,
                &mut visited,
                0,
            );

            if let Some(target_node) = target_node {
                if reference.language == Language::Python
                    && reference.reference_kind == EdgeKind::Calls
                    && matches!(target_node.kind, NodeKind::Variable | NodeKind::Constant)
                {
                    let member = reference.reference_name[imp.local_name.len() + 1..]
                        .split('.')
                        .next()?;
                    if let Some(type_name) =
                        infer_receiver_type_from_declaration(&target_node, context)
                    {
                        if let Some(resolved) = resolve_method_on_type(
                            &type_name,
                            member,
                            reference,
                            context,
                            0.85,
                            ResolvedBy::InstanceMethod,
                            None,
                        ) {
                            return Some(resolved);
                        }
                    }
                }
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: target_node.id,
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Import,
                });
            }
        }
    }

    None
}
