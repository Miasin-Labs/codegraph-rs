use super::*;

// -- "should match qualified name references" ----------------------------
#[test]
fn matches_qualified_name_references() {
    let class_node = node(
        "class:user.ts:User:5",
        NodeKind::Class,
        "User",
        "user.ts::User",
        "user.ts",
        Language::Typescript,
        5,
        30,
    );
    let method_node = node(
        "method:user.ts:User.save:15",
        NodeKind::Method,
        "save",
        "user.ts::User::save",
        "user.ts",
        Language::Typescript,
        15,
        25,
    );
    let ctx = Fixture::new(vec![class_node, method_node]);

    let r = make_ref(
        "User.save",
        EdgeKind::Calls,
        5,
        "main.ts",
        Language::Typescript,
    );
    let result = match_reference(&r, &ctx).expect("should resolve");

    assert_eq!(result.target_node_id, "method:user.ts:User.save:15");
}

// -- Rust-side coverage: partial qualified-name suffix match --------------
#[test]
fn qualified_name_partial_suffix_match() {
    let method_node = node(
        "method:src/user.ts:User::save:15",
        NodeKind::Method,
        "save",
        "src/user.ts::User::save",
        "src/user.ts",
        Language::Typescript,
        15,
        25,
    );
    let ctx = Fixture::new(vec![method_node]);
    let r = make_ref(
        "User::save",
        EdgeKind::Calls,
        5,
        "main.ts",
        Language::Typescript,
    );
    let result = match_by_qualified_name(&r, &ctx).expect("should resolve");
    assert_eq!(result.target_node_id, "method:src/user.ts:User::save:15");
    assert_eq!(result.confidence, 0.85);
    assert_eq!(result.resolved_by, ResolvedBy::QualifiedName);
}
