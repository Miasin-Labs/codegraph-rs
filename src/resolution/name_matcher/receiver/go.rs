use regex::Regex;

use super::{infer_local_receiver_type, resolve_method_on_type};
use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Language, NodeKind};

const GO_BUILTIN_FIELD_TYPES: &[&str] = &[
    "string",
    "bool",
    "byte",
    "rune",
    "error",
    "any",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "float32",
    "float64",
    "complex64",
    "complex128",
    "chan",
    "map",
    "func",
    "struct",
    "interface",
];

pub(in crate::resolution::name_matcher) fn match_go_field_chain_call(
    receiver_chain: &str,
    method_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (base, field) = receiver_chain.split_once('.')?;
    if base.is_empty() || field.is_empty() || field.contains('.') {
        return None;
    }
    let base_type = infer_local_receiver_type(base, reference, context)?;
    let field_type_re = Regex::new(&format!(
        r"\b{}\s+\*?\[?\]?([A-Za-z_][0-9A-Za-z_.]*)",
        regex::escape(field)
    ))
    .expect("escaped Go field regex");
    let mut owners = context.get_nodes_by_name(&base_type);
    owners.sort_by_key(|node| node.file_path != reference.file_path);

    for owner in owners.iter().filter(|node| {
        matches!(node.kind, NodeKind::Struct | NodeKind::Class) && node.language == Language::Go
    }) {
        let Some(source) = context.read_file_arc(&owner.file_path) else {
            continue;
        };
        let lines: Vec<&str> = source.lines().collect();
        let start = owner.start_line.saturating_sub(1) as usize;
        let end = (owner.end_line as usize).min(lines.len());
        for raw_line in lines.get(start..end).unwrap_or_default() {
            let line = raw_line.split("//").next().unwrap_or(raw_line);
            let Some(raw_type) = field_type_re
                .captures(line)
                .and_then(|captures| captures.get(1))
                .map(|capture| capture.as_str())
            else {
                continue;
            };
            if let Some((package, _)) = raw_type.split_once('.') {
                let Some(module) = context.get_go_module() else {
                    continue;
                };
                let Some(import) = context
                    .get_import_mappings(&owner.file_path, Language::Go)
                    .into_iter()
                    .find(|mapping| mapping.local_name == package)
                else {
                    continue;
                };
                if module.matching_root(&import.source).is_none() {
                    continue;
                }
            }
            let Some(field_type) = raw_type.rsplit('.').next() else {
                continue;
            };
            if GO_BUILTIN_FIELD_TYPES.contains(&field_type) {
                continue;
            }
            if let Some(resolved) = resolve_method_on_type(
                field_type,
                method_name,
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
    None
}
