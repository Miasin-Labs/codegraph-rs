use super::super::support::{angle_re, array_brackets_re, dot_space_split_re, varargs_re};
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::NodeKind;

/// Java/Kotlin: infer a receiver's declared type by walking field declarations
/// in the class enclosing the call site. The field's `signature` is already in
/// the form "<TypeName> <fieldName>" (set by tree-sitter.ts extractField), so we
/// pull the type from there. Handles Spring `@Resource UserBO userbo;` /
/// `@Autowired private UserService userService;` where the receiver field name
/// doesn't match the class name by Java naming convention.
///
/// Returns the bare type name (generics stripped, dotted package stripped) or
/// None when no matching field is in the enclosing class.
pub(in crate::resolution::name_matcher) fn infer_java_field_receiver_type(
    receiver_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    // The class enclosing the call line (tightest: latest start). Indexed
    // lookups — this runs per reference.
    let enclosing = context
        .scopes_enclosing_line(&reference.file_path, reference.line)
        .into_iter()
        .find(|n| {
            matches!(n.kind, NodeKind::Class | NodeKind::Interface)
                && n.language == reference.language
                && n.end_line >= reference.line
        })?;

    let enclosing_end = enclosing.end_line;
    let fields = context.get_nodes_in_file_named(&reference.file_path, receiver_name);
    let field = fields.iter().find(|n| {
        n.kind == NodeKind::Field
            && n.language == reference.language
            && n.start_line >= enclosing.start_line
            && n.end_line <= enclosing_end
    })?;
    let signature = field.signature.as_deref().filter(|s| !s.is_empty())?;

    // Signature shape: "<TypeName> <fieldName>" (extractField). Pull the type,
    // strip generics + dotted package, drop array/varargs markers.
    // (JS `lastIndexOf` returning -1 made `slice(0, -1)` drop the last char;
    // mirror that defensive edge.)
    let before_name = match signature.rfind(&field.name) {
        Some(i) => &signature[..i],
        None => {
            let mut chars = signature.chars();
            chars.next_back();
            chars.as_str()
        }
    };
    let type_raw = before_name.trim();
    if type_raw.is_empty() {
        return None;
    }

    let type_no_generics = angle_re().replace_all(type_raw, "");
    let type_no_generics = type_no_generics.trim();
    let type_no_array = array_brackets_re().replace_all(type_no_generics, "");
    let type_no_array = varargs_re().replace_all(&type_no_array, "");
    let type_no_array = type_no_array.trim();
    let parts: Vec<&str> = dot_space_split_re()
        .split(type_no_array)
        .filter(|p| !p.is_empty())
        .collect();
    let last_part = *parts.last()?;
    if last_part.is_empty() {
        return None;
    }
    if !last_part
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
    {
        return None; // primitives / lowercase → skip
    }
    Some(last_part.to_string())
}
