use super::{Fixture, match_reference, node};
use crate::resolution::types::UnresolvedRef;
use crate::types::{EdgeKind, Language, NodeKind};

fn rust_ref(from: &str, name: &str, file: &str) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: from.into(),
        reference_name: name.into(),
        reference_kind: EdgeKind::Calls,
        line: 10,
        column: 4,
        file_path: file.into(),
        language: Language::Rust,
        candidates: None,
        metadata: None,
    }
}

fn func(id: &str, name: &str, qualified: &str, file: &str) -> crate::types::Node {
    node(
        id,
        NodeKind::Function,
        name,
        qualified,
        file,
        Language::Rust,
        1,
        5,
    )
}

fn method(id: &str, name: &str, qualified: &str, file: &str) -> crate::types::Node {
    node(
        id,
        NodeKind::Method,
        name,
        qualified,
        file,
        Language::Rust,
        1,
        5,
    )
}

#[test]
fn crate_path_picks_the_item_in_the_named_module() {
    let ctx = Fixture::new(vec![
        func("caller", "build", "build", "src/context/builder.rs"),
        func("cancel-check", "check", "check", "src/graph/cancel.rs"),
        func("decoy-check", "check", "check", "src/sync/watch.rs"),
    ]);
    let r = rust_ref(
        "caller",
        "crate::graph::cancel::check",
        "src/context/builder.rs",
    );
    let hit = match_reference(&r, &ctx).expect("crate:: path resolves");
    assert_eq!(hit.target_node_id, "cancel-check");
    assert_eq!(hit.confidence, 0.95);
}

#[test]
fn self_path_uses_the_enclosing_impl_type() {
    let ctx = Fixture::new(vec![
        method(
            "emit",
            "emit",
            "RustIrLowering::emit",
            "analysis/src/ir/model.rs",
        ),
        method(
            "rust-label",
            "fresh_label",
            "RustIrLowering::fresh_label",
            "analysis/src/ir/model.rs",
        ),
        method(
            "py-label",
            "fresh_label",
            "PythonIrLowering::fresh_label",
            "analysis/src/ir/model.rs",
        ),
    ]);
    let r = rust_ref("emit", "Self::fresh_label", "analysis/src/ir/model.rs");
    let hit = match_reference(&r, &ctx).expect("Self:: resolves");
    assert_eq!(hit.target_node_id, "rust-label");
}

#[test]
fn super_path_climbs_one_module() {
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/a/b.rs"),
        func("helper", "helper", "helper", "src/a/mod.rs"),
        func("other-helper", "helper", "helper", "src/c.rs"),
    ]);
    let r = rust_ref("caller", "super::helper", "src/a/b.rs");
    assert_eq!(
        match_reference(&r, &ctx)
            .expect("super:: resolves")
            .target_node_id,
        "helper"
    );
}

#[test]
fn crate_path_to_an_associated_function() {
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/main_loop.rs"),
        func("type-new", "new", "Graph::new", "src/graph.rs"),
        func("other-new", "new", "Cache::new", "src/cache.rs"),
    ]);
    let r = rust_ref("caller", "crate::graph::Graph::new", "src/main_loop.rs");
    assert_eq!(
        match_reference(&r, &ctx)
            .expect("assoc fn resolves")
            .target_node_id,
        "type-new"
    );
}

#[test]
fn crate_path_follows_a_workspace_reexport() {
    // Root crate calls `crate::ensure_sufficient_stack`, which it re-exports
    // from the analysis crate where it is defined.
    let ctx = Fixture::new(vec![
        func("caller", "walk", "walk", "src/graph/traversal.rs"),
        func(
            "stack",
            "ensure_sufficient_stack",
            "ensure_sufficient_stack",
            "analysis/src/lib.rs",
        ),
    ]);
    let r = rust_ref(
        "caller",
        "crate::ensure_sufficient_stack",
        "src/graph/traversal.rs",
    );
    let hit = match_reference(&r, &ctx).expect("re-export resolves");
    assert_eq!(hit.target_node_id, "stack");
    assert!(
        hit.confidence < 0.95,
        "fallback must carry lower confidence"
    );
}

#[test]
fn never_guesses_between_same_named_items() {
    // `crate::helper` names the crate root, but neither helper lives there,
    // and two same-crate candidates are equally plausible re-exports.
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/x.rs"),
        func("h1", "helper", "helper", "src/a.rs"),
        func("h2", "helper", "helper", "src/b.rs"),
    ]);
    let r = rust_ref("caller", "crate::helper", "src/x.rs");
    let hit = super::super::match_rust_path(&r, &ctx);
    assert!(hit.is_none(), "ambiguous path must not resolve: {hit:?}");
}

#[test]
fn leaves_external_paths_alone() {
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/x.rs"),
        func("take", "take", "take", "src/mem.rs"),
    ]);
    let r = rust_ref("caller", "std::mem::take", "src/x.rs");
    assert!(super::super::match_rust_path(&r, &ctx).is_none());
}

#[test]
fn self_in_a_trait_impl_falls_back_to_the_same_file_type() {
    // `impl Default for AnalysisCache { fn default() -> Self { Self::new() } }`
    // keys the method by the trait, so `Self` must be found another way.
    let ctx = Fixture::new(vec![
        method(
            "default",
            "default",
            "Default::default",
            "analysis/src/cache.rs",
        ),
        method(
            "cache-new",
            "new",
            "AnalysisCache::new",
            "analysis/src/cache.rs",
        ),
        method("other-new", "new", "Graph::new", "analysis/src/graph.rs"),
    ]);
    let r = rust_ref("default", "Self::new", "analysis/src/cache.rs");
    let hit = match_reference(&r, &ctx).expect("trait-impl Self resolves");
    assert_eq!(hit.target_node_id, "cache-new");
    assert!(hit.confidence < 0.95);
}

#[test]
fn cfg_variants_of_one_item_are_not_ambiguous() {
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/mcp/proxy.rs"),
        node(
            "alive-unix",
            NodeKind::Function,
            "is_process_alive",
            "is_process_alive",
            "src/utils.rs",
            Language::Rust,
            18,
            30,
        ),
        node(
            "alive-windows",
            NodeKind::Function,
            "is_process_alive",
            "is_process_alive",
            "src/utils.rs",
            Language::Rust,
            47,
            60,
        ),
    ]);
    let r = rust_ref(
        "caller",
        "crate::utils::is_process_alive",
        "src/mcp/proxy.rs",
    );
    assert_eq!(
        match_reference(&r, &ctx)
            .expect("cfg variants resolve")
            .target_node_id,
        "alive-unix"
    );
}

#[test]
fn super_inside_an_inline_test_module_reaches_the_file_module() {
    // `mod tests { fn case() { super::production(); } }` in src/lib.rs.
    let ctx = Fixture::new(vec![
        func("case", "case", "tests::case", "src/lib.rs"),
        func("prod", "production", "production", "src/lib.rs"),
        func("decoy", "production", "production", "src/other.rs"),
    ]);
    let r = rust_ref("case", "super::production", "src/lib.rs");
    assert_eq!(
        match_reference(&r, &ctx)
            .expect("super:: from inline mod")
            .target_node_id,
        "prod"
    );
}
