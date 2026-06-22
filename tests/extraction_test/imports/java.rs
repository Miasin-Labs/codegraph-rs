use super::*;

#[test]
fn java_imports_simple() {
    let result = extract("Main.java", "import java.util.List;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.util.List");
    assert_eq!(sig(import_node), "import java.util.List;");
}

#[test]
fn java_imports_static() {
    let result = extract(
        "Utils.java",
        "import static java.util.Collections.emptyList;",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.util.Collections.emptyList");
    assert!(sig(import_node).contains("static"));
}

#[test]
fn java_imports_wildcard() {
    let result = extract("App.java", "import java.util.*;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.util");
    assert!(sig(import_node).contains(".*"));
}

#[test]
fn java_imports_nested_class() {
    let result = extract("MapUtil.java", "import java.util.Map.Entry;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.util.Map.Entry");
}

#[test]
fn java_imports_multiple() {
    let code = "
import java.util.List;
import java.util.Map;
import java.io.IOException;
";
    let result = extract("Service.java", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"java.util.List".to_string()));
    assert!(import_names.contains(&"java.util.Map".to_string()));
    assert!(import_names.contains(&"java.io.IOException".to_string()));
}

// --- C# imports ---
