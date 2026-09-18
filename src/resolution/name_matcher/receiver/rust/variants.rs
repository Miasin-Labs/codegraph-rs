//! The type of a field a tuple-variant or tuple-struct pattern binds:
//! `Backend::Headless(headless) => …` gives `headless` the type the
//! variant's declaration writes (`Headless(Headless),`), whatever the
//! scrutinee is.

use super::lookup::resolve_type;
use super::types::{matching_paren, split_top_level};
use super::{Inference, Value};
use crate::types::{Language, NodeKind};

impl Inference<'_> {
    /// Field `position` of the tuple variant or struct `path` (`Kind::Leaf`,
    /// `Self::Leaf`, `Wrapper`) as its declaration writes it.
    pub(super) fn variant_field(&self, path: &str, position: usize) -> Option<Value> {
        let (owner, name) = match path.rsplit_once("::") {
            Some((owner, name)) => (Some(owner), name),
            None => (None, path),
        };
        let owner = match owner {
            Some("Self") => Some(self.owner.clone()?),
            Some(owner) => Some(resolve_type(
                owner,
                &self.reference.file_path,
                self.reference,
                self.context,
            )),
            None => None,
        };
        let nodes = self.context.get_nodes_by_name(name);
        let mut declarations = nodes.iter().filter(|node| {
            node.language == Language::Rust
                && match &owner {
                    Some(owner) => {
                        node.kind == NodeKind::EnumMember
                            && (node.qualified_name == format!("{}::{name}", owner.name)
                                || node
                                    .qualified_name
                                    .ends_with(&format!("::{}::{name}", owner.name)))
                    }
                    None => matches!(node.kind, NodeKind::EnumMember | NodeKind::Struct),
                }
        });
        let declaration = declarations.next()?;
        if declarations.next().is_some() {
            return None;
        }
        let source = self.context.read_file_arc(&declaration.file_path)?;
        let line = source
            .split('\n')
            .nth((declaration.start_line as usize).checked_sub(1)?)?;
        let text = field_type(line, name, position)?;
        let self_ty = match declaration.kind {
            NodeKind::EnumMember => owner.or_else(|| {
                let (enum_path, _) = declaration.qualified_name.rsplit_once("::")?;
                Some(resolve_type(
                    enum_path,
                    &declaration.file_path,
                    self.reference,
                    self.context,
                ))
            }),
            _ => None,
        };
        Some(Value::Written {
            text,
            self_ty,
            file: declaration.file_path.clone(),
        })
    }
}

/// Field `position` of the tuple variant or struct `name` declared on
/// `line`: `Leaf(u32, Node),` or `pub struct Wrapper(pub Inner);`.
fn field_type(line: &str, name: &str, position: usize) -> Option<String> {
    let at = line.find(&format!("{name}("))?;
    let fields = &line[at + name.len()..];
    let close = matching_paren(fields)?;
    let field = *split_top_level(&fields[1..close], b',').get(position)?;
    // `pub`, `pub(crate)`, `pub(in a::b)`.
    let field = match field.strip_prefix("pub") {
        Some(rest) if rest.starts_with('(') => &rest[matching_paren(rest)? + 1..],
        Some(rest) if rest.starts_with(char::is_whitespace) => rest,
        _ => field,
    }
    .trim();
    (!field.is_empty()).then(|| field.to_string())
}

#[cfg(test)]
mod tests {
    use super::field_type;

    #[test]
    fn reads_tuple_fields_from_declarations() {
        assert_eq!(
            field_type("    Headless(Headless),", "Headless", 0).as_deref(),
            Some("Headless")
        );
        assert_eq!(
            field_type("    Pair(u32, Arc<Node>),", "Pair", 1).as_deref(),
            Some("Arc<Node>")
        );
        assert_eq!(
            field_type("pub struct Wrapper(pub Inner);", "Wrapper", 0).as_deref(),
            Some("Inner")
        );
        assert_eq!(
            field_type("struct Id(pub(in crate::a) u32, pubsub::Hub);", "Id", 1).as_deref(),
            Some("pubsub::Hub")
        );
        assert_eq!(
            field_type("struct Id(pub(in crate::a) u32);", "Id", 0).as_deref(),
            Some("u32")
        );
        assert_eq!(field_type("    Unit,", "Unit", 0), None);
    }
}
