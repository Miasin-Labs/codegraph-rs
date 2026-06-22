use std::path::PathBuf;

use codegraph_analysis::edges::EdgeKind as AEdgeKind;
use codegraph_analysis::nodes::{NodeKind as ANodeKind, Span as ASpan, Visibility as AVisibility};

use crate::types::{EdgeKind, Node, NodeKind, Visibility};

/// Map a codegraph node kind onto the analysis engine's 5-kind model.
/// Returns `None` for kinds that are not represented as analysis nodes.
pub fn map_node_kind(kind: NodeKind) -> Option<ANodeKind> {
    match kind {
        NodeKind::Function | NodeKind::Method => Some(ANodeKind::Function),
        NodeKind::Class | NodeKind::Struct => Some(ANodeKind::Struct),
        NodeKind::Enum => Some(ANodeKind::Enum),
        NodeKind::File | NodeKind::Module | NodeKind::Namespace => Some(ANodeKind::Module),
        NodeKind::Trait | NodeKind::Interface | NodeKind::Protocol => Some(ANodeKind::Trait),
        NodeKind::Property
        | NodeKind::Field
        | NodeKind::Variable
        | NodeKind::Constant
        | NodeKind::EnumMember
        | NodeKind::TypeAlias
        | NodeKind::Parameter
        | NodeKind::Import
        | NodeKind::Export
        | NodeKind::Route
        | NodeKind::Component
        | NodeKind::DataSymbol
        | NodeKind::StringLiteral
        | NodeKind::Macro => None,
    }
}

/// Map a codegraph edge kind onto an analysis edge kind, given the already
/// mapped source/target node kinds. Returns `None` when the combination
/// cannot be represented under the analysis graph's insertion invariants.
pub fn map_edge_kind(kind: EdgeKind, source: ANodeKind, target: ANodeKind) -> Option<AEdgeKind> {
    let fn_to_type = source == ANodeKind::Function
        && matches!(
            target,
            ANodeKind::Struct | ANodeKind::Enum | ANodeKind::Trait
        );
    match kind {
        EdgeKind::Calls => (source == ANodeKind::Function && target == ANodeKind::Function)
            .then_some(AEdgeKind::Calls),
        EdgeKind::Contains => matches!(
            source,
            ANodeKind::Module | ANodeKind::Struct | ANodeKind::Enum | ANodeKind::Trait
        )
        .then_some(AEdgeKind::Contains),
        EdgeKind::Implements => (matches!(source, ANodeKind::Struct | ANodeKind::Enum)
            && target == ANodeKind::Trait)
            .then_some(AEdgeKind::Implements),
        EdgeKind::Extends => {
            if matches!(source, ANodeKind::Struct | ANodeKind::Enum) && target == ANodeKind::Trait {
                Some(AEdgeKind::Implements)
            } else {
                Some(AEdgeKind::References)
            }
        }
        EdgeKind::References
        | EdgeKind::Imports
        | EdgeKind::Exports
        | EdgeKind::Instantiates
        | EdgeKind::TypeOf
        | EdgeKind::Returns
        | EdgeKind::Overrides
        | EdgeKind::Decorates => Some(if fn_to_type {
            AEdgeKind::UsesType
        } else {
            AEdgeKind::References
        }),
        EdgeKind::Reads | EdgeKind::Writes | EdgeKind::Aliases => None,
    }
}

pub(super) fn map_visibility(v: Option<Visibility>) -> AVisibility {
    match v {
        Some(Visibility::Public) | None => AVisibility::Public,
        Some(Visibility::Private) => AVisibility::Private,
        Some(Visibility::Protected) => AVisibility::Super,
        Some(Visibility::Internal) => AVisibility::Crate,
    }
}

pub(super) fn engine_safe_field_name(name: &str) -> bool {
    !name.is_empty() && !name.contains([';', ':', ','])
}

pub(super) fn field_type_from(node: &Node) -> String {
    let Some(sig) = node.signature.as_deref() else {
        return String::new();
    };
    let sig = sig.trim();
    let name = node.name.as_str();
    if let Some(rest) = sig.strip_prefix(name) {
        if let Some(ty) = rest.trim_start().strip_prefix(':') {
            return sanitize_field_type(ty.trim());
        }
    }
    if let Some(prefix) = sig.strip_suffix(name) {
        let prefix = prefix.trim_end().trim_end_matches('$').trim_end();
        return sanitize_field_type(prefix);
    }
    String::new()
}

fn sanitize_field_type(ty: &str) -> String {
    ty.replace(';', " ").trim().to_string()
}

pub(super) fn node_span(node: &Node) -> ASpan {
    ASpan {
        file: PathBuf::from(&node.file_path),
        start_line: node.start_line,
        start_col: node.start_column,
        end_line: node.end_line,
        end_col: node.end_column,
        byte_range: node.byte_range().unwrap_or(0..0),
    }
}
