//! MyBatis Java/Kotlin mapper to XML statement synthesis.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::types::{Edge, Language, Node, NodeKind};

/// MyBatis: link a Java mapper interface method to the XML statement that holds
/// its SQL. The XML extractor (`src/extraction/mybatis-extractor.ts`) qualifies
/// each `<select|insert|update|delete|sql id="X">` as `<namespace>::<id>` where
/// `<namespace>` is the Java FQN of the mapper interface. A Java method's
/// qualifiedName ends with `<ClassName>::<methodName>`, so we suffix-match the
/// last two segments of the XML qualified name to find a unique Java method by
/// `<ClassName>::<methodName>` (`ClassName` = last dotted segment of the XML
/// namespace). Cross-mapper `<include refid="other.X">` references go through
/// the normal qualified-name resolver — only the Java↔XML bridge is synthetic.
///
/// Precision over recall: ambiguous mappers (multiple Java classes with the
/// same simple name) are dropped. We need-not bridge by package because Java
/// mapper interfaces are typically uniquely named within a project.
pub(super) fn mybatis_java_xml_edges(queries: &QueryBuilder) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Index Java methods by `<ClassName>::<methodName>` for O(1) lookup.
    let mut java_index: HashMap<String, Vec<Node>> = HashMap::new();
    queries.iterate_nodes_by_kind(NodeKind::Method, |m| {
        if m.language != Language::Java && m.language != Language::Kotlin {
            return true;
        }
        let parts: Vec<&str> = m.qualified_name.split("::").collect();
        if parts.len() < 2 {
            return true;
        }
        let last = parts[parts.len() - 1];
        let cls = parts[parts.len() - 2];
        if last.is_empty() || cls.is_empty() {
            return true;
        }
        let key = format!("{}::{}", cls, last);
        java_index.entry(key).or_default().push(m);
        true
    })?;

    queries.iterate_nodes_by_kind(NodeKind::Method, |xml| {
        if xml.language != Language::Xml {
            return true;
        }
        // Qualified name: `<namespace>::<id>`. Extract the simple class name.
        let Some(colon_idx) = xml.qualified_name.rfind("::") else {
            return true;
        };
        let namespace = &xml.qualified_name[..colon_idx];
        let id = &xml.qualified_name[colon_idx + 2..];
        if namespace.is_empty() || id.is_empty() {
            return true;
        }
        let class_name = match namespace.rfind('.') {
            Some(dot_idx) => &namespace[dot_idx + 1..],
            None => namespace,
        };
        let Some(candidates) = java_index.get(&format!("{}::{}", class_name, id)) else {
            return true;
        };
        if candidates.is_empty() {
            return true;
        }
        // Drop ambiguous matches (multiple same-name classes); the user can
        // disambiguate by adding the package-suffix match in a future enhancement.
        if candidates.len() > 1 {
            return true;
        }
        let java = &candidates[0];
        let key = format!("{}>{}", java.id, xml.id);
        if !seen.insert(key) {
            return true;
        }
        edges.push(synthesized_edge(
            &java.id,
            &xml.id,
            Some(java.start_line),
            edge_meta(vec![
                ("synthesizedBy", Value::from("mybatis-java-xml")),
                ("via", Value::from(format!("{}.{}", class_name, id))),
                (
                    "registeredAt",
                    Value::from(format!("{}:{}", xml.file_path, xml.start_line)),
                ),
            ]),
        ));
        true
    })?;
    Ok(edges)
}
