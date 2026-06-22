mod end_to_end {
    use codegraph::EdgeKind;

    use super::*;

    /// extraction.test.ts — "should resolve IDA sub callers and callees after
    /// indexAll" (deferred by notes/extraction-orchestrator.md).
    #[test]
    fn resolves_ida_sub_callers_and_callees_after_index_all() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join("sub_1000.c"),
            "__int64 __fastcall sub_1000(__int64 a1)\n{\n  return sub_2000(a1);\n}\n",
        );
        write(
            &dir.path().join("sub_2000.c"),
            "__int64 __fastcall sub_2000(__int64 a1)\n{\n  return a1 + 1;\n}\n",
        );

        let cg = CodeGraph::init(dir.path(), &codegraph::InitOptions::default()).unwrap();

        let caller = cg
            .get_nodes_in_file("sub_1000.c")
            .unwrap()
            .into_iter()
            .find(|n| n.kind == NodeKind::Function && n.name == "sub_1000")
            .expect("sub_1000 should be indexed");
        let callee = cg
            .get_nodes_in_file("sub_2000.c")
            .unwrap()
            .into_iter()
            .find(|n| n.kind == NodeKind::Function && n.name == "sub_2000")
            .expect("sub_2000 should be indexed");

        let callees = cg.get_callees(&caller.id, None).unwrap();
        assert!(
            callees
                .iter()
                .any(|r| r.node.id == callee.id && r.edge.kind == EdgeKind::Calls)
        );

        let callers = cg.get_callers(&callee.id, None).unwrap();
        assert!(
            callers
                .iter()
                .any(|r| r.node.id == caller.id && r.edge.kind == EdgeKind::Calls)
        );

        cg.close();
    }

    /// object-literal-methods.test.ts — "resolves callers of store actions
    /// across files (destructured + chained getState())" (deferred by
    /// notes/resolution-stitch.md).
    #[test]
    fn resolves_callers_of_store_actions_across_files() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join("package.json"),
            "{\"name\":\"t\",\"dependencies\":{\"zustand\":\"^4\"}}\n",
        );
        write(
            &dir.path().join("store.ts"),
            concat!(
                "import { create } from 'zustand'\n",
                "interface S { fetchUser(): Promise<void>; reset(): void }\n",
                "export const useStore = create<S>((set, get) => ({\n",
                "  fetchUser: async () => { get().reset() },\n",
                "  reset: () => set({}),\n",
                "}))\n",
            ),
        );
        write(
            &dir.path().join("caller.ts"),
            concat!(
                "import { useStore } from './store'\n",
                "export async function loginFlow() {\n",
                "  const { fetchUser } = useStore.getState()\n",
                "  await fetchUser()\n",
                "}\n",
                "export function hardReset() {\n",
                "  useStore.getState().reset()\n",
                "}\n",
            ),
        );

        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        cg.index_all(&IndexOptions::default()).unwrap();

        let fns = cg.get_nodes_by_kind(NodeKind::Function).unwrap();
        let fetch_user = fns
            .iter()
            .find(|n| n.name == "fetchUser" && n.file_path.ends_with("store.ts"))
            .expect("fetchUser should be indexed");
        let reset = fns
            .iter()
            .find(|n| n.name == "reset" && n.file_path.ends_with("store.ts"))
            .expect("reset should be indexed");

        // Destructured-then-bare call: loginFlow -> fetchUser
        let fetch_user_callers: Vec<String> = cg
            .get_callers(&fetch_user.id, None)
            .unwrap()
            .into_iter()
            .map(|r| r.node.name)
            .collect();
        assert!(fetch_user_callers.contains(&"loginFlow".to_string()));

        // Chained getState() call: hardReset -> reset, AND in-store sibling:
        // fetchUser -> reset
        let reset_callers: Vec<String> = cg
            .get_callers(&reset.id, None)
            .unwrap()
            .into_iter()
            .map(|r| r.node.name)
            .collect();
        assert!(reset_callers.contains(&"hardReset".to_string()));
        assert!(reset_callers.contains(&"fetchUser".to_string()));

        cg.close();
    }

    /// Smoke test: index a small fixture project end-to-end; node/edge counts
    /// are stable across a re-index, and callers/callees resolve through
    /// import-based resolution.
    #[test]
    fn smoke_index_fixture_counts_stable_and_call_edges_resolve() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join("src/a.ts"),
            "export function helper(): number { return 1; }\n",
        );
        write(
            &dir.path().join("src/b.ts"),
            concat!(
                "import { helper } from './a';\n",
                "export function main(): number { return helper(); }\n",
            ),
        );

        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        let first = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(first.success);
        assert_eq!(first.files_indexed, 2);
        assert!(first.nodes_created > 0);
        assert!(first.edges_created > 0);

        let stats_first = cg.get_stats().unwrap();
        assert!(stats_first.node_count > 0);
        assert!(stats_first.edge_count > 0);
        assert_eq!(stats_first.file_count, 2);

        // Re-index: no node/edge explosion, counts stable.
        let second = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(second.success);
        let stats_second = cg.get_stats().unwrap();
        assert_eq!(stats_second.node_count, stats_first.node_count);
        assert_eq!(stats_second.edge_count, stats_first.edge_count);

        // callers/callees resolve across the import.
        let helper = cg
            .search_nodes("helper", None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.kind == NodeKind::Function && n.name == "helper")
            .expect("helper should be indexed");
        let main = cg
            .search_nodes("main", None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.kind == NodeKind::Function && n.name == "main")
            .expect("main should be indexed");

        let callers = cg.get_callers(&helper.id, None).unwrap();
        assert!(
            callers.iter().any(|r| r.node.id == main.id),
            "main should be a caller of helper"
        );
        let callees = cg.get_callees(&main.id, None).unwrap();
        assert!(
            callees.iter().any(|r| r.node.id == helper.id),
            "helper should be a callee of main"
        );

        // The graph-shaped read APIs work end-to-end on the fixture.
        let impact = cg.get_impact_radius(&helper.id, None).unwrap();
        assert!(impact.nodes.contains_key(&main.id));
        let imports = cg.get_nodes_by_kind(NodeKind::Import).unwrap();
        assert!(imports.iter().any(|n| n.name == "./a"));
        let path = cg.find_path(&main.id, &helper.id, None).unwrap();
        assert!(path.is_some(), "a call path main -> helper should exist");

        cg.close();
    }
}
