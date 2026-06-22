use crate::fixture::*;

#[test]
fn warm_caches_and_resolve_completes() {
    // "Resolution Warm Caches" — resolveReferences internally warms caches
    // and completes without error.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/a.ts",
        "export function myFunc(): void {}\nexport function otherFunc(): void { myFunc(); }\n",
    );
    fx.track(&q, "src/a.ts", Language::Typescript);

    let my_func = exported(node(
        "func:src/a.ts:myFunc:1",
        NodeKind::Function,
        "myFunc",
        "src/a.ts::myFunc",
        "src/a.ts",
        Language::Typescript,
        1,
        1,
    ));
    let other_func = exported(node(
        "func:src/a.ts:otherFunc:2",
        NodeKind::Function,
        "otherFunc",
        "src/a.ts::otherFunc",
        "src/a.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[my_func.clone(), other_func.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &other_func.id,
        "myFunc",
        EdgeKind::Calls,
        2,
        "src/a.ts",
        Language::Typescript,
    )])
    .unwrap();

    let resolver = fx.resolver();
    resolver.warm_caches();
    let result = resolver.resolve_and_persist_batched(None, None).unwrap();

    assert!(result.stats.total >= 1);
    assert_eq!(result.stats.resolved, 1);
    // The post-resolution callback-synthesis pass always records its count.
    assert!(result.stats.by_method.contains_key("callback-synthesis"));
    // Resolved row was deleted from unresolved_refs (metrics accuracy).
    assert_eq!(q.get_unresolved_references_count().unwrap(), 0);
}

#[test]
fn resolve_all_reports_progress_and_stats() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write("src/x.ts", "export function target(): void {}\n");
    fx.track(&q, "src/x.ts", Language::Typescript);

    let target = exported(node(
        "func:src/x.ts:target:1",
        NodeKind::Function,
        "target",
        "src/x.ts::target",
        "src/x.ts",
        Language::Typescript,
        1,
        1,
    ));
    let caller = exported(node(
        "func:src/x.ts:caller:2",
        NodeKind::Function,
        "caller",
        "src/x.ts::caller",
        "src/x.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[target.clone(), caller.clone()]).unwrap();

    let refs = vec![
        uref(
            &caller.id,
            "target",
            EdgeKind::Calls,
            2,
            "src/x.ts",
            Language::Typescript,
        ),
        uref(
            &caller.id,
            "nothingHasThisName",
            EdgeKind::Calls,
            2,
            "src/x.ts",
            Language::Typescript,
        ),
    ];

    let resolver = fx.resolver();
    let mut calls: Vec<(usize, usize)> = Vec::new();
    let mut cb = |current: usize, total: usize| calls.push((current, total));
    let result = resolver.resolve_all(&refs, Some(&mut cb));

    assert_eq!(result.stats.total, 2);
    assert_eq!(result.stats.resolved, 1);
    assert_eq!(result.stats.unresolved, 1);
    assert_eq!(result.unresolved[0].reference_name, "nothingHasThisName");
    assert_eq!(*result.stats.by_method.get("exact-match").unwrap(), 1);
    // Final progress report is always (total, total).
    assert_eq!(calls.last(), Some(&(2, 2)));

    // Denormalized fields missing → resolver back-fills from the source node.
    let bare = UnresolvedReference {
        from_node_id: caller.id.clone(),
        reference_name: "target".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 2,
        column: 0,
        file_path: None,
        language: None,
        candidates: None,
    };
    let result = resolver.resolve_all(std::slice::from_ref(&bare), None);
    assert_eq!(result.stats.resolved, 1);
    assert_eq!(result.resolved[0].original.file_path, "src/x.ts");
    assert_eq!(result.resolved[0].original.language, Language::Typescript);
}
