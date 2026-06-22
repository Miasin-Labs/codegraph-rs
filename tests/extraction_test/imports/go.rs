use super::*;

#[test]
fn go_imports_single() {
    let code = "
package main

import \"fmt\"
";
    let result = extract("main.go", code);
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "fmt");
}

#[test]
fn go_imports_grouped() {
    let code = "
package main

import (
\t\"fmt\"
\t\"os\"
\t\"encoding/json\"
)
";
    let result = extract("main.go", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"fmt".to_string()));
    assert!(import_names.contains(&"os".to_string()));
    assert!(import_names.contains(&"encoding/json".to_string()));
}

#[test]
fn go_imports_aliased() {
    let code = "
package main

import f \"fmt\"
";
    let result = extract("main.go", code);
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "fmt");
    assert!(sig(import_node).contains("f"));
}

#[test]
fn go_imports_dot() {
    let code = "
package main

import . \"math\"
";
    let result = extract("main.go", code);
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "math");
    assert!(sig(import_node).contains("."));
}

#[test]
fn go_imports_blank() {
    let code = "
package main

import _ \"github.com/go-sql-driver/mysql\"
";
    let result = extract("main.go", code);
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "github.com/go-sql-driver/mysql");
    assert!(sig(import_node).contains("_"));
}

// --- Swift imports ---
