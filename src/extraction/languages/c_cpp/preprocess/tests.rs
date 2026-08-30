use super::*;
use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
use crate::types::{Language, NodeKind};

fn assert_offsets_preserved(source: &str, output: &str) {
    assert_eq!(output.len(), source.len());
    let source_newlines: Vec<_> = source.match_indices('\n').map(|(index, _)| index).collect();
    let output_newlines: Vec<_> = output.match_indices('\n').map(|(index, _)| index).collect();
    assert_eq!(output_newlines, source_newlines);
}

#[test]
fn cpp_pipeline_recovers_all_source_preparse_shapes() {
    let source = concat!(
        "#define ENGINE_API VALUE\n",
        "class ENGINE_API Actor {\n",
        "  GENERATED_BODY()\n",
        "  ENGINE_API FORCEINLINE int tick(UPARAM(ref) int& value) { return value; }\n",
        "};\n",
        "FMT_BEGIN_NAMESPACE\n",
        "SEC_ATTR UINT32 helper(void) { return 0; }\n",
    );
    let output = pre_parse_cpp_source(source, "actor.cpp");
    assert_offsets_preserved(source, &output);
    for removed in [
        "class ENGINE_API",
        "GENERATED_BODY",
        "ENGINE_API FORCEINLINE",
        "UPARAM",
        "FMT_BEGIN_NAMESPACE",
        "SEC_ATTR",
    ] {
        assert!(
            !output.contains(removed),
            "expected {removed:?} to be blanked"
        );
    }
    assert!(output.contains("#define ENGINE_API VALUE"));
}

#[test]
fn c_pipeline_recovers_all_source_preparse_shapes() {
    let source = concat!(
        "#ifdef __cplusplus\nextern \"C\" {\n#endif\n",
        "static notrace int __init walk(int argc UNUSED) {\n",
        "  list_for_each_entry(pos, head, member) {\n",
        "    use(pos);\n",
        "  }\n",
        "  auto hb = get_hb();\n",
        "  return va_arg(ap, const char *);\n",
        "}\n",
        "struct file *f __free(fput) = NULL;\n",
        "static DEFINE_PER_CPU(struct state, cpu_state);\n",
        "#define verbose(env, fmt, args...) log(env, fmt, ##args)\n",
    );
    let output = pre_parse_c_source(source);
    assert_offsets_preserved(source, &output);
    for removed in [
        "extern \"C\"",
        "notrace",
        "__init",
        "UNUSED",
        "list_for_each_entry",
        "auto hb",
        "const char *",
        "__free",
        "DEFINE_PER_CPU",
        "args...",
    ] {
        assert!(
            !output.contains(removed),
            "expected {removed:?} to be normalized"
        );
    }
    assert!(output.contains("##args"));
}

#[test]
fn cpp_helpers_match_source_fixtures_and_controls() {
    let source = "class MYGAME_API Widget : Base {};\nENGINE_API FORCEINLINE int f(UPARAM(ref) int& x) { return x; }\n";
    let mut output = blank_cpp_export_macros(source);
    output = blank_cpp_inline_macros(&output);
    output = blank_cpp_api_prefix_macros(&output);
    output = blank_cpp_inline_annotation_macros(&output);
    assert_offsets_preserved(source, &output);
    for removed in ["MYGAME_API", "ENGINE_API", "FORCEINLINE", "UPARAM"] {
        assert!(!output.contains(removed));
    }
    for unchanged in [
        "class FOO : Base {};",
        "struct FOO value;",
        "int x = SOME_API;",
        "x = FORCEINLINE + 1;",
        "int FORCEINLINE_COUNT = 3;",
    ] {
        assert_eq!(blank_cpp_export_macros(unchanged), unchanged);
        assert_eq!(blank_cpp_api_prefix_macros(unchanged), unchanged);
        assert_eq!(blank_cpp_inline_macros(unchanged), unchanged);
    }

    let markup = "\tUPROPERTY(EditAnywhere, meta=(ClampMin=\"0\"))\n\tfloat X;\n";
    let blanked = blank_cpp_annotation_macro_calls(markup);
    assert_offsets_preserved(markup, &blanked);
    assert!(!blanked.contains("UPROPERTY"));
    assert!(blanked.contains("float X;"));
    let statement = "void f() {\n\tLOG_MESSAGE(\"hi\");\n}";
    assert_eq!(blank_cpp_annotation_macro_calls(statement), statement);
}

#[test]
fn common_helpers_match_source_fixtures_and_controls() {
    let source = "FMT_BEGIN_NAMESPACE\nstruct S {};\nFMT_END_NAMESPACE\n";
    let output = blank_lone_macro_lines(source);
    assert_offsets_preserved(source, &output);
    assert!(!output.contains("FMT_BEGIN_NAMESPACE"));
    assert_eq!(
        blank_lone_macro_lines("int y =\nSOME_FLAG\n| OTHER;\n"),
        "int y =\nSOME_FLAG\n| OTHER;\n"
    );
    assert_eq!(
        blank_lone_macro_lines("NDEBUG\nint z;\n"),
        "NDEBUG\nint z;\n"
    );

    assert_eq!(
        blank_c_leading_attr_macros("SEC_ATTR UINT32 f(void) {}"),
        "         UINT32 f(void) {}"
    );
    for unchanged in [
        "UINT32 helper(void) {}",
        "MY_ASSERT(x);",
        "#define SEC_ATTR value",
        "SEC_ATTR unsigned int f(void) {}",
        "x = SEC_ATTR UINT32 y(z);",
    ] {
        assert_eq!(blank_c_leading_attr_macros(unchanged), unchanged);
    }

    let directive = "#define FMT_API FMT_VISIBILITY(\"default\")\nFMT_API int f(void);\n";
    let blanked = blank_cpp_api_prefix_macros(directive);
    let restored = restore_directive_lines(directive, &blanked);
    assert!(restored.starts_with("#define FMT_API"));
    assert!(
        !restored
            .lines()
            .nth(1)
            .expect("second line")
            .contains("FMT_API")
    );
}

#[test]
fn c_annotation_helpers_match_source_fixtures() {
    let guarded = "#ifdef __cplusplus\nextern \"C\" {\n#endif\nint real_decl(void);\n";
    let output = blank_c_cpp_guard_bodies(guarded);
    assert_offsets_preserved(guarded, &output);
    assert!(!output.contains("extern \"C\""));
    let nested = "#ifdef __cplusplus\n#define EXTERNC extern \"C\"\n#endif\n";
    assert_eq!(blank_c_cpp_guard_bodies(nested), nested);

    let annotations = "static int __init f(void);\nvoid copy(void __user *dst);\n__printf(1, 2) void log_it(void);\n__u32 count;\n";
    let output = blank_c_kernel_annotations(annotations);
    assert_offsets_preserved(annotations, &output);
    assert!(!output.contains("__init"));
    assert!(!output.contains("__user"));
    assert!(output.contains("__printf(1, 2)"));
    assert!(output.contains("__u32 count"));

    let parameterized = "struct file *f __free(fput) = NULL;\nstruct C {\n\t__bpf_md_ptr(struct M *, meta);\n};\nint keep = __hash(key);\n";
    let output = blank_c_parameterized_annotation_macros(parameterized);
    assert_offsets_preserved(parameterized, &output);
    assert!(!output.contains("__free"));
    assert_eq!(output.lines().nth(2).expect("field line").trim(), "");
    assert!(output.contains("__hash(key)"));

    let sandwiched = "static notrace void tick(void);\nstatic nokprobe_inline void arm(void);\n";
    let output = blank_c_sandwiched_annotations(sandwiched);
    assert!(!output.contains("notrace"));
    assert!(!output.contains("nokprobe_inline"));
    assert_eq!(
        blank_c_auto_inference("auto hb = get_hb();\nauto int x = 1;\n"),
        "     hb = get_hb();\nauto int x = 1;\n"
    );
    assert!(!blank_c_trailing_param_attr_macros("int f(int x UNUSED);\n").contains("UNUSED"));
}

#[test]
fn c_call_helpers_match_source_fixtures_and_controls() {
    let source =
        "void walk(void) {\n\tlist_for_each_entry(pos, head, member) {\n\t\tuse(pos);\n\t}\n}\n";
    let output = blank_c_statement_macro_calls(source);
    assert_offsets_preserved(source, &output);
    assert!(!output.contains("list_for_each_entry"));
    assert!(output.contains("use(pos);"));
    let call = "void f(void) {\n\tdo_thing(a, b);\n}\n";
    assert_eq!(blank_c_statement_macro_calls(call), call);

    let types = "void f(void *head) {\n\tvoid *p = kzalloc_obj(struct opts);\n\treturn container_of(head, struct item, member);\n}\nDEFINE_PER_CPU(struct task_struct *, worker);\n";
    let output = blank_c_type_keyword_args(types);
    assert_offsets_preserved(types, &output);
    assert!(!output.contains("struct opts"));
    assert!(output.contains("       task_struct  , worker"));
    for valid in [
        "sizeof(struct point)",
        "offsetof(struct point, y)",
        "int wf(struct a, struct b);",
        "use((struct foo *)p);",
    ] {
        assert_eq!(blank_c_type_keyword_args(valid), valid);
    }

    let va = "va_arg(ap, const char *); va_arg(ap, int);";
    let output = blank_c_va_arg_qualified_type_args(va);
    assert_offsets_preserved(va, &output);
    assert!(output.contains("va_arg(ap              )"));
    assert!(output.contains("va_arg(ap, int)"));
}

#[test]
fn c_declaration_helpers_match_source_fixtures() {
    let source = "static DEFINE_PER_CPU(struct state, cpu_state);\nEXPORT_SYMBOL(value);\nstatic DEFINE_PER_CPU(struct state, initialized) = {\n";
    let output = blank_c_file_scope_prefixed_decl_macros(source);
    assert_offsets_preserved(source, &output);
    assert!(!output.contains("cpu_state"));
    assert!(output.contains("EXPORT_SYMBOL(value)"));
    assert!(output.contains("initialized) = {"));

    let line = "static DEFINE_PER_CPU(struct cpuhp_cpu_state, cpuhp_state) = {";
    let rewritten = rewrite_c_prefixed_decl_macro_initializers(line);
    assert_offsets_preserved(line, &rewritten);
    assert!(rewritten.contains("static struct cpuhp_cpu_state"));
    assert_eq!(rewritten.rfind("cpuhp_state"), line.rfind("cpuhp_state"));
    assert_eq!(rewritten.find("= {"), line.find("= {"));
    let three_arg = "static DEFINE_TIMER(t, f, 0) = {";
    assert_eq!(
        rewrite_c_prefixed_decl_macro_initializers(three_arg),
        three_arg
    );

    let named = "#define verbose(env, fmt, args...) log(env, fmt, ##args)\n";
    let output = blank_c_named_variadic_define_dots(named);
    assert_offsets_preserved(named, &output);
    assert!(output.contains("args   )"));
    assert!(output.contains("##args"));
    let standard = "#define pr(fmt, ...) printk(fmt, __VA_ARGS__)\n";
    assert_eq!(blank_c_named_variadic_define_dots(standard), standard);
}

#[test]
fn preprocessing_recovers_c_and_cpp_extraction_with_original_lines() {
    let c_source = "SEC_ATTR UINT32 recovered(void) { return 0; }\n";
    let c_result = TreeSitterExtractor::new(
        "src/recovered.c",
        c_source,
        Some(Language::C),
        Some(&super::super::CExtractor),
    )
    .extract();
    let c_function = c_result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Function && node.name == "recovered")
        .expect("C preprocessing should recover the real function name");
    assert_eq!(c_function.start_line, 1);

    let cpp_source =
        "class ENGINE_API Recovered { public: FORCEINLINE int value() { return 1; } };\n";
    let cpp_result = TreeSitterExtractor::new(
        "src/recovered.cpp",
        cpp_source,
        Some(Language::Cpp),
        Some(&super::super::CppExtractor),
    )
    .extract();
    let class = cpp_result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Class && node.name == "Recovered")
        .expect("C++ preprocessing should recover the class");
    assert_eq!(class.start_line, 1);
    assert!(cpp_result.nodes.iter().any(|node| node.name == "value"));
}

#[test]
fn cpp_preparse_preserves_raw_string_content_and_delimiters() {
    // Macro-shaped text inside a raw string must survive pre-parse untouched,
    // so the closing delimiter is never blanked (upstream #1505).
    let source = concat!(
        "const char* q = R\"SQL(\n",
        "CALL_SOMETHING(arg\n",
        "BIG_UPPER_MACRO_NAME\n",
        ")SQL\";\n",
        "int after() { return 0; }\n",
    );
    let output = pre_parse_cpp_source(source, "q.cpp");
    assert_offsets_preserved(source, &output);
    // The raw-string body and its closing delimiter are intact.
    assert!(output.contains("CALL_SOMETHING(arg"));
    assert!(output.contains(")SQL\";"));
    // The real declaration after the raw string is untouched.
    assert!(output.contains("int after() { return 0; }"));
}
