//! Names every Rust file has without importing them — the standard
//! prelude (`Ok`, `Some`, `Vec`, `Clone`, `drop`…) — and receivers whose
//! type the literal spells (`"a".to_owned()`), answered through the API
//! indexes when `std` has one.
//!
//! A prelude name is only the prelude's when nothing in the file can
//! shadow it: no `use` binds it, no glob import could bring it (a glob of
//! an indexed module is checked; any other glob is taken to), and no item
//! of the file has it. A `#[derive(..)]` line names the derive macro, not
//! the trait, and is left alone.

use super::api;
use super::lookup::Found;
use crate::resolution::external::context::ExternalContext;
use crate::resolution::line_index::Lines;
use crate::resolution::name_matcher::UseBinding;
use crate::resolution::name_matcher::external::CrateType;
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{NodeKind, RECEIVER_TEXT};

/// The prelude item a bare name is, when the file cannot mean anything
/// else by it.
pub(super) fn prelude_item(
    reference: &UnresolvedRef,
    context: &ExternalContext<'_>,
) -> Option<Found> {
    let name = reference.reference_name.as_str();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let cache = context.declarations.cache();
    if !api::has_index(cache, "std") || shadowed(reference, context, name) {
        return None;
    }
    if on_derive_line(reference, context) || written_qualifier(reference, context).is_some() {
        return None;
    }
    api::prelude_lookup(cache, name, reference.reference_kind)
}

/// The path written before a bare reference's name (`fmt::Result` recorded
/// as `Result`: `fmt`), read from its line.
pub(super) fn written_qualifier(
    reference: &UnresolvedRef,
    context: &ExternalContext<'_>,
) -> Option<String> {
    let line = line_text(reference, context)?;
    let before = line.get(..reference.column as usize)?;
    let mut rest = before.strip_suffix("::")?;
    let mut segments: Vec<&str> = Vec::new();
    loop {
        // `<T as Tr>::Name`: qualified, by something that is not a path.
        let Some(start) = rest
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
            .last()
            .map(|(index, _)| index)
        else {
            return Some(String::new());
        };
        segments.push(&rest[start..]);
        match rest[..start].strip_suffix("::") {
            Some(outer) => rest = outer,
            None => break,
        }
    }
    segments.reverse();
    Some(segments.join("::"))
}

/// Something in the file may give `name` another meaning.
fn shadowed(reference: &UnresolvedRef, context: &ExternalContext<'_>, name: &str) -> bool {
    let cache = context.declarations.cache();
    let file = reference.file_path.as_str();
    let binds = |binding: &UseBinding| match binding {
        UseBinding::Name(bound) | UseBinding::Module(bound) => bound == name,
        UseBinding::Glob => false,
    };
    for found in context.get_rust_use_leaves(file).iter() {
        let leaf = &found.leaf;
        if binds(&leaf.binding) {
            return true;
        }
        if leaf.binding != UseBinding::Glob {
            continue;
        }
        // `mod tests { use super::*; }` re-imports the file's own names,
        // already checked here.
        if !found.inline_modules.is_empty() && leaf.path == ["super"] {
            continue;
        }
        let Some((root, rest)) = leaf.path.split_first() else {
            return true;
        };
        let reachable = !matches!(root.as_str(), "crate" | "self" | "super") && cache.knows(root);
        if !reachable || api::module_exports(cache, root, rest, name) != Some(false) {
            return true;
        }
    }
    if context
        .get_rust_fn_local_uses(file)
        .iter()
        .any(|local| binds(&local.leaf.binding) || local.leaf.binding == UseBinding::Glob)
    {
        return true;
    }
    context
        .get_nodes_in_file_named(file, name)
        .iter()
        .any(|node| {
            matches!(
                node.kind,
                NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Union
                    | NodeKind::Trait
                    | NodeKind::TypeAlias
                    | NodeKind::Function
                    | NodeKind::Constant
                    | NodeKind::Variable
                    | NodeKind::EnumMember
                    | NodeKind::Macro
                    | NodeKind::Module
                    | NodeKind::Import
            )
        })
}

/// The reference sits on a `#[derive(..)]` line.
fn on_derive_line(reference: &UnresolvedRef, context: &ExternalContext<'_>) -> bool {
    line_text(reference, context).is_some_and(|line| line.contains("derive("))
}

fn line_text(reference: &UnresolvedRef, context: &ExternalContext<'_>) -> Option<String> {
    let source = context.read_file_arc(&reference.file_path)?;
    let at = (reference.line as usize).checked_sub(1)?;
    Lines::of(&source).get(at).map(str::to_string)
}

/// The type of a dropped receiver that is a literal: a string literal is
/// a `str` (`"a".to_owned()`), a char literal a `char`.
pub(super) fn literal_receiver(
    reference: &UnresolvedRef,
    context: &ExternalContext<'_>,
) -> Option<CrateType> {
    let written = reference
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get(RECEIVER_TEXT))
        .and_then(|value| value.as_str().map(str::to_string));
    let primitive = match written {
        Some(text) => literal_type(&text)?,
        None => {
            // The receiver text is not recorded (too long): read what ends
            // just before `.method` on the reference's line.
            let line = line_text(reference, context)?;
            let before = line.get(..reference.column as usize)?.trim_end();
            let before = before.strip_suffix('.')?.trim_end();
            literal_type(trailing_literal(before)?)?
        }
    };
    Some(CrateType {
        krate: "core".to_string(),
        path: vec![primitive.to_string()],
    })
}

/// The primitive a literal's text is (`"a"` → `str`, `'c'` → `char`).
fn literal_type(text: &str) -> Option<&'static str> {
    let text = text.trim();
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        return Some("str");
    }
    if text.starts_with("r#") || text.starts_with("r\"") {
        return text
            .ends_with('"')
            .then_some("str")
            .or_else(|| text.ends_with('#').then_some("str"));
    }
    if text.len() >= 3 && text.starts_with('\'') && text.ends_with('\'') {
        return Some("char");
    }
    None
}

/// The string or char literal `text` ends with, when it starts on this
/// line: from its opening quote (a byte or C string's prefix excluded).
fn trailing_literal(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let quote = *bytes.last()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    // Scan back to the unescaped opening quote.
    let mut index = bytes.len() - 1;
    loop {
        index = index.checked_sub(1)?;
        if bytes[index] == quote {
            let escapes = bytes[..index]
                .iter()
                .rev()
                .take_while(|b| **b == b'\\')
                .count();
            if escapes % 2 == 0 {
                break;
            }
        }
    }
    let prefix = bytes[..index].last().copied();
    if prefix.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'#') {
        return None; // b"…", c"…", r#"…"#, or an identifier's quote
    }
    Some(&text[index..])
}

#[cfg(test)]
mod tests {
    use super::{literal_type, trailing_literal};

    #[test]
    fn literals_name_their_type() {
        assert_eq!(literal_type("\"a\""), Some("str"));
        assert_eq!(literal_type("\"\""), Some("str"));
        assert_eq!(literal_type("'c'"), Some("char"));
        assert_eq!(literal_type("s"), None);
        assert_eq!(trailing_literal("(treemap!(\"a\""), Some("\"a\""));
        assert_eq!(trailing_literal("x(\"a \\\" b\""), Some("\"a \\\" b\""));
        assert_eq!(trailing_literal("b\"ab\""), None);
        assert_eq!(trailing_literal("f(x)"), None);
    }
}
