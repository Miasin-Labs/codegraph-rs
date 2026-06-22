use super::super::paths::resolve_import_path;
use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{EdgeKind, Language, Node, NodeKind};

pub(super) fn resolve_cpp_include_reference(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // C/C++ #include references — resolve directly to the included file
    // (file→file edge), bypassing symbol lookup. The extractor emits these
    // with `referenceKind: 'imports'` and `referenceName: <include path>`
    // (e.g. "uint256.h" or "common/args.h"). Without this branch the
    // include-dir scan path inside resolveImportPath never produces an
    // edge — resolveViaImport's symbol lookup below would search the
    // resolved file for a symbol named like the file extension and fail.
    if (reference.language != Language::C && reference.language != Language::Cpp)
        || reference.reference_kind != EdgeKind::Imports
    {
        return None;
    }

    let resolved_path = resolve_import_path(
        &reference.reference_name,
        &reference.file_path,
        reference.language,
        context,
    )?;
    let basename = resolved_path.split('/').next_back().unwrap_or("");
    let file_nodes: Vec<Node> = context
        .get_nodes_by_name(basename)
        .into_iter()
        .filter(|n| n.kind == NodeKind::File)
        .collect();
    let file_node = file_nodes.iter().find(|n| n.file_path == resolved_path)?;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: file_node.id.clone(),
        confidence: 0.9,
        resolved_by: ResolvedBy::Import,
    })
}
