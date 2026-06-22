use super::*;

#[test]
fn cpp_imports_system_include() {
    let result = extract("main.cpp", "#include <iostream>");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "iostream");
    assert_eq!(sig(import_node), "#include <iostream>");
}

#[test]
fn cpp_imports_system_include_with_path() {
    let result = extract("app.cpp", "#include <nlohmann/json.hpp>");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "nlohmann/json.hpp");
}

#[test]
fn cpp_imports_local_include() {
    let result = extract("main.cpp", "#include \"myheader.h\"");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "myheader.h");
}

#[test]
fn c_imports_header() {
    let result = extract("main.c", "#include <stdio.h>");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "stdio.h");
}

#[test]
fn cpp_imports_multiple_includes() {
    let code = "
#include <iostream>
#include <vector>
#include \"config.h\"
";
    let result = extract("app.cpp", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"iostream".to_string()));
    assert!(import_names.contains(&"vector".to_string()));
    assert!(import_names.contains(&"config.h".to_string()));
}

#[test]
fn cpp_imports_creates_unresolved_references_for_local_includes() {
    let result = extract("main.cpp", "#include \"myheader.h\"");
    let import_ref = find_ref(&result, EdgeKind::Imports, "myheader.h").expect("imports ref");
    assert_eq!(import_ref.line, 1);
}

#[test]
fn cpp_imports_creates_unresolved_references_for_system_includes() {
    let result = extract("main.cpp", "#include <iostream>");
    assert!(find_ref(&result, EdgeKind::Imports, "iostream").is_some());
}

// --- Dart imports ---
