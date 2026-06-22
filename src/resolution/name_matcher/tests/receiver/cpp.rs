use super::super::*;

#[test]
fn cpp_receiver_type_inference_resolves_out_of_line_method() {
    let method = node(
        "method:src/logger.cpp:Logger::flush:12",
        NodeKind::Method,
        "flush",
        "src/logger.cpp::Logger::flush",
        "src/logger.cpp",
        Language::Cpp,
        12,
        20,
    );
    let mut ctx = Fixture::new(vec![method]);
    ctx.files.insert(
        "src/a.cpp".into(),
        "Logger logger;\nlogger.flush();\n".into(),
    );

    let r = make_ref(
        "logger.flush",
        EdgeKind::Calls,
        2,
        "src/a.cpp",
        Language::Cpp,
    );
    let result = match_reference(&r, &ctx).expect("should resolve");
    assert_eq!(
        result.target_node_id,
        "method:src/logger.cpp:Logger::flush:12"
    );
    assert_eq!(result.resolved_by, ResolvedBy::InstanceMethod);
    assert_eq!(result.confidence, 0.9);
}

#[test]
fn cpp_receiver_type_inference_falls_back_to_header() {
    let method = node(
        "method:src/logger.cpp:Logger::flush:12",
        NodeKind::Method,
        "flush",
        "src/logger.cpp::Logger::flush",
        "src/logger.cpp",
        Language::Cpp,
        12,
        20,
    );
    let mut ctx = Fixture::new(vec![method]);
    ctx.files
        .insert("src/a.cpp".into(), "logger.flush();\n".into());
    ctx.files
        .insert("src/a.h".into(), "class A {\n  Logger logger;\n};\n".into());

    let r = make_ref(
        "logger.flush",
        EdgeKind::Calls,
        1,
        "src/a.cpp",
        Language::Cpp,
    );
    let result = match_reference(&r, &ctx).expect("should resolve");
    assert_eq!(
        result.target_node_id,
        "method:src/logger.cpp:Logger::flush:12"
    );
    assert_eq!(result.resolved_by, ResolvedBy::InstanceMethod);
}

#[test]
fn cpp_keyword_before_receiver_is_not_a_type() {
    assert_eq!(normalize_cpp_type_name("return"), None);
    assert_eq!(
        normalize_cpp_type_name("const Logger&"),
        Some("Logger".into())
    );
    assert_eq!(
        normalize_cpp_type_name("std::vector<int>*"),
        Some("vector".into())
    );
    assert_eq!(normalize_cpp_type_name("xor"), None);
}
