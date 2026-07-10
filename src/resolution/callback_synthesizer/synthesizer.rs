//! Callback synthesis orchestration.

use std::collections::HashSet;

use super::arkui::{arkui_emitter_edges, arkui_router_edges, arkui_state_build_edges};
use super::c_fnptr::c_fn_pointer_dispatch_edges;
use super::celery::celery_dispatch_edges;
use super::channels::{closure_collection_edges, event_emitter_edges, field_channel_edges};
use super::conventions::{pascal_form_edges, sveltekit_load_edges};
use super::cross_platform::{expo_cross_platform_edges, rn_cross_platform_edges};
use super::erlang::erlang_behaviour_dispatch_edges;
use super::fabric::fabric_native_impl_edges;
use super::flutter::flutter_build_edges;
use super::gin::gin_middleware_chain_edges;
use super::go_grpc::go_grpc_stub_impl_edges;
use super::go_interfaces::{go_cross_file_method_contains_edges, go_implements_edges};
use super::goframe::goframe_route_edges;
use super::jsx::react_jsx_child_edges;
use super::kotlin::kotlin_expect_actual_edges;
use super::laravel_events::laravel_event_edges;
use super::mediatr::mediatr_dispatch_edges;
use super::mybatis::mybatis_java_xml_edges;
use super::nix::nix_option_path_edges;
use super::object_registry::object_registry_edges;
use super::overrides::{cpp_override_edges, interface_override_edges};
use super::react::react_render_edges;
use super::rn::rn_event_edges;
use super::sidekiq::sidekiq_dispatch_edges;
use super::spring::spring_event_edges;
use super::state_stores::{
    pinia_store_edges,
    redux_thunk_edges,
    rtk_query_edges,
    vuex_dispatch_edges,
};
use super::vue::vue_template_edges;
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::types::ResolutionContext;
use crate::types::Edge;

const SYNTH_INSERT_BATCH: usize = 2_000;

fn persist_pass(
    queries: &QueryBuilder,
    edges: Vec<Edge>,
    seen: &mut HashSet<String>,
) -> Result<usize> {
    let mut count = 0usize;
    let mut batch = Vec::with_capacity(SYNTH_INSERT_BATCH);
    for edge in edges {
        let key = format!("{}>{}", edge.source, edge.target);
        if !seen.insert(key) {
            continue;
        }
        batch.push(edge);
        if batch.len() == SYNTH_INSERT_BATCH {
            queries.insert_edges(&batch)?;
            count += batch.len();
            batch.clear();
        }
    }
    if !batch.is_empty() {
        queries.insert_edges(&batch)?;
        count += batch.len();
    }
    Ok(count)
}

/// Synthesize dispatcher→callback edges (field observers + EventEmitters +
/// React re-render + JSX children + Vue templates + RN event channel +
/// Fabric native-impl + MyBatis Java↔XML + Gin middleware chain). Returns the
/// count added. Errors never throw into indexing — the TS callers wrap in
/// try/catch; Rust callers handle the `Result`.
pub fn synthesize_callback_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<usize> {
    let languages = queries.get_distinct_file_languages()?;
    let has = |wanted: &[&str]| wanted.iter().any(|language| languages.contains(*language));
    let js_family = ["typescript", "javascript", "tsx", "jsx"];
    let mut seen: HashSet<String> = HashSet::new();
    let mut count = 0usize;

    count += persist_pass(queries, field_channel_edges(queries, ctx)?, &mut seen)?;
    count += persist_pass(queries, closure_collection_edges(queries, ctx)?, &mut seen)?;
    count += persist_pass(queries, event_emitter_edges(ctx), &mut seen)?;
    count += persist_pass(queries, react_render_edges(queries, ctx)?, &mut seen)?;
    count += persist_pass(queries, react_jsx_child_edges(ctx), &mut seen)?;
    if has(&["vue"]) {
        count += persist_pass(queries, vue_template_edges(ctx), &mut seen)?;
    }
    if has(&["svelte"]) {
        count += persist_pass(queries, sveltekit_load_edges(ctx), &mut seen)?;
    }
    if has(&["pascal"]) {
        count += persist_pass(queries, pascal_form_edges(ctx), &mut seen)?;
    }
    if has(&["dart"]) {
        count += persist_pass(queries, flutter_build_edges(queries, ctx)?, &mut seen)?;
    }
    // These edges are inputs to the generic interface override pass below.
    if has(&["go"]) {
        count += persist_pass(
            queries,
            go_cross_file_method_contains_edges(queries)?,
            &mut seen,
        )?;
        count += persist_pass(queries, go_implements_edges(queries)?, &mut seen)?;
    }
    if has(&["cpp"]) {
        count += persist_pass(queries, cpp_override_edges(queries)?, &mut seen)?;
    }
    if has(&[
        "java",
        "kotlin",
        "csharp",
        "swift",
        "scala",
        "go",
        "rust",
        "arkts",
        "typescript",
        "javascript",
        "tsx",
        "jsx",
    ]) {
        count += persist_pass(queries, interface_override_edges(queries)?, &mut seen)?;
    }
    if has(&["kotlin"]) {
        count += persist_pass(queries, kotlin_expect_actual_edges(queries)?, &mut seen)?;
    }
    if has(&["go"]) {
        count += persist_pass(queries, go_grpc_stub_impl_edges(queries)?, &mut seen)?;
    }
    if has(&js_family) {
        count += persist_pass(queries, rn_event_edges(ctx), &mut seen)?;
        count += persist_pass(queries, rn_cross_platform_edges(queries)?, &mut seen)?;
    }
    // Expo Modules pairs native Swift/Kotlin implementations directly and
    // remains meaningful in a native-only package with no JS-family files.
    count += persist_pass(queries, expo_cross_platform_edges(queries)?, &mut seen)?;
    count += persist_pass(queries, fabric_native_impl_edges(ctx), &mut seen)?;
    if has(&["java", "kotlin"]) && has(&["xml"]) {
        count += persist_pass(queries, mybatis_java_xml_edges(queries)?, &mut seen)?;
    }
    if has(&["go"]) {
        count += persist_pass(
            queries,
            gin_middleware_chain_edges(queries, ctx)?,
            &mut seen,
        )?;
    }
    if has(&js_family) {
        count += persist_pass(queries, redux_thunk_edges(queries, ctx)?, &mut seen)?;
    }
    count += persist_pass(queries, object_registry_edges(ctx), &mut seen)?;
    if has(&js_family) {
        count += persist_pass(queries, rtk_query_edges(queries, ctx)?, &mut seen)?;
    }
    if has(&["vue", "typescript", "javascript", "tsx", "jsx"]) {
        count += persist_pass(queries, pinia_store_edges(ctx), &mut seen)?;
        count += persist_pass(queries, vuex_dispatch_edges(ctx), &mut seen)?;
    }
    if has(&["ruby"]) {
        count += persist_pass(queries, sidekiq_dispatch_edges(ctx), &mut seen)?;
    }
    if has(&["python"]) {
        count += persist_pass(queries, celery_dispatch_edges(ctx), &mut seen)?;
    }
    if has(&["java"]) {
        count += persist_pass(queries, spring_event_edges(ctx), &mut seen)?;
    }
    if has(&["csharp"]) {
        count += persist_pass(queries, mediatr_dispatch_edges(ctx), &mut seen)?;
    }
    if has(&["php"]) {
        count += persist_pass(queries, laravel_event_edges(ctx), &mut seen)?;
    }
    if has(&["c", "cpp"]) {
        count += persist_pass(
            queries,
            c_fn_pointer_dispatch_edges(queries, ctx)?,
            &mut seen,
        )?;
    }
    if has(&["go"]) {
        count += persist_pass(queries, goframe_route_edges(queries)?, &mut seen)?;
    }
    if has(&["arkts"]) {
        count += persist_pass(queries, arkui_state_build_edges(queries, ctx)?, &mut seen)?;
        count += persist_pass(queries, arkui_emitter_edges(ctx), &mut seen)?;
        count += persist_pass(queries, arkui_router_edges(ctx), &mut seen)?;
    }
    if has(&["erlang"]) {
        count += persist_pass(
            queries,
            erlang_behaviour_dispatch_edges(queries, ctx)?,
            &mut seen,
        )?;
    }
    if has(&["nix"]) {
        count += persist_pass(queries, nix_option_path_edges(queries)?, &mut seen)?;
    }

    Ok(count)
}
