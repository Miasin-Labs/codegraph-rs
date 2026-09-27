//! What the files' syntax says about shared storage and types: each
//! class's fields, each file's globals, and who extends or implements whom.
//! Read in one iterative walk per file, so it works the same with an index
//! (the project sweep) and without one (`--check` examples).

use std::collections::{HashMap, HashSet};

use codegraph_analysis::ir::shared::SharedNames;
use tree_sitter::Node;

use super::super::engine::FileInput;
use crate::types::Language;

/// Fields, globals and type hierarchy of a set of files.
#[derive(Debug, Default)]
pub(super) struct Facts {
    /// (class simple name, field name).
    fields: HashSet<(String, String)>,
    /// File-scope variables (C/C++ globals).
    globals: HashSet<String>,
    /// Class simple name → its direct supertypes' simple names.
    supers: HashMap<String, Vec<String>>,
    /// Class simple name → its direct subtypes' simple names.
    subs: HashMap<String, Vec<String>>,
    /// Every declared class, interface, struct.
    classes: HashSet<String>,
}

/// Node kinds declaring a class-like type, with its name in field `name`.
const CLASS_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "class_specifier",
    "struct_specifier",
];

/// The simple name of a type as written: generics, package and namespace
/// dropped (`java.util.List<String>` → `List`).
pub(super) fn simple_type(text: &str) -> String {
    let bare = text.split('<').next().unwrap_or(text).trim();
    bare.rsplit(['.', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or(bare)
        .trim()
        .to_string()
}

impl Facts {
    /// Read `files`.
    pub fn read(files: &[&FileInput]) -> Self {
        let mut facts = Facts::default();
        for file in files {
            facts.read_file(file);
        }
        facts
    }

    fn read_file(&mut self, file: &FileInput) {
        let source = file.source;
        let text = |node: Node| source.get(node.byte_range()).unwrap_or_default();
        let c_like = matches!(file.language, Language::C | Language::Cpp);
        // (node, enclosing class)
        let mut stack: Vec<(Node, Option<String>)> = vec![(file.tree.root_node(), None)];
        while let Some((node, class)) = stack.pop() {
            let kind = node.kind();
            let mut class = class;
            if CLASS_KINDS.contains(&kind) {
                if let Some(name) = node.child_by_field_name("name") {
                    let name = simple_type(text(name));
                    self.classes.insert(name.clone());
                    for field in ["superclass", "interfaces"] {
                        if let Some(list) = node.child_by_field_name(field) {
                            for ty in type_names(list, source) {
                                self.link(&name, &ty);
                            }
                        }
                    }
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        if child.kind() == "base_class_clause"
                            || child.kind() == "extends_interfaces"
                        {
                            for ty in type_names(child, source) {
                                self.link(&name, &ty);
                            }
                        }
                    }
                    class = Some(name);
                }
            }
            if kind == "field_declaration" {
                if let Some(owner) = &class {
                    for name in declared_names(node, source) {
                        self.fields.insert((owner.clone(), name));
                    }
                }
            }
            // A C/C++ declaration at file scope declares globals.
            if c_like
                && kind == "declaration"
                && node
                    .parent()
                    .is_some_and(|p| p.kind() == "translation_unit")
            {
                for name in declared_names(node, source) {
                    self.globals.insert(name);
                }
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                stack.push((child, class.clone()));
            }
        }
    }

    fn link(&mut self, sub: &str, sup: &str) {
        let supers = self.supers.entry(sub.to_string()).or_default();
        if !supers.iter().any(|s| s == sup) {
            supers.push(sup.to_string());
        }
        let subs = self.subs.entry(sup.to_string()).or_default();
        if !subs.iter().any(|s| s == sub) {
            subs.push(sub.to_string());
        }
    }

    pub fn is_class(&self, name: &str) -> bool {
        self.classes.contains(name)
    }

    /// `class` and its supertypes, nearest first (at most 16).
    pub fn lineage(&self, class: &str) -> Vec<String> {
        let mut out = vec![class.to_string()];
        let mut index = 0;
        while index < out.len() && out.len() < 16 {
            if let Some(supers) = self.supers.get(&out[index]) {
                for sup in supers {
                    if !out.contains(sup) {
                        out.push(sup.clone());
                    }
                }
            }
            index += 1;
        }
        out
    }

    /// Every (transitive) subtype of `class` (at most 64).
    pub fn subtypes(&self, class: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut stack = vec![class.to_string()];
        while let Some(current) = stack.pop() {
            for sub in self.subs.get(&current).into_iter().flatten() {
                if !out.contains(sub) && out.len() < 64 {
                    out.push(sub.clone());
                    stack.push(sub.clone());
                }
            }
        }
        out
    }

    /// Shared names as seen from a function of class `owner` (or none).
    pub fn scope<'f>(&'f self, owner: Option<&'f str>) -> Scope<'f> {
        Scope { facts: self, owner }
    }
}

/// The simple names of the types in a `superclass`/`interfaces`/base list.
fn type_names(node: Node, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if matches!(
            current.kind(),
            "type_identifier" | "scoped_type_identifier" | "generic_type" | "qualified_identifier"
        ) {
            let text = source.get(current.byte_range()).unwrap_or_default();
            out.push(simple_type(text));
            continue;
        }
        let mut cursor = current.walk();
        stack.extend(current.named_children(&mut cursor));
    }
    out
}

/// The names a (field) declaration declares: each declarator's
/// identifier.
fn declared_names(node: Node, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let mut current = child;
        for _ in 0..8 {
            match current.kind() {
                "identifier" | "field_identifier" => {
                    out.push(
                        source
                            .get(current.byte_range())
                            .unwrap_or_default()
                            .to_string(),
                    );
                    break;
                }
                "variable_declarator"
                | "init_declarator"
                | "pointer_declarator"
                | "array_declarator"
                | "reference_declarator" => {
                    match current
                        .child_by_field_name("name")
                        .or_else(|| current.child_by_field_name("declarator"))
                    {
                        Some(next) => current = next,
                        None => break,
                    }
                }
                _ => break,
            }
        }
    }
    out
}

/// [`SharedNames`] for one function: its class's fields (bare or through
/// `this`), other classes' fields through their name, and globals.
pub(super) struct Scope<'f> {
    facts: &'f Facts,
    owner: Option<&'f str>,
}

impl SharedNames for Scope<'_> {
    fn bare(&self, name: &str) -> Option<String> {
        if let Some(owner) = self.owner {
            for class in self.facts.lineage(owner) {
                if self
                    .facts
                    .fields
                    .contains(&(class.clone(), name.to_string()))
                {
                    return Some(format!("{class}.{name}"));
                }
            }
        }
        self.facts.globals.contains(name).then(|| name.to_string())
    }

    fn member(&self, owner: &str, field: &str) -> Option<String> {
        let class = if owner == "this" {
            self.owner?.to_string()
        } else {
            simple_type(owner)
        };
        self.facts
            .lineage(&class)
            .into_iter()
            .find(|c| self.facts.fields.contains(&(c.clone(), field.to_string())))
            .map(|c| format!("{c}.{field}"))
    }
}
