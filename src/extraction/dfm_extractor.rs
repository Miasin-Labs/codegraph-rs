//! Custom extractor for Delphi DFM/FMX form files.
//!
//! DFM/FMX files describe the visual component hierarchy and event handler
//! bindings. They use a simple text format (object/end blocks) that we parse
//! with regex — no tree-sitter grammar exists for this format.
//!
//! Extracted information:
//! - Components as NodeKind `component`
//! - Nesting as EdgeKind `contains`
//! - Event handlers (OnClick = MethodName) as UnresolvedReference → EdgeKind `references`
//!
//! Ported from `src/extraction/dfm-extractor.ts`.

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

/// TS: `/^\s*(object|inherited|inline)\s+(\w+)\s*:\s*(\w+)/`
static OBJECT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(object|inherited|inline)\s+([0-9A-Za-z_]+)\s*:\s*([0-9A-Za-z_]+)")
        .expect("valid regex")
});

/// TS: `/^\s*(On\w+)\s*=\s*(\w+)\s*$/`
static EVENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(On[0-9A-Za-z_]+)\s*=\s*([0-9A-Za-z_]+)\s*$").expect("valid regex")
});

/// TS: `/^\s*end\s*$/`
static END_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*end\s*$").expect("valid regex"));

/// TS: `/=\s*\(\s*$/`
static MULTI_LINE_START_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"=\s*\(\s*$").expect("valid regex"));

/// TS: `/=\s*<\s*$/`
static MULTI_LINE_ITEM_START_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"=\s*<\s*$").expect("valid regex"));

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before epoch")
        .as_millis() as i64
}

/// Custom extractor for Delphi DFM/FMX form files.
pub struct DfmExtractor<'a> {
    file_path: String,
    source: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
}

impl<'a> DfmExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str) -> Self {
        DfmExtractor {
            file_path: file_path.into(),
            source,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Extract components and event handler references from DFM/FMX source
    pub fn extract(mut self) -> ExtractionResult {
        let start_time = std::time::Instant::now();

        // The TS body wraps these in try/catch emitting a `DFM extraction
        // error:` parse_error; the Rust parsing below is infallible, so the
        // catch arm has no equivalent.
        let file_node_id = self.create_file_node();
        self.parse_components(&file_node_id);

        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms: start_time.elapsed().as_millis() as f64,
        }
    }

    /// Create a file node for the DFM form file
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
            Language::Pascal,
            1,
            lines.len() as u32,
        );
        file_node.start_column = 0;
        file_node.end_column = lines.last().map(|l| l.len()).unwrap_or(0) as u32;
        // The file node spans the whole source by definition.
        file_node.start_byte = Some(0);
        file_node.end_byte = Some(self.source.len() as u32);
        file_node.updated_at = now_ms();

        self.nodes.push(file_node);
        id
    }

    /// Parse object/end blocks and extract components + event handlers
    fn parse_components(&mut self, file_node_id: &str) {
        let source = self.source;
        let lines: Vec<&str> = source.split('\n').collect();
        let mut stack: Vec<String> = vec![file_node_id.to_string()];

        let mut in_multi_line = false;
        let mut multi_line_end_char = ')';

        for (i, line) in lines.iter().enumerate() {
            let line_num = (i + 1) as u32;

            // Skip multi-line properties
            if in_multi_line {
                if line.trim_end().ends_with(multi_line_end_char) {
                    in_multi_line = false;
                }
                continue;
            }
            if MULTI_LINE_START_RE.is_match(line) {
                in_multi_line = true;
                multi_line_end_char = ')';
                continue;
            }
            if MULTI_LINE_ITEM_START_RE.is_match(line) {
                in_multi_line = true;
                multi_line_end_char = '>';
                continue;
            }

            // Component declaration
            if let Some(obj_match) = OBJECT_RE.captures(line) {
                let name = obj_match.get(2).expect("group 2").as_str();
                let type_name = obj_match.get(3).expect("group 3").as_str();
                let node_id =
                    generate_node_id(&self.file_path, NodeKind::Component, name, line_num);

                let mut node = Node::new(
                    node_id.clone(),
                    NodeKind::Component,
                    name,
                    format!("{}#{}", self.file_path, name),
                    self.file_path.clone(),
                    Language::Pascal,
                    line_num,
                    line_num,
                );
                node.start_column = 0;
                node.end_column = line.len() as u32;
                node.signature = Some(type_name.to_string());
                node.updated_at = now_ms();
                self.nodes.push(node);

                self.edges.push(Edge::new(
                    stack.last().expect("non-empty stack").clone(),
                    node_id.clone(),
                    EdgeKind::Contains,
                ));
                stack.push(node_id);
                continue;
            }

            // Event handler
            if let Some(event_match) = EVENT_RE.captures(line) {
                let method_name = event_match.get(2).expect("group 2").as_str();
                self.unresolved_references.push(UnresolvedReference {
                    from_node_id: stack.last().expect("non-empty stack").clone(),
                    reference_name: method_name.to_string(),
                    reference_kind: EdgeKind::References,
                    line: line_num,
                    column: 0,
                    file_path: None,
                    language: None,
                    candidates: None,
                });
                continue;
            }

            // Block end
            if END_RE.is_match(line) && stack.len() > 1 {
                stack.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(path: &str, source: &str) -> ExtractionResult {
        DfmExtractor::new(path, source).extract()
    }

    fn components(result: &ExtractionResult) -> Vec<&Node> {
        result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Component)
            .collect()
    }

    #[test]
    fn extracts_components_from_dfm() {
        let code = "object Form1: TForm1\n  Left = 0\n  Top = 0\n  Caption = 'My Form'\n  object Button1: TButton\n    Left = 10\n    Top = 10\n    Caption = 'Click Me'\n  end\nend";
        let result = extract("Form1.dfm", code);

        let comps = components(&result);
        assert_eq!(comps.len(), 2);
        let names: Vec<&str> = comps.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Form1"));
        assert!(names.contains(&"Button1"));

        let button = comps.iter().find(|c| c.name == "Button1").unwrap();
        assert_eq!(button.signature.as_deref(), Some("TButton"));
        assert_eq!(button.language, Language::Pascal);
        assert_eq!(button.qualified_name, "Form1.dfm#Button1");
    }

    #[test]
    fn extracts_nested_component_hierarchy() {
        let code = "object Form1: TForm1\n  object Panel1: TPanel\n    object Label1: TLabel\n      Caption = 'Hello'\n    end\n  end\nend";
        let result = extract("Form1.dfm", code);

        let comps = components(&result);
        assert_eq!(comps.len(), 3);

        // Check nesting: Panel1 contains Label1
        let panel = comps.iter().find(|c| c.name == "Panel1").unwrap();
        let label = comps.iter().find(|c| c.name == "Label1").unwrap();
        assert!(
            result.edges.iter().any(|e| e.source == panel.id
                && e.target == label.id
                && e.kind == EdgeKind::Contains)
        );
    }

    #[test]
    fn extracts_event_handler_references() {
        let code = "object Form1: TForm1\n  OnCreate = FormCreate\n  OnDestroy = FormDestroy\n  object Button1: TButton\n    OnClick = Button1Click\n  end\nend";
        let result = extract("Form1.dfm", code);

        let refs = &result.unresolved_references;
        assert_eq!(refs.len(), 3);
        let names: Vec<&str> = refs.iter().map(|r| r.reference_name.as_str()).collect();
        assert!(names.contains(&"FormCreate"));
        assert!(names.contains(&"FormDestroy"));
        assert!(names.contains(&"Button1Click"));
        assert!(
            refs.iter()
                .all(|r| r.reference_kind == EdgeKind::References)
        );
    }

    #[test]
    fn handles_multi_line_properties() {
        let code = "object Form1: TForm1\n  SQL.Strings = (\n    'SELECT * FROM users'\n    'WHERE active = 1')\n  object Button1: TButton\n    OnClick = Button1Click\n  end\nend";
        let result = extract("Form1.dfm", code);

        assert_eq!(components(&result).len(), 2);

        let refs = &result.unresolved_references;
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].reference_name, "Button1Click");
    }

    #[test]
    fn handles_inherited_keyword() {
        let code = "inherited Form1: TForm1\n  Caption = 'Inherited Form'\n  object Button1: TButton\n    OnClick = Button1Click\n  end\nend";
        let result = extract("Form1.dfm", code);

        let comps = components(&result);
        assert_eq!(comps.len(), 2);
        assert!(comps.iter().any(|c| c.name == "Form1"));
    }

    #[test]
    fn handles_item_collection_properties() {
        let code = "object Form1: TForm1\n  object StatusBar1: TStatusBar\n    Panels = <\n      item\n        Width = 200\n      end\n      item\n        Width = 200\n      end>\n  end\nend";
        let result = extract("Form1.dfm", code);

        assert_eq!(components(&result).len(), 2);
    }

    const MAIN_FORM_DFM: &str = "object frmMain: TfrmMain\n  Left = 0\n  Top = 0\n  Caption = 'CodeGraph DFM Fixture'\n  ClientHeight = 480\n  ClientWidth = 640\n  OnCreate = FormCreate\n  OnDestroy = FormDestroy\n  object pnlTop: TPanel\n    Left = 0\n    Top = 0\n    Width = 640\n    Height = 50\n    object lblTitle: TLabel\n      Left = 16\n      Top = 16\n      Caption = 'Authentication Service'\n    end\n    object btnLogin: TButton\n      Left = 540\n      Top = 12\n      OnClick = btnLoginClick\n    end\n  end\n  object pnlContent: TPanel\n    Left = 0\n    Top = 50\n    object edtUsername: TEdit\n      Left = 16\n      Top = 16\n      OnChange = edtUsernameChange\n    end\n    object edtPassword: TEdit\n      Left = 16\n      Top = 48\n      OnKeyPress = edtPasswordKeyPress\n    end\n    object mmoLog: TMemo\n      Left = 16\n      Top = 88\n    end\n  end\n  object pnlStatus: TStatusBar\n    Left = 0\n    Top = 440\n    Panels = <\n      item\n        Width = 200\n      end\n      item\n        Width = 200\n      end>\n  end\nend";

    #[test]
    fn full_fixture_extracts_all_components() {
        let result = extract("MainForm.dfm", MAIN_FORM_DFM);

        let comps = components(&result);
        assert_eq!(comps.len(), 9);
        let names: Vec<&str> = comps.iter().map(|c| c.name.as_str()).collect();
        for expected in [
            "frmMain",
            "pnlTop",
            "lblTitle",
            "btnLogin",
            "pnlContent",
            "edtUsername",
            "edtPassword",
            "mmoLog",
            "pnlStatus",
        ] {
            assert!(names.contains(&expected), "missing component {expected}");
        }
    }

    #[test]
    fn full_fixture_extracts_all_event_handlers() {
        let result = extract("MainForm.dfm", MAIN_FORM_DFM);

        let refs = &result.unresolved_references;
        assert_eq!(refs.len(), 5);
        let names: Vec<&str> = refs.iter().map(|r| r.reference_name.as_str()).collect();
        for expected in [
            "FormCreate",
            "FormDestroy",
            "btnLoginClick",
            "edtUsernameChange",
            "edtPasswordKeyPress",
        ] {
            assert!(names.contains(&expected), "missing handler {expected}");
        }
    }
}
