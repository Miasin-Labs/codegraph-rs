//! CFML extraction with automatic tag/script dialect switching.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::extraction::grammars::create_parser;
use crate::extraction::languages;
use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::extraction::tree_sitter_types::SyntaxNode;
use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
use crate::types::{
    Edge,
    EdgeKind,
    ExtractionError,
    ExtractionResult,
    Language,
    Node,
    NodeKind,
    Severity,
    UnresolvedReference,
    Visibility,
};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Extracts both modern bare-script CFML and legacy tag-based CFML.
pub struct CfmlExtractor<'a> {
    file_path: String,
    source: &'a str,
    language: Language,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
}

impl<'a> CfmlExtractor<'a> {
    /// `language` is the detected file dialect. Delegated CFScript/CFQuery
    /// symbols are restamped with it so `.cfm`, `.cfc`, and `.cfs` stay grouped.
    pub fn new(file_path: impl Into<String>, source: &'a str, language: Language) -> Self {
        Self {
            file_path: file_path.into(),
            source,
            language,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn extract(mut self) -> ExtractionResult {
        let start = std::time::Instant::now();

        if is_bare_script_cfml(self.source) {
            self.extract_bare_script();
        } else {
            self.extract_tag_based();
        }

        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms: start.elapsed().as_secs_f64() * 1_000.0,
        }
    }

    fn extract_bare_script(&mut self) {
        let result = TreeSitterExtractor::new(
            self.file_path.clone(),
            self.source,
            Some(Language::Cfscript),
            languages::extractor_for(Language::Cfscript),
        )
        .extract();

        let component_name = self.component_name_from_path();
        for mut node in result.nodes {
            node.language = self.language;
            if node.name == "<anonymous>"
                && matches!(node.kind, NodeKind::Class | NodeKind::Interface)
            {
                node.name = component_name.clone();
                node.qualified_name = format!("{}::{component_name}", self.file_path);
            } else if node.qualified_name == "<anonymous>"
                || node.qualified_name.starts_with("<anonymous>::")
            {
                node.qualified_name = format!(
                    "{component_name}{}",
                    &node.qualified_name["<anonymous>".len()..]
                );
            }
            self.nodes.push(node);
        }
        self.edges.extend(result.edges);
        for mut reference in result.unresolved_references {
            reference.language = Some(self.language);
            reference.file_path = Some(self.file_path.clone());
            self.unresolved_references.push(reference);
        }
        self.errors.extend(result.errors);
    }

    fn extract_tag_based(&mut self) {
        let Some(mut parser) = create_parser(Language::Cfml) else {
            self.push_error("cfml grammar not loaded", "unsupported_language");
            return;
        };
        let Some(tree) = parser.parse(self.source, None) else {
            self.push_error("Failed to parse CFML source", "parse_error");
            return;
        };

        let file_node = self.create_file_node();
        self.walk_program(tree.root_node(), &file_node.id);
    }

    fn push_error(&mut self, message: &str, code: &str) {
        self.errors.push(ExtractionError {
            message: message.to_string(),
            file_path: Some(self.file_path.clone()),
            line: None,
            column: None,
            severity: Severity::Error,
            code: Some(code.to_string()),
        });
    }

    fn create_file_node(&mut self) -> Node {
        let line_count = self.source.split('\n').count().max(1) as u32;
        let last_line_length =
            self.source
                .rsplit_once('\n')
                .map_or(self.source.len(), |(_, line)| line.len()) as u32;
        let name = self
            .file_path
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.file_path)
            .to_string();
        let id = generate_node_id(&self.file_path, NodeKind::File, &self.file_path, 1);
        let mut node = Node::new(
            id,
            NodeKind::File,
            name,
            self.file_path.clone(),
            self.file_path.clone(),
            self.language,
            1,
            line_count,
        );
        node.end_column = last_line_length;
        node.start_byte = Some(0);
        node.end_byte = Some(self.source.len() as u32);
        node.updated_at = now_ms();
        self.nodes.push(node.clone());
        node
    }

    fn walk_program(&mut self, root: SyntaxNode<'_>, file_node_id: &str) {
        let mut child = root.named_child(0);
        while let Some(current) = child {
            if current.kind() == "cf_component_open_tag" {
                let last = self.extract_component(current, Some(file_node_id));
                child = last.next_named_sibling();
                continue;
            }
            if current.kind() == "cf_function_tag" {
                self.extract_function_tag(current, None, Some(file_node_id), None);
            } else if current.kind() == "cf_script_tag" {
                self.delegate_script_tag(current, Some(file_node_id), None);
            } else if current.kind() == "cf_query_tag" {
                self.delegate_query_tag(current, Some(file_node_id));
            } else {
                self.delegate_nested_tags(current, Some(file_node_id), None);
            }
            child = current.next_named_sibling();
        }
    }

    fn extract_component<'tree>(
        &mut self,
        open_tag: SyntaxNode<'tree>,
        container_id: Option<&str>,
    ) -> SyntaxNode<'tree> {
        let name = self
            .tag_attr(open_tag, "name")
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.component_name_from_path());
        let line = open_tag.start_position().row as u32 + 1;
        let id = generate_node_id(&self.file_path, NodeKind::Class, &name, line);
        let mut class_node = Node::new(
            id,
            NodeKind::Class,
            name.clone(),
            format!("{}::{name}", self.file_path),
            self.file_path.clone(),
            self.language,
            line,
            line,
        );
        class_node.start_column = open_tag.start_position().column as u32;
        class_node.end_column = open_tag.end_position().column as u32;
        class_node.start_byte = Some(open_tag.start_byte() as u32);
        class_node.end_byte = Some(open_tag.end_byte() as u32);
        class_node.is_exported = Some(true);
        class_node.updated_at = now_ms();
        let class_id = class_node.id.clone();
        self.nodes.push(class_node);
        if let Some(container_id) = container_id {
            self.edges.push(Edge::new(
                container_id,
                class_id.as_str(),
                EdgeKind::Contains,
            ));
        }

        if let Some(extends_name) = self
            .tag_attr(open_tag, "extends")
            .filter(|name| !name.is_empty())
        {
            self.push_reference(&class_id, extends_name, EdgeKind::Extends, open_tag);
        }
        if let Some(implements) = self.tag_attr(open_tag, "implements") {
            for interface in implements
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
            {
                self.push_reference(
                    &class_id,
                    interface.to_string(),
                    EdgeKind::Implements,
                    open_tag,
                );
            }
        }

        let mut sibling = open_tag.next_named_sibling();
        let mut last_node = open_tag;
        while let Some(current) = sibling {
            if current.kind() == "cf_component_close_tag" {
                last_node = current;
                break;
            }
            if current.kind() == "cf_function_tag" {
                self.extract_function_tag(current, Some(&class_id), Some(&class_id), Some(&name));
            } else if current.kind() == "cf_script_tag" {
                self.delegate_script_tag(current, Some(&class_id), Some(&name));
            } else if current.kind() == "cf_query_tag" {
                self.delegate_query_tag(current, Some(&class_id));
            } else {
                self.delegate_nested_tags(current, Some(&class_id), Some(&name));
            }
            last_node = current;
            sibling = current.next_named_sibling();
        }

        if let Some(node) = self.nodes.iter_mut().find(|node| node.id == class_id) {
            node.end_line = last_node.end_position().row as u32 + 1;
            node.end_column = last_node.end_position().column as u32;
            node.end_byte = Some(last_node.end_byte() as u32);
        }
        last_node
    }

    fn extract_function_tag(
        &mut self,
        tag: SyntaxNode<'_>,
        parent_class_id: Option<&str>,
        container_id: Option<&str>,
        parent_class_name: Option<&str>,
    ) {
        let Some(name) = self.tag_attr(tag, "name").filter(|name| !name.is_empty()) else {
            return;
        };
        let kind = if parent_class_id.is_some() {
            NodeKind::Method
        } else {
            NodeKind::Function
        };
        let line = tag.start_position().row as u32 + 1;
        let id = generate_node_id(&self.file_path, kind, &name, line);
        let qualified_name = parent_class_name.map_or_else(
            || format!("{}::{name}", self.file_path),
            |class_name| format!("{class_name}::{name}"),
        );
        let visibility = self.tag_attr(tag, "access").map(|access| {
            if access.eq_ignore_ascii_case("private") {
                Visibility::Private
            } else if access.eq_ignore_ascii_case("package") {
                Visibility::Internal
            } else {
                Visibility::Public
            }
        });
        let mut function = Node::new(
            id,
            kind,
            name,
            qualified_name,
            self.file_path.clone(),
            self.language,
            line,
            tag.end_position().row as u32 + 1,
        );
        function.start_column = tag.start_position().column as u32;
        function.end_column = tag.end_position().column as u32;
        function.start_byte = Some(tag.start_byte() as u32);
        function.end_byte = Some(tag.end_byte() as u32);
        function.visibility = visibility;
        function.return_type = self.tag_attr(tag, "returntype");
        function.updated_at = now_ms();
        let function_id = function.id.clone();
        self.nodes.push(function);

        if let Some(container_id) = container_id {
            self.edges.push(Edge::new(
                container_id,
                function_id.as_str(),
                EdgeKind::Contains,
            ));
        }
        self.delegate_nested_tags(tag, Some(&function_id), None);
    }

    fn delegate_nested_tags(
        &mut self,
        node: SyntaxNode<'_>,
        container_id: Option<&str>,
        parent_class_name: Option<&str>,
    ) {
        crate::ensure_sufficient_stack(|| {
            self.delegate_nested_tags_inner(node, container_id, parent_class_name)
        });
    }

    fn delegate_nested_tags_inner(
        &mut self,
        node: SyntaxNode<'_>,
        container_id: Option<&str>,
        parent_class_name: Option<&str>,
    ) {
        for index in 0..node.named_child_count() as u32 {
            let Some(child) = node.named_child(index) else {
                continue;
            };
            if child.kind() == "cf_script_tag" {
                self.delegate_script_tag(child, container_id, parent_class_name);
            } else if child.kind() == "cf_query_tag" {
                self.delegate_query_tag(child, container_id);
            } else if child.kind() != "cf_function_tag" {
                self.delegate_nested_tags(child, container_id, parent_class_name);
            }
        }
    }

    fn delegate_script_tag(
        &mut self,
        script_tag: SyntaxNode<'_>,
        parent_id: Option<&str>,
        parent_class_name: Option<&str>,
    ) {
        let Some(content) = named_child_of_kind(script_tag, "cf_script_content") else {
            return;
        };
        let Some(inner) = self.source.get(content.byte_range()) else {
            return;
        };
        let start_line = content.start_position().row as u32;
        let start_byte = content.start_byte() as u32;
        let result = TreeSitterExtractor::new(
            self.file_path.clone(),
            inner,
            Some(Language::Cfscript),
            languages::extractor_for(Language::Cfscript),
        )
        .extract();

        let inner_file_node_id = result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::File)
            .map(|node| node.id.clone());
        let top_level_ids: std::collections::HashSet<String> = result
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == EdgeKind::Contains
                    && inner_file_node_id
                        .as_ref()
                        .is_some_and(|file_id| edge.source == *file_id)
            })
            .map(|edge| edge.target.clone())
            .collect();

        for mut node in result.nodes {
            if node.kind == NodeKind::File {
                continue;
            }
            node.start_line += start_line;
            node.end_line += start_line;
            node.start_byte = node.start_byte.map(|byte| byte + start_byte);
            node.end_byte = node.end_byte.map(|byte| byte + start_byte);
            node.language = self.language;
            if let Some(class_name) = parent_class_name {
                if node.kind == NodeKind::Function && top_level_ids.contains(&node.id) {
                    node.kind = NodeKind::Method;
                }
                node.qualified_name = format!("{class_name}::{}", node.qualified_name);
            }
            let node_id = node.id.clone();
            self.nodes.push(node);
            if let Some(parent_id) = parent_id {
                self.edges
                    .push(Edge::new(parent_id, node_id, EdgeKind::Contains));
            }
        }

        for mut edge in result.edges {
            if inner_file_node_id
                .as_ref()
                .is_some_and(|file_id| edge.source == *file_id || edge.target == *file_id)
            {
                continue;
            }
            if let Some(line) = edge.line.filter(|line| *line != 0) {
                edge.line = Some(line + start_line);
            }
            self.edges.push(edge);
        }
        for mut reference in result.unresolved_references {
            reference.line += start_line;
            reference.file_path = Some(self.file_path.clone());
            reference.language = Some(self.language);
            if let Some(parent_id) = parent_id {
                if reference.from_node_id.is_empty()
                    || inner_file_node_id
                        .as_ref()
                        .is_some_and(|file_id| reference.from_node_id == *file_id)
                {
                    reference.from_node_id = parent_id.to_string();
                }
            }
            self.unresolved_references.push(reference);
        }
        for mut error in result.errors {
            if let Some(line) = error.line.filter(|line| *line != 0) {
                error.line = Some(line + start_line);
            }
            self.errors.push(error);
        }
    }

    fn delegate_query_tag(&mut self, query_tag: SyntaxNode<'_>, parent_id: Option<&str>) {
        let Some(content) = named_child_of_kind(query_tag, "cf_query_content") else {
            return;
        };
        let Some(sql) = self.source.get(content.byte_range()) else {
            return;
        };
        let start_line = content.start_position().row as u32;
        let result = TreeSitterExtractor::new(
            self.file_path.clone(),
            sql,
            Some(Language::Cfquery),
            languages::extractor_for(Language::Cfquery),
        )
        .extract();
        let inner_file_node_id = result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::File)
            .map(|node| node.id.clone());

        for mut reference in result.unresolved_references {
            reference.line += start_line;
            reference.file_path = Some(self.file_path.clone());
            reference.language = Some(self.language);
            if let Some(parent_id) = parent_id {
                if reference.from_node_id.is_empty()
                    || inner_file_node_id
                        .as_ref()
                        .is_some_and(|file_id| reference.from_node_id == *file_id)
                {
                    reference.from_node_id = parent_id.to_string();
                }
            }
            self.unresolved_references.push(reference);
        }
        for mut error in result.errors {
            if let Some(line) = error.line.filter(|line| *line != 0) {
                error.line = Some(line + start_line);
            }
            self.errors.push(error);
        }
    }

    fn push_reference(
        &mut self,
        from_node_id: &str,
        reference_name: String,
        reference_kind: EdgeKind,
        node: SyntaxNode<'_>,
    ) {
        self.unresolved_references.push(UnresolvedReference {
            from_node_id: from_node_id.to_string(),
            reference_name,
            reference_kind,
            line: node.start_position().row as u32 + 1,
            column: node.start_position().column as u32,
            file_path: Some(self.file_path.clone()),
            language: Some(self.language),
            candidates: None,
            metadata: None,
        });
    }

    fn tag_attr(&self, tag: SyntaxNode<'_>, attr_name: &str) -> Option<String> {
        let mut attributes = Vec::new();
        for index in 0..tag.named_child_count() as u32 {
            let Some(child) = tag.named_child(index) else {
                continue;
            };
            if child.kind() == "cf_attribute" {
                attributes.push(child);
            } else if child.kind() == "cf_tag_attributes" {
                for inner_index in 0..child.named_child_count() as u32 {
                    if let Some(inner) = child.named_child(inner_index) {
                        if inner.kind() == "cf_attribute" {
                            attributes.push(inner);
                        }
                    }
                }
            }
        }

        for attribute in attributes {
            let Some(name_node) = named_child_of_kind(attribute, "cf_attribute_name") else {
                continue;
            };
            let Some(name) = self.source.get(name_node.byte_range()) else {
                continue;
            };
            if !name.eq_ignore_ascii_case(attr_name) {
                continue;
            }
            let value_wrapper = named_child_of_kind(attribute, "quoted_cf_attribute_value")
                .or_else(|| named_child_of_kind(attribute, "cf_attribute_value"));
            let Some(value_node) =
                value_wrapper.and_then(|wrapper| named_child_of_kind(wrapper, "attribute_value"))
            else {
                return Some(String::new());
            };
            return self
                .source
                .get(value_node.byte_range())
                .map(ToString::to_string);
        }
        None
    }

    fn component_name_from_path(&self) -> String {
        let file_name = self
            .file_path
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.file_path);
        for suffix in [".cfc", ".cfm", ".cfs"] {
            if file_name
                .get(file_name.len().saturating_sub(suffix.len())..)
                .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
            {
                return file_name[..file_name.len() - suffix.len()].to_string();
            }
        }
        file_name.to_string()
    }
}

fn named_child_of_kind<'tree>(node: SyntaxNode<'tree>, kind: &str) -> Option<SyntaxNode<'tree>> {
    for index in 0..node.named_child_count() as u32 {
        if let Some(child) = node.named_child(index) {
            if child.kind() == kind {
                return Some(child);
            }
        }
    }
    None
}

/// Detect modern bare-script CFML by finding the first real token.
pub fn is_bare_script_cfml(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b' ' | b'\t' | b'\n' | b'\r' => index += 1,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = source[index..]
                    .find('\n')
                    .map_or(bytes.len(), |offset| index + offset + 1);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = source[index + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |offset| index + 2 + offset + 2);
            }
            // UTF-8 BOM.
            0xEF if bytes.get(index..index + 3) == Some(&[0xEF, 0xBB, 0xBF]) => {
                index += 3;
            }
            _ => return bytes[index] != b'<',
        }
    }
    true
}
