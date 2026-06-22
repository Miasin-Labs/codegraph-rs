//! Partial struct selection — field-level granularity for context windows.
//!
//! When a function only accesses 2 of 8 struct fields, the context should
//! only show those 2 fields. This module provides the analysis primitives.
//!
//! # Registering field data from a host
//!
//! Field data rides two node-metadata entries rather than dedicated nodes,
//! so any host that projects its own index into this graph (e.g. a bridge
//! that drops field/property nodes from a richer schema) can light up
//! partial-struct views without changing the node model:
//!
//! - [`STRUCT_FIELDS_KEY`] on a `Struct` node — the field list, encoded by
//!   [`encode_fields_metadata`] / decoded by [`parse_fields_metadata`].
//! - [`ACCESSED_FIELDS_KEY`] on a `Function` node — comma-separated names
//!   of the fields that function reads/writes.
//!
//! Use the typed setters [`set_struct_fields`] / [`set_accessed_fields`]
//! (or the [`crate::session::GraphSession`] wrappers, which also honor the
//! `PartialStruct` capability flag) instead of writing the strings by hand —
//! they own the encoding and validate node kinds.

use crate::graph::CodeGraph;
use crate::nodes::{NodeId, NodeKind};

/// Node-metadata key on a `Struct` node holding the encoded field list.
/// Value format is owned by [`encode_fields_metadata`].
pub const STRUCT_FIELDS_KEY: &str = "fields";

/// Node-metadata key on a `Function` node holding the comma-separated names
/// of struct fields the function accesses.
pub const ACCESSED_FIELDS_KEY: &str = "accessed_fields";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    pub type_str: String,
    pub is_public: bool,
}

/// Errors from the field-registration APIs ([`set_struct_fields`],
/// [`set_accessed_fields`]) and the session-level partial-struct surface.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PartialStructError {
    /// The referenced node is not in the graph.
    #[error("node not found: {0:?}")]
    NodeNotFound(NodeId),
    /// The node exists but has the wrong kind for this operation.
    #[error("expected a {expected:?} node, got {got:?} ({id:?})")]
    WrongKind {
        id: NodeId,
        expected: NodeKind,
        got: NodeKind,
    },
    /// A field name that would corrupt the metadata encoding (empty, or
    /// containing one of the `;` / `:` / `,` separator characters).
    #[error("invalid field name '{0}': must be non-empty and contain none of ';', ':', ','")]
    InvalidFieldName(String),
    /// A field type containing the `;` entry separator.
    #[error("invalid field type '{0}': must not contain ';'")]
    InvalidFieldType(String),
    /// The struct node has no registered or extracted field data.
    #[error("no field data registered for struct {0:?} (metadata key 'fields' absent)")]
    NoFieldData(NodeId),
    /// The `PartialStruct` capability is disabled
    /// (`CODEGRAPH_ANALYSIS_CAP_PARTIAL_STRUCT=0`).
    #[error("partial-struct capability is disabled (CODEGRAPH_ANALYSIS_CAP_PARTIAL_STRUCT)")]
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialView {
    pub struct_name: String,
    pub struct_id: NodeId,
    pub all_fields: Vec<FieldInfo>,
    pub accessed_fields: Vec<String>,
    pub is_partial: bool,
}

impl PartialView {
    pub fn visible_fields(&self) -> Vec<&FieldInfo> {
        self.all_fields
            .iter()
            .filter(|f| self.accessed_fields.contains(&f.name))
            .collect()
    }

    pub fn all_fields_with_markers(&self) -> Vec<(&FieldInfo, bool)> {
        self.all_fields
            .iter()
            .map(|f| (f, self.accessed_fields.contains(&f.name)))
            .collect()
    }
}

/// Get a partial view of a struct as seen from a specific accessing function.
pub fn get_partial_struct(
    graph: &CodeGraph,
    struct_id: &NodeId,
    accessing_fn: &NodeId,
) -> Option<PartialView> {
    let struct_node = graph.get_node(struct_id)?;
    if struct_node.kind != NodeKind::Struct {
        return None;
    }

    let fields_str = struct_node.metadata.get(STRUCT_FIELDS_KEY)?;
    let all_fields = parse_fields_metadata(fields_str);

    let fn_node = graph.get_node(accessing_fn)?;
    let accessed: Vec<String> = fn_node
        .metadata
        .get(ACCESSED_FIELDS_KEY)
        .map(|s| s.split(',').map(|f| f.trim().to_string()).collect())
        .unwrap_or_default();
    let visible_accessed: Vec<String> = all_fields
        .iter()
        .filter(|field| accessed.contains(&field.name))
        .map(|field| field.name.clone())
        .collect();

    let is_partial = !visible_accessed.is_empty() && visible_accessed.len() < all_fields.len();

    Some(PartialView {
        struct_name: struct_node.name.clone(),
        struct_id: struct_id.clone(),
        all_fields,
        accessed_fields: visible_accessed,
        is_partial,
    })
}

/// Register the field list of a `Struct` node.
///
/// This is the public surface for hosts whose own index carries field /
/// property nodes that this graph's projection drops: encode them once per
/// struct and partial-struct rendering ([`get_partial_struct`]) works over
/// the bridged data. Overwrites any previously registered list; the graph
/// revision is bumped so `since N` queries observe the change.
///
/// Validation: every field name must be non-empty and free of the `;`/`:`/`,`
/// separator characters; types must not contain `;` (qualified types with
/// `::` are fine — the decoder handles them). On error the graph is
/// untouched.
pub fn set_struct_fields(
    graph: &mut CodeGraph,
    struct_id: &NodeId,
    fields: &[FieldInfo],
) -> Result<(), PartialStructError> {
    let node = graph
        .get_node(struct_id)
        .ok_or_else(|| PartialStructError::NodeNotFound(struct_id.clone()))?;
    if node.kind != NodeKind::Struct {
        return Err(PartialStructError::WrongKind {
            id: struct_id.clone(),
            expected: NodeKind::Struct,
            got: node.kind,
        });
    }
    let encoded = encode_fields_metadata(fields)?;
    graph.update_node_metadata(struct_id, |meta| {
        meta.insert(STRUCT_FIELDS_KEY.to_string(), encoded);
    });
    Ok(())
}

/// Register the set of struct-field names a `Function` node accesses.
///
/// Pairs with [`set_struct_fields`]: once both sides are registered,
/// [`get_partial_struct`] can answer "which fields of S does f touch?".
/// An empty `accessed` slice **removes** the annotation (distinct from
/// "accesses zero fields" — absent data must not masquerade as an empty
/// access set). On error the graph is untouched.
pub fn set_accessed_fields(
    graph: &mut CodeGraph,
    fn_id: &NodeId,
    accessed: &[String],
) -> Result<(), PartialStructError> {
    let node = graph
        .get_node(fn_id)
        .ok_or_else(|| PartialStructError::NodeNotFound(fn_id.clone()))?;
    if node.kind != NodeKind::Function {
        return Err(PartialStructError::WrongKind {
            id: fn_id.clone(),
            expected: NodeKind::Function,
            got: node.kind,
        });
    }
    for name in accessed {
        validate_field_name(name)?;
    }
    let joined = accessed.join(", ");
    graph.update_node_metadata(fn_id, |meta| {
        if joined.is_empty() {
            meta.remove(ACCESSED_FIELDS_KEY);
        } else {
            meta.insert(ACCESSED_FIELDS_KEY.to_string(), joined);
        }
    });
    Ok(())
}

/// Encode a field list into the [`STRUCT_FIELDS_KEY`] metadata value.
///
/// Format: `name:type:pub;name:type:priv;...` — entries separated by `;`,
/// the first `:` ends the name, a trailing `:pub` / `:priv` carries
/// visibility, and everything between is the type (so qualified types like
/// `std::path::PathBuf` survive). Round-trips through
/// [`parse_fields_metadata`].
pub fn encode_fields_metadata(fields: &[FieldInfo]) -> Result<String, PartialStructError> {
    let mut entries = Vec::with_capacity(fields.len());
    for f in fields {
        validate_field_name(&f.name)?;
        if f.type_str.contains(';') {
            return Err(PartialStructError::InvalidFieldType(f.type_str.clone()));
        }
        let vis = if f.is_public { "pub" } else { "priv" };
        entries.push(format!("{}:{}:{vis}", f.name, f.type_str));
    }
    Ok(entries.join(";"))
}

fn validate_field_name(name: &str) -> Result<(), PartialStructError> {
    if name.is_empty() || name.contains([';', ':', ',']) {
        return Err(PartialStructError::InvalidFieldName(name.to_string()));
    }
    Ok(())
}

/// Like [`get_partial_struct`] but with precise error reporting instead of
/// a flat `None` — distinguishes "node missing", "not a struct", and "no
/// field data registered" so callers (CLI / context renderers) can print an
/// honest capability note rather than silently showing nothing.
pub fn try_get_partial_struct(
    graph: &CodeGraph,
    struct_id: &NodeId,
    accessing_fn: &NodeId,
) -> Result<PartialView, PartialStructError> {
    let struct_node = graph
        .get_node(struct_id)
        .ok_or_else(|| PartialStructError::NodeNotFound(struct_id.clone()))?;
    if struct_node.kind != NodeKind::Struct {
        return Err(PartialStructError::WrongKind {
            id: struct_id.clone(),
            expected: NodeKind::Struct,
            got: struct_node.kind,
        });
    }
    if !struct_node.metadata.contains_key(STRUCT_FIELDS_KEY) {
        return Err(PartialStructError::NoFieldData(struct_id.clone()));
    }
    if graph.get_node(accessing_fn).is_none() {
        return Err(PartialStructError::NodeNotFound(accessing_fn.clone()));
    }
    get_partial_struct(graph, struct_id, accessing_fn)
        .ok_or_else(|| PartialStructError::NoFieldData(struct_id.clone()))
}

/// Parse the fields metadata string ([`STRUCT_FIELDS_KEY`] value).
///
/// Format: `name:type:pub;name:type:priv;...`. The first `:` delimits the
/// name; a trailing `:pub` / `:priv` segment carries visibility; everything
/// in between is the type — qualified type names containing `::` parse
/// intact. Entries without a `:` are skipped. Public so hosts can render
/// field lists they registered via [`set_struct_fields`] without going
/// through a [`PartialView`].
pub fn parse_fields_metadata(raw: &str) -> Vec<FieldInfo> {
    raw.split(';')
        .filter(|s| !s.is_empty())
        .filter_map(|entry| {
            let (name, rest) = entry.split_once(':')?;
            let (type_str, is_public) = match rest.rsplit_once(':') {
                Some((ty, "pub")) => (ty, true),
                Some((ty, "priv")) => (ty, false),
                // No recognised visibility suffix: the whole rest is the
                // type. (Legacy two-part entries like `port:u16` land here.)
                _ => (rest, false),
            };
            Some(FieldInfo {
                name: name.trim().to_string(),
                type_str: type_str.trim().to_string(),
                is_public,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::nodes::{NodeData, Span, Visibility};

    fn sample_span() -> Span {
        Span {
            file: PathBuf::from("src/lib.rs"),
            start_line: 1,
            start_col: 0,
            end_line: 10,
            end_col: 1,
            byte_range: 0..100,
        }
    }

    const BIG_CONFIG_FIELDS: &str = "name:String:pub;port:u16:pub;host:String:pub;debug:bool:pub;\
         max_retries:u32:pub;timeout_ms:u64:pub;log_level:String:pub;workers:usize:pub";

    fn make_struct_node(fields_meta: &str) -> NodeData {
        let id = NodeId::new("src/lib.rs", "crate::BigConfig", NodeKind::Struct);
        NodeData {
            id,
            kind: NodeKind::Struct,
            name: "BigConfig".to_string(),
            qualified_name: "crate::BigConfig".to_string(),
            file_path: PathBuf::from("src/lib.rs"),
            span: sample_span(),
            visibility: Visibility::Public,
            metadata: HashMap::from([("fields".to_string(), fields_meta.to_string())]),
            birth_revision: 0,
            last_modified_revision: 0,
            complexity: None,
            cfg: None,
            dataflow: None,
        }
    }

    fn make_fn_node(name: &str, accessed: Option<&str>) -> NodeData {
        let id = NodeId::new("src/lib.rs", &format!("crate::{name}"), NodeKind::Function);
        let mut metadata = HashMap::new();
        if let Some(fields) = accessed {
            metadata.insert("accessed_fields".to_string(), fields.to_string());
        }
        NodeData {
            id,
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: format!("crate::{name}"),
            file_path: PathBuf::from("src/lib.rs"),
            span: sample_span(),
            visibility: Visibility::Public,
            metadata,
            birth_revision: 0,
            last_modified_revision: 0,
            complexity: None,
            cfg: None,
            dataflow: None,
        }
    }

    #[test]
    fn test_partial_struct_two_fields() {
        let mut graph = CodeGraph::new();

        let struct_node = make_struct_node(BIG_CONFIG_FIELDS);
        let struct_id = graph.add_node(struct_node);

        let fn_node = make_fn_node("uses_two_fields", Some("name, port"));
        let fn_id = graph.add_node(fn_node);

        let view = get_partial_struct(&graph, &struct_id, &fn_id).unwrap();

        assert!(view.is_partial);
        assert_eq!(view.struct_name, "BigConfig");
        assert_eq!(view.all_fields.len(), 8);
        assert_eq!(view.accessed_fields.len(), 2);

        let visible = view.visible_fields();
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().any(|f| f.name == "name"));
        assert!(visible.iter().any(|f| f.name == "port"));
    }

    #[test]
    fn test_partial_struct_all_fields() {
        let mut graph = CodeGraph::new();

        let struct_node = make_struct_node(BIG_CONFIG_FIELDS);
        let struct_id = graph.add_node(struct_node);

        let all = "name, port, host, debug, max_retries, timeout_ms, log_level, workers";
        let fn_node = make_fn_node("uses_all_fields", Some(all));
        let fn_id = graph.add_node(fn_node);

        let view = get_partial_struct(&graph, &struct_id, &fn_id).unwrap();

        assert!(!view.is_partial);
        assert_eq!(view.visible_fields().len(), 8);
    }

    #[test]
    fn test_partial_struct_ignores_unrelated_accessed_fields() {
        let mut graph = CodeGraph::new();

        let struct_node = make_struct_node(BIG_CONFIG_FIELDS);
        let struct_id = graph.add_node(struct_node);

        let unrelated = "name, other_a, other_b, other_c, other_d, other_e, other_f, other_g";
        let fn_node = make_fn_node("uses_one_field_and_other_struct", Some(unrelated));
        let fn_id = graph.add_node(fn_node);

        let view = get_partial_struct(&graph, &struct_id, &fn_id).unwrap();

        assert!(view.is_partial);
        assert_eq!(view.visible_fields().len(), 1);
        assert_eq!(view.accessed_fields, vec!["name"]);
    }

    #[test]
    fn test_partial_struct_verbose() {
        let mut graph = CodeGraph::new();

        let struct_node = make_struct_node(BIG_CONFIG_FIELDS);
        let struct_id = graph.add_node(struct_node);

        let fn_node = make_fn_node("uses_two_fields", Some("name, port"));
        let fn_id = graph.add_node(fn_node);

        let view = get_partial_struct(&graph, &struct_id, &fn_id).unwrap();
        let markers = view.all_fields_with_markers();

        assert_eq!(markers.len(), 8);

        let accessed_names: Vec<&str> = markers
            .iter()
            .filter(|(_, accessed)| *accessed)
            .map(|(f, _)| f.name.as_str())
            .collect();
        assert_eq!(accessed_names.len(), 2);
        assert!(accessed_names.contains(&"name"));
        assert!(accessed_names.contains(&"port"));

        let not_accessed: Vec<&str> = markers
            .iter()
            .filter(|(_, accessed)| !*accessed)
            .map(|(f, _)| f.name.as_str())
            .collect();
        assert_eq!(not_accessed.len(), 6);
    }

    #[test]
    fn test_partial_struct_not_a_struct() {
        let mut graph = CodeGraph::new();

        let fn_as_struct = make_fn_node("not_a_struct", Some("x, y"));
        let fn_id = graph.add_node(fn_as_struct);

        let accessor = make_fn_node("accessor", Some("x"));
        let accessor_id = graph.add_node(accessor);

        let result = get_partial_struct(&graph, &fn_id, &accessor_id);
        assert!(result.is_none());
    }

    // ── Host registration surface (set_struct_fields / set_accessed_fields) ──

    fn fields_fixture() -> Vec<FieldInfo> {
        vec![
            FieldInfo {
                name: "name".into(),
                type_str: "String".into(),
                is_public: true,
            },
            FieldInfo {
                name: "path".into(),
                type_str: "std::path::PathBuf".into(),
                is_public: false,
            },
            FieldInfo {
                name: "map".into(),
                type_str: "HashMap<String, Vec<u8>>".into(),
                is_public: true,
            },
        ]
    }

    // Normal: encode → parse round-trips, including qualified type names
    // (`std::path::PathBuf`) and generic types containing commas.
    #[test]
    fn encode_parse_fields_roundtrip_normal() {
        let fields = fields_fixture();
        let encoded = encode_fields_metadata(&fields).unwrap();
        assert_eq!(parse_fields_metadata(&encoded), fields);
    }

    // Robust: the legacy two-part / three-part encodings still parse — and
    // a qualified type without a visibility suffix is no longer truncated
    // at its first ':'.
    #[test]
    fn parse_fields_legacy_and_qualified_types_robust() {
        let parsed = parse_fields_metadata("name:String:pub;port:u16;p:std::path::PathBuf");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].name, "name");
        assert_eq!(parsed[0].type_str, "String");
        assert!(parsed[0].is_public);
        assert_eq!(parsed[1].type_str, "u16");
        assert!(!parsed[1].is_public);
        assert_eq!(parsed[2].type_str, "std::path::PathBuf");
        assert!(!parsed[2].is_public);
        // Entries without any ':' are skipped, empty input parses empty.
        assert!(parse_fields_metadata("").is_empty());
        assert_eq!(parse_fields_metadata("garbage;x:u8").len(), 1);
    }

    // Normal: a host registers fields + accessed fields onto bridge-like
    // bare nodes (no pre-existing metadata) and the partial view lights up.
    #[test]
    fn register_then_partial_view_end_to_end_normal() {
        let mut graph = CodeGraph::new();
        // Bridge-like: struct node WITHOUT fields metadata.
        let mut bare_struct = make_struct_node("");
        bare_struct.metadata.clear();
        let struct_id = graph.add_node(bare_struct);
        let fn_id = graph.add_node(make_fn_node("uses_two", None));

        // Before registration: no field data → no view.
        assert!(get_partial_struct(&graph, &struct_id, &fn_id).is_none());

        set_struct_fields(&mut graph, &struct_id, &fields_fixture()).unwrap();
        set_accessed_fields(&mut graph, &fn_id, &["name".to_string(), "map".to_string()]).unwrap();

        let view = get_partial_struct(&graph, &struct_id, &fn_id).unwrap();
        assert!(view.is_partial);
        assert_eq!(view.all_fields.len(), 3);
        let visible = view.visible_fields();
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().any(|f| f.name == "map"));

        // Re-registration overwrites (not appends).
        set_struct_fields(
            &mut graph,
            &struct_id,
            &[FieldInfo {
                name: "only".into(),
                type_str: "u8".into(),
                is_public: false,
            }],
        )
        .unwrap();
        let view = get_partial_struct(&graph, &struct_id, &fn_id).unwrap();
        assert_eq!(view.all_fields.len(), 1);
    }

    // Robust: kind/existence/encoding validation — errors leave the graph
    // untouched, and clearing accessed fields removes the annotation.
    #[test]
    fn register_validation_and_clearing_robust() {
        let mut graph = CodeGraph::new();
        let mut bare_struct = make_struct_node("");
        bare_struct.metadata.clear();
        let struct_id = graph.add_node(bare_struct);
        let fn_id = graph.add_node(make_fn_node("f", Some("name")));

        // Wrong kinds, both directions.
        assert!(matches!(
            set_struct_fields(&mut graph, &fn_id, &fields_fixture()),
            Err(PartialStructError::WrongKind {
                expected: NodeKind::Struct,
                ..
            })
        ));
        assert!(matches!(
            set_accessed_fields(&mut graph, &struct_id, &["x".to_string()]),
            Err(PartialStructError::WrongKind {
                expected: NodeKind::Function,
                ..
            })
        ));

        // Missing node.
        let ghost = NodeId::new("src/lib.rs", "crate::Ghost", NodeKind::Struct);
        assert!(matches!(
            set_struct_fields(&mut graph, &ghost, &fields_fixture()),
            Err(PartialStructError::NodeNotFound(_))
        ));

        // Separator characters in names / entry separator in types.
        let bad_name = [FieldInfo {
            name: "a;b".into(),
            type_str: "u8".into(),
            is_public: false,
        }];
        assert!(matches!(
            set_struct_fields(&mut graph, &struct_id, &bad_name),
            Err(PartialStructError::InvalidFieldName(_))
        ));
        let bad_type = [FieldInfo {
            name: "a".into(),
            type_str: "u8;drop".into(),
            is_public: false,
        }];
        assert!(matches!(
            set_struct_fields(&mut graph, &struct_id, &bad_type),
            Err(PartialStructError::InvalidFieldType(_))
        ));
        assert!(
            !graph
                .get_node(&struct_id)
                .unwrap()
                .metadata
                .contains_key(STRUCT_FIELDS_KEY),
            "failed registration must not write partial data"
        );
        assert!(matches!(
            set_accessed_fields(&mut graph, &fn_id, &["a,b".to_string()]),
            Err(PartialStructError::InvalidFieldName(_))
        ));

        // Clearing: empty slice removes the annotation entirely (absent
        // data, not "accesses zero fields").
        set_accessed_fields(&mut graph, &fn_id, &[]).unwrap();
        assert!(
            !graph
                .get_node(&fn_id)
                .unwrap()
                .metadata
                .contains_key(ACCESSED_FIELDS_KEY)
        );
    }
}
