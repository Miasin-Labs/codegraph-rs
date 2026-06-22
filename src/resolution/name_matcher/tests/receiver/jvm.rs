use super::super::*;

#[test]
fn java_field_receiver_type_resolves_via_field_signature() {
    let class_node = node(
        "class:src/A.java:A:1",
        NodeKind::Class,
        "A",
        "src/A.java::A",
        "src/A.java",
        Language::Java,
        1,
        50,
    );
    let mut field_node = node(
        "field:src/A.java:A::userbo:3",
        NodeKind::Field,
        "userbo",
        "src/A.java::A::userbo",
        "src/A.java",
        Language::Java,
        3,
        3,
    );
    field_node.signature = Some("UserBO userbo".into());
    let method = node(
        "method:src/UserBO.java:UserBO::getUser:8",
        NodeKind::Method,
        "getUser",
        "src/UserBO.java::UserBO::getUser",
        "src/UserBO.java",
        Language::Java,
        8,
        12,
    );
    let ctx = Fixture::new(vec![class_node, field_node, method]);

    let r = make_ref(
        "userbo.getUser",
        EdgeKind::Calls,
        10,
        "src/A.java",
        Language::Java,
    );
    let result = match_reference(&r, &ctx).expect("should resolve");
    assert_eq!(
        result.target_node_id,
        "method:src/UserBO.java:UserBO::getUser:8"
    );
    assert_eq!(result.resolved_by, ResolvedBy::InstanceMethod);
    assert_eq!(result.confidence, 0.9);
}

#[test]
fn java_preferred_fqn_disambiguates_same_named_classes() {
    let class_node = node(
        "class:src/com/x/Caller.java:Caller:1",
        NodeKind::Class,
        "Caller",
        "src/com/x/Caller.java::Caller",
        "src/com/x/Caller.java",
        Language::Java,
        1,
        50,
    );
    let mut field_node = node(
        "field:src/com/x/Caller.java:Caller::conv:3",
        NodeKind::Field,
        "conv",
        "src/com/x/Caller.java::Caller::conv",
        "src/com/x/Caller.java",
        Language::Java,
        3,
        3,
    );
    field_node.signature = Some("FooConverter conv".into());
    let dao_method = node(
        "method:src/com/x/dao/converter/FooConverter.java:convert",
        NodeKind::Method,
        "convert",
        "src/com/x/dao/converter/FooConverter.java::FooConverter::convert",
        "src/com/x/dao/converter/FooConverter.java",
        Language::Java,
        5,
        9,
    );
    let service_method = node(
        "method:src/com/x/service/converter/FooConverter.java:convert",
        NodeKind::Method,
        "convert",
        "src/com/x/service/converter/FooConverter.java::FooConverter::convert",
        "src/com/x/service/converter/FooConverter.java",
        Language::Java,
        5,
        9,
    );
    let mut ctx = Fixture::new(vec![class_node, field_node, dao_method, service_method]);
    ctx.imports.push(ImportMapping {
        local_name: "FooConverter".into(),
        exported_name: "FooConverter".into(),
        source: "com.x.service.converter.FooConverter".into(),
        is_default: false,
        is_namespace: false,
        resolved_path: None,
    });

    let r = make_ref(
        "conv.convert",
        EdgeKind::Calls,
        10,
        "src/com/x/Caller.java",
        Language::Java,
    );
    let result = match_reference(&r, &ctx).expect("should resolve");
    assert_eq!(
        result.target_node_id,
        "method:src/com/x/service/converter/FooConverter.java:convert"
    );
}
