//! LiquidExtractor - Extracts relationships from Liquid template files
//!
//! Liquid is a templating language (used by Shopify, Jekyll, etc.) that doesn't
//! have traditional functions or classes. Instead, we extract:
//! - Section references (`{% section 'name' %}`)
//! - Snippet references (`{% render 'name' %}` and `{% include 'name' %}`)
//! - Schema blocks (`{% schema %}...{% endschema %}`)
//!
//! Ported from `src/extraction/liquid-extractor.ts`.

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

/// Match `{% render 'name' %}` or `{% include 'name' %}` with optional parameters
/// (TS: `/\{%[-]?\s*(render|include)\s+['"]([^'"]+)['"]/g`).
static RENDER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\{%-?\s*(render|include)\s+['"]([^'"]+)['"]"#).expect("valid regex")
});

/// Match `{% section 'name' %}` (TS: `/\{%[-]?\s*section\s+['"]([^'"]+)['"]/g`).
static SECTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\{%-?\s*section\s+['"]([^'"]+)['"]"#).expect("valid regex"));

/// Match `{% schema %}...{% endschema %}`
/// (TS: `/\{%[-]?\s*schema\s*[-]?%\}([\s\S]*?)\{%[-]?\s*endschema\s*[-]?%\}/g`).
static SCHEMA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{%-?\s*schema\s*-?%\}([\s\S]*?)\{%-?\s*endschema\s*-?%\}").expect("valid regex")
});

/// Match `{% assign variable_name = ... %}` (TS: `/\{%[-]?\s*assign\s+(\w+)\s*=/g`;
/// `\w` written as the explicit ASCII class to match JS semantics).
static ASSIGN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{%-?\s*assign\s+([0-9A-Za-z_]+)\s*=").expect("valid regex"));

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before epoch")
        .as_millis() as i64
}

/// JS truthiness for a JSON value (used for the schema `name` field checks).
fn js_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        serde_json::Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// LiquidExtractor - Extracts relationships from Liquid template files
pub struct LiquidExtractor<'a> {
    file_path: String,
    source: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
}

impl<'a> LiquidExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str) -> Self {
        LiquidExtractor {
            file_path: file_path.into(),
            source,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Extract from Liquid source
    pub fn extract(mut self) -> ExtractionResult {
        let start_time = std::time::Instant::now();

        // The TS body wraps these in try/catch emitting a `Liquid extraction
        // error:` parse_error; none of the operations below can fail in Rust
        // (the JSON parse failure is handled inline), so the catch arm has no
        // equivalent.

        // Create file node
        let file_node_id = self.create_file_node();

        // Extract render/include statements (snippet references)
        self.extract_snippet_references(&file_node_id);

        // Extract section references
        self.extract_section_references(&file_node_id);

        // Extract schema block
        self.extract_schema(&file_node_id);

        // Extract assign statements as variables
        self.extract_assignments(&file_node_id);

        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms: start_time.elapsed().as_millis() as f64,
        }
    }

    /// Create a file node for the Liquid template
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

        let mut file_node = Node::new(
            id.clone(),
            NodeKind::File,
            name,
            self.file_path.clone(),
            self.file_path.clone(),
            Language::Liquid,
            1,
            lines.len() as u32,
        );
        file_node.start_column = 0;
        file_node.end_column = lines.last().map(|l| l.len()).unwrap_or(0) as u32;
        file_node.updated_at = now_ms();

        self.nodes.push(file_node);
        id
    }

    /// Extract `{% render 'snippet' %}` and `{% include 'snippet' %}` references
    fn extract_snippet_references(&mut self, file_node_id: &str) {
        let source = self.source;

        for caps in RENDER_RE.captures_iter(source) {
            let full_match = caps.get(0).expect("match");
            let tag_type = caps.get(1).expect("group 1").as_str();
            let snippet_name = caps.get(2).expect("group 2").as_str();
            let line = self.get_line_number(full_match.start());
            let column = (full_match.start() - self.get_line_start(line)) as u32;

            // Create an import node for searchability
            let import_node_id =
                generate_node_id(&self.file_path, NodeKind::Import, snippet_name, line);
            let mut import_node = Node::new(
                import_node_id.clone(),
                NodeKind::Import,
                snippet_name,
                format!("{}::import:{}", self.file_path, snippet_name),
                self.file_path.clone(),
                Language::Liquid,
                line,
                line,
            );
            import_node.signature = Some(full_match.as_str().to_string());
            import_node.start_column = column;
            import_node.end_column = column + full_match.as_str().len() as u32;
            import_node.updated_at = now_ms();
            self.nodes.push(import_node);

            // Add containment edge from file to import
            self.edges
                .push(Edge::new(file_node_id, import_node_id, EdgeKind::Contains));

            // Create a component node for the snippet reference
            let node_id = generate_node_id(
                &self.file_path,
                NodeKind::Component,
                &format!("{}:{}", tag_type, snippet_name),
                line,
            );

            let mut node = Node::new(
                node_id.clone(),
                NodeKind::Component,
                snippet_name,
                format!("{}::{}:{}", self.file_path, tag_type, snippet_name),
                self.file_path.clone(),
                Language::Liquid,
                line,
                line,
            );
            node.start_column = column;
            node.end_column = column + full_match.as_str().len() as u32;
            node.updated_at = now_ms();

            self.nodes.push(node);

            // Add containment edge from file
            self.edges
                .push(Edge::new(file_node_id, node_id, EdgeKind::Contains));

            // Add unresolved reference to the snippet file
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: file_node_id.to_string(),
                reference_name: format!("snippets/{}.liquid", snippet_name),
                reference_kind: EdgeKind::References,
                line,
                column,
                file_path: None,
                language: None,
                candidates: None,
            });
        }
    }

    /// Extract `{% section 'name' %}` references
    fn extract_section_references(&mut self, file_node_id: &str) {
        let source = self.source;

        for caps in SECTION_RE.captures_iter(source) {
            let full_match = caps.get(0).expect("match");
            let section_name = caps.get(1).expect("group 1").as_str();
            let line = self.get_line_number(full_match.start());
            let column = (full_match.start() - self.get_line_start(line)) as u32;

            // Create an import node for searchability
            let import_node_id =
                generate_node_id(&self.file_path, NodeKind::Import, section_name, line);
            let mut import_node = Node::new(
                import_node_id.clone(),
                NodeKind::Import,
                section_name,
                format!("{}::import:{}", self.file_path, section_name),
                self.file_path.clone(),
                Language::Liquid,
                line,
                line,
            );
            import_node.signature = Some(full_match.as_str().to_string());
            import_node.start_column = column;
            import_node.end_column = column + full_match.as_str().len() as u32;
            import_node.updated_at = now_ms();
            self.nodes.push(import_node);

            // Add containment edge from file to import
            self.edges
                .push(Edge::new(file_node_id, import_node_id, EdgeKind::Contains));

            // Create a component node for the section reference
            let node_id = generate_node_id(
                &self.file_path,
                NodeKind::Component,
                &format!("section:{}", section_name),
                line,
            );

            let mut node = Node::new(
                node_id.clone(),
                NodeKind::Component,
                section_name,
                format!("{}::section:{}", self.file_path, section_name),
                self.file_path.clone(),
                Language::Liquid,
                line,
                line,
            );
            node.start_column = column;
            node.end_column = column + full_match.as_str().len() as u32;
            node.updated_at = now_ms();

            self.nodes.push(node);

            // Add containment edge from file
            self.edges
                .push(Edge::new(file_node_id, node_id, EdgeKind::Contains));

            // Add unresolved reference to the section file
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: file_node_id.to_string(),
                reference_name: format!("sections/{}.liquid", section_name),
                reference_kind: EdgeKind::References,
                line,
                column,
                file_path: None,
                language: None,
                candidates: None,
            });
        }
    }

    /// Extract `{% schema %}...{% endschema %}` blocks
    fn extract_schema(&mut self, file_node_id: &str) {
        let source = self.source;

        for caps in SCHEMA_RE.captures_iter(source) {
            let full_match = caps.get(0).expect("match");
            let schema_content = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let start_line = self.get_line_number(full_match.start());
            let end_line = self.get_line_number(full_match.end());

            // Try to parse the schema JSON to get the name
            let mut schema_name = String::from("schema");
            if let Ok(schema_json) = serde_json::from_str::<serde_json::Value>(schema_content) {
                if let Some(name) = schema_json.get("name") {
                    if js_truthy(name) {
                        if let serde_json::Value::String(s) = name {
                            schema_name = s.clone();
                        } else {
                            // Shopify schema names can be translation objects like
                            // {"en": "...", "fr": "..."} — TS:
                            // `name.en || Object.values(name)[0] || 'schema'`
                            let first_value = match name {
                                serde_json::Value::Object(map) => map.values().next(),
                                serde_json::Value::Array(arr) => arr.first(),
                                _ => None,
                            };
                            let pick = name
                                .get("en")
                                .filter(|v| js_truthy(v))
                                .or_else(|| first_value.filter(|v| js_truthy(v)));
                            match pick {
                                Some(serde_json::Value::String(s)) => schema_name = s.clone(),
                                Some(serde_json::Value::Number(n)) => schema_name = n.to_string(),
                                Some(serde_json::Value::Bool(b)) => schema_name = b.to_string(),
                                _ => {}
                            }
                        }
                    }
                }
            }
            // Schema isn't valid JSON → use default name

            // Create a node for the schema
            let node_id = generate_node_id(
                &self.file_path,
                NodeKind::Constant,
                &format!("schema:{}", schema_name),
                start_line,
            );

            let mut node = Node::new(
                node_id.clone(),
                NodeKind::Constant,
                schema_name.clone(),
                format!("{}::schema:{}", self.file_path, schema_name),
                self.file_path.clone(),
                Language::Liquid,
                start_line,
                end_line,
            );
            node.start_column = (full_match.start() - self.get_line_start(start_line)) as u32;
            node.end_column = 0;
            // Store first 200 chars as docstring
            node.docstring = Some(schema_content.trim().chars().take(200).collect());
            node.updated_at = now_ms();

            self.nodes.push(node);

            // Add containment edge from file
            self.edges
                .push(Edge::new(file_node_id, node_id, EdgeKind::Contains));
        }
    }

    /// Extract `{% assign var = value %}` statements
    fn extract_assignments(&mut self, file_node_id: &str) {
        let source = self.source;

        for caps in ASSIGN_RE.captures_iter(source) {
            let full_match = caps.get(0).expect("match");
            let variable_name = caps.get(1).expect("group 1").as_str();
            let line = self.get_line_number(full_match.start());
            let column = (full_match.start() - self.get_line_start(line)) as u32;

            // Create a variable node
            let node_id =
                generate_node_id(&self.file_path, NodeKind::Variable, variable_name, line);

            let mut node = Node::new(
                node_id.clone(),
                NodeKind::Variable,
                variable_name,
                format!("{}::{}", self.file_path, variable_name),
                self.file_path.clone(),
                Language::Liquid,
                line,
                line,
            );
            node.start_column = column;
            node.end_column = column + full_match.as_str().len() as u32;
            node.updated_at = now_ms();

            self.nodes.push(node);

            // Add containment edge from file
            self.edges
                .push(Edge::new(file_node_id, node_id, EdgeKind::Contains));
        }
    }

    /// Get the line number for a character index
    fn get_line_number(&self, index: usize) -> u32 {
        (self.source[..index].matches('\n').count() + 1) as u32
    }

    /// Get the character index of the start of a line
    fn get_line_start(&self, line_number: u32) -> usize {
        let lines: Vec<&str> = self.source.split('\n').collect();
        let mut index = 0;
        let mut i = 0usize;
        while i + 1 < line_number as usize && i < lines.len() {
            index += lines[i].len() + 1; // +1 for newline
            i += 1;
        }
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(path: &str, source: &str) -> ExtractionResult {
        LiquidExtractor::new(path, source).extract()
    }

    #[test]
    fn extracts_render_tag() {
        let code = "{% render 'loading-spinner' %}";
        let result = extract("template.liquid", code);

        let import_node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import_node.name, "loading-spinner");
        assert!(import_node.signature.as_deref().unwrap().contains("render"));

        // Component node + containment + unresolved snippet reference
        let component = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .expect("component node");
        assert_eq!(component.name, "loading-spinner");
        assert_eq!(
            component.qualified_name,
            "template.liquid::render:loading-spinner"
        );

        let file_node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::File)
            .expect("file node");
        assert!(result.edges.iter().any(|e| e.source == file_node.id
            && e.target == component.id
            && e.kind == EdgeKind::Contains));

        let snippet_ref = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_name == "snippets/loading-spinner.liquid")
            .expect("snippet reference");
        assert_eq!(snippet_ref.reference_kind, EdgeKind::References);
        assert_eq!(snippet_ref.line, 1);
    }

    #[test]
    fn extracts_section_tag() {
        let code = "{% section 'header' %}";
        let result = extract("layout/theme.liquid", code);

        let import_node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import_node.name, "header");
        assert!(
            import_node
                .signature
                .as_deref()
                .unwrap()
                .contains("section")
        );

        let section_ref = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_name == "sections/header.liquid")
            .expect("section reference");
        assert_eq!(section_ref.reference_kind, EdgeKind::References);
    }

    #[test]
    fn extracts_include_tag() {
        let code = "{% include 'icon-cart' %}";
        let result = extract("snippets/header.liquid", code);

        let import_node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import_node.name, "icon-cart");
        assert!(
            import_node
                .signature
                .as_deref()
                .unwrap()
                .contains("include")
        );
    }

    #[test]
    fn extracts_render_with_whitespace_control() {
        let code = "{%- render 'price' -%}";
        let result = extract("snippets/product.liquid", code);

        let import_node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import_node.name, "price");
    }

    #[test]
    fn extracts_multiple_imports() {
        let code = "\n{% section 'header' %}\n{% render 'loading-spinner' %}\n{% render 'cart-drawer' %}\n";
        let result = extract("layout/theme.liquid", code);

        let import_nodes: Vec<&Node> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Import)
            .collect();
        assert_eq!(import_nodes.len(), 3);

        let names: Vec<&str> = import_nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"header"));
        assert!(names.contains(&"loading-spinner"));
        assert!(names.contains(&"cart-drawer"));

        // Lines are 1-based and computed from match offsets. Section imports
        // are extracted after render/include imports (TS pass order).
        let section = import_nodes.iter().find(|n| n.name == "header").unwrap();
        let spinner = import_nodes
            .iter()
            .find(|n| n.name == "loading-spinner")
            .unwrap();
        let drawer = import_nodes
            .iter()
            .find(|n| n.name == "cart-drawer")
            .unwrap();
        assert_eq!(section.start_line, 2);
        assert_eq!(spinner.start_line, 3);
        assert_eq!(drawer.start_line, 4);
    }

    #[test]
    fn extracts_schema_block_with_json_name() {
        let code = "{% schema %}\n{\n  \"name\": \"Featured product\",\n  \"settings\": []\n}\n{% endschema %}\n";
        let result = extract("sections/featured.liquid", code);

        let schema = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Constant)
            .expect("schema node");
        assert_eq!(schema.name, "Featured product");
        assert_eq!(
            schema.qualified_name,
            "sections/featured.liquid::schema:Featured product"
        );
        assert_eq!(schema.start_line, 1);
        assert_eq!(schema.end_line, 6);
        assert!(schema.docstring.as_deref().unwrap().starts_with('{'));
    }

    #[test]
    fn extracts_schema_block_with_translation_object_name() {
        let code = "{% schema %}\n{ \"name\": { \"fr\": \"Produit\", \"en\": \"Product\" } }\n{% endschema %}";
        let result = extract("sections/product.liquid", code);

        let schema = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Constant)
            .expect("schema node");
        // `name.en` takes priority over the first value
        assert_eq!(schema.name, "Product");
    }

    #[test]
    fn schema_with_invalid_json_uses_default_name() {
        let code = "{% schema %}\nnot json at all\n{% endschema %}";
        let result = extract("sections/broken.liquid", code);

        let schema = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Constant)
            .expect("schema node");
        assert_eq!(schema.name, "schema");
    }

    #[test]
    fn extracts_assignments_as_variables() {
        let code = "{% assign product_title = product.title %}\n{%- assign total = 0 -%}\n";
        let result = extract("snippets/price.liquid", code);

        let variables: Vec<&Node> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Variable)
            .collect();
        assert_eq!(variables.len(), 2);
        assert_eq!(variables[0].name, "product_title");
        assert_eq!(
            variables[0].qualified_name,
            "snippets/price.liquid::product_title"
        );
        assert_eq!(variables[1].name, "total");
        assert_eq!(variables[1].start_line, 2);
    }

    #[test]
    fn file_node_shape() {
        let code = "{% render 'a' %}\nplain text";
        let result = extract("layout/theme.liquid", code);
        let file_node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::File)
            .expect("file node");
        assert_eq!(file_node.name, "theme.liquid");
        assert_eq!(file_node.qualified_name, "layout/theme.liquid");
        assert_eq!(file_node.language, Language::Liquid);
        assert_eq!(file_node.start_line, 1);
        assert_eq!(file_node.end_line, 2);
        assert!(result.errors.is_empty());
    }
}
