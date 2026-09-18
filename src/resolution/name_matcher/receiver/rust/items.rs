//! Values named by a file-level item rather than a local: a `static` or
//! `const` (declared in this file or elsewhere in the project) or a unit
//! struct.

use super::bindings::{Binding, binding_in_line};
use super::lookup::resolve_type;
use super::types::starts_uppercase;
use super::{Inference, MAX_LINE_BYTES, Value};
use crate::types::{Language, NodeKind};

impl Inference<'_> {
    /// A file-level `static`/`const` (`RE.is_match(..)`), or a project unit
    /// struct used as a value (`MyResolver.detect(..)`).
    pub(super) fn item_value(&self, name: &str) -> Option<Value> {
        if !starts_uppercase(name) {
            return None;
        }
        if !name.bytes().any(|byte| byte.is_ascii_lowercase()) {
            let here = self
                .lines
                .iter()
                .find_map(|line| annotated_item(line, name))
                .map(|annotation| self.here(annotation));
            return here.or_else(|| self.project_item_value(name));
        }
        let ty = resolve_type(
            name,
            &self.reference.file_path,
            self.reference,
            self.context,
        );
        ty.is_project_type(self.context)
            .then_some(Value::Resolved(ty))
    }

    /// A `static`/`const` another file of the project declares
    /// (`pub static ALL_TARGETS: [&dyn AgentTarget; 13]`), when every
    /// declaration of that name agrees on its type.
    fn project_item_value(&self, name: &str) -> Option<Value> {
        let mut found: Option<(String, String)> = None;
        for node in self.context.get_nodes_by_name(name) {
            if node.language != Language::Rust
                || !matches!(node.kind, NodeKind::Constant | NodeKind::Variable)
            {
                continue;
            }
            let Some(source) = self.context.read_file_arc(&node.file_path) else {
                continue;
            };
            let line = (node.start_line as usize)
                .checked_sub(1)
                .and_then(|index| source.split('\n').nth(index))
                .unwrap_or_default();
            let annotation = annotated_item(line, name)?.to_string();
            match &found {
                Some((seen, _)) if *seen != annotation => return None,
                Some(_) => {}
                None => found = Some((annotation, node.file_path.clone())),
            }
        }
        let (text, file) = found?;
        Some(Value::Written {
            text,
            self_ty: None,
            file,
        })
    }
}

/// The type written on the `static`/`const` item `name` declared on `line`.
fn annotated_item<'l>(line: &'l str, name: &str) -> Option<&'l str> {
    if line.len() > MAX_LINE_BYTES {
        return None;
    }
    match binding_in_line(line, &[], name) {
        Some(Binding::Let {
            annotation: Some(annotation),
            ..
        }) => Some(annotation),
        _ => None,
    }
}
