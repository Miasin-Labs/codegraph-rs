//! [`ForeignTypes`] over the reachable graphs: what a dependency declares a
//! method, fn or field to be, and what a type it writes names.

use std::cell::Cell;

use super::open::GraphCache;
use super::rust::lookup::{lookup_field, lookup_method, lookup_path, type_path};
use crate::resolution::name_matcher::external::{field_declared_type, signature_return};
use crate::resolution::{ForeignText, ForeignTypes};
use crate::types::{EdgeKind, NodeKind};

/// Declarations of the reachable graphs, counting how many answers chain
/// inference took from them.
pub(crate) struct Declarations<'a> {
    cache: &'a GraphCache<'a>,
    answers: Cell<usize>,
}

impl<'a> Declarations<'a> {
    pub(crate) fn new(cache: &'a GraphCache<'a>) -> Self {
        Declarations {
            cache,
            answers: Cell::new(0),
        }
    }

    pub(crate) fn cache(&self) -> &'a GraphCache<'a> {
        self.cache
    }

    /// Answers given so far: a count that moved during one inference means
    /// the type came through a dependency's declaration.
    pub(crate) fn answers(&self) -> usize {
        self.answers.get()
    }

    fn answered(&self, text: ForeignText) -> Option<ForeignText> {
        self.answers.set(self.answers.get() + 1);
        Some(text)
    }
}

impl ForeignTypes for Declarations<'_> {
    fn method_return(&self, krate: &str, owner: &[String], method: &str) -> Option<ForeignText> {
        let graph = self.cache.get(krate)?;
        let node = lookup_method(self.cache, &graph, owner, method)?;
        let text = signature_return(node.signature.as_deref()?)?.to_string();
        self.answered(ForeignText {
            text,
            krate: krate.to_string(),
            file: node.file_path,
        })
    }

    fn fn_return(&self, krate: &str, path: &[String]) -> Option<ForeignText> {
        let found = lookup_path(self.cache, krate, path, EdgeKind::Calls)?;
        let text = match found.node.kind {
            NodeKind::Function | NodeKind::Method => {
                signature_return(found.node.signature.as_deref()?)?.to_string()
            }
            // A tuple struct's constructor makes the struct.
            NodeKind::Struct => found.node.name.clone(),
            _ => return None,
        };
        self.answered(ForeignText {
            text,
            krate: found.graph.krate.clone(),
            file: found.node.file_path,
        })
    }

    fn field_type(&self, krate: &str, owner: &[String], field: &str) -> Option<ForeignText> {
        let graph = self.cache.get(krate)?;
        let node = lookup_field(self.cache, &graph, owner.last()?, field)?;
        let text = field_declared_type(&node, &graph.context)?;
        self.answered(ForeignText {
            text,
            krate: krate.to_string(),
            file: node.file_path,
        })
    }

    fn resolve_type(&self, krate: &str, file: &str, path: &str) -> Option<(String, Vec<String>)> {
        type_path(self.cache, krate, file, path)
    }
}
