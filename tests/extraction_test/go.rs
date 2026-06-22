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
