use std::collections::HashSet;

use super::super::paths::resolve_import_path;
use super::re_exports::{WantedSymbol, find_exported_symbol};
use crate::resolution::types::{
    ImportMapping,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};

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
            let Some(resolved_path) = resolve_import_path(
                &imp.source,
                &reference.file_path,
                reference.language,
                context,
            ) else {
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
