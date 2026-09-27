use std::cell::RefCell;
use std::collections::HashSet;

use super::super::paths::resolve_import_path;
use crate::resolution::alias_binding::{LocalExportAlias, extract_local_export_aliases};
use crate::resolution::line_index::SourceMemo;
use crate::resolution::types::{ReExport, ResolutionContext};
use crate::types::{Language, Node, NodeKind};

/// Recursive depth cap for re-export chain following. Real codebases
/// rarely chain barrels more than 2–3 deep; 8 is a generous safety
/// net that still bounds worst-case work.
const REEXPORT_MAX_DEPTH: u32 = 8;

/// What [`find_exported_symbol`] is looking for (TS inline `want` object).
pub(super) struct WantedSymbol {
    pub(super) is_default: bool,
    pub(super) is_namespace: bool,
    pub(super) exported_name: String,
    pub(super) member_name: Option<String>,
}

/// Find an exported symbol in `file_path`, following `export { x } from
/// './other'` and `export * from './other'` chains until the original
/// declaration is reached. Cycle-safe via the `visited` set.
///
/// Without this, every barrel-style import (`import { Foo } from
/// './index'` where `index.ts` only re-exports) used to resolve to
/// nothing — the existing code only looked for declarations IN the
/// resolved file, not declarations the file forwarded.
pub(super) fn find_exported_symbol(
    file_path: &str,
    want: &WantedSymbol,
    language: Language,
    context: &dyn ResolutionContext,
    visited: &mut HashSet<String>,
    depth: u32,
) -> Option<Node> {
    if depth > REEXPORT_MAX_DEPTH {
        return None;
    }
    if visited.contains(file_path) {
        return None;
    }
    visited.insert(file_path.to_string());

    // 1. Direct hit: the symbol is declared in this file. Named lookups —
    // this runs per import hop of every reference.
    if want.is_default {
        let nodes_in_file = context.get_nodes_in_file(file_path);
        // Svelte/Vue single-file components ARE the module's default export,
        // but are extracted as kind 'component' (not function/class). Prefer
        // the component node; fall back to an exported function/class for the
        // `.ts`/`.tsx` `export default fn`/`class` case. Without the component
        // branch, an `export { default as X } from './X.svelte'` barrel never
        // resolves and the component shows a false 0 callers (#629).
        let direct = nodes_in_file
            .iter()
            .find(|n| n.is_exported == Some(true) && n.kind == NodeKind::Component)
            .or_else(|| {
                nodes_in_file.iter().find(|n| {
                    n.is_exported == Some(true)
                        && (n.kind == NodeKind::Function || n.kind == NodeKind::Class)
                })
            });
        if let Some(direct) = direct {
            return Some(direct.clone());
        }
    } else if want.is_namespace && want.member_name.as_deref().is_some_and(|m| !m.is_empty()) {
        // (TS `want.memberName` truthiness — an empty string falls through
        // to the exported-name branch below.)
        let member_name = want.member_name.as_deref().unwrap_or("");
        let direct = context
            .get_nodes_in_file_named(file_path, member_name)
            .into_iter()
            .find(|n| n.is_exported == Some(true));
        if direct.is_some() {
            return direct;
        }
    } else {
        let direct = context
            .get_nodes_in_file_named(file_path, &want.exported_name)
            .into_iter()
            .find(|n| n.is_exported == Some(true));
        if direct.is_some() {
            return direct;
        }
        // Local export clause: `export { realImpl as alias }` (no `from`).
        // `extractReExports` only models the `export ... from './other'` form,
        // so the exported name matches no `isExported` declaration - the
        // extractor never flagged the bare declaration exported. Bind the
        // clause's exported name to its local declaration so an importer asking
        // for the renamed name gets the real symbol instead of falling through
        // to the name-matcher (which cannot cross the rename). #1482 / PR #1485.
        if let Some(local) = resolve_local_export_alias(&want.exported_name, context, file_path) {
            return Some(local);
        }
    }

    // 2. Re-export hit: the file forwards the symbol to another module.
    let re_exports = context.get_re_exports(file_path, language);
    if re_exports.is_empty() {
        return None;
    }

    // Look for explicit `export { want } from './other'` (with optional rename).
    let target_name = if want.is_default {
        "default"
    } else {
        &want.exported_name
    };
    for rex in &re_exports {
        if let ReExport::Named {
            exported_name,
            original_name,
            source,
        } = rex
        {
            if exported_name == target_name {
                let Some(next) = resolve_import_path(source, file_path, language, context) else {
                    continue;
                };
                // After rename: `export { foo as bar } from './x'` — to chase
                // `bar`, we look for `foo` in `./x`.
                let chained = find_exported_symbol(
                    &next,
                    &WantedSymbol {
                        is_default: original_name == "default",
                        is_namespace: false,
                        exported_name: original_name.clone(),
                        member_name: None,
                    },
                    language,
                    context,
                    visited,
                    depth + 1,
                );
                if chained.is_some() {
                    return chained;
                }
            }
        }
    }

    // 3. Wildcard re-export: `export * from './other'` — try every
    //    forwarding source. This is the barrel-of-barrels case.
    for rex in &re_exports {
        if let ReExport::Wildcard { source } = rex {
            let Some(next) = resolve_import_path(source, file_path, language, context) else {
                continue;
            };
            let chained = find_exported_symbol(&next, want, language, context, visited, depth + 1);
            if chained.is_some() {
                return chained;
            }
        }
    }

    None
}

/// Bind a local `export { local as exported }` clause to its declaration.
///
/// Reads the file's local export clauses (see
/// [`extract_local_export_aliases`]) and, when one exports `exported_name`,
/// returns the matching declaration in the file - exported or not, since the
/// extractor never flags a bare declaration exported. Guarded to JS/TS-family
/// files (the only export-clause shape this understands). #1482 / PR #1485.
fn resolve_local_export_alias(
    exported_name: &str,
    context: &dyn ResolutionContext,
    file_path: &str,
) -> Option<Node> {
    thread_local! {
        /// Each file's export clauses, read once rather than per import hop.
        static ALIASES: RefCell<SourceMemo<Vec<LocalExportAlias>>> =
            const { RefCell::new(SourceMemo::new()) };
    }
    let content = context.read_file_arc(file_path)?;
    let aliases = ALIASES.with(|memo| {
        memo.borrow_mut().get_or_insert_with(&content, |text| {
            if text.is_empty() || !text.contains("export") {
                Vec::new()
            } else {
                extract_local_export_aliases(text)
            }
        })
    });
    let local_name = &aliases
        .iter()
        .find(|alias| alias.exported_name == exported_name)?
        .local_name;
    context
        .get_nodes_in_file_named(file_path, local_name)
        .into_iter()
        .next()
}
