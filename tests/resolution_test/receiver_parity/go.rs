use crate::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn go_field_chain_resolves_project_field_and_rejects_external_same_name_decoy() {
    // Given: one in-project field chain and one external sql.DB field chain.
    let fx = Fx::new();
    let q = fx.q();
    fx.write("go.mod", "module example.com/app\n\ngo 1.22\n");
    fx.write(
        "flow.go",
        "package flow\n\nimport \"database/sql\"\n\ntype Repo struct { db *Store; conn *sql.DB }\nfunc (r *Repo) Save() { r.db.Put(\"k\") }\nfunc (r *Repo) Write() { r.conn.Exec(\"insert\") }\n",
    );
    fx.track(&q, "flow.go", Language::Go);

    let repo = node(
        "struct:repo",
        NodeKind::Struct,
        "Repo",
        "Repo",
        "flow.go",
        Language::Go,
        5,
        5,
    );
    let save = node(
        "method:repo:save",
        NodeKind::Method,
        "Save",
        "Repo::Save",
        "flow.go",
        Language::Go,
        6,
        6,
    );
    let write = node(
        "method:repo:write",
        NodeKind::Method,
        "Write",
        "Repo::Write",
        "flow.go",
        Language::Go,
        7,
        7,
    );
    let put = node(
        "method:store:put",
        NodeKind::Method,
        "Put",
        "Store::Put",
        "store.go",
        Language::Go,
        2,
        2,
    );
    let exec_decoy = node(
        "method:decoy:exec",
        NodeKind::Method,
        "Exec",
        "InternalStore::Exec",
        "decoy.go",
        Language::Go,
        2,
        2,
    );
    q.insert_nodes(&[
        repo,
        save.clone(),
        write.clone(),
        put.clone(),
        exec_decoy.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[
        uref(
            &save.id,
            "r.db.Put",
            EdgeKind::Calls,
            6,
            "flow.go",
            Language::Go,
        ),
        uref(
            &write.id,
            "r.conn.Exec",
            EdgeKind::Calls,
            7,
            "flow.go",
            Language::Go,
        ),
    ])
    .unwrap();

    // When: both selector-chain calls are resolved.
    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    // Then: the project field resolves and the external field stays unlinked.
    let save_calls = outgoing(&q, &save.id, EdgeKind::Calls);
    assert_eq!(save_calls.len(), 1);
    assert_eq!(save_calls[0].target, put.id);
    assert!(outgoing(&q, &write.id, EdgeKind::Calls).is_empty());
    assert!(incoming(&q, &exec_decoy.id, EdgeKind::Calls).is_empty());
}
