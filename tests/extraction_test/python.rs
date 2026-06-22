use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Python Extraction')
// =============================================================================

#[test]
fn python_extracts_function_definitions() {
    let code = r#"
def calculate_total(items: list, tax_rate: float) -> float:
    """Calculate total with tax."""
    subtotal = sum(item.price for item in items)
    return subtotal * (1 + tax_rate)
"#;
    let result = extract("calc.py", code);

    assert!(find_kind(&result, NodeKind::File).is_some());

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "calculate_total");
    assert_eq!(func_node.language, Language::Python);
}

#[test]
fn python_extracts_class_definitions() {
    let code = r#"
class UserService:
    """Service for managing users."""

    def __init__(self, db):
        self.db = db

    def get_user(self, user_id: str) -> User:
        return self.db.find_user(user_id)
"#;
    let result = extract("service.py", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "UserService");
}
