mod cpp;
mod go;
mod jvm;
mod local;
mod rust;
mod typed;

pub(super) use cpp::infer_cpp_receiver_type;
#[cfg(test)]
pub(super) use cpp::normalize_cpp_type_name;
pub(super) use go::match_go_field_chain_call;
pub(super) use jvm::infer_java_field_receiver_type;
pub(super) use local::infer_local_receiver_type;
pub(crate) use local::infer_receiver_type_from_declaration;
pub(super) use rust::{
    RustType,
    declared_type,
    external_path,
    file_is_module,
    fn_local_uses,
    infer_rust_chain_type,
    infer_rust_receiver_type,
    is_local_at_call,
    resolve_type,
    self_field_receiver_type,
    signature_return,
};
pub(crate) use typed::resolve_method_on_type;
