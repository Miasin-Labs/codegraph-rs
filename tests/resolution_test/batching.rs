use super::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn batched_resolution_keeps_references_after_the_first_five_thousand() {
    // Given: one caller with 5,001 independently located calls to one target.
    let fx = Fx::new();
    fx.write("src/caller.ts", "export function caller() {}\n");
    fx.write("src/target.ts", "export function target() {}\n");
    let q = fx.q();
    fx.track(&q, "src/caller.ts", Language::Typescript);
    fx.track(&q, "src/target.ts", Language::Typescript);
    let caller = node(
        "caller",
        NodeKind::Function,
        "caller",
        "caller",
        "src/caller.ts",
        Language::Typescript,
        1,
        5_002,
    );
    let target = exported(node(
        "target",
        NodeKind::Function,
        "target",
        "target",
        "src/target.ts",
        Language::Typescript,
        1,
        1,
    ));
    q.insert_nodes(&[caller, target.clone()]).unwrap();
    let references: Vec<UnresolvedReference> = (1..=5_001)
        .map(|line| {
            uref(
                "caller",
                "target",
                EdgeKind::Calls,
                line,
                "src/caller.ts",
                Language::Typescript,
            )
        })
        .collect();
    q.insert_unresolved_refs_batch(&references).unwrap();

    // When: resolution crosses its default 5,000-row batch boundary.
    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    // Then: every located call survives and no unresolved row is discarded.
    let calls = incoming(&q, &target.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 5_001);
    assert_eq!(q.get_unresolved_references_count().unwrap(), 0);
}
