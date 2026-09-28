//! Nominal override and interface dispatch synthesis.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::methods_of;
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::types::{Edge, EdgeKind, Language, Node, NodeKind};

const MAX_CALLBACKS_PER_CHANNEL: usize = 40;

/// How many implementations one Rust trait method's dispatch edges reach.
/// A call on `dyn Trait` or a `T: Trait` lands on the trait's declaration
/// (resolution's `trait-dispatch`); callers and impact follow these edges
/// on to the implementations, so a trait implemented everywhere (`Future`
/// for every state machine of a crate) must not fan every such call out.
pub(crate) const MAX_RUST_IMPLEMENTATIONS: usize = 64;

/// Phase 4c: C++ virtual override. A call through a base/interface pointer
/// (`db->Get(...)`, `iter->Next()`) dispatches at runtime to a subclass override,
/// but that hop is a vtable indirection — no static call edge — so a flow stops at
/// the abstract base method. Bridge it like react-render: for each C++ class that
/// `extends` a base, link each base method → the subclass method of the same name
/// (the override), so trace/callees from the interface method reach the
/// implementation(s). Over-approximation accepted (reachability-correct); capped
/// per class and gated to C++ to avoid touching other languages' dispatch.
pub(super) fn cpp_override_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for cls in queries.get_nodes_by_kind(NodeKind::Class)? {
        let sub_methods: Vec<Node> = methods_of(queries, &cls.id)?
            .into_iter()
            .filter(|n| n.language == Language::Cpp)
            .collect();
        if sub_methods.is_empty() {
            continue;
        }
        for ext in queries.get_outgoing_edges(&cls.id, Some(&[EdgeKind::Extends]), None)? {
            let Some(base) = queries.get_node_by_id(&ext.target)? else {
                continue;
            };
            if base.language != Language::Cpp || base.id == cls.id {
                continue;
            }
            // JS `new Map(...)` semantics: a later same-name method overwrites.
            let mut base_methods: HashMap<String, Node> = HashMap::new();
            for bm in methods_of(queries, &base.id)? {
                base_methods.insert(bm.name.clone(), bm);
            }
            let mut added = 0usize;
            for m in &sub_methods {
                if added >= MAX_CALLBACKS_PER_CHANNEL {
                    break;
                }
                let Some(bm) = base_methods.get(&m.name) else {
                    continue;
                };
                if bm.id == m.id {
                    continue;
                }
                let key = format!("{}>{}", bm.id, m.id);
                if !seen.insert(key) {
                    continue;
                }
                edges.push(synthesized_edge(
                    &bm.id,
                    &m.id,
                    Some(bm.start_line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("cpp-override")),
                        ("via", Value::from(m.name.as_str())),
                        (
                            "registeredAt",
                            Value::from(format!("{}:{}", m.file_path, m.start_line)),
                        ),
                    ]),
                ));
                added += 1;
            }
        }
    }
    Ok(edges)
}

/// Languages whose static `implements`/`extends` edges should bridge an
/// interface (or abstract base) method to the matching concrete-class method.
/// The set is "languages with explicit nominal subtyping and a single class
/// kind that holds methods" — i.e. the shape this loop expects. Swift and
/// Scala fit shape-wise (Swift `protocol`/`class`, Scala `trait`/`class`)
/// and are included; their concrete-side nodes can be a `struct` (Swift)
/// or an `object` (Scala) so the loop also iterates those kinds.
fn is_iface_override_lang(lang: Language) -> bool {
    // Mirrors TS `IFACE_OVERRIDE_LANGS`. Go is included so an interface-method
    // call bridges to the concrete struct's override (paired with the
    // `go-implements` method-set pass), closing caller -> interface -> impl for
    // the package-accessor chain (#1640).
    matches!(
        lang,
        Language::Java
            | Language::Kotlin
            | Language::Csharp
            | Language::Typescript
            | Language::Javascript
            | Language::Rust
            | Language::Swift
            | Language::Scala
            | Language::Go
            | Language::Arkts
    )
}

/// Phase 5.5: interface / abstract dispatch (Java, Kotlin). A call through an
/// injected interface (`@Autowired FooService svc; svc.list()`) or an abstract
/// base dispatches at runtime to the implementing class's override — a vtable
/// indirection with no static call edge — so a request→service flow stops at the
/// interface method. Bridge it like cpp-override: for each class that
/// `implements` an interface (or `extends` an abstract base), link each
/// base/interface method → the class's same-name method (the override) so
/// trace/callees reach the implementation. Over-approximation accepted
/// (reachability-correct); capped per class, gated to JVM languages.
pub(super) fn interface_override_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Concrete-side kinds vary by language: `class` covers Java / Kotlin /
    // C# / TS / Swift-classes / Scala-classes; `struct` covers Swift value
    // types that conform to protocols; a Rust `enum` implements traits like
    // a struct does.
    let concrete_kinds = [
        NodeKind::Class,
        NodeKind::Struct,
        NodeKind::Union,
        NodeKind::Enum,
    ];
    for kind in concrete_kinds {
        for cls in queries.get_nodes_by_kind(kind)? {
            if kind == NodeKind::Enum && cls.language != Language::Rust {
                continue;
            }
            let impl_methods: Vec<Node> = methods_of(queries, &cls.id)?
                .into_iter()
                .filter(|n| is_iface_override_lang(n.language))
                .collect();
            if impl_methods.is_empty() {
                continue;
            }
            for sup in queries.get_outgoing_edges(
                &cls.id,
                Some(&[EdgeKind::Implements, EdgeKind::Extends]),
                None,
            )? {
                let Some(base) = queries.get_node_by_id(&sup.target)? else {
                    continue;
                };
                if !is_iface_override_lang(base.language) || base.id == cls.id {
                    continue;
                }
                // Group impl methods by name to handle OVERLOADS: an interface `list()` and
                // `list(params)` are distinct nodes and a call may resolve to either, so
                // link every base overload → every same-name impl overload (keying by name
                // alone would drop all but one and miss the resolved overload).
                let mut impl_by_name: HashMap<&str, Vec<&Node>> = HashMap::new();
                for m in &impl_methods {
                    impl_by_name.entry(m.name.as_str()).or_default().push(m);
                }
                let mut added = 0usize;
                for bm in methods_of(queries, &base.id)? {
                    if added >= MAX_CALLBACKS_PER_CHANNEL {
                        break;
                    }
                    let Some(impls) = impl_by_name.get(bm.name.as_str()) else {
                        continue;
                    };
                    for m in impls {
                        if added >= MAX_CALLBACKS_PER_CHANNEL {
                            break;
                        }
                        if bm.id == m.id {
                            continue;
                        }
                        let key = format!("{}>{}", bm.id, m.id);
                        if !seen.insert(key) {
                            continue;
                        }
                        edges.push(synthesized_edge(
                            &bm.id,
                            &m.id,
                            Some(bm.start_line),
                            edge_meta(vec![
                                ("synthesizedBy", Value::from("interface-impl")),
                                ("via", Value::from(m.name.as_str())),
                                (
                                    "registeredAt",
                                    Value::from(format!("{}:{}", m.file_path, m.start_line)),
                                ),
                            ]),
                        ));
                        added += 1;
                    }
                }
            }
        }
    }
    cap_rust_dispatch(queries, &mut edges)?;
    Ok(edges)
}

/// Keep at most [`MAX_RUST_IMPLEMENTATIONS`] dispatch edges per Rust trait
/// method, the implementations nearest the declaration first (same file,
/// then by path), and say so on the edges kept: `implementations` (how
/// many there are) and `capped`.
fn cap_rust_dispatch(queries: &QueryBuilder, edges: &mut Vec<Edge>) -> Result<()> {
    let mut per_source: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, edge) in edges.iter().enumerate() {
        per_source
            .entry(edge.source.clone())
            .or_default()
            .push(index);
    }
    let mut dropped: HashSet<usize> = HashSet::new();
    for (source, indexes) in per_source {
        if indexes.len() <= MAX_RUST_IMPLEMENTATIONS {
            continue;
        }
        let Some(declaration) = queries.get_node_by_id(&source)? else {
            continue;
        };
        if declaration.language != Language::Rust {
            continue;
        }
        let place = |index: usize| {
            edges[index]
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("registeredAt"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let mut ranked: Vec<(bool, String, usize)> = indexes
            .iter()
            .map(|&index| {
                let at = place(index);
                let other_file = !at.starts_with(&format!("{}:", declaration.file_path));
                (other_file, at, index)
            })
            .collect();
        ranked.sort();
        let total = ranked.len();
        for (rank, (_, _, index)) in ranked.into_iter().enumerate() {
            if rank >= MAX_RUST_IMPLEMENTATIONS {
                dropped.insert(index);
                continue;
            }
            if let Some(metadata) = edges[index].metadata.as_mut() {
                metadata.insert("implementations".to_string(), Value::from(total));
                metadata.insert("capped".to_string(), Value::Bool(true));
            }
        }
    }
    if !dropped.is_empty() {
        let mut index = 0;
        edges.retain(|_| {
            let keep = !dropped.contains(&index);
            index += 1;
            keep
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::db::{DatabaseConnection, QueryBuilder};

    fn node(id: &str, kind: NodeKind, name: &str, qualified_name: &str) -> Node {
        Node::new(
            id,
            kind,
            name,
            qualified_name,
            "src/lib.rs",
            Language::Rust,
            1,
            1,
        )
    }

    #[test]
    fn bridges_rust_trait_method_to_union_implementor() {
        let directory = tempdir().unwrap();
        let connection =
            DatabaseConnection::initialize(directory.path().join("codegraph.db")).unwrap();
        let queries = QueryBuilder::new(connection.get_db().unwrap());
        queries
            .insert_nodes(&[
                node("reg", NodeKind::Union, "Reg", "Reg"),
                node("describe", NodeKind::Trait, "Describe", "Describe"),
                node(
                    "trait-method",
                    NodeKind::Method,
                    "describe",
                    "Describe::describe",
                ),
                node(
                    "union-method",
                    NodeKind::Method,
                    "describe",
                    "Reg::describe",
                ),
            ])
            .unwrap();
        queries
            .insert_edges(&[
                Edge::new("reg", "describe", EdgeKind::Implements),
                Edge::new("describe", "trait-method", EdgeKind::Contains),
                Edge::new("reg", "union-method", EdgeKind::Contains),
            ])
            .unwrap();

        let edges = interface_override_edges(&queries).unwrap();
        let edge = edges
            .iter()
            .find(|edge| edge.source == "trait-method" && edge.target == "union-method")
            .expect("trait dispatch edge");
        assert!(
            edge.metadata.as_ref().unwrap().get("capped").is_none(),
            "one implementation is no cap"
        );
        assert_eq!(edge.kind, EdgeKind::Calls);
        assert_eq!(edge.provenance, Some(crate::types::Provenance::Heuristic));
        assert_eq!(
            edge.metadata.as_ref().unwrap()["synthesizedBy"],
            "interface-impl"
        );
    }

    /// A Rust trait method implemented everywhere keeps at most
    /// `MAX_RUST_IMPLEMENTATIONS` dispatch edges, the ones in its own file
    /// first, each saying how many there are; an enum implementor counts.
    #[test]
    fn caps_a_rust_trait_methods_dispatch_edges_and_says_so() {
        let directory = tempdir().unwrap();
        let connection =
            DatabaseConnection::initialize(directory.path().join("codegraph.db")).unwrap();
        let queries = QueryBuilder::new(connection.get_db().unwrap());
        let total = MAX_RUST_IMPLEMENTATIONS + 6;
        let mut nodes = vec![
            node("tr", NodeKind::Trait, "Poll", "Poll"),
            node("tr-poll", NodeKind::Method, "poll", "Poll::poll"),
        ];
        let mut edges = vec![Edge::new("tr", "tr-poll", EdgeKind::Contains)];
        for i in 0..total {
            let kind = if i == 0 {
                NodeKind::Enum
            } else {
                NodeKind::Struct
            };
            let mut owner = node(&format!("t{i}"), kind, &format!("T{i}"), &format!("T{i}"));
            let mut method = node(
                &format!("m{i}"),
                NodeKind::Method,
                "poll",
                &format!("T{i}::poll"),
            );
            // The last implementor lives beside the trait; the rest elsewhere.
            if i + 1 != total {
                owner.file_path = format!("src/t{i:03}.rs");
                method.file_path = owner.file_path.clone();
            }
            nodes.push(owner);
            nodes.push(method);
            edges.push(Edge::new(format!("t{i}"), "tr", EdgeKind::Implements));
            edges.push(Edge::new(
                format!("t{i}"),
                format!("m{i}"),
                EdgeKind::Contains,
            ));
        }
        queries.insert_nodes(&nodes).unwrap();
        queries.insert_edges(&edges).unwrap();

        let dispatch = interface_override_edges(&queries).unwrap();
        assert_eq!(dispatch.len(), MAX_RUST_IMPLEMENTATIONS);
        for edge in &dispatch {
            let metadata = edge.metadata.as_ref().unwrap();
            assert_eq!(metadata["implementations"], total);
            assert_eq!(metadata["capped"], true);
        }
        let targets: Vec<&str> = dispatch.iter().map(|edge| edge.target.as_str()).collect();
        assert!(
            targets.contains(&format!("m{}", total - 1).as_str()),
            "same file first"
        );
        assert!(targets.contains(&"m0"), "the enum implementor");
    }
}
