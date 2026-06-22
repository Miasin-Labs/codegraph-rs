use crate::extraction_test::fixture::*;

// describe('PHP Extraction')
// =============================================================================

#[test]
fn php_extracts_class_declarations() {
    let code = r#"<?php

class UserController
{
    private UserService $userService;

    public function __construct(UserService $userService)
    {
        $this->userService = $userService;
    }

    public function show(string $id): User
    {
        return $this->userService->find($id);
    }
}
"#;
    let result = extract("UserController.php", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "UserController");
}

#[test]
fn php_extracts_class_inheritance_extends_and_interface_implementation() {
    let code = r#"<?php

class ChildController extends BaseController implements Serializable, JsonSerializable
{
    public function serialize(): string
    {
        return json_encode($this);
    }
}
"#;
    let result = extract("ChildController.php", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "ChildController");

    let extends_ref = refs_of_kind(&result, EdgeKind::Extends);
    assert_eq!(
        extends_ref.first().map(|r| r.reference_name.as_str()),
        Some("BaseController")
    );

    let implements_refs = refs_of_kind(&result, EdgeKind::Implements);
    assert_eq!(implements_refs.len(), 2);
    let impl_names = ref_names(&implements_refs);
    assert!(impl_names.contains(&"Serializable".to_string()));
    assert!(impl_names.contains(&"JsonSerializable".to_string()));
}

// =============================================================================
