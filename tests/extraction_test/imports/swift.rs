use super::*;

#[test]
fn swift_imports_simple() {
    let result = extract("main.swift", "import Foundation");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "Foundation");
    assert_eq!(sig(import_node), "import Foundation");
}

#[test]
fn swift_imports_testable() {
    let result = extract("Tests.swift", "@testable import Alamofire");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "Alamofire");
    assert!(sig(import_node).contains("@testable"));
}

#[test]
fn swift_imports_preconcurrency() {
    let result = extract("Auth.swift", "@preconcurrency import Security");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "Security");
}

#[test]
fn swift_imports_multiple() {
    let code = "
import Foundation
import UIKit
import Alamofire
";
    let result = extract("App.swift", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"Foundation".to_string()));
    assert!(import_names.contains(&"UIKit".to_string()));
    assert!(import_names.contains(&"Alamofire".to_string()));
}

// --- Kotlin imports ---
