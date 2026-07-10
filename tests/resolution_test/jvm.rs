use crate::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn java_import_disambiguates_same_name_classes_across_modules_314() {
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
        .await
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

#[tokio::test(flavor = "current_thread")]
async fn kotlin_imports_without_semicolon_resolve_top_level_symbols_by_qualified_name() {
    let fx = Fx::new();
    let q = fx.q();
    let helper_file = "src/main/kotlin/com/example/foo/Helpers.kt";
    let caller_file = "src/main/kotlin/com/example/app/Use.kt";
    fx.write(
        helper_file,
        "package com.example.foo\nfun util(): String = \"ok\"\nfun aliased(): String = \"ok\"\n",
    );
    fx.write(
        caller_file,
        "package com.example.app\nimport com.example.foo.util\nimport com.example.foo.aliased as doUtil\nfun use() = util()\nfun useAlias() = doUtil()\n",
    );
    for f in [helper_file, caller_file] {
        fx.track(&q, f, Language::Kotlin);
    }

    let util = node(
        "fn:kotlin:util:2",
        NodeKind::Function,
        "util",
        "com.example.foo::util",
        helper_file,
        Language::Kotlin,
        2,
        2,
    );
    let aliased = node(
        "fn:kotlin:aliased:3",
        NodeKind::Function,
        "aliased",
        "com.example.foo::aliased",
        helper_file,
        Language::Kotlin,
        3,
        3,
    );
    let use_fn = node(
        "fn:kotlin:use:4",
        NodeKind::Function,
        "use",
        "com.example.app::use",
        caller_file,
        Language::Kotlin,
        4,
        4,
    );
    let use_alias = node(
        "fn:kotlin:useAlias:5",
        NodeKind::Function,
        "useAlias",
        "com.example.app::useAlias",
        caller_file,
        Language::Kotlin,
        5,
        5,
    );
    q.insert_nodes(&[
        util.clone(),
        aliased.clone(),
        use_fn.clone(),
        use_alias.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[
        uref(
            &use_fn.id,
            "util",
            EdgeKind::Calls,
            4,
            caller_file,
            Language::Kotlin,
        ),
        uref(
            &use_alias.id,
            "doUtil",
            EdgeKind::Calls,
            5,
            caller_file,
            Language::Kotlin,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let direct = outgoing(&q, &use_fn.id, EdgeKind::Calls);
    assert_eq!(direct.len(), 1, "got {direct:?}");
    assert_eq!(direct[0].target, util.id);
    let alias = outgoing(&q, &use_alias.id, EdgeKind::Calls);
    assert_eq!(alias.len(), 1, "got {alias:?}");
    assert_eq!(alias[0].target, aliased.id);
}

#[tokio::test(flavor = "current_thread")]
async fn resolve_one_skips_jvm_namespace_segments_but_not_types() {
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
        metadata: None,
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
        metadata: None,
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

#[tokio::test(flavor = "current_thread")]
async fn resolve_one_keeps_project_classes_that_match_jvm_stdlib_names() {
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
        metadata: None,
    };
    let resolved = resolver
        .resolve_one(&type_ref)
        .expect("local String resolves");
    assert_eq!(resolved.target_node_id, "class:src/String.java:String:1");
}
