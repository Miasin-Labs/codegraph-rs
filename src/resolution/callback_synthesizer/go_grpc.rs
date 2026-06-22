//! Go gRPC generated-stub to handwritten-impl synthesis.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::methods_of;
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::extraction::generated_detection::is_generated_file;
use crate::types::{Edge, Language, Node, NodeKind};

const MAX_CALLBACKS_PER_CHANNEL: usize = 40;

/// Go gRPC stub → impl bridge. The protoc-gen-go-grpc codegen emits an
/// `UnimplementedXxxServer` struct in `*_grpc.pb.go` carrying one method
/// per service RPC; the real handler is a hand-written struct in another
/// file (`x/bank/keeper/msg_server.go::msgServer.Send` in cosmos-sdk).
/// Go's structural typing means no `implements` edge exists for our
/// resolver to follow, so `trace("Send","SendCoins")` lands on the
/// empty stub and reports "no path" (validated empirically — the cosmos
/// Q1 r1 trace failure that drove this work).
///
/// Bridge: for each `UnimplementedXxxServer` whose RPC-method names are
/// a SUBSET of some other Go struct's method names, emit `calls` edges
/// `stub.method → impl.method` (paired by name). Excludes the gRPC
/// internal markers `mustEmbedUnimplementedXxxServer` and
/// `testEmbeddedByValue`, and skips candidate impls that themselves
/// live in a generated file (their `xxxClient` / sibling stubs would
/// otherwise look like impls).
///
/// Multiple candidates is allowed and capped at MAX_CALLBACKS_PER_CHANNEL —
/// a service often has both a production impl and one or more test
/// mocks; linking to all preserves trace utility without false-favoring.
///
/// Provenance: `heuristic`, `synthesizedBy: 'go-grpc-stub-impl'`. The
/// stub's source line is the wiring site shown in the trace trail.
pub(super) fn go_grpc_stub_impl_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    static STUB_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^Unimplemented.*Server$").expect("valid regex"));
    // gRPC internal-helper methods that appear on every Unimplemented*Server;
    // not part of the service contract, so exclude when computing the RPC-method
    // signature used to match impls.
    fn is_internal_marker(n: &str) -> bool {
        n.starts_with("mustEmbed") || n == "testEmbeddedByValue"
    }

    // Methods directly contained by each Go struct, name-only. Built once.
    let mut method_names_by_struct: HashMap<String, HashSet<String>> = HashMap::new();
    let mut method_nodes_by_struct: HashMap<String, Vec<Node>> = HashMap::new();
    let mut go_structs: Vec<Node> = Vec::new();
    for s in queries.get_nodes_by_kind(NodeKind::Struct)? {
        if s.language != Language::Go {
            continue;
        }
        let ms = methods_of(queries, &s.id)?;
        method_names_by_struct.insert(s.id.clone(), ms.iter().map(|m| m.name.clone()).collect());
        method_nodes_by_struct.insert(s.id.clone(), ms);
        go_structs.push(s);
    }

    for stub in &go_structs {
        if !STUB_RE.is_match(&stub.name) {
            continue;
        }
        // The stub MUST live in a generated file — that's what tells us this is
        // a protoc-emitted scaffold rather than someone naming a struct
        // `UnimplementedXxxServer` by hand. Without this gate we'd also bridge
        // such hand-written structs and create misleading edges.
        if !is_generated_file(&stub.file_path) {
            continue;
        }

        let stub_methods: Vec<&Node> = method_nodes_by_struct
            .get(&stub.id)
            .map(|ms| ms.iter().filter(|m| !is_internal_marker(&m.name)).collect())
            .unwrap_or_default();
        if stub_methods.is_empty() {
            continue;
        }
        let stub_method_names: Vec<&str> = stub_methods.iter().map(|m| m.name.as_str()).collect();

        for cand in &go_structs {
            if cand.id == stub.id {
                continue;
            }
            // Skip generated-file candidates — they're siblings (msgClient,
            // UnsafeMsgServer, …) whose method sets coincidentally match.
            if is_generated_file(&cand.file_path) {
                continue;
            }

            let Some(cand_names) = method_names_by_struct.get(&cand.id) else {
                continue;
            };
            // Subset: every RPC method must exist on the candidate by name.
            // Signature-level match would tighten this further, but name-match
            // alone already gives one-to-one pairing in real codebases because
            // gRPC method-name sets are highly distinctive (Send + MultiSend +
            // UpdateParams + SetSendEnabled is unique to bank's MsgServer).
            if !stub_method_names.iter().all(|n| cand_names.contains(*n)) {
                continue;
            }

            let empty: Vec<Node> = Vec::new();
            let cand_methods = method_nodes_by_struct.get(&cand.id).unwrap_or(&empty);
            let mut added = 0usize;
            for sm in &stub_methods {
                if added >= MAX_CALLBACKS_PER_CHANNEL {
                    break;
                }
                for cm in cand_methods {
                    if added >= MAX_CALLBACKS_PER_CHANNEL {
                        break;
                    }
                    if cm.name != sm.name {
                        continue;
                    }
                    let key = format!("{}>{}", sm.id, cm.id);
                    if !seen.insert(key) {
                        continue;
                    }
                    edges.push(synthesized_edge(
                        &sm.id,
                        &cm.id,
                        Some(sm.start_line),
                        edge_meta(vec![
                            ("synthesizedBy", Value::from("go-grpc-stub-impl")),
                            ("via", Value::from(cm.name.as_str())),
                            (
                                "registeredAt",
                                Value::from(format!("{}:{}", cm.file_path, cm.start_line)),
                            ),
                        ]),
                    ));
                    added += 1;
                }
            }
        }
    }
    Ok(edges)
}
