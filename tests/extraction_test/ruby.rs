use crate::extraction_test::fixture::*;

#[test]
fn ruby_imports_require() {
    let result = extract("app.rb", "require 'json'");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "json");
    assert_eq!(sig(import_node), "require 'json'");
}

#[test]
fn ruby_imports_require_with_path() {
    let result = extract("config.rb", "require 'active_support/core_ext/string'");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "active_support/core_ext/string");
}

#[test]
fn ruby_imports_require_relative() {
    let result = extract("test/my_test.rb", "require_relative '../test_helper'");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "../test_helper");
    assert!(sig(import_node).contains("require_relative"));
}

#[test]
fn ruby_imports_does_not_extract_non_require_calls() {
    let result = extract("app.rb", "puts 'hello'");
    assert!(first_import(&result).is_none());
}

#[test]
fn ruby_imports_multiple_requires() {
    let code = "
require 'json'
require 'yaml'
require_relative 'helper'
";
    let result = extract("lib.rb", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"json".to_string()));
    assert!(import_names.contains(&"yaml".to_string()));
    assert!(import_names.contains(&"helper".to_string()));
}

// --- Ruby modules ---

#[test]
fn ruby_modules_extracts_module_as_module_node_with_containment() {
    let code = "
module CachedCounting
  def self.disable
    @enabled = false
  end

  def perform_increment!(key, count)
    write_cache!(key, count)
  end
end
";
    let result = extract("concerns/cached_counting.rb", code);

    let module_node =
        find_named(&result, NodeKind::Module, "CachedCounting").expect("CachedCounting");
    assert_eq!(module_node.qualified_name, "CachedCounting");

    // Methods inside module should have module-qualified names
    let disable_method = result
        .nodes
        .iter()
        .find(|n| n.name == "disable" && n.kind == NodeKind::Method)
        .expect("disable");
    assert_eq!(disable_method.qualified_name, "CachedCounting::disable");

    let increment_method = result
        .nodes
        .iter()
        .find(|n| n.name == "perform_increment!" && n.kind == NodeKind::Method)
        .expect("perform_increment!");
    assert_eq!(
        increment_method.qualified_name,
        "CachedCounting::perform_increment!"
    );

    // Containment edge from module to methods
    let contains_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.source == module_node.id && e.kind == EdgeKind::Contains)
        .collect();
    assert!(contains_edges.len() >= 2);
}

#[test]
fn ruby_modules_handles_nested_modules_with_classes() {
    let code = "
module Discourse
  module Auth
    class AuthProvider
      def authenticate(params)
        validate(params)
      end
    end
  end
end
";
    let result = extract("lib/auth.rb", code);

    assert!(find_named(&result, NodeKind::Module, "Discourse").is_some());

    let auth_module = find_named(&result, NodeKind::Module, "Auth").expect("Auth");
    assert_eq!(auth_module.qualified_name, "Discourse::Auth");

    let auth_provider = find_named(&result, NodeKind::Class, "AuthProvider").expect("AuthProvider");
    assert_eq!(
        auth_provider.qualified_name,
        "Discourse::Auth::AuthProvider"
    );

    let auth_method = result
        .nodes
        .iter()
        .find(|n| n.name == "authenticate")
        .expect("authenticate");
    assert_eq!(
        auth_method.qualified_name,
        "Discourse::Auth::AuthProvider::authenticate"
    );
}

// --- C/C++ imports ---
