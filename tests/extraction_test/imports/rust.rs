use super::*;

#[test]
fn rust_imports_simple_use_declaration() {
    let result = extract("main.rs", "use std::io;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "std");
    assert_eq!(sig(import_node), "use std::io;");
}

#[test]
fn rust_imports_scoped_use_list() {
    let result = extract("main.rs", "use std::{ffi::OsStr, io, path::Path};");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "std");
    assert!(sig(import_node).contains("ffi::OsStr"));
    assert!(sig(import_node).contains("path::Path"));
}

#[test]
fn rust_imports_crate() {
    let result = extract("lib.rs", "use crate::error::Error;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "crate");
}

#[test]
fn rust_imports_super() {
    let result = extract("submod.rs", "use super::utils;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "super");
}

#[test]
fn rust_imports_external_crate() {
    let result = extract("types.rs", "use serde::{Serialize, Deserialize};");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "serde");
    assert!(sig(import_node).contains("Serialize"));
    assert!(sig(import_node).contains("Deserialize"));
}

// --- Go imports ---
