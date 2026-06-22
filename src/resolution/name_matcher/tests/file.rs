use super::*;

// -- Rust-side coverage: file-path strategy ------------------------------
#[test]
fn file_path_match_prefers_exact_then_suffix_then_singleton() {
    let exact = node(
        "file:snippets/drawer-menu.liquid",
        NodeKind::File,
        "drawer-menu.liquid",
        "snippets/drawer-menu.liquid",
        "snippets/drawer-menu.liquid",
        Language::Liquid,
        1,
        1,
    );
    let ctx = Fixture::new(vec![exact]);
    let r = make_ref(
        "snippets/drawer-menu.liquid",
        EdgeKind::References,
        5,
        "templates/index.liquid",
        Language::Liquid,
    );
    let result = match_reference(&r, &ctx).expect("should resolve");
    assert_eq!(result.target_node_id, "file:snippets/drawer-menu.liquid");
    assert_eq!(result.resolved_by, ResolvedBy::FilePath);
    assert_eq!(result.confidence, 0.95);

    // Suffix match: indexed under src/ prefix
    let suffix = node(
        "file:src/snippets/foo.liquid",
        NodeKind::File,
        "foo.liquid",
        "src/snippets/foo.liquid",
        "src/snippets/foo.liquid",
        Language::Liquid,
        1,
        1,
    );
    let ctx = Fixture::new(vec![suffix]);
    let r = make_ref(
        "snippets/foo.liquid",
        EdgeKind::References,
        5,
        "templates/index.liquid",
        Language::Liquid,
    );
    let result = match_reference(&r, &ctx).expect("should resolve");
    assert_eq!(result.target_node_id, "file:src/snippets/foo.liquid");
    assert_eq!(result.confidence, 0.85);

    // Singleton fallback: name matches but path doesn't
    let only = node(
        "file:theme/bits/bar.liquid",
        NodeKind::File,
        "bar.liquid",
        "theme/bits/bar.liquid",
        "theme/bits/bar.liquid",
        Language::Liquid,
        1,
        1,
    );
    let ctx = Fixture::new(vec![only]);
    let r = make_ref(
        "other/bar.liquid",
        EdgeKind::References,
        5,
        "templates/index.liquid",
        Language::Liquid,
    );
    let result = match_reference(&r, &ctx).expect("should resolve");
    assert_eq!(result.target_node_id, "file:theme/bits/bar.liquid");
    assert_eq!(result.confidence, 0.7);
}
