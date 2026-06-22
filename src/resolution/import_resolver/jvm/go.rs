use crate::resolution::go_module::go_package_dir_for_import;
use crate::resolution::types::{
    ImportMapping,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::Language;

/// Resolve a Go cross-package qualified reference (`pkga.FuncX`) by matching
/// the package alias against an in-module import, stripping the module prefix
/// to a project-relative directory, and locating the exported symbol in any
/// `.go` file under that directory. Returns `None` for stdlib / third-party
/// imports (no `go.mod`-relative match) so the rest of `resolve_via_import`
/// can still try the file-based path.
pub(super) fn resolve_go_cross_package_reference(
    reference: &UnresolvedRef,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let module = context.get_go_module()?;

    // Qualified call: receiver before `.`, member after. A bare reference
    // (no dot) is a same-file/in-package call — handled elsewhere.
    let dot_idx = reference.reference_name.find('.')?;
    if dot_idx == 0 {
        return None;
    }
    let receiver = &reference.reference_name[..dot_idx];
    let member_name = &reference.reference_name[dot_idx + 1..];
    if member_name.is_empty() {
        return None;
    }

    for imp in imports {
        if imp.local_name != receiver {
            continue;
        }
        let Some(pkg_dir) =
            go_package_dir_for_import(module, &imp.source, context.get_project_root())
        else {
            continue;
        };

        // Look up the member by name and pick the candidate whose file lives
        // directly in the package directory. Match the immediate parent dir
        // exactly so a call to `pkga.FuncX` doesn't accidentally land on a
        // `FuncX` declared in `pkga/subpkg/`.
        let candidates = context.get_nodes_by_name(member_name);
        for node in &candidates {
            if node.language != Language::Go {
                continue;
            }
            if node.is_exported != Some(true) {
                continue;
            }
            let fp = node.file_path.replace('\\', "/");
            let file_dir = match fp.rfind('/') {
                Some(last_slash) => &fp[..last_slash],
                None => "",
            };
            if file_dir == pkg_dir {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: node.id.clone(),
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Import,
                });
            }
        }
    }
    None
}
