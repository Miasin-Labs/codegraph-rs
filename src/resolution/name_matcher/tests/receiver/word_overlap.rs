use super::super::*;

#[test]
fn capitalized_receiver_finds_class_method() {
    let class_node = node(
        "class:src/engine.ts:PermissionEngine:1",
        NodeKind::Class,
        "PermissionEngine",
        "src/engine.ts::PermissionEngine",
        "src/engine.ts",
        Language::Typescript,
        1,
        40,
    );
    let method_node = node(
        "method:src/engine.ts:PermissionEngine::check:5",
        NodeKind::Method,
        "check",
        "src/engine.ts::PermissionEngine::check",
        "src/engine.ts",
        Language::Typescript,
        5,
        10,
    );
    let ctx = Fixture::new(vec![class_node, method_node]);

    let r = make_ref(
        "permissionEngine.check",
        EdgeKind::Calls,
        7,
        "src/main.ts",
        Language::Typescript,
    );
    let result = match_reference(&r, &ctx).expect("should resolve");
    assert_eq!(
        result.target_node_id,
        "method:src/engine.ts:PermissionEngine::check:5"
    );
    assert_eq!(result.resolved_by, ResolvedBy::InstanceMethod);
    assert_eq!(result.confidence, 0.8);
}

#[test]
fn split_camel_case_matches_ts_behavior() {
    assert_eq!(
        split_camel_case("permissionEngine"),
        vec!["permission".to_string(), "Engine".to_string()]
    );
    assert_eq!(
        split_camel_case("src/engine.ts::PermissionRuleEngine::check"),
        vec![
            "src".to_string(),
            "engine".to_string(),
            "ts".to_string(),
            "Permission".to_string(),
            "Rule".to_string(),
            "Engine".to_string(),
            "check".to_string()
        ]
    );
    assert_eq!(
        split_camel_case("HTTPServer"),
        vec!["HTTP".to_string(), "Server".to_string()]
    );
}
