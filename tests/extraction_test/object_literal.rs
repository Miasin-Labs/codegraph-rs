use crate::extraction_test::fixture::*;

// =============================================================================
// describe('object-literal method extraction')
// (from __tests__/object-literal-methods.test.ts — the extraction half; the
// end-to-end resolution describe needs CodeGraph + resolution, deferred)
// =============================================================================

#[test]
fn object_literal_extracts_zustand_store_actions_as_function_nodes() {
    let code = "
      import { create } from 'zustand'
      interface Store {
        count: number
        fetchUser(): Promise<void>
        switchOrganization(id: string): Promise<void>
        reset(): void
      }
      export const useStore = create<Store>((set, get) => ({
        count: 0,
        fetchUser: async () => { await get().reset() },
        switchOrganization: async (id: string) => { set({ count: 1 }) },
        reset: () => set({ count: 0 }),
      }))
    ";
    let result = extract("store.ts", code);
    let fn_names = names(&filter_kind(&result, NodeKind::Function));
    assert!(fn_names.contains(&"fetchUser".to_string()));
    assert!(fn_names.contains(&"switchOrganization".to_string()));
    assert!(fn_names.contains(&"reset".to_string()));

    // Each action's body was walked: fetchUser references its sibling `reset`,
    // so an in-store calls edge will resolve once the pipeline runs.
    let fetch_user = result
        .nodes
        .iter()
        .find(|n| n.name == "fetchUser")
        .expect("fetchUser");
    let fetch_user_refs: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.from_node_id == fetch_user.id)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(fetch_user_refs.contains(&"reset"));

    // The action's body wasn't mis-attributed to the file scope (the reason we
    // skip the generic body-visit for the store-factory call).
    let file_node = find_kind(&result, NodeKind::File).expect("file node");
    let file_refs: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.from_node_id == file_node.id)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(!file_refs.contains(&"reset"));
}

#[test]
fn object_literal_extracts_actions_through_a_middleware_wrapper() {
    let code = "
      import { create } from 'zustand'
      import { persist } from 'zustand/middleware'
      export const useCounter = create(
        persist(
          (set, get) => ({
            value: 0,
            increment: () => set({ value: get().value + 1 }),
          }),
          { name: 'counter' }
        )
      )
    ";
    let result = extract("counter.ts", code);
    let fn_names = names(&filter_kind(&result, NodeKind::Function));
    assert!(fn_names.contains(&"increment".to_string()));
}

#[test]
fn object_literal_extracts_actions_when_the_initializer_returns_via_a_block() {
    let code = "
      import { create } from 'zustand'
      export const useThing = create((set) => {
        const initial = 0
        return {
          value: initial,
          bump: () => set({ value: 1 }),
        }
      })
    ";
    let result = extract("thing.ts", code);
    let fn_names = names(&filter_kind(&result, NodeKind::Function));
    assert!(fn_names.contains(&"bump".to_string()));
}

#[test]
fn object_literal_does_not_extract_methods_from_a_non_exported_call_wrapped_object() {
    let code = "
      function wrap(f: any) { return f }
      const local = wrap(() => ({ shouldNotExtract: () => {} }))
    ";
    let result = extract("inline.ts", code);
    let all_names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(!all_names.contains(&"shouldNotExtract"));
}

#[test]
fn object_literal_still_extracts_the_existing_direct_object_shape() {
    let code = "
      export const actions = {
        load: async () => { helper() },
      }
      function helper() {}
    ";
    let result = extract("actions.ts", code);
    let fn_names = names(&filter_kind(&result, NodeKind::Function));
    assert!(fn_names.contains(&"load".to_string()));
}
