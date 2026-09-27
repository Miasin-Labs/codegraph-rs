//! Rust method calls on a receiver of unknown type whose method a direct
//! dependency of the project also defines (`node.walk()`, `stmt.query(..)`,
//! `a.b().as_object()`; see `crate::resolution::rust_deps`).
//!
//! Name matching alone would land such a call on whichever project method
//! shares the name. Name matching here may only pick a project method whose
//! owning type the calling file names somewhere (a `use`, an annotation, a
//! constructor): `tile.window()` in a file full of `Tile` still reaches
//! `Tile::window`, but tree-sitter's `node.walk()` in a file that never
//! mentions `ModuleLocation` no longer lands on `ModuleLocation::walk`.
//! With no such method the call stays unresolved; so does `recv.m()` with
//! several (a dropped receiver's call keeps nearest-first among them, as
//! `rust_call` picks).

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use crate::resolution::line_index::SourceMemo;
use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Language, Node, NodeKind};

/// Decide `recv.method(..)` on a local `recv` of unknown type: `None` when
/// no dependency defines `method` (the call is not decided here). Only the
/// one project method of a type the file names is taken; with several, as
/// with none, the call stays unresolved.
pub(super) fn match_dependency_method(
    method: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    if reference.language != Language::Rust || !context.is_rust_dependency_method(method) {
        return None;
    }
    let file = FileText::read(reference, context);
    let candidates: Vec<Node> = context
        .get_nodes_by_name(method)
        .into_iter()
        .filter(|node| {
            node.language == Language::Rust
                && node.kind == NodeKind::Method
                && file.names_owner_of(node)
        })
        .collect();
    // As sure as the old sole-method-of-that-name fallback, no surer.
    Some(match candidates.as_slice() {
        [only] => Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: only.id.clone(),
            confidence: 0.7,
            resolved_by: ResolvedBy::InstanceMethod,
        }),
        _ => None,
    })
}

/// The calling file's text and identifiers, for the owner check.
pub(super) struct FileText(Option<(Arc<str>, Arc<HashSet<String>>)>);

impl FileText {
    pub(super) fn read(reference: &UnresolvedRef, context: &dyn ResolutionContext) -> Self {
        thread_local! {
            /// Each file's identifiers: the check runs per candidate method
            /// of every reference, and scanning the file for a word each
            /// time was quadratic on large files.
            static IDENTS: RefCell<SourceMemo<HashSet<String>>> =
                const { RefCell::new(SourceMemo::new()) };
        }
        FileText(context.read_file_arc(&reference.file_path).map(|text| {
            let idents =
                IDENTS.with(|memo| memo.borrow_mut().get_or_insert_with(&text, identifiers));
            (text, idents)
        }))
    }

    /// The file names the type (or trait) `method` is defined on.
    pub(super) fn names_owner_of(&self, method: &Node) -> bool {
        let Some(owner) = method
            .qualified_name
            .rsplit_once("::")
            .and_then(|(owner, _)| owner.rsplit("::").next())
        else {
            return false;
        };
        self.0.as_ref().is_some_and(|(text, idents)| {
            if !owner.is_empty() && owner.bytes().all(is_ident_byte) {
                idents.contains(owner)
            } else {
                names_word(text, owner)
            }
        })
    }
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The maximal identifier runs of `text`: a word made of identifier bytes
/// is in this set exactly when [`names_word`] finds it.
fn identifiers(text: &str) -> HashSet<String> {
    text.split(|ch: char| !(ch.is_ascii() && is_ident_byte(ch as u8)))
        .filter(|run| !run.is_empty())
        .map(str::to_string)
        .collect()
}

/// `word` occurs in `text` as a whole identifier.
fn names_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let is_ident = is_ident_byte;
    text.match_indices(word).any(|(start, _)| {
        let end = start + word.len();
        start
            .checked_sub(1)
            .is_none_or(|before| !is_ident(bytes[before]))
            && bytes.get(end).is_none_or(|after| !is_ident(*after))
    })
}

#[cfg(test)]
mod tests {
    use super::{identifiers, names_word};

    #[test]
    fn owner_names_are_whole_words() {
        assert!(names_word("use crate::layout::Tile;", "Tile"));
        assert!(!names_word("let tiles = TileSet::new();", "Tile"));
        assert!(names_word("fn f(t: &Tile<W>)", "Tile"));
    }

    #[test]
    fn identifier_set_agrees_with_the_word_scan() {
        let text =
            "use crate::layout::Tile;\nlet tiles = TileSet::new(); // é_x Ω9\nfn f(t: &Tile<W>) {}";
        let idents = identifiers(text);
        for word in [
            "Tile", "TileSet", "tiles", "tile", "W", "layout", "x", "_x", "9", "new", "fn",
        ] {
            assert_eq!(idents.contains(word), names_word(text, word), "{word}");
        }
    }
}
