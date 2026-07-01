//! Node-kind classifiers for MCP rendering.

use crate::types::NodeKind;

/// Node kinds that contain other symbols. For these, `codegraph_node` with
/// `includeCode=true` returns a structural outline (member names + signatures +
/// line numbers) instead of the full body, which for a large class is a
/// multi-thousand-character wall of source that bloats the agent's context.
pub(in crate::mcp::tools) fn is_container_node_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
            | NodeKind::Namespace
            | NodeKind::Module
    )
}

/// Callable kinds — TS also lists `'constructor'`, which is not a `NodeKind`
/// in either implementation (dead-letter entry kept out of the Rust enum).
pub(in crate::mcp::tools) fn is_callable_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::Component
    )
}

pub(in crate::mcp::tools) fn is_explore_seed_kind(kind: NodeKind) -> bool {
    is_callable_kind(kind) || matches!(kind, NodeKind::DataSymbol | NodeKind::StringLiteral)
}
