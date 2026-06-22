#[test]
fn search_pagination_pages_after_final_scoring_and_filtering() {
    let dir = TempDir::new().unwrap();
    let cg = setup_indexed(dir.path());

    for i in 0..130 {
        write(
            &dir.path().join(format!("src/vendor/alpha-{i}.ts")),
            &format!("export function alphaVendor{i}() {{ return {i}; }}"),
        );
    }
    for i in 0..8 {
        write(
            &dir.path().join(format!("src/focused/alpha-{i}.ts")),
            &format!("export function alphaFocused{i}() {{ return {i}; }}"),
        );
    }
    cg.index_all(&IndexOptions::default()).unwrap();

    let all = cg
        .search_nodes(
            "alpha path:focused",
            Some(&SearchOptions {
                limit: Some(8),
                ..Default::default()
            }),
        )
        .unwrap();
    let first = cg
        .search_nodes(
            "alpha path:focused",
            Some(&SearchOptions {
                limit: Some(3),
                offset: Some(0),
                ..Default::default()
            }),
        )
        .unwrap();
    let second = cg
        .search_nodes(
            "alpha path:focused",
            Some(&SearchOptions {
                limit: Some(3),
                offset: Some(3),
                ..Default::default()
            }),
        )
        .unwrap();

    assert_eq!(all.len(), 8);
    let ids = |v: &[codegraph::SearchResult]| {
        v.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(ids(&first), ids(&all[0..3]));
    assert_eq!(ids(&second), ids(&all[3..6]));
    let mut combined = ids(&first);
    combined.extend(ids(&second));
    combined.sort();
    combined.dedup();
    assert_eq!(combined.len(), 6);
}
