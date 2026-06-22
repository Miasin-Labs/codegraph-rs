//! Built-in/external filtering and resolver prefilter policy.

mod apex;
mod bash;
mod c_cpp;
mod filters;
mod go;
mod js;
mod jvm;
mod pascal;
mod prefilter;
mod python;

pub(super) use apex::{APEX_BUILT_IN_METHODS, APEX_SYSTEM_TYPES};
pub(super) use bash::BASH_BUILT_INS;
pub(super) use c_cpp::{C_BUILT_INS, C_CPP_STDLIB_CALLS, CPP_BUILT_INS};
pub(super) use go::{GO_BUILT_INS, GO_STDLIB_PACKAGES};
pub(super) use js::{JS_BUILT_INS, REACT_HOOKS};
pub(super) use jvm::{
    JVM_NAMESPACE_SEGMENTS,
    JVM_STDLIB_EXTERNAL_CALLS,
    JVM_STDLIB_IMPORT_PREFIXES,
    JVM_STDLIB_TYPES,
};
pub(super) use pascal::{PASCAL_BUILT_INS, PASCAL_UNIT_PREFIXES};
pub(super) use prefilter::{capitalize_first, has_any_possible_match_in};
pub(super) use python::{PYTHON_BUILT_IN_METHODS, PYTHON_BUILT_IN_TYPES, PYTHON_BUILT_INS};
