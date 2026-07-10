//! Extraction configuration for SQL inside `<cfquery>` tags.
//!
//! The CFQuery grammar parses `#hash#` expressions as CFML expressions. Calls
//! inside those expressions are meaningful graph references; surrounding SQL
//! identifiers are intentionally not modeled as code symbols.

use crate::extraction::tree_sitter_types::LanguageExtractor;

pub struct CfqueryExtractor;

impl LanguageExtractor for CfqueryExtractor {
    fn function_types(&self) -> &[&str] {
        &[]
    }

    fn class_types(&self) -> &[&str] {
        &[]
    }

    fn method_types(&self) -> &[&str] {
        &[]
    }

    fn interface_types(&self) -> &[&str] {
        &[]
    }

    fn struct_types(&self) -> &[&str] {
        &[]
    }

    fn enum_types(&self) -> &[&str] {
        &[]
    }

    fn type_alias_types(&self) -> &[&str] {
        &[]
    }

    fn import_types(&self) -> &[&str] {
        &[]
    }

    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &[&str] {
        &[]
    }

    fn name_field(&self) -> &str {
        "name"
    }

    fn body_field(&self) -> &str {
        "body"
    }

    fn params_field(&self) -> &str {
        "parameters"
    }
}
