use super::super::paths::resolve_import_path;
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
use crate::types::{Language, NodeKind};

pub(super) fn resolve_python_receiver(
    reference: &UnresolvedRef,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (receiver, tail) = reference.reference_name.split_once('.')?;
    let member = tail.split('.').next()?;
    for import in imports
        .iter()
        .filter(|import| import.local_name == receiver)
    {
        let module_path = if import.is_namespace {
            import.source.clone()
        } else if import.source.ends_with('.') {
            format!("{}{receiver}", import.source)
        } else {
            format!("{}.{}", import.source, import.exported_name)
        };
        let resolved_path = resolve_import_path(
            &module_path,
            &reference.file_path,
            Language::Python,
            context,
        )
        .or_else(|| find_python_module_file(&module_path, reference, context));
        if let Some(resolved_path) = resolved_path {
            if resolved_path != reference.file_path {
                if let Some(target) =
                    context
                        .get_nodes_in_file(&resolved_path)
                        .into_iter()
                        .find(|node| {
                            node.name == member
                                && matches!(
                                    node.kind,
                                    NodeKind::Function
                                        | NodeKind::Class
                                        | NodeKind::Variable
                                        | NodeKind::Constant
                                )
                        })
                {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: target.id,
                        confidence: 0.85,
                        resolved_by: ResolvedBy::Import,
                    });
                }
            }
        }

        if let Some(resolved) = resolve_imported_instance(import, member, reference, context) {
            return Some(resolved);
        }
    }
    None
}

fn resolve_imported_instance(
    import: &ImportMapping,
    member: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let source_file = resolve_import_path(
        &import.source,
        &reference.file_path,
        Language::Python,
        context,
    )
    .or_else(|| find_python_module_file(&import.source, reference, context))?;
    let value = context
        .get_nodes_in_file(&source_file)
        .into_iter()
        .find(|node| {
            node.name == import.exported_name
                && matches!(node.kind, NodeKind::Variable | NodeKind::Constant)
        })?;
    let type_name = infer_receiver_type_from_declaration(&value, context)?;
    resolve_method_on_type(
        &type_name,
        member,
        reference,
        context,
        0.85,
        ResolvedBy::InstanceMethod,
        None,
    )
}

fn find_python_module_file(
    module_path: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if module_path.is_empty() || module_path.starts_with('.') {
        return None;
    }
    let relative = module_path.replace('.', "/");
    let last = module_path.rsplit('.').next()?;
    let module_suffix = format!("{relative}.py");
    if let Some(node) = context
        .get_nodes_by_name(&format!("{last}.py"))
        .into_iter()
        .find(|node| {
            node.kind == NodeKind::File
                && node.file_path != reference.file_path
                && (node.file_path == module_suffix
                    || node.file_path.ends_with(&format!("/{module_suffix}")))
        })
    {
        return Some(node.file_path);
    }
    let package_suffix = format!("{relative}/__init__.py");
    context
        .get_nodes_by_name("__init__.py")
        .into_iter()
        .find(|node| {
            node.kind == NodeKind::File
                && node.file_path != reference.file_path
                && (node.file_path == package_suffix
                    || node.file_path.ends_with(&format!("/{package_suffix}")))
        })
        .map(|node| node.file_path)
}
