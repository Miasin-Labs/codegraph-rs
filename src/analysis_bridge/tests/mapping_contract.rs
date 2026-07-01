use super::{AEdgeKind, ANodeKind, EdgeKind, NodeKind, map_edge_kind, map_node_kind};

#[test]
fn node_kind_mapping_covers_all_kinds() {
    use crate::types::NODE_KINDS;
    let mapped: Vec<NodeKind> = NODE_KINDS
        .iter()
        .copied()
        .filter(|k| map_node_kind(*k).is_some())
        .collect();
    let skipped = [
        NodeKind::Property,
        NodeKind::Field,
        NodeKind::Variable,
        NodeKind::EnumMember,
        NodeKind::TypeAlias,
        NodeKind::Parameter,
        NodeKind::Import,
        NodeKind::Export,
        NodeKind::Route,
        NodeKind::Component,
        NodeKind::Macro,
    ];
    assert_eq!(mapped.len(), NODE_KINDS.len() - skipped.len());
    assert_eq!(map_node_kind(NodeKind::Method), Some(ANodeKind::Function));
    assert_eq!(map_node_kind(NodeKind::Class), Some(ANodeKind::Struct));
    assert_eq!(map_node_kind(NodeKind::File), Some(ANodeKind::Module));
    assert_eq!(map_node_kind(NodeKind::Interface), Some(ANodeKind::Trait));
    assert_eq!(map_node_kind(NodeKind::Protocol), Some(ANodeKind::Trait));
    assert_eq!(
        map_node_kind(NodeKind::DataSymbol),
        Some(ANodeKind::Constant)
    );
    assert_eq!(
        map_node_kind(NodeKind::StringLiteral),
        Some(ANodeKind::Constant)
    );
    for kind in skipped {
        assert_eq!(map_node_kind(kind), None);
    }
}

#[test]
fn edge_kind_mapping_respects_invariants() {
    use ANodeKind::*;
    assert_eq!(
        map_edge_kind(EdgeKind::Calls, Function, Function),
        Some(AEdgeKind::Calls)
    );
    assert_eq!(map_edge_kind(EdgeKind::Calls, Module, Function), None);
    assert_eq!(map_edge_kind(EdgeKind::Calls, Function, Struct), None);
    assert_eq!(
        map_edge_kind(EdgeKind::Contains, Module, Function),
        Some(AEdgeKind::Contains)
    );
    assert_eq!(map_edge_kind(EdgeKind::Contains, Function, Function), None);
    assert_eq!(
        map_edge_kind(EdgeKind::Implements, Struct, Trait),
        Some(AEdgeKind::Implements)
    );
    assert_eq!(map_edge_kind(EdgeKind::Implements, Struct, Struct), None);
    assert_eq!(
        map_edge_kind(EdgeKind::Extends, Struct, Trait),
        Some(AEdgeKind::Implements)
    );
    assert_eq!(
        map_edge_kind(EdgeKind::Extends, Struct, Struct),
        Some(AEdgeKind::References)
    );
    assert_eq!(
        map_edge_kind(EdgeKind::Extends, Trait, Trait),
        Some(AEdgeKind::References)
    );
    assert_eq!(
        map_edge_kind(EdgeKind::Instantiates, Function, Struct),
        Some(AEdgeKind::UsesType)
    );
    assert_eq!(
        map_edge_kind(EdgeKind::Returns, Function, Enum),
        Some(AEdgeKind::UsesType)
    );
    assert_eq!(
        map_edge_kind(EdgeKind::References, Function, Trait),
        Some(AEdgeKind::UsesType)
    );
    assert_eq!(
        map_edge_kind(EdgeKind::References, Module, Module),
        Some(AEdgeKind::References)
    );
    assert_eq!(
        map_edge_kind(EdgeKind::Imports, Module, Module),
        Some(AEdgeKind::References)
    );
    assert_eq!(
        map_edge_kind(EdgeKind::Overrides, Function, Function),
        Some(AEdgeKind::References)
    );
}

#[test]
fn mapped_edges_always_satisfy_analysis_invariants() {
    use crate::types::EDGE_KINDS;
    let akinds = [
        ANodeKind::Function,
        ANodeKind::Struct,
        ANodeKind::Enum,
        ANodeKind::Module,
        ANodeKind::Trait,
    ];
    for kind in EDGE_KINDS {
        for s in akinds {
            for t in akinds {
                if let Some(mapped) = map_edge_kind(kind, s, t) {
                    assert!(
                        mapped.valid_for(s, t),
                        "map_edge_kind({kind:?}, {s:?}, {t:?}) = {mapped:?} violates valid_for"
                    );
                }
            }
        }
    }
}
