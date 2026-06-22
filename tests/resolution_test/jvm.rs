use crate::fixture::*;

#[test]
fn java_import_disambiguates_same_name_classes_across_modules_314() {
    let fx = Fx::new();
    let q = fx.q();
    let dao = "dao/src/main/java/com/example/dao/converter/FooConverter.java";
    let service = "service/src/main/java/com/example/service/converter/FooConverter.java";
    let web = "web/src/main/java/com/example/web/Handler.java";
    fx.write(
        dao,
        "package com.example.dao.converter;\npublic class FooConverter { public String convert(String x) { return \"dao:\" + x; } }\n",
    );
    fx.write(
        service,
        "package com.example.service.converter;\npublic class FooConverter { public String convert(String x) { return \"svc:\" + x; } }\n",
    );
    // The caller imports the SERVICE version.
    fx.write(
        web,
        "package com.example.web;\n\nimport com.example.service.converter.FooConverter;\n\npublic class Handler {\n  private FooConverter fooConverter;\n  public String use() { return fooConverter.convert(\"input\"); }\n}\n",
    );
    for f in [dao, service, web] {
        fx.track(&q, f, Language::Java);
    }

    let dao_class = exported(node(
        "class:dao:FooConverter:2",
        NodeKind::Class,
        "FooConverter",
        "com.example.dao.converter::FooConverter",
        dao,
        Language::Java,
        2,
        2,
    ));
    let dao_convert = node(
        "method:dao:FooConverter.convert:2",
        NodeKind::Method,
        "convert",
        "com.example.dao.converter::FooConverter::convert",
        dao,
        Language::Java,
        2,
        2,
    );
    let service_class = exported(node(
        "class:service:FooConverter:2",
        NodeKind::Class,
        "FooConverter",
        "com.example.service.converter::FooConverter",
        service,
        Language::Java,
        2,
        2,
    ));
    let service_convert = node(
        "method:service:FooConverter.convert:2",
        NodeKind::Method,
        "convert",
        "com.example.service.converter::FooConverter::convert",
        service,
        Language::Java,
        2,
        2,
    );
    let handler_class = exported(node(
        "class:web:Handler:5",
        NodeKind::Class,
        "Handler",
        "com.example.web::Handler",
        web,
        Language::Java,
        5,
        8,
    ));
    let mut field = node(
        "field:web:Handler.fooConverter:6",
        NodeKind::Field,
        "fooConverter",
        "com.example.web::Handler::fooConverter",
        web,
        Language::Java,
        6,
        6,
    );
    field.signature = Some("FooConverter fooConverter".to_string());
    let use_method = node(
        "method:web:Handler.use:7",
        NodeKind::Method,
        "use",
        "com.example.web::Handler::use",
        web,
        Language::Java,
        7,
        7,
    );
    q.insert_nodes(&[
        dao_class,
        dao_convert,
        service_class,
        service_convert.clone(),
        handler_class,
        field,
        use_method.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &use_method.id,
        "fooConverter.convert",
        EdgeKind::Calls,
        7,
        web,
        Language::Java,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let calls = outgoing(&q, &use_method.id, EdgeKind::Calls);
    assert!(!calls.is_empty());
    let target = q.get_node_by_id(&calls[0].target).unwrap().unwrap();
    assert_eq!(target.name, "convert");
    // The import must trump candidate order — even though dao is
    // lexically first.
    assert_eq!(target.id, service_convert.id);
    assert_eq!(target.file_path.replace('\\', "/"), service);
}

#[test]
fn resolve_one_skips_jvm_namespace_segments_but_not_types() {
    let fx = Fx::new();
    let q = fx.q();
    let caller = node(
        "method:src/Main.java:Main::run:1",
        NodeKind::Method,
        "run",
        "src/Main.java::Main::run",
        "src/Main.java",
        Language::Java,
        1,
        5,
    );
    let target = node(
        "class:src/com/example/Builder.java:Builder:1",
        NodeKind::Class,
        "Builder",
        "src/com/example/Builder.java::Builder",
        "src/com/example/Builder.java",
        Language::Java,
        1,
        20,
    );
    q.insert_nodes(&[caller, target]).unwrap();

    let resolver = fx.resolver();
    resolver.warm_caches();
    let package_ref = UnresolvedRef {
        from_node_id: "method:src/Main.java:Main::run:1".to_string(),
        reference_name: "org".to_string(),
        reference_kind: EdgeKind::References,
        line: 1,
        column: 0,
        file_path: "src/Main.java".to_string(),
        language: Language::Java,
        candidates: None,
    };
    assert!(resolver.resolve_one(&package_ref).is_none());

    let type_ref = UnresolvedRef {
        reference_name: "Builder".to_string(),
        ..package_ref
    };
    let resolved = resolver.resolve_one(&type_ref).expect("type resolves");
    assert_eq!(
        resolved.target_node_id,
        "class:src/com/example/Builder.java:Builder:1"
    );

    let external_call = UnresolvedRef {
        from_node_id: "method:src/Main.java:Main::run:1".to_string(),
        reference_name: "assertEquals".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 2,
        column: 0,
        file_path: "src/Main.java".to_string(),
        language: Language::Kotlin,
        candidates: None,
    };
    assert!(resolver.resolve_one(&external_call).is_none());

    let stdlib_import = UnresolvedRef {
        reference_name: "java.util.List".to_string(),
        reference_kind: EdgeKind::Imports,
        language: Language::Java,
        ..external_call.clone()
    };
    assert!(resolver.resolve_one(&stdlib_import).is_none());

    let stdlib_type = UnresolvedRef {
        reference_name: "String".to_string(),
        reference_kind: EdgeKind::References,
        language: Language::Java,
        ..external_call
    };
    assert!(resolver.resolve_one(&stdlib_type).is_none());
}

#[test]
fn resolve_one_keeps_project_classes_that_match_jvm_stdlib_names() {
    let fx = Fx::new();
    let q = fx.q();
    let caller = node(
        "method:src/Main.java:Main::run:1",
        NodeKind::Method,
        "run",
        "src/Main.java::Main::run",
        "src/Main.java",
        Language::Java,
        1,
        5,
    );
    let local_string = node(
        "class:src/String.java:String:1",
        NodeKind::Class,
        "String",
        "src/String.java::String",
        "src/String.java",
        Language::Java,
        1,
        20,
    );
    q.insert_nodes(&[caller, local_string]).unwrap();

    let resolver = fx.resolver();
    resolver.warm_caches();
    let type_ref = UnresolvedRef {
        from_node_id: "method:src/Main.java:Main::run:1".to_string(),
        reference_name: "String".to_string(),
        reference_kind: EdgeKind::References,
        line: 2,
        column: 0,
        file_path: "src/Main.java".to_string(),
        language: Language::Java,
        candidates: None,
    };
    let resolved = resolver
        .resolve_one(&type_ref)
        .expect("local String resolves");
    assert_eq!(resolved.target_node_id, "class:src/String.java:String:1");
}
