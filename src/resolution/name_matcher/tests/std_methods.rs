use super::{Fixture, make_ref, match_reference, node};
use crate::resolution::types::UnresolvedRef;
use crate::types::{EdgeKind, Language, NodeKind, receiver_dropped_metadata};

fn project() -> Fixture {
    Fixture::new(vec![
        node(
            "method:src/frontier.rs:FrontierIter::next:10",
            NodeKind::Method,
            "next",
            "FrontierIter::next",
            "src/frontier.rs",
            Language::Rust,
            10,
            12,
        ),
        node(
            "field:src/map.rs:OrderedMap::map:3",
            NodeKind::Field,
            "map",
            "OrderedMap::map",
            "src/map.rs",
            Language::Rust,
            3,
            3,
        ),
        node(
            "method:src/graph.rs:Graph::get_outgoing_edges:20",
            NodeKind::Method,
            "get_outgoing_edges",
            "Graph::get_outgoing_edges",
            "src/graph.rs",
            Language::Rust,
            20,
            30,
        ),
    ])
}

/// A bare method call as `calls.rs` records `a.b().name()`.
fn dropped_receiver_call(name: &str) -> UnresolvedRef {
    UnresolvedRef {
        metadata: Some(receiver_dropped_metadata()),
        ..make_ref(name, EdgeKind::Calls, 5, "src/main.rs", Language::Rust)
    }
}

/// `v.iter().next()` must not land on the only project `next`.
#[test]
fn dropped_receiver_std_method_calls_stay_unresolved() {
    let ctx = project();
    assert!(match_reference(&dropped_receiver_call("next"), &ctx).is_none());
    // Method-call syntax never names a field either (`.map(..)`).
    assert!(match_reference(&dropped_receiver_call("map"), &ctx).is_none());
}

#[test]
fn dropped_receiver_project_method_calls_still_resolve() {
    let resolved = match_reference(&dropped_receiver_call("get_outgoing_edges"), &project())
        .expect("a project-unique method name still resolves");
    assert_eq!(
        resolved.target_node_id,
        "method:src/graph.rs:Graph::get_outgoing_edges:20"
    );
}

/// Only the dropped receiver makes a std name unresolvable: `self.next()`
/// and plain `next()` keep resolving as before.
#[test]
fn std_method_names_resolve_when_the_receiver_was_kept() {
    let kept = make_ref("next", EdgeKind::Calls, 5, "src/main.rs", Language::Rust);
    let resolved = match_reference(&kept, &project()).expect("resolves by exact name");
    assert_eq!(
        resolved.target_node_id,
        "method:src/frontier.rs:FrontierIter::next:10"
    );
}
