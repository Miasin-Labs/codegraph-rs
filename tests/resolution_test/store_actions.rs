use crate::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn resolves_callers_of_store_actions_across_files() {
    // Resolution half of the Zustand store case: extraction makes the
    // object-literal actions real `function` nodes; every call form reduces
    // to a bare-name ref that the exact-name matcher connects.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "package.json",
        "{\"name\":\"t\",\"dependencies\":{\"zustand\":\"^4\"}}\n",
    );
    fx.write(
        "store.ts",
        "import { create } from 'zustand'\ninterface S { fetchUser(): Promise<void>; reset(): void }\nexport const useStore = create<S>((set, get) => ({\n  fetchUser: async () => { get().reset() },\n  reset: () => set({}),\n}))\n",
    );
    fx.write(
        "caller.ts",
        "import { useStore } from './store'\nexport async function loginFlow() {\n  const { fetchUser } = useStore.getState()\n  await fetchUser()\n}\nexport function hardReset() {\n  useStore.getState().reset()\n}\n",
    );
    fx.track(&q, "store.ts", Language::Typescript);
    fx.track(&q, "caller.ts", Language::Typescript);

    let use_store = exported(node(
        "var:store.ts:useStore:3",
        NodeKind::Variable,
        "useStore",
        "store.ts::useStore",
        "store.ts",
        Language::Typescript,
        3,
        6,
    ));
    let fetch_user = node(
        "func:store.ts:fetchUser:4",
        NodeKind::Function,
        "fetchUser",
        "store.ts::fetchUser",
        "store.ts",
        Language::Typescript,
        4,
        4,
    );
    let reset = node(
        "func:store.ts:reset:5",
        NodeKind::Function,
        "reset",
        "store.ts::reset",
        "store.ts",
        Language::Typescript,
        5,
        5,
    );
    let login_flow = exported(node(
        "func:caller.ts:loginFlow:2",
        NodeKind::Function,
        "loginFlow",
        "caller.ts::loginFlow",
        "caller.ts",
        Language::Typescript,
        2,
        5,
    ));
    let hard_reset = exported(node(
        "func:caller.ts:hardReset:6",
        NodeKind::Function,
        "hardReset",
        "caller.ts::hardReset",
        "caller.ts",
        Language::Typescript,
        6,
        8,
    ));
    q.insert_nodes(&[
        use_store,
        fetch_user.clone(),
        reset.clone(),
        login_flow.clone(),
        hard_reset.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[
        // Destructured-then-bare call: loginFlow -> fetchUser
        uref(
            &login_flow.id,
            "fetchUser",
            EdgeKind::Calls,
            4,
            "caller.ts",
            Language::Typescript,
        ),
        // Chained getState() call reduces to bare `reset`
        uref(
            &hard_reset.id,
            "reset",
            EdgeKind::Calls,
            7,
            "caller.ts",
            Language::Typescript,
        ),
        // In-store sibling: fetchUser -> reset (get().reset())
        uref(
            &fetch_user.id,
            "reset",
            EdgeKind::Calls,
            4,
            "store.ts",
            Language::Typescript,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let fetch_user_callers: Vec<String> = incoming(&q, &fetch_user.id, EdgeKind::Calls)
        .iter()
        .filter_map(|e| q.get_node_by_id(&e.source).ok().flatten())
        .map(|n| n.name)
        .collect();
    assert!(fetch_user_callers.contains(&"loginFlow".to_string()));

    let reset_callers: Vec<String> = incoming(&q, &reset.id, EdgeKind::Calls)
        .iter()
        .filter_map(|e| q.get_node_by_id(&e.source).ok().flatten())
        .map(|n| n.name)
        .collect();
    assert!(reset_callers.contains(&"hardReset".to_string()));
    assert!(reset_callers.contains(&"fetchUser".to_string()));
}

// =============================================================================
// pr19-improvements.test.ts — resolution parts
// =============================================================================
