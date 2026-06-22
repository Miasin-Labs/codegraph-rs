//! Nominal override and interface dispatch synthesis.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::methods_of;
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::types::{Edge, EdgeKind, Language, Node, NodeKind};

const MAX_CALLBACKS_PER_CHANNEL: usize = 40;

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
    matches!(
        lang,
        Language::Java
            | Language::Kotlin
            | Language::Csharp
            | Language::Typescript
            | Language::Javascript
            | Language::Swift
            | Language::Scala
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
    // types that conform to protocols. Iterate both.
    let concrete_kinds = [NodeKind::Class, NodeKind::Struct];
    for kind in concrete_kinds {
        for cls in queries.get_nodes_by_kind(kind)? {
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
    Ok(edges)
}
