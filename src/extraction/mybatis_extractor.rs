//! MyBatisExtractor — parses MyBatis mapper XML files.
//!
//! MyBatis splits a DAO interface across two files: a Java interface (parsed by
//! tree-sitter) declares the method, and an XML mapper file holds the SQL keyed
//! by `<namespace>` (the fully-qualified Java type name) and `id` (the method
//! name). Without the XML side in the graph, `trace(Controller, ...DAO.method)`
//! dead-ends at the interface method — the SQL it actually runs is invisible,
//! and "what does this query touch" / "where is this column written" can't be
//! answered.
//!
//! This extractor emits one method-shaped node per `<select|insert|update|
//! delete>` and per `<sql>` fragment, qualified as `<namespace>::<id>` so the
//! MyBatis framework synthesizer (`src/resolution/frameworks/mybatis.ts`) can
//! link the matching Java method → XML statement by suffix-matching qualified
//! names. `<include refid="...">` inside a statement yields an unresolved
//! reference to the SQL fragment, also keyed by `<namespace>::<refid>`.
//!
//! Non-mapper XML (Maven `pom.xml`, Spring beans XML, `web.xml`, log4j config,
//! etc.) is detected by the absence of a `<mapper namespace="...">` root and
//! returns just a file node — we still need the file row so the watcher can
//! track it, but we emit no symbols.
//!
//! Ported from `src/extraction/mybatis-extractor.ts`.

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::types::{
    Edge,
    EdgeKind,
    ExtractionError,
    ExtractionResult,
    Language,
    Node,
    NodeKind,
    UnresolvedReference,
};

/// TS: `/<mapper\b([^>]*)>/`
static MAPPER_OPEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<mapper\b([^>]*)>").expect("valid regex"));

/// TS: `/\bnamespace\s*=\s*"([^"]+)"/`
static NAMESPACE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bnamespace\s*=\s*"([^"]+)""#).expect("valid regex"));

/// Opening tag of a statement-shaped element. The TS regex
/// `/<(select|insert|update|delete|sql)\b([^>]*)>([\s\S]*?)<\/\1>/g` uses a
/// backreference (`\1`) which the `regex` crate does not support; we match the
/// opening tag here and pair it with the first matching `</elem>` manually,
/// which is exactly what the non-greedy backreference match did.
static STMT_OPEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<(select|insert|update|delete|sql)\b([^>]*)>").expect("valid regex")
});

/// TS: `/\bid\s*=\s*"([^"]+)"/`
static ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bid\s*=\s*"([^"]+)""#).expect("valid regex"));

/// TS: `/<include\b[^>]*\brefid\s*=\s*"([^"]+)"/g`
static INCLUDE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<include\b[^>]*\brefid\s*=\s*"([^"]+)""#).expect("valid regex"));

/// TS: `/\bresultType\s*=\s*"([^"]+)"/`
static RESULT_TYPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bresultType\s*=\s*"([^"]+)""#).expect("valid regex"));

/// TS: `/\bparameterType\s*=\s*"([^"]+)"/`
static PARAMETER_TYPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bparameterType\s*=\s*"([^"]+)""#).expect("valid regex"));

/// TS: `/<[^>]+>/g`
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").expect("valid regex"));

/// TS: `/\s+/g`
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid regex"));

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before epoch")
        .as_millis() as i64
}

struct MapperRoot {
    namespace: String,
    body_start: usize,
    body_end: usize,
}

/// MyBatisExtractor — parses MyBatis mapper XML files.
pub struct MyBatisExtractor<'a> {
    file_path: String,
    source: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
    line_starts: Vec<usize>,
}

impl<'a> MyBatisExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str) -> Self {
        let mut extractor = MyBatisExtractor {
            file_path: file_path.into(),
            source,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
            line_starts: Vec::new(),
        };
        extractor.compute_line_starts();
        extractor
    }

    pub fn extract(mut self) -> ExtractionResult {
        let start_time = std::time::Instant::now();

        let file_node_id = self.create_file_node();

        // The TS body wraps this in try/catch emitting a `MyBatis extraction
        // error:` parse_error; the Rust parsing below is infallible, so the
        // catch arm has no equivalent.
        if let Some(mapper) = self.find_mapper_root() {
            self.extract_mapper(
                &file_node_id,
                &mapper.namespace,
                mapper.body_start,
                mapper.body_end,
            );
        }

        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms: start_time.elapsed().as_millis() as f64,
        }
    }

    fn create_file_node(&mut self) -> String {
        let lines: Vec<&str> = self.source.split('\n').collect();
        let id = generate_node_id(&self.file_path, NodeKind::File, &self.file_path, 1);

        let name = self
            .file_path
            .split('/')
            .next_back()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.file_path)
            .to_string();

        let mut node = Node::new(
            id.clone(),
            NodeKind::File,
            name,
            self.file_path.clone(),
            self.file_path.clone(),
            Language::Xml,
            1,
            (lines.len().max(1)) as u32,
        );
        node.start_column = 0;
        node.end_column = lines.last().map(|l| l.len()).unwrap_or(0) as u32;
        node.updated_at = now_ms();

        self.nodes.push(node);
        id
    }

    /// Find the `<mapper namespace="X">` opening tag. Returns the namespace and
    /// the byte offsets of the body (between the opening and closing tag) so
    /// statement extraction can be scoped to mapper contents.
    fn find_mapper_root(&self) -> Option<MapperRoot> {
        let caps = MAPPER_OPEN_RE.captures(self.source)?;
        let open = caps.get(0).expect("match");
        let attrs = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let ns_caps = NAMESPACE_RE.captures(attrs)?;
        let namespace = ns_caps.get(1).expect("group 1").as_str().to_string();
        let body_start = open.end();
        let body_end = self.source[body_start..]
            .find("</mapper>")
            .map(|i| i + body_start)
            .unwrap_or(self.source.len());
        Some(MapperRoot {
            namespace,
            body_start,
            body_end,
        })
    }

    fn extract_mapper(
        &mut self,
        file_node_id: &str,
        namespace: &str,
        body_start: usize,
        body_end: usize,
    ) {
        let source = self.source;
        let body = &source[body_start..body_end];
        // Match each top-level statement-shaped element. The body may have nested
        // tags (`<if>`, `<foreach>`, `<include>`), so we scan with a regex that
        // pairs an opening tag to its matching close — the simple form below works
        // because MyBatis statement elements are not themselves nested.
        let mut search_from = 0usize;
        while search_from <= body.len() {
            let Some(caps) = STMT_OPEN_RE.captures_at(body, search_from) else {
                break;
            };
            let open = caps.get(0).expect("match");
            let elem_type = caps.get(1).expect("group 1").as_str();
            let attrs = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let close_tag = format!("</{}>", elem_type);

            let Some(rel_close) = body[open.end()..].find(&close_tag) else {
                // No closing tag: the JS engine would fail this start position
                // and resume scanning at the next one.
                search_from = open.start() + 1;
                continue;
            };
            let elem_body = &body[open.end()..open.end() + rel_close];
            let full_len = open.end() + rel_close + close_tag.len() - open.start();
            let stmt_start = open.start();
            // JS exec resumes after the end of the full match.
            search_from = stmt_start + full_len;

            let Some(id) = ID_RE
                .captures(attrs)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str())
            else {
                continue;
            };

            let absolute_index = body_start + stmt_start;
            let start_line = self.get_line_number(absolute_index);
            let end_line = self.get_line_number(absolute_index + full_len);
            let qualified = format!("{}::{}", namespace, id);
            let is_sql_fragment = elem_type == "sql";
            let node_id =
                generate_node_id(&self.file_path, NodeKind::Method, &qualified, start_line);

            let mut node = Node::new(
                node_id.clone(),
                NodeKind::Method,
                id,
                qualified,
                self.file_path.clone(),
                Language::Xml,
                start_line,
                end_line,
            );
            node.signature = Some(build_signature(elem_type, attrs, is_sql_fragment));
            node.start_column = 0;
            node.end_column = 0;
            node.docstring = Some(preview_sql(elem_body));
            node.updated_at = now_ms();
            self.nodes.push(node);
            self.edges
                .push(Edge::new(file_node_id, node_id.clone(), EdgeKind::Contains));

            // <include refid="X"/> → reference to the SQL fragment in this mapper
            // (or in another mapper, when the refid is qualified — `ns.X`).
            for inc in INCLUDE_RE.captures_iter(elem_body) {
                let refid = inc.get(1).expect("group 1").as_str();
                let ref_qualified = if refid.contains('.') {
                    refid.replace('.', "::")
                } else {
                    format!("{}::{}", namespace, refid)
                };
                let opening_tag_len = full_len - elem_body.len() - close_tag.len();
                let include_offset =
                    absolute_index + opening_tag_len + inc.get(0).expect("match").start();
                let line = self.get_line_number(include_offset);
                self.unresolved_references.push(UnresolvedReference {
                    from_node_id: node_id.clone(),
                    reference_name: ref_qualified,
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: None,
                    language: None,
                    candidates: None,
                });
            }
        }
    }

    fn compute_line_starts(&mut self) {
        self.line_starts = vec![0];
        for (i, b) in self.source.bytes().enumerate() {
            if b == 10 {
                self.line_starts.push(i + 1);
            }
        }
    }

    fn get_line_number(&self, offset: usize) -> u32 {
        // Binary search
        let mut lo = 0usize;
        let mut hi = self.line_starts.len() - 1;
        while lo < hi {
            let mid = (lo + hi + 1) >> 1;
            if self.line_starts[mid] <= offset {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        (lo + 1) as u32
    }
}

fn build_signature(elem_type: &str, attrs: &str, is_sql_fragment: bool) -> String {
    if is_sql_fragment {
        return "<sql>".to_string();
    }
    let verb = elem_type.to_uppercase();
    let result = RESULT_TYPE_RE
        .captures(attrs)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str());
    let param = PARAMETER_TYPE_RE
        .captures(attrs)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str());
    let mut parts = vec![verb];
    if let Some(param) = param {
        parts.push(format!("param={}", param));
    }
    if let Some(result) = result {
        parts.push(format!("result={}", result));
    }
    parts.join(" ")
}

fn preview_sql(body: &str) -> String {
    let no_tags = TAG_RE.replace_all(body, " ");
    let collapsed = WS_RE.replace_all(&no_tags, " ");
    collapsed.trim().chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAPPER_XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE mapper PUBLIC \"-//mybatis.org//DTD Mapper 3.0//EN\" \"http://mybatis.org/dtd/mybatis-3-mapper.dtd\">\n<mapper namespace=\"com.example.dao.UserDAOMapper\">\n  <sql id=\"userCols\">id, name, email</sql>\n  <select id=\"getById\" parameterType=\"int\" resultType=\"User\">\n    SELECT <include refid=\"userCols\"/> FROM users WHERE id = #{id}\n  </select>\n  <update id=\"updateUser\" parameterType=\"User\">\n    UPDATE users SET name=#{name}, email=#{email} WHERE id=#{id}\n  </update>\n</mapper>\n";

    fn extract(path: &str, source: &str) -> ExtractionResult {
        MyBatisExtractor::new(path, source).extract()
    }

    #[test]
    fn extracts_statements_and_sql_fragments() {
        let result = extract("src/main/resources/mappers/UserDAOMapper.xml", MAPPER_XML);

        let methods: Vec<&Node> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Method)
            .collect();
        assert_eq!(methods.len(), 3);

        let sql_frag = methods.iter().find(|m| m.name == "userCols").unwrap();
        let get_by_id = methods.iter().find(|m| m.name == "getById").unwrap();
        let update_user = methods.iter().find(|m| m.name == "updateUser").unwrap();

        // XML statement qualified name must be `<namespace>::<id>` — the
        // load-bearing contract between extractor + synthesis.
        assert_eq!(
            get_by_id.qualified_name,
            "com.example.dao.UserDAOMapper::getById"
        );
        assert_eq!(
            sql_frag.qualified_name,
            "com.example.dao.UserDAOMapper::userCols"
        );
        assert_eq!(
            update_user.qualified_name,
            "com.example.dao.UserDAOMapper::updateUser"
        );

        assert_eq!(
            get_by_id.signature.as_deref(),
            Some("SELECT param=int result=User")
        );
        assert_eq!(update_user.signature.as_deref(), Some("UPDATE param=User"));
        assert_eq!(sql_frag.signature.as_deref(), Some("<sql>"));

        // All statements are XML-language nodes contained by the file node.
        let file_node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::File)
            .unwrap();
        for m in &methods {
            assert_eq!(m.language, Language::Xml);
            assert!(result.edges.iter().any(|e| e.source == file_node.id
                && e.target == m.id
                && e.kind == EdgeKind::Contains));
        }

        // Statement positions: <sql> is on line 4, <select> spans 5-7.
        assert_eq!(sql_frag.start_line, 4);
        assert_eq!(get_by_id.start_line, 5);
        assert_eq!(get_by_id.end_line, 7);

        // Docstring previews strip tags and collapse whitespace.
        assert_eq!(
            get_by_id.docstring.as_deref(),
            Some("SELECT FROM users WHERE id = #{id}")
        );
    }

    #[test]
    fn include_refid_yields_unresolved_reference_to_fragment() {
        let result = extract("UserDAOMapper.xml", MAPPER_XML);

        let get_by_id = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Method && n.name == "getById")
            .unwrap();

        let inc = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_name == "com.example.dao.UserDAOMapper::userCols")
            .expect("include reference");
        assert_eq!(inc.from_node_id, get_by_id.id);
        assert_eq!(inc.reference_kind, EdgeKind::References);
        assert_eq!(inc.line, 6);
    }

    #[test]
    fn qualified_refid_maps_dots_to_double_colons() {
        let xml = "<mapper namespace=\"com.example.A\">\n  <select id=\"q\">\n    <include refid=\"com.example.B.frag\"/>\n  </select>\n</mapper>\n";
        let result = extract("A.xml", xml);

        let r = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_name == "com::example::B::frag")
            .expect("qualified include reference");
        assert_eq!(r.reference_kind, EdgeKind::References);
    }

    #[test]
    fn non_mapper_xml_emits_only_a_file_node() {
        for (path, xml) in [
            (
                "pom.xml",
                "<project><groupId>x</groupId><artifactId>y</artifactId></project>\n",
            ),
            (
                "log4j.xml",
                "<?xml version=\"1.0\"?><Configuration><Loggers><Root level=\"info\"/></Loggers></Configuration>\n",
            ),
        ] {
            let result = extract(path, xml);
            assert_eq!(
                result.nodes.len(),
                1,
                "{path} should yield only a file node"
            );
            assert_eq!(result.nodes[0].kind, NodeKind::File);
            assert_eq!(result.nodes[0].language, Language::Xml);
            assert!(result.edges.is_empty());
            assert!(result.unresolved_references.is_empty());
        }
    }

    #[test]
    fn mapper_without_namespace_emits_only_a_file_node() {
        let result = extract(
            "X.xml",
            "<mapper>\n  <select id=\"q\">SELECT 1</select>\n</mapper>\n",
        );
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].kind, NodeKind::File);
    }

    #[test]
    fn statement_without_id_is_skipped() {
        let xml = "<mapper namespace=\"ns\">\n  <select>SELECT 1</select>\n  <select id=\"ok\">SELECT 2</select>\n</mapper>\n";
        let result = extract("X.xml", xml);
        let methods: Vec<&Node> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Method)
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "ok");
        assert_eq!(methods[0].qualified_name, "ns::ok");
    }
}
