//! Callback synthesis orchestration.

use std::collections::HashSet;

use super::channels::{closure_collection_edges, event_emitter_edges, field_channel_edges};
use super::fabric::fabric_native_impl_edges;
use super::flutter::flutter_build_edges;
use super::gin::gin_middleware_chain_edges;
use super::go_grpc::go_grpc_stub_impl_edges;
use super::jsx::react_jsx_child_edges;
use super::mybatis::mybatis_java_xml_edges;
use super::overrides::{cpp_override_edges, interface_override_edges};
use super::react::react_render_edges;
use super::rn::rn_event_edges;
use super::vue::vue_template_edges;
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::types::ResolutionContext;
use crate::types::Edge;

/// Synthesize dispatcher→callback edges (field observers + EventEmitters +
/// React re-render + JSX children + Vue templates + RN event channel +
/// Fabric native-impl + MyBatis Java↔XML + Gin middleware chain). Returns the
/// count added. Errors never throw into indexing — the TS callers wrap in
/// try/catch; Rust callers handle the `Result`.
pub fn synthesize_callback_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<usize> {
    let field_edges = field_channel_edges(queries, ctx)?;
    let closure_coll_edges = closure_collection_edges(queries, ctx)?;
    let emitter_edges = event_emitter_edges(ctx);
    let render_edges = react_render_edges(queries, ctx)?;
    let jsx_edges = react_jsx_child_edges(ctx);
    let vue_edges = vue_template_edges(ctx);
    let flutter_edges = flutter_build_edges(queries, ctx)?;
    let cpp_edges = cpp_override_edges(queries)?;
    let iface_edges = interface_override_edges(queries)?;
    let go_grpc_edges = go_grpc_stub_impl_edges(queries)?;
    let rn_event_edges_list = rn_event_edges(ctx);
    let fabric_native_edges = fabric_native_impl_edges(ctx);
    let mybatis_edges = mybatis_java_xml_edges(queries)?;
    let gin_edges = gin_middleware_chain_edges(queries, ctx)?;

    let mut merged: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for e in field_edges
        .into_iter()
        .chain(closure_coll_edges)
        .chain(emitter_edges)
        .chain(render_edges)
        .chain(jsx_edges)
        .chain(vue_edges)
        .chain(flutter_edges)
        .chain(cpp_edges)
        .chain(iface_edges)
        .chain(go_grpc_edges)
        .chain(rn_event_edges_list)
        .chain(fabric_native_edges)
        .chain(mybatis_edges)
        .chain(gin_edges)
    {
        let key = format!("{}>{}", e.source, e.target);
        if !seen.insert(key) {
            continue;
        }
        merged.push(e);
    }
    if !merged.is_empty() {
        queries.insert_edges(&merged)?;
    }
    Ok(merged.len())
}
