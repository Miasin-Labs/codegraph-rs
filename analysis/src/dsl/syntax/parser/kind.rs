use super::super::{EntrypointKind, ParseError, Projection};
use crate::edges::EdgeKind;
use crate::nodes::NodeKind;

pub(super) fn parse_node_kind(s: &str, pos: usize) -> Result<NodeKind, ParseError> {
    match s {
        "Function" => Ok(NodeKind::Function),
        "Struct" => Ok(NodeKind::Struct),
        "Enum" => Ok(NodeKind::Enum),
        "Module" => Ok(NodeKind::Module),
        "Trait" => Ok(NodeKind::Trait),
        "EnumVariant" => Ok(NodeKind::EnumVariant),
        "Field" => Ok(NodeKind::Field),
        "TypeAlias" => Ok(NodeKind::TypeAlias),
        "Constant" => Ok(NodeKind::Constant),
        "Interface" => Ok(NodeKind::Interface),
        _ => Err(ParseError::new(
            pos,
            format!(
                "unknown node kind '{s}'. Valid kinds: Function, Struct, Enum, Module, Trait, EnumVariant, Field, TypeAlias, Constant, Interface"
            ),
        )),
    }
}

pub(super) fn parse_projection(s: &str, pos: usize) -> Result<Projection, ParseError> {
    match s {
        "fields" => Ok(Projection::Fields),
        "signature" => Ok(Projection::Signature),
        "body" => Ok(Projection::Body),
        _ => Err(ParseError::new(
            pos,
            format!("unknown projection '{s}'. Valid projections: fields, signature, body"),
        )),
    }
}

pub(super) fn parse_edge_kind(s: &str, pos: usize) -> Result<EdgeKind, ParseError> {
    match s {
        "Calls" => Ok(EdgeKind::Calls),
        "UnresolvedCall" => Ok(EdgeKind::UnresolvedCall(String::new())),
        "UsesType" => Ok(EdgeKind::UsesType),
        "References" => Ok(EdgeKind::References),
        "Contains" => Ok(EdgeKind::Contains),
        "Implements" => Ok(EdgeKind::Implements),
        "ExternalCall" => Ok(EdgeKind::ExternalCall(String::new(), String::new())),
        "Extends" => Ok(EdgeKind::Extends),
        "Returns" => Ok(EdgeKind::Returns),
        "TypeOf" => Ok(EdgeKind::TypeOf),
        _ => Err(ParseError::new(
            pos,
            format!(
                "unknown edge kind '{s}'. Valid: Calls, UnresolvedCall, UsesType, \
                 References, Contains, Implements, ExternalCall, Extends, Returns, TypeOf"
            ),
        )),
    }
}

pub(super) fn parse_entrypoint_kind(s: &str, pos: usize) -> Result<EntrypointKind, ParseError> {
    match s {
        "Main" => Ok(EntrypointKind::Main),
        "PublicApi" => Ok(EntrypointKind::PublicApi),
        "Test" => Ok(EntrypointKind::Test),
        "Bench" => Ok(EntrypointKind::Bench),
        "FfiExport" => Ok(EntrypointKind::FfiExport),
        _ => Err(ParseError::new(
            pos,
            format!(
                "unknown entrypoint kind '{s}'. Valid: Main, PublicApi, Test, Bench, FfiExport"
            ),
        )),
    }
}
