use super::*;

#[test]
fn py_imports_simple() {
    let result = extract("utils.py", "import json");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "json");
}

#[test]
fn py_imports_from() {
    let result = extract("utils.py", "from os import path");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "os");
    assert!(sig(import_node).contains("path"));
}

#[test]
fn py_imports_multiple_from_same_module() {
    let result = extract("types.py", "from typing import List, Dict, Optional");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "typing");
    assert!(sig(import_node).contains("List"));
    assert!(sig(import_node).contains("Dict"));
}

#[test]
fn py_imports_multiple_statements() {
    let code = "
import os
import sys
";
    let result = extract("main.py", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 2);
    let import_names = names(&imports);
    assert!(import_names.contains(&"os".to_string()));
    assert!(import_names.contains(&"sys".to_string()));
}

#[test]
fn py_imports_aliased() {
    let result = extract("data.py", "import numpy as np");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "numpy");
    assert!(sig(import_node).contains("as np"));
}

#[test]
fn py_imports_relative() {
    let result = extract("module.py", "from .utils import helper");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, ".utils");
    assert!(sig(import_node).contains("helper"));
}

#[test]
fn py_imports_wildcard() {
    let result = extract("types.py", "from typing import *");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "typing");
    assert!(sig(import_node).contains("*"));
}

// --- Rust imports ---
