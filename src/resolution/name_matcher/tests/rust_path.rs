use super::{Fixture, match_reference, node};
use crate::resolution::types::UnresolvedRef;
use crate::types::{EdgeKind, Language, NodeKind, Visibility};

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

#[test]
fn calls_ignore_modules_and_imports_that_share_the_name() {
    // `crate::mcp::tools::tools` next to a `tools` module and a `use` of it.
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/mcp/service.rs"),
        node(
            "tools-mod",
            NodeKind::Module,
            "tools",
            "tools",
            "src/mcp/mod.rs",
            Language::Rust,
            1,
            1,
        ),
        node(
            "tools-use",
            NodeKind::Import,
            "tools",
            "tools",
            "src/mcp/tools/registry.rs",
            Language::Rust,
            1,
            1,
        ),
        func(
            "tools-fn",
            "tools",
            "tools",
            "src/mcp/tools/registry/catalog.rs",
        ),
    ]);
    let r = rust_ref("caller", "crate::mcp::tools::tools", "src/mcp/service.rs");
    assert_eq!(
        match_reference(&r, &ctx)
            .expect("resolves to the fn")
            .target_node_id,
        "tools-fn"
    );
}

fn import(id: &str, qualified: &str, file: &str, signature: &str) -> crate::types::Node {
    let name = qualified.rsplit("::").next().unwrap_or(qualified);
    let mut node = node(
        id,
        NodeKind::Import,
        name,
        qualified,
        file,
        Language::Rust,
        1,
        1,
    );
    node.signature = Some(signature.into());
    node
}

fn module(id: &str, name: &str, file: &str) -> crate::types::Node {
    node(id, NodeKind::Module, name, name, file, Language::Rust, 1, 1)
}

fn with_visibility(mut node: crate::types::Node, visibility: Visibility) -> crate::types::Node {
    node.visibility = Some(visibility);
    node
}

#[test]
fn crate_path_follows_a_pub_use_chain_across_modules() {
    // `crate::mcp::tools::tools` reaches `registry/catalog.rs` through
    // `pub use registry::{…, tools}` (tools/mod.rs) and `pub use
    // catalog::tools` (registry.rs). Same-named modules, imports, and an
    // unrelated `tools` fn must not interfere.
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/mcp/service.rs"),
        module("tools-mod", "tools", "src/mcp/mod.rs"),
        import(
            "mcp-use",
            "tools",
            "src/mcp/mod.rs",
            "pub use tools::{ToolHandler, tools as tool_list};",
        ),
        module("tools-ctx-mod", "tools", "src/mcp/tools/context.rs"),
        import(
            "tools-reexport",
            "registry",
            "src/mcp/tools/mod.rs",
            "pub use registry::{get_static_tools, tools};",
        ),
        import(
            "registry-reexport",
            "catalog",
            "src/mcp/tools/registry.rs",
            "pub use catalog::tools;",
        ),
        func(
            "tools-fn",
            "tools",
            "tools",
            "src/mcp/tools/registry/catalog.rs",
        ),
        func("decoy-fn", "tools", "tools", "src/cli/help.rs"),
    ]);
    for path in [
        "crate::mcp::tools::tools",
        "crate::mcp::tools::registry::tools",
    ] {
        let r = rust_ref("caller", path, "src/mcp/service.rs");
        let hit = match_reference(&r, &ctx).expect("re-export chain resolves");
        assert_eq!(hit.target_node_id, "tools-fn", "{path}");
        assert_eq!(hit.confidence, 0.95, "{path}");
    }
    // A rename one module up follows the same chain.
    let r = rust_ref("caller", "crate::mcp::tool_list", "src/mcp/service.rs");
    assert_eq!(
        match_reference(&r, &ctx)
            .expect("renamed re-export resolves")
            .target_node_id,
        "tools-fn"
    );
}

#[test]
fn super_super_reaches_a_restricted_brace_list_reexport() {
    // notices.rs calls `super::super::format::now_ms`; format.rs re-exports
    // it from `format/budget.rs` in a `pub(in crate::mcp::tools)` list.
    let ctx = Fixture::new(vec![
        func(
            "caller",
            "append",
            "append",
            "src/mcp/tools/context/notices.rs",
        ),
        import(
            "format-use",
            "budget",
            "src/mcp/tools/format.rs",
            "pub(in crate::mcp::tools) use budget::{\n    adaptive_explore_enabled,\n    // wall clock\n    now_ms,\n    output_char_cap,\n};",
        ),
        func("now", "now_ms", "now_ms", "src/mcp/tools/format/budget.rs"),
        func("daemon-now", "now_ms", "now_ms", "src/mcp/daemon.rs"),
    ]);
    let r = rust_ref(
        "caller",
        "super::super::format::now_ms",
        "src/mcp/tools/context/notices.rs",
    );
    let hit = match_reference(&r, &ctx).expect("brace-list re-export resolves");
    assert_eq!(hit.target_node_id, "now");
    assert_eq!(hit.confidence, 0.95);
}

#[test]
fn an_as_rename_binds_the_new_name() {
    // `pub use crate::engine::start as launch;` — no node is named `launch`.
    let ctx = Fixture::new(vec![
        func("caller", "main", "main", "src/cli.rs"),
        import(
            "api-use",
            "crate",
            "src/api.rs",
            "pub use crate::engine::start as launch;",
        ),
        func("start", "start", "start", "src/engine.rs"),
        func("other-start", "start", "start", "src/worker.rs"),
    ]);
    let r = rust_ref("caller", "crate::api::launch", "src/cli.rs");
    let hit = match_reference(&r, &ctx).expect("renamed re-export resolves");
    assert_eq!(hit.target_node_id, "start");
    assert_eq!(hit.confidence, 0.95);
}

#[test]
fn a_glob_reexport_supplies_names_its_module_does_not_define() {
    // `pub use crate::util::*; pub use crate::internal::*;` — `internal`'s
    // private `helper` is invisible to the glob, so only `util`'s counts.
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/app.rs"),
        import(
            "prelude-util",
            "crate",
            "src/prelude.rs",
            "pub use crate::util::*;",
        ),
        import(
            "prelude-internal",
            "crate",
            "src/prelude.rs",
            "pub use crate::internal::*;",
        ),
        with_visibility(
            func("util-helper", "helper", "helper", "src/util.rs"),
            Visibility::Public,
        ),
        with_visibility(
            func("internal-helper", "helper", "helper", "src/internal.rs"),
            Visibility::Private,
        ),
    ]);
    let r = rust_ref("caller", "crate::prelude::helper", "src/app.rs");
    let hit = match_reference(&r, &ctx).expect("glob re-export resolves");
    assert_eq!(hit.target_node_id, "util-helper");
    assert_eq!(hit.confidence, 0.95);
}

#[test]
fn a_single_import_shadows_a_glob() {
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/app.rs"),
        import(
            "prelude-glob",
            "crate",
            "src/prelude.rs",
            "pub use crate::a::*;",
        ),
        import(
            "prelude-single",
            "crate",
            "src/prelude.rs",
            "pub use crate::b::helper;",
        ),
        func("a-helper", "helper", "helper", "src/a.rs"),
        func("b-helper", "helper", "helper", "src/b.rs"),
    ]);
    let r = rust_ref("caller", "crate::prelude::helper", "src/app.rs");
    assert_eq!(
        match_reference(&r, &ctx)
            .expect("single import wins")
            .target_node_id,
        "b-helper"
    );
}

#[test]
fn globs_naming_different_items_are_ambiguous() {
    // Two globs bring different `helper`s. Only the one in `a.rs` has the
    // bare-name shape the last-resort fallback accepts, so that fallback
    // would pick it — ambiguity must stop resolution before it does.
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/app.rs"),
        import(
            "prelude-a",
            "crate",
            "src/prelude.rs",
            "pub use crate::a::*;",
        ),
        import(
            "prelude-b",
            "crate",
            "src/prelude.rs",
            "pub use crate::b::*;",
        ),
        func("a-helper", "helper", "helper", "src/a.rs"),
        // `mod b { pub fn helper() {} }` inline in lib.rs.
        func("b-helper", "helper", "b::helper", "src/lib.rs"),
    ]);
    let r = rust_ref("caller", "crate::prelude::helper", "src/app.rs");
    let hit = super::super::match_rust_path(&r, &ctx);
    assert!(
        hit.is_none(),
        "glob-vs-glob ambiguity must not resolve: {hit:?}"
    );

    // Two globs that reach the SAME item are not ambiguous.
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/app.rs"),
        import(
            "prelude-a",
            "crate",
            "src/prelude.rs",
            "pub use crate::a::*;",
        ),
        import(
            "prelude-b",
            "crate",
            "src/prelude.rs",
            "pub use crate::b::*;",
        ),
        import("b-use", "crate", "src/b.rs", "pub use crate::a::helper;"),
        func("a-helper", "helper", "helper", "src/a.rs"),
        func("c-helper", "helper", "helper", "src/c.rs"),
    ]);
    let hit = match_reference(&r, &ctx).expect("one item through two globs");
    assert_eq!(hit.target_node_id, "a-helper");
    assert_eq!(hit.confidence, 0.95);
}

#[test]
fn reexport_cycles_terminate() {
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/app.rs"),
        // Glob cycle: a <-> b. `g` lives in b.
        import("a-glob", "crate", "src/a.rs", "pub use crate::b::*;"),
        import("b-glob", "crate", "src/b.rs", "pub use crate::a::*;"),
        func("g", "g", "g", "src/b.rs"),
        func("other-g", "g", "g", "src/c.rs"),
        // Single-import cycle: x <-> y, and nothing defines `f`.
        import("x-use", "crate", "src/x.rs", "pub use crate::y::f;"),
        import("y-use", "crate", "src/y.rs", "pub use crate::x::f;"),
    ]);
    let r = rust_ref("caller", "crate::a::g", "src/app.rs");
    assert_eq!(
        match_reference(&r, &ctx)
            .expect("glob cycle still finds g")
            .target_node_id,
        "g"
    );
    let r = rust_ref("caller", "crate::a::missing", "src/app.rs");
    assert!(super::super::match_rust_path(&r, &ctx).is_none());
    let r = rust_ref("caller", "crate::x::f", "src/app.rs");
    assert!(super::super::match_rust_path(&r, &ctx).is_none());
}

#[test]
fn a_use_inside_an_inline_module_binds_only_there() {
    // `mod tests { use crate::util::helper; }` in lib.rs does not put
    // `helper` in the crate root.
    let ctx = Fixture::new(vec![
        func("caller", "run", "run", "src/app.rs"),
        import(
            "tests-use",
            "tests::crate",
            "src/lib.rs",
            "use crate::util::helper;",
        ),
        func("util-helper", "helper", "helper", "src/util.rs"),
        func("misc-helper", "helper", "helper", "src/misc.rs"),
        func("case", "case", "tests::case", "src/lib.rs"),
    ]);
    let r = rust_ref("caller", "crate::helper", "src/app.rs");
    assert!(super::super::match_rust_path(&r, &ctx).is_none());
    // …but it does bind inside `tests`.
    let r = rust_ref("case", "self::helper", "src/lib.rs");
    assert_eq!(
        match_reference(&r, &ctx)
            .expect("the inline module sees its own use")
            .target_node_id,
        "util-helper"
    );
}
