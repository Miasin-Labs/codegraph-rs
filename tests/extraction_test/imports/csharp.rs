use super::*;

#[test]
fn csharp_imports_simple_using() {
    let result = extract("Program.cs", "using System;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "System");
    assert_eq!(sig(import_node), "using System;");
}

#[test]
fn csharp_imports_qualified_using() {
    let result = extract("Utils.cs", "using System.Collections.Generic;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "System.Collections.Generic");
}

#[test]
fn csharp_imports_static_using() {
    let result = extract("App.cs", "using static System.Console;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "System.Console");
    assert!(sig(import_node).contains("static"));
}

#[test]
fn csharp_imports_alias_using() {
    let result = extract(
        "Types.cs",
        "using MyList = System.Collections.Generic.List<int>;",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "System.Collections.Generic.List<int>");
    assert!(sig(import_node).contains("MyList ="));
}

#[test]
fn csharp_imports_multiple_usings() {
    let code = "
using System;
using System.Threading.Tasks;
using Microsoft.Extensions.DependencyInjection;
";
    let result = extract("Service.cs", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"System".to_string()));
    assert!(import_names.contains(&"System.Threading.Tasks".to_string()));
    assert!(import_names.contains(&"Microsoft.Extensions.DependencyInjection".to_string()));
}

// --- PHP imports ---
