use super::*;

#[test]
fn dart_imports_dart_scheme() {
    let result = extract("main.dart", "import 'dart:async';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "dart:async");
    assert_eq!(sig(import_node), "import 'dart:async';");
}

#[test]
fn dart_imports_package() {
    let result = extract("app.dart", "import 'package:flutter/material.dart';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "package:flutter/material.dart");
}

#[test]
fn dart_imports_aliased() {
    let result = extract("api.dart", "import 'package:http/http.dart' as http;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "package:http/http.dart");
    assert!(sig(import_node).contains("as http"));
}

#[test]
fn dart_imports_multiple() {
    let code = "
import 'dart:async';
import 'dart:convert';
import 'package:flutter/material.dart';
";
    let result = extract("main.dart", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"dart:async".to_string()));
    assert!(import_names.contains(&"dart:convert".to_string()));
    assert!(import_names.contains(&"package:flutter/material.dart".to_string()));
}

#[test]
fn dart_imports_relative() {
    let result = extract("lib/main.dart", "import '../utils/helpers.dart';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "../utils/helpers.dart");
}

// --- Liquid imports ---
