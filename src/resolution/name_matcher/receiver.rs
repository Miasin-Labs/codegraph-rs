mod cpp;
mod jvm;
mod typed;

pub(super) use cpp::infer_cpp_receiver_type;
#[cfg(test)]
pub(super) use cpp::normalize_cpp_type_name;
pub(super) use jvm::infer_java_field_receiver_type;
pub(super) use typed::resolve_method_on_type;
