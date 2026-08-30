use std::borrow::Cow;

mod c;
mod common;
mod cpp;

use c::{
    blank_c_auto_inference,
    blank_c_cpp_guard_bodies,
    blank_c_file_scope_prefixed_decl_macros,
    blank_c_kernel_annotations,
    blank_c_named_variadic_define_dots,
    blank_c_parameterized_annotation_macros,
    blank_c_sandwiched_annotations,
    blank_c_statement_macro_calls,
    blank_c_trailing_param_attr_macros,
    blank_c_type_keyword_args,
    blank_c_va_arg_qualified_type_args,
    rewrite_c_prefixed_decl_macro_initializers,
};
use common::{
    blank_c_leading_attr_macros,
    blank_cpp_annotation_macro_calls,
    blank_lone_macro_lines,
    restore_directive_lines,
};
use cpp::{
    blank_cpp_api_prefix_macros,
    blank_cpp_export_macros,
    blank_cpp_inline_annotation_macros,
    blank_cpp_inline_macros,
};

pub(super) fn pre_parse_cpp_source<'a>(source: &'a str, file_path: &str) -> Cow<'a, str> {
    let mut blanked = blank_cpp_export_macros(source);
    blanked = blank_cpp_inline_macros(&blanked);
    blanked = blank_cpp_api_prefix_macros(&blanked);
    blanked = blank_cpp_inline_annotation_macros(&blanked);
    blanked = blank_cpp_annotation_macro_calls(&blanked);
    blanked = blank_c_leading_attr_macros(&blanked);
    blanked = blank_lone_macro_lines(&blanked);

    let lower = file_path.to_ascii_lowercase();
    if lower.ends_with(".metal") {
        blanked = super::blank_metal_attributes(&blanked);
    } else if lower.ends_with(".cu")
        || lower.ends_with(".cuh")
        || super::looks_like_cuda_source(source)
    {
        blanked = super::blank_cuda_constructs(&blanked);
    }
    Cow::Owned(restore_directive_lines(source, &blanked))
}

pub(super) fn pre_parse_c_source(source: &str) -> Cow<'_, str> {
    let guarded = blank_c_cpp_guard_bodies(source);
    let mut blanked = blank_c_kernel_annotations(&guarded);
    blanked = blank_c_sandwiched_annotations(&blanked);
    blanked = blank_c_auto_inference(&blanked);
    blanked = blank_c_parameterized_annotation_macros(&blanked);
    blanked = blank_c_type_keyword_args(&blanked);
    blanked = blank_c_va_arg_qualified_type_args(&blanked);
    blanked = blank_c_file_scope_prefixed_decl_macros(&blanked);
    blanked = rewrite_c_prefixed_decl_macro_initializers(&blanked);
    blanked = blank_cpp_annotation_macro_calls(&blanked);
    blanked = blank_c_trailing_param_attr_macros(&blanked);
    blanked = blank_c_statement_macro_calls(&blanked);
    blanked = blank_lone_macro_lines(&blanked);
    blanked = blank_c_leading_attr_macros(&blanked);
    if super::looks_like_cuda_source(&blanked) {
        blanked = super::blank_cuda_constructs(&blanked);
    }
    let restored = restore_directive_lines(source, &blanked);
    Cow::Owned(blank_c_named_variadic_define_dots(&restored))
}

#[cfg(test)]
mod tests;
