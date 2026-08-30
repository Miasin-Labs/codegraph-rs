use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Go Extraction')
// =============================================================================

#[test]
fn go_extracts_function_declarations() {
    let code = r#"
package main

func ProcessOrder(order Order) (Receipt, error) {
    // Process the order
    return Receipt{}, nil
}
"#;
    let result = extract("main.go", code);

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "ProcessOrder");
}

#[test]
fn go_extracts_method_declarations() {
    let code = r#"
package main

type Service struct {
    db *Database
}

func (s *Service) GetUser(id string) (*User, error) {
    return s.db.FindUser(id)
}
"#;
    let result = extract("service.go", code);

    let method_node = find_kind(&result, NodeKind::Method).expect("method");
    assert_eq!(method_node.name, "GetUser");
}

// =============================================================================
// describe('Go const/var extraction (grouped var + isExported)')
// Ported from __tests__/extraction.test.ts (PR #1521 defects A + B).
// =============================================================================

#[test]
fn go_extracts_every_entry_in_a_grouped_var_block() {
    // Defect B: a grouped `var ( ... )` wraps its specs in a `var_spec_list`
    // node (tree-sitter-go asymmetry vs grouped const), so they were skipped.
    let code = "package main\n\nvar (\n  A = 1\n  b = 2\n)";
    let result = extract("main.go", code);
    let names = names(&filter_kind(&result, NodeKind::Variable));
    assert!(names.contains(&"A".to_string()), "names={names:?}");
    assert!(names.contains(&"b".to_string()), "names={names:?}");
}

#[test]
fn go_does_not_regress_grouped_const_extraction() {
    let code = "package main\n\nconst (\n  C = 1\n  D = 2\n)";
    let result = extract("main.go", code);
    let mut names = names(&filter_kind(&result, NodeKind::Constant));
    names.sort();
    assert_eq!(names, vec!["C".to_string(), "D".to_string()]);
}

#[test]
fn go_sets_is_exported_by_leading_case_for_all_var_const_forms() {
    // Defect A: Go const/variable isExported was always false. All four forms.
    let code = "package main\n\nvar SingleVar = 1\nvar (\n  GroupedA = 1\n  groupedB = 2\n)\nconst SingleConst = 1\nconst (\n  ConstA = 1\n  constB = 2\n)";
    let result = extract("main.go", code);
    let node = |kind: NodeKind, name: &str| {
        result
            .nodes
            .iter()
            .find(|n| n.kind == kind && n.name == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    assert_eq!(
        node(NodeKind::Variable, "SingleVar").is_exported,
        Some(true)
    );
    assert_eq!(node(NodeKind::Variable, "GroupedA").is_exported, Some(true));
    assert_eq!(
        node(NodeKind::Variable, "groupedB").is_exported,
        Some(false)
    );
    assert_eq!(
        node(NodeKind::Constant, "SingleConst").is_exported,
        Some(true)
    );
    assert_eq!(node(NodeKind::Constant, "ConstA").is_exported, Some(true));
    assert_eq!(node(NodeKind::Constant, "constB").is_exported, Some(false));
}

#[test]
fn go_sets_is_exported_on_methods_by_leading_case() {
    // Defect A (method half): extractMethod never called the isExported hook.
    let code = "package main\n\ntype T struct {}\n\nfunc (r *T) Exported() int { return 1 }\nfunc (r *T) unexported() int { return 2 }\n";
    let result = extract("thing.go", code);
    let exp = find_named(&result, NodeKind::Method, "Exported").expect("Exported");
    let unexp = find_named(&result, NodeKind::Method, "unexported").expect("unexported");
    assert_eq!(exp.is_exported, Some(true));
    assert_eq!(unexp.is_exported, Some(false));
}

#[test]
fn go_var_const_is_exported_does_not_change_other_languages() {
    // TypeScript: export-marked const is exported, plain const is not.
    let ts = extract("a.ts", "export const Exp = 1;\nconst priv = 2;\n");
    assert_eq!(
        ts.nodes
            .iter()
            .find(|n| n.kind == NodeKind::Constant && n.name == "Exp")
            .and_then(|n| n.is_exported),
        Some(true)
    );
    let priv_exported = ts
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Constant && n.name == "priv")
        .and_then(|n| n.is_exported);
    assert!(priv_exported != Some(true), "priv should not be exported");

    // Python has no isExported predicate — symbols stay unset (unchanged).
    let py = extract("a.py", "MAX = 100\ncounter = 0\n");
    let py_max = py
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Constant | NodeKind::Variable) && n.name == "MAX");
    assert!(py_max.is_some());
    assert_ne!(py_max.unwrap().is_exported, Some(true));
}
