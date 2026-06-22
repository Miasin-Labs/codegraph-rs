use crate::fixture::*;

#[test]
fn connects_include_to_real_header_file_via_include_dir_scan() {
    clear_cpp_include_dir_cache();
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "include/utils.h",
        "#ifndef UTILS_H\n#define UTILS_H\nint add(int, int);\n#endif\n",
    );
    fx.write(
        "src/main.cpp",
        "#include \"utils.h\"\n#include <vector>\nint main(){ return add(1,2); }\n",
    );
    fx.track(&q, "include/utils.h", Language::C);
    fx.track(&q, "src/main.cpp", Language::Cpp);

    // Extraction emits #include refs from the FILE node, with the include
    // path as the reference name and kind `imports`.
    let header_file = node(
        "file:include/utils.h",
        NodeKind::File,
        "utils.h",
        "include/utils.h",
        "include/utils.h",
        Language::C,
        1,
        4,
    );
    let main_file = node(
        "file:src/main.cpp",
        NodeKind::File,
        "main.cpp",
        "src/main.cpp",
        "src/main.cpp",
        Language::Cpp,
        1,
        3,
    );
    q.insert_nodes(&[header_file.clone(), main_file.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[
        uref(
            &main_file.id,
            "utils.h",
            EdgeKind::Imports,
            1,
            "src/main.cpp",
            Language::Cpp,
        ),
        uref(
            &main_file.id,
            "vector",
            EdgeKind::Imports,
            2,
            "src/main.cpp",
            Language::Cpp,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();
    clear_cpp_include_dir_cache();

    // The `#include "utils.h"` edge should target the real `include/utils.h`
    // file node — not a floating `import` node living inside main.cpp.
    let imports = outgoing(&q, &main_file.id, EdgeKind::Imports);
    let resolved_to_header = imports.iter().any(|e| e.target == header_file.id);
    assert!(
        resolved_to_header,
        "main.cpp → include/utils.h imports edge missing"
    );
    // `<vector>` should NOT produce a file edge — it's a stdlib header.
    let stdlib_edge = imports.iter().any(|e| {
        q.get_node_by_id(&e.target)
            .ok()
            .flatten()
            .map(|n| n.file_path.ends_with("vector"))
            .unwrap_or(false)
    });
    assert!(!stdlib_edge);
}

// =============================================================================
// object-literal method resolution, end-to-end (object-literal-methods.test.ts)
// =============================================================================

#[test]
fn resolve_one_skips_c_stdlib_calls_unless_declared_locally() {
    let fx = Fx::new();
    let q = fx.q();
    let caller = node(
        "func:src/main.c:main:1",
        NodeKind::Function,
        "main",
        "src/main.c::main",
        "src/main.c",
        Language::C,
        1,
        5,
    );
    q.insert_nodes(&[caller]).unwrap();

    let resolver = fx.resolver();
    resolver.warm_caches();
    let stdlib_call = UnresolvedRef {
        from_node_id: "func:src/main.c:main:1".to_string(),
        reference_name: "printf".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 2,
        column: 0,
        file_path: "src/main.c".to_string(),
        language: Language::C,
        candidates: None,
    };
    assert!(resolver.resolve_one(&stdlib_call).is_none());

    let fx = Fx::new();
    let q = fx.q();
    let caller = node(
        "func:src/main.c:main:1",
        NodeKind::Function,
        "main",
        "src/main.c::main",
        "src/main.c",
        Language::C,
        1,
        5,
    );
    let local_printf = node(
        "func:src/main.c:printf:7",
        NodeKind::Function,
        "printf",
        "src/main.c::printf",
        "src/main.c",
        Language::C,
        7,
        9,
    );
    q.insert_nodes(&[caller, local_printf]).unwrap();

    let resolver = fx.resolver();
    resolver.warm_caches();
    let resolved = resolver
        .resolve_one(&stdlib_call)
        .expect("declared local printf resolves");
    assert_eq!(resolved.target_node_id, "func:src/main.c:printf:7");
}

// =============================================================================
// Progress reporting + resolve_all shape (resolver-internal contracts the TS
// suite exercised through cg.resolveReferences)
// =============================================================================
