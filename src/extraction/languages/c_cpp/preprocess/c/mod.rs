mod annotations;
mod calls;
mod declarations;

pub(super) use annotations::{
    blank_c_auto_inference,
    blank_c_cpp_guard_bodies,
    blank_c_kernel_annotations,
    blank_c_parameterized_annotation_macros,
    blank_c_sandwiched_annotations,
    blank_c_trailing_param_attr_macros,
};
pub(super) use calls::{
    blank_c_statement_macro_calls,
    blank_c_type_keyword_args,
    blank_c_va_arg_qualified_type_args,
};
pub(super) use declarations::{
    blank_c_file_scope_prefixed_decl_macros,
    blank_c_named_variadic_define_dots,
    rewrite_c_prefixed_decl_macro_initializers,
};
