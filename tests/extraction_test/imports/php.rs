use super::*;

#[test]
fn php_imports_simple_use() {
    let result = extract("Test.php", "<?php use PHPUnit\\Framework\\TestCase;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "PHPUnit\\Framework\\TestCase");
}

#[test]
fn php_imports_aliased_use() {
    let result = extract("Test.php", "<?php use Mockery as m;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "Mockery");
    assert!(sig(import_node).contains("as m"));
}

#[test]
fn php_imports_function_use() {
    let result = extract(
        "helpers.php",
        "<?php use function Illuminate\\Support\\env;",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "Illuminate\\Support\\env");
    assert!(sig(import_node).contains("function"));
}

#[test]
fn php_imports_grouped_use() {
    let result = extract(
        "Models.php",
        "<?php use Illuminate\\Database\\{Model, Builder};",
    );
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 2);
    let import_names = names(&imports);
    assert!(import_names.contains(&"Illuminate\\Database\\Model".to_string()));
    assert!(import_names.contains(&"Illuminate\\Database\\Builder".to_string()));
}

#[test]
fn php_imports_multiple_uses() {
    let code = "<?php
use Illuminate\\Support\\Collection;
use Illuminate\\Support\\Str;
use Closure;
";
    let result = extract("Service.php", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"Illuminate\\Support\\Collection".to_string()));
    assert!(import_names.contains(&"Illuminate\\Support\\Str".to_string()));
    assert!(import_names.contains(&"Closure".to_string()));
}

// --- Ruby imports ---
