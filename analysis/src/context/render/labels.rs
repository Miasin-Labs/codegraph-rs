use crate::edges::EdgeKind;
use crate::nodes::{NodeData, NodeKind, Visibility};

pub(super) fn symbol_label(node: &NodeData) -> String {
    let base = if node.qualified_name.is_empty() {
        node.name.as_str()
    } else {
        node.qualified_name.as_str()
    };
    if base.contains("::") || base.contains('.') {
        base.to_string()
    } else {
        let file = node.file_path.display().to_string();
        if file.is_empty() || file == "<unresolved>" {
            base.to_string()
        } else {
            format!("{file}::{base}")
        }
    }
}

pub(super) fn line_label(node: &NodeData) -> String {
    format!("{}:{}", symbol_label(node), node.span.start_line)
}

pub(super) fn relation_label(node: &NodeData) -> String {
    line_label(node)
}

pub fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::Trait => "trait",
        NodeKind::Module => "module",
        _ => "symbol",
    }
}

pub fn edge_kind_label(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "calls",
        EdgeKind::UnresolvedCall(_) => "unresolved_call",
        EdgeKind::UsesType => "uses_type",
        EdgeKind::References => "references",
        EdgeKind::Contains => "contains",
        EdgeKind::Implements => "implements",
        EdgeKind::ExternalCall(_, _) => "external_call",
        _ => "→",
    }
}

pub fn visibility_label(v: &Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Crate => "pub(crate)",
        Visibility::Super => "pub(super)",
        Visibility::Private => "private",
    }
}

pub(super) fn line_suffix(node: &NodeData) -> String {
    if node.span.start_line > 0 {
        format!(":{}", node.span.start_line)
    } else {
        String::new()
    }
}

/// Full line range (`:start-end`) for a node, so callers can `Read offset` or
/// `symbol_edit` precisely without a follow-up `sed`/`nl`. Falls back to the
/// single start line when the span is degenerate.
pub(crate) fn line_range(node: &NodeData) -> String {
    if node.span.start_line == 0 {
        return String::new();
    }
    if node.span.end_line > node.span.start_line {
        format!(":{}-{}", node.span.start_line, node.span.end_line)
    } else {
        format!(":{}", node.span.start_line)
    }
}

/// Best-effort signature reconstruction. Functions surface their
/// metadata `signature` if the adapter populated it; otherwise we
/// fall back to `name(...)`.
pub(super) fn signature_for(node: &NodeData) -> String {
    if let Some(sig) = node.metadata.get("signature") {
        return sig.clone();
    }
    match node.kind {
        NodeKind::Function => format!("fn {}(...)", node.name),
        NodeKind::Struct => format!("struct {}", node.name),
        NodeKind::Enum => format!("enum {}", node.name),
        NodeKind::Trait => format!("trait {}", node.name),
        NodeKind::Module => format!("mod {}", node.name),
        _ => format!("{} ({:?})", node.name, node.kind),
    }
}
