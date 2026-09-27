//! C/C++ files read the object-like macros of the local headers they
//! include (`#include "x.h"` with `#define COMMAND_ARG3 data`), so an
//! argument written as a macro lowers to the variable it stands for, as it
//! does when the macro is defined in the file itself.

use std::collections::HashMap;
use std::path::Path;

use tree_sitter::Node;

use super::super::engine::FileInput;
use super::super::lang;
use crate::types::Language;

/// Give every C/C++ file of `files` the macros of the headers among
/// `files` it includes.
pub(super) fn link(files: &[&FileInput]) {
    let is_c = |file: &FileInput| matches!(file.language, Language::C | Language::Cpp);
    if !files.iter().any(|file| is_c(file)) {
        return;
    }
    let mut by_path: HashMap<&str, usize> = HashMap::new();
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, file) in files.iter().enumerate() {
        if !is_c(file) {
            continue;
        }
        by_path.insert(file.path, index);
        let name = file.path.rsplit('/').next().unwrap_or(file.path);
        by_name.entry(name).or_default().push(index);
    }
    let mut header_macros: HashMap<usize, HashMap<String, String>> = HashMap::new();
    for file in files.iter().filter(|file| is_c(file)) {
        let Some(ir_lang) = lang::for_language(file.language).ir else {
            continue;
        };
        let dir = Path::new(file.path).parent().unwrap_or(Path::new(""));
        let mut merged: HashMap<String, String> = HashMap::new();
        for include in local_includes(file.tree.root_node(), file.source) {
            let joined = dir.join(&include);
            let joined = joined.to_string_lossy();
            let name = include.rsplit('/').next().unwrap_or(&include);
            let header = by_path.get(joined.as_ref()).copied().or_else(|| {
                by_name
                    .get(name)
                    .filter(|found| found.len() == 1)
                    .map(|found| found[0])
            });
            let Some(header) = header else {
                continue;
            };
            let macros = header_macros.entry(header).or_insert_with(|| {
                let header = files[header];
                codegraph_analysis::ir::macro_aliases(
                    ir_lang,
                    header.tree.root_node(),
                    header.source,
                )
            });
            for (name, value) in macros.iter() {
                merged.entry(name.clone()).or_insert_with(|| value.clone());
            }
        }
        file.include_macros(ir_lang, &merged);
    }
}

/// The `"…"` includes of a file, in preprocessor blocks too (a walk of the
/// preprocessor nodes only).
fn local_includes(root: Node, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "preproc_include" {
            if let Some(path) = node.child_by_field_name("path") {
                let text = source.get(path.byte_range()).unwrap_or_default();
                if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
                    out.push(inner.to_string());
                }
            }
            continue;
        }
        if node.parent().is_none() || node.kind().starts_with("preproc_") {
            let mut cursor = node.walk();
            stack.extend(
                node.named_children(&mut cursor)
                    .filter(|child| child.kind().starts_with("preproc_")),
            );
        }
    }
    out
}
