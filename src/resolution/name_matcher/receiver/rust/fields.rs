//! The type of `self.field` in a method call extraction recorded without
//! its receiver: `self.graph.get(x)` reaches resolution as a bare `get`.
//!
//! The call's source names the field, the enclosing method names the impl
//! type, and the field's declaration (`graph: Arc<Graph>`) names the type
//! the call runs on. Only that one hop is read; `self.a.b.m()`,
//! `self.a().m()`, and tuple fields (`self.0.m()`) stay unknown.

use super::bindings::word_positions;
use super::locals::caller_fn;
use super::lookup::{RustType, resolve_named, resolve_type};
use super::method_owner;
use super::types::{is_ident_byte, named_type};
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::Node;

/// How many lines a `self.field\n.method(` call may span.
const MAX_CALL_LINES: usize = 4;
/// How many lines one field declaration may span.
const MAX_FIELD_LINES: usize = 8;

/// The type of the receiver of the method call at `reference` when that
/// receiver is `self.field`.
pub(in crate::resolution::name_matcher) fn self_field_receiver_type(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<RustType> {
    let source = context.read_file_arc(&reference.file_path)?;
    let line_index = (reference.line as usize).checked_sub(1)?;
    let call = source
        .split('\n')
        .skip(line_index)
        .take(MAX_CALL_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let column = reference.column as usize;
    let method = &reference.reference_name;
    // A parsed call starts at `self`; one inside a macro's arguments at the
    // method, after `self.field.` on its line.
    let field = self_field_before(call.get(column..)?, method).or_else(|| {
        let line = call.split('\n').next()?;
        let (before, at) = (line.get(..column)?, line.get(column..)?);
        at.starts_with(method.as_str())
            .then(|| self_field_ending(before))
            .flatten()
    })?;
    let scope = caller_fn(reference, context)?;
    let owner = resolve_type(
        method_owner(&scope)?,
        &reference.file_path,
        reference,
        context,
    );
    if !owner.is_project_type(context) {
        return None;
    }
    let nodes = context.get_nodes_by_name(field);
    let declaration = owner.field(field, &nodes, reference)?;
    let written = declared_type(declaration, context)?;
    resolve_named(
        named_type(&written)?,
        Some(&owner),
        &declaration.file_path,
        reference,
        context,
    )
}

/// The field of `self.field.method(…)` written at the start of `call`.
fn self_field_before<'t>(call: &'t str, method: &str) -> Option<&'t str> {
    let rest = call
        .strip_prefix("self")?
        .trim_start()
        .strip_prefix('.')?
        .trim_start();
    let length = rest.bytes().take_while(|byte| is_ident_byte(*byte)).count();
    let field = &rest[..length];
    if field.is_empty() || field.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let after = rest[length..]
        .trim_start()
        .strip_prefix('.')?
        .trim_start()
        .strip_prefix(method)?
        .trim_start();
    (after.starts_with('(') || after.starts_with("::<")).then_some(field)
}

/// The field of `self.field.` ending `before` (the text before a method
/// name).
fn self_field_ending(before: &str) -> Option<&str> {
    let rest = before.trim_end().strip_suffix('.')?.trim_end();
    let length = rest
        .bytes()
        .rev()
        .take_while(|byte| is_ident_byte(*byte))
        .count();
    let (rest, field) = rest.split_at(rest.len() - length);
    if field.is_empty() || field.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let receiver = rest.trim_end().strip_suffix('.')?.trim_end();
    let owner = receiver.strip_suffix("self")?;
    owner
        .bytes()
        .next_back()
        .is_none_or(|byte| !is_ident_byte(byte) && byte != b'.')
        .then_some(field)
}

/// The type written in the field's declaration (`pub graph: Graph,`).
pub(super) fn declared_type(field: &Node, context: &dyn ResolutionContext) -> Option<String> {
    let source = context.read_file_arc(&field.file_path)?;
    let declaration = source
        .split('\n')
        .skip(field.start_line.checked_sub(1)? as usize)
        .take(MAX_FIELD_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let at = word_positions(&declaration, &field.name).next()?;
    let written = declaration[at + field.name.len()..]
        .trim_start()
        .strip_prefix(':')
        .filter(|rest| !rest.starts_with(':'))?;
    let end = type_end(written);
    Some(written[..end].trim().to_string()).filter(|text| !text.is_empty())
}

/// Where the type starting `text` ends: the `,` or `}` closing the field,
/// outside any brackets (the `>` of `->` closes nothing).
fn type_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b'>' if index > 0 && bytes[index - 1] == b'-' => {}
            b')' | b']' | b'>' => depth = depth.saturating_sub(1),
            b'}' if depth == 0 => return index,
            b'}' => depth -= 1,
            b',' | b';' if depth == 0 => return index,
            _ => {}
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::{self_field_before, self_field_ending, type_end};

    /// Macro-argument calls are anchored at the method name.
    #[test]
    fn reads_the_field_before_a_method_name() {
        assert_eq!(self_field_ending("    matches!(self.rules."), Some("rules"));
        assert_eq!(self_field_ending("x(self . rules . "), Some("rules"));
        assert_eq!(self_field_ending("self.a.rules."), None);
        assert_eq!(self_field_ending("myself.rules."), None);
        assert_eq!(self_field_ending("self.0."), None);
        assert_eq!(self_field_ending("self.rules"), None);
    }

    #[test]
    fn reads_the_field_of_a_self_field_method_call() {
        assert_eq!(self_field_before("self.graph.get(1)", "get"), Some("graph"));
        assert_eq!(
            self_field_before("self.graph\n        .get(1)", "get"),
            Some("graph")
        );
        assert_eq!(
            self_field_before("self.graph.get::<u8>(1)", "get"),
            Some("graph")
        );
        // Only the first call after the field is on it.
        assert_eq!(
            self_field_before("self.graph.get(1).unwrap()", "unwrap"),
            None
        );
        assert_eq!(self_field_before("self.graph.get_mut(1)", "get"), None);
        assert_eq!(self_field_before("self.0.finish()", "finish"), None);
        assert_eq!(self_field_before("self.a.b.get(1)", "get"), None);
        assert_eq!(self_field_before("other.graph.get(1)", "get"), None);
    }

    #[test]
    fn a_field_type_ends_at_its_comma_or_brace() {
        let text = " Arc<Mutex<Graph>>, next: u8";
        assert_eq!(&text[..type_end(text)], " Arc<Mutex<Graph>>");
        let text = " Box<dyn Fn(u8) -> u8> }";
        assert_eq!(&text[..type_end(text)], " Box<dyn Fn(u8) -> u8> ");
        let text = " HashMap<String, Vec<u8>>,";
        assert_eq!(&text[..type_end(text)], " HashMap<String, Vec<u8>>");
    }
}
