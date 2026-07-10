//! VueExtractor — port of `src/extraction/vue-extractor.ts`.

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::extraction::grammars::is_language_supported;
use crate::extraction::svelte_extractor::ScriptExtractorLookup;
use crate::extraction::tree_sitter_helpers::generate_node_id;
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
};

/// Vue built-in components — skipped so a `<Transition>` / `<KeepAlive>` in the
/// template doesn't become a phantom reference to a user component. Checked
/// AFTER kebab→Pascal conversion, so `<keep-alive>` is caught here too.
const VUE_BUILTIN_COMPONENTS: &[&str] = &[
    "Transition",
    "TransitionGroup",
    "KeepAlive",
    "Suspense",
    "Teleport",
    "Component",
    "Slot",
];

/// `my-component` → `MyComponent` (Vue allows either form in templates).
fn kebab_to_pascal(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn count_newlines(s: &str) -> usize {
    s.bytes().filter(|&b| b == b'\n').count()
}

/// TS: `/<script(\s[^>]*)?>(?<content>[\s\S]*?)<\/script>/g`
static SCRIPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<script(\s[^>]*)?>(?P<content>.*?)</script>").unwrap());

/// TS: `/lang\s*=\s*["'](ts|typescript)["']/`
static LANG_TS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"lang\s*=\s*["'](ts|typescript)["']"#).unwrap());

/// TS: `/\bsetup\b/`
static SETUP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u:\b)setup(?-u:\b)").unwrap());

/// TS: `/<(script|style)(\s[^>]*)?>[\s\S]*?<\/\1>/g` — the backreference is
/// expanded into an equivalent two-branch alternation (the `regex` crate has
/// no backreferences; with only two alternatives this is exactly equivalent).
static SCRIPT_STYLE_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<script(\s[^>]*)?>.*?</script>|<style(\s[^>]*)?>.*?</style>").unwrap()
});

/// TS: `/<([A-Za-z][A-Za-z0-9_-]*)\b/g`
static TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([A-Za-z][A-Za-z0-9_-]*)(?-u:\b)").unwrap());

/// A `<script>` / `<script setup>` block extracted from the Vue source.
struct ScriptBlock<'a> {
    content: &'a str,
    /// 0-indexed line where the script content starts (line after `<script>`).
    start_line: u32,
    /// Byte offset of `content` within the full `.vue` source. Used to
    /// remap the inner extraction's byte ranges back to whole-file offsets.
    start_byte: u32,
    /// Computed for parity with TS, which stores but never reads it.
    #[allow(dead_code)]
    is_setup: bool,
    is_typescript: bool,
}

/// VueExtractor - Extracts code relationships from Vue Single-File Component files
///
/// Vue SFCs are multi-language (script + template + style). Rather than
/// parsing the full Vue grammar, we extract the `<script>` block content
/// and delegate it to the TypeScript/JavaScript TreeSitterExtractor.
///
/// Every .vue file produces a component node (Vue components are always importable).
pub struct VueExtractor<'a> {
    file_path: String,
    source: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
    script_extractor_lookup: ScriptExtractorLookup,
}

impl<'a> VueExtractor<'a> {
    pub fn new(
        file_path: impl Into<String>,
        source: &'a str,
        script_extractor_lookup: ScriptExtractorLookup,
    ) -> Self {
        VueExtractor {
            file_path: file_path.into(),
            source,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
            script_extractor_lookup,
        }
    }

    /// Extract from Vue source
    pub fn extract(mut self) -> ExtractionResult {
        let start_time = now_ms();

        // Create component node for the .vue file itself
        let component_node = self.create_component_node();

        // Extract and process script blocks
        let script_blocks = self.extract_script_blocks();

        for block in &script_blocks {
            self.process_script_block(block, &component_node.id);
        }

        // Extract component usages from the <template> (<ComponentName>).
        // Without this, a Vue component used only in another component's
        // markup (incl. through a barrel import) is invisible to callers /
        // impact (#629 follow-up).
        self.extract_template_components(&component_node.id);

        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms: (now_ms() - start_time) as f64,
        }
    }

    /// Create a component node for the .vue file
    fn create_component_node(&mut self) -> Node {
        let lines: Vec<&str> = self.source.split('\n').collect();
        let last_segment = self
            .file_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default();
        // TS: `split(/[/\\]/).pop() || this.filePath` — "" is falsy.
        let file_name = if last_segment.is_empty() {
            self.file_path.as_str()
        } else {
            last_segment
        };
        let component_name = file_name.strip_suffix(".vue").unwrap_or(file_name);
        let id = generate_node_id(&self.file_path, NodeKind::Component, component_name, 1);

        let node = Node {
            id,
            kind: NodeKind::Component,
            name: component_name.to_string(),
            qualified_name: format!("{}::{}", self.file_path, component_name),
            file_path: self.file_path.clone(),
            language: Language::Vue,
            start_line: 1,
            end_line: lines.len() as u32,
            start_column: 0,
            end_column: lines.last().map(|l| l.len() as u32).unwrap_or(0),
            // The component node spans the whole .vue file by definition.
            start_byte: Some(0),
            end_byte: Some(self.source.len() as u32),
            address: None,
            size: None,
            docstring: None,
            signature: None,
            return_type: None,
            visibility: None,
            is_exported: Some(true), // Vue components are always importable
            is_async: None,
            is_static: None,
            is_abstract: None,
            decorators: None,
            type_parameters: None,
            updated_at: now_ms(),
        };

        self.nodes.push(node.clone());
        node
    }

    /// Extract `<script>` and `<script setup>` blocks from the Vue source
    fn extract_script_blocks(&self) -> Vec<ScriptBlock<'a>> {
        let mut blocks: Vec<ScriptBlock<'a>> = Vec::new();

        for caps in SCRIPT_RE.captures_iter(self.source) {
            let attrs = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let content = caps.name("content").map(|m| m.as_str()).unwrap_or("");

            // Detect TypeScript from lang attribute
            let is_typescript = LANG_TS_RE.is_match(attrs);

            // Detect <script setup>
            let is_setup = SETUP_RE.is_match(attrs);

            // Calculate start line of the script content (line after <script>)
            let full = caps.get(0).unwrap();
            let before_script = &self.source[..full.start()];
            let script_tag_line = count_newlines(before_script);
            // The content starts on the line after the opening <script> tag
            let gt_end = full.as_str().find('>').map(|i| i + 1).unwrap_or(0);
            let opening_tag = &full.as_str()[..gt_end];
            let opening_tag_lines = count_newlines(opening_tag);
            let content_start_line = script_tag_line + opening_tag_lines + 1; // 0-indexed line

            blocks.push(ScriptBlock {
                content,
                start_line: content_start_line as u32,
                start_byte: caps.name("content").map(|m| m.start() as u32).unwrap_or(0),
                is_setup,
                is_typescript,
            });
        }

        blocks
    }

    /// Process a script block by delegating to TreeSitterExtractor
    fn process_script_block(&mut self, block: &ScriptBlock<'a>, component_node_id: &str) {
        let script_language = if block.is_typescript {
            Language::Typescript
        } else {
            Language::Javascript
        };

        // Check if the script language parser is available
        if !is_language_supported(script_language) {
            self.errors.push(ExtractionError {
                message: format!(
                    "Parser for {} not available, cannot parse Vue script block",
                    script_language
                ),
                file_path: None,
                line: None,
                column: None,
                severity: Severity::Warning,
                code: None,
            });
            return;
        }

        // Delegate to TreeSitterExtractor
        let extractor = (self.script_extractor_lookup)(script_language);
        let result = TreeSitterExtractor::new(
            self.file_path.clone(),
            block.content,
            Some(script_language),
            extractor,
        )
        .extract();

        // Offset line numbers from script block back to .vue file positions
        for mut node in result.nodes {
            node.start_line += block.start_line;
            node.end_line += block.start_line;
            // Byte offsets from the inner extraction are relative to the
            // script slice; shift them to whole-file offsets. `content` is a
            // byte slice of the original source, so the shift is exact.
            node.start_byte = node.start_byte.map(|b| b + block.start_byte);
            node.end_byte = node.end_byte.map(|b| b + block.start_byte);
            node.language = Language::Vue; // Mark as vue, not TS/JS

            let target = node.id.clone();
            self.nodes.push(node);

            // Add containment edge from component to this node
            self.edges
                .push(Edge::new(component_node_id, target, EdgeKind::Contains));
        }

        // Offset edges (they reference line numbers)
        for mut edge in result.edges {
            // TS: `if (edge.line)` — 0 is falsy and stays untouched.
            if let Some(line) = edge.line {
                if line != 0 {
                    edge.line = Some(line + block.start_line);
                }
            }
            self.edges.push(edge);
        }

        // Offset unresolved references
        for mut reference in result.unresolved_references {
            reference.line += block.start_line;
            reference.file_path = Some(self.file_path.clone());
            reference.language = Some(Language::Vue);
            self.unresolved_references.push(reference);
        }

        // Carry over errors
        for mut error in result.errors {
            // TS: `if (error.line)` — 0 is falsy and stays untouched.
            if let Some(line) = error.line {
                if line != 0 {
                    error.line = Some(line + block.start_line);
                }
            }
            self.errors.push(error);
        }
    }

    /// Extract component usages from the Vue `<template>`.
    ///
    /// PascalCase tags (`<Modal>`, `<Button />`) and kebab-case tags
    /// (`<my-button>`) both represent component instantiations — analogous to
    /// function calls in imperative code. Capturing them creates parent→child
    /// component edges and lets `callers` / `impact` see a component that is
    /// only ever used in markup. Vue's extractor previously parsed only the
    /// `<script>` block, so these usages produced no edge at all (#629).
    ///
    /// HTML elements (lowercase, no hyphen) and Vue built-ins are skipped.
    /// Unmatched names create no edge during resolution, so converting
    /// kebab-case is safe even for native custom elements.
    fn extract_template_components(&mut self, component_node_id: &str) {
        // Ranges covered by <script> / <style> blocks — skip them so script
        // identifiers and CSS selectors aren't mistaken for template tags. This
        // also correctly handles nested <template> tags (v-if / slots), which a
        // single non-greedy <template>…</template> match would mis-bound.
        let mut covered_ranges: Vec<(usize, usize)> = Vec::new();
        for m in SCRIPT_STYLE_BLOCK_RE.find_iter(self.source) {
            let start_line = count_newlines(&self.source[..m.start()]);
            let end_line = start_line + count_newlines(m.as_str());
            covered_ranges.push((start_line, end_line));
        }

        let source = self.source;
        // Opening / self-closing tags (closing `</Foo>` starts with `</`, so the
        // leading `<` followed by a name letter won't match it).
        for (line_idx, line) in source.split('\n').enumerate() {
            if covered_ranges
                .iter()
                .any(|&(start, end)| line_idx >= start && line_idx <= end)
            {
                continue;
            }

            for caps in TAG_RE.captures_iter(line) {
                let raw = caps.get(1).unwrap().as_str();
                let component_name = if raw.starts_with(|c: char| c.is_ascii_uppercase()) {
                    raw.to_string() // PascalCase component
                } else if raw.contains('-') {
                    kebab_to_pascal(raw) // kebab-case component
                } else {
                    continue; // lowercase, no hyphen → native HTML element
                };
                if VUE_BUILTIN_COMPONENTS.contains(&component_name.as_str()) {
                    continue;
                }

                self.unresolved_references.push(UnresolvedReference {
                    from_node_id: component_node_id.to_string(),
                    reference_name: component_name,
                    reference_kind: EdgeKind::References,
                    line: (line_idx + 1) as u32, // 1-indexed
                    column: (caps.get(0).unwrap().start() + 1) as u32,
                    file_path: Some(self.file_path.clone()),
                    language: Some(Language::Vue),
                    candidates: None,
                    metadata: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::svelte_extractor::test_support::test_lookup;

    fn extract(file_path: &str, source: &str) -> ExtractionResult {
        VueExtractor::new(file_path, source, test_lookup).extract()
    }

    /// Mirrors extraction.test.ts "should extract component node from a Vue SFC".
    #[test]
    fn extracts_component_node_from_vue_sfc() {
        let source = "<template>\n  <div>{{ message }}</div>\n</template>\n\n<script>\nexport default {\n  data() {\n    return { message: 'Hello' };\n  }\n}\n</script>\n";
        let result = extract("HelloWorld.vue", source);

        let component = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .expect("component node");
        assert_eq!(component.name, "HelloWorld");
        assert_eq!(component.language, Language::Vue);
        assert_eq!(component.is_exported, Some(true));
        assert_eq!(component.qualified_name, "HelloWorld.vue::HelloWorld");
    }

    /// Mirrors "should extract functions from <script> block" (plain JS).
    #[test]
    fn extracts_functions_from_script_block() {
        let source = "<template>\n  <button @click=\"handleClick\">Click</button>\n</template>\n\n<script>\nfunction handleClick() {\n  console.log('clicked');\n}\n\nconst count = 0;\n</script>\n";
        let result = extract("Button.vue", source);

        let component = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .expect("component node");
        assert_eq!(component.name, "Button");

        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function && n.name == "handleClick")
            .expect("handleClick node");
        assert_eq!(func.language, Language::Vue);
    }

    /// Mirrors "should extract from <script setup lang=\"ts\"> block":
    /// all nodes are marked as vue language.
    #[test]
    fn extracts_from_script_setup_ts_block() {
        let source = "<template>\n  <div>{{ count }}</div>\n</template>\n\n<script setup lang=\"ts\">\nimport { ref } from 'vue';\n\nconst count = ref(0);\n\nfunction increment(): void {\n  count.value++;\n}\n</script>\n";
        let result = extract("Counter.vue", source);

        let component = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .expect("component node");
        assert_eq!(component.name, "Counter");

        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function && n.name == "increment")
            .expect("increment node");
        assert_eq!(func.language, Language::Vue);

        for node in &result.nodes {
            assert_eq!(node.language, Language::Vue);
        }
    }

    /// Mirrors "should extract component usages from the Vue template
    /// (PascalCase + kebab, skipping built-ins) (#629)". Line/column and node
    /// offsets are parity vectors probed from the TS implementation.
    #[test]
    fn extracts_template_component_usages_629() {
        let source = "<template>\n  <div class=\"wrap\">\n    <UserCard :user=\"u\" />\n    <my-button>Click</my-button>\n    <Transition><span>x</span></Transition>\n  </div>\n</template>\n\n<script setup lang=\"ts\">\nimport UserCard from './UserCard.vue';\nfunction increment(): void {\n  count.value++;\n}\n</script>\n";
        let result = extract("components/Host.vue", source);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let refs: Vec<String> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::References)
            .map(|r| format!("{}@{}:{}", r.reference_name, r.line, r.column))
            .collect();
        assert_eq!(refs, vec!["UserCard@3:5", "MyButton@4:5"]);

        let names: Vec<&str> = result
            .unresolved_references
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(!names.contains(&"Transition")); // Vue built-in skipped
        assert!(!names.contains(&"Div")); // native HTML element skipped
        assert!(!names.contains(&"Span"));
        assert!(!names.contains(&"Template"));

        // Script-block node offsets back to .vue positions (TS parity).
        let summary: Vec<String> = result
            .nodes
            .iter()
            .map(|n| {
                format!(
                    "{}:{}@{}-{}",
                    n.kind.as_str(),
                    n.name,
                    n.start_line,
                    n.end_line
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                "component:Host@1-15",
                "file:Host.vue@10-15",
                "import:./UserCard.vue@11-11",
                "function:increment@12-14",
            ]
        );

        // The import reference is offset and re-labeled vue.
        let import_ref = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_kind == EdgeKind::Imports)
            .expect("imports ref");
        assert_eq!(import_ref.reference_name, "./UserCard.vue");
        assert_eq!(import_ref.line, 11);
        assert_eq!(import_ref.language, Some(Language::Vue));
        assert_eq!(import_ref.file_path.as_deref(), Some("components/Host.vue"));
    }

    /// Kebab-case Vue built-ins are caught after conversion (<keep-alive>).
    #[test]
    fn skips_kebab_case_builtins() {
        let source = "<template>\n  <keep-alive><user-card /></keep-alive>\n</template>\n";
        let result = extract("Cache.vue", source);

        let names: Vec<&str> = result
            .unresolved_references
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(!names.contains(&"KeepAlive"));
        assert!(names.contains(&"UserCard"));
    }

    /// Mirrors "should extract from both <script> and <script setup> blocks".
    #[test]
    fn extracts_from_both_script_and_script_setup_blocks() {
        let source = "<template>\n  <div>{{ msg }}</div>\n</template>\n\n<script>\nexport default {\n  name: 'DualScript'\n}\n</script>\n\n<script setup>\nconst msg = 'hello';\n\nfunction greet() {\n  return msg;\n}\n</script>\n";
        let result = extract("DualScript.vue", source);

        assert!(result.nodes.iter().any(|n| n.kind == NodeKind::Component));

        // TS-parity node summary, incl. one file node per script block.
        let summary: Vec<String> = result
            .nodes
            .iter()
            .map(|n| {
                format!(
                    "{}:{}@{}-{}",
                    n.kind.as_str(),
                    n.name,
                    n.start_line,
                    n.end_line
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                "component:DualScript@1-18",
                "file:DualScript.vue@6-10",
                "file:DualScript.vue@12-18",
                "constant:msg@13-13",
                "function:greet@15-17",
            ]
        );
    }

    /// Mirrors "should create component node for template-only Vue file".
    #[test]
    fn creates_component_node_for_template_only_file() {
        let source = "<template>\n  <div>Static content</div>\n</template>\n";
        let result = extract("Static.vue", source);

        assert_eq!(result.nodes.len(), 1);
        let component = &result.nodes[0];
        assert_eq!(component.kind, NodeKind::Component);
        assert_eq!(component.name, "Static");
        assert_eq!(component.language, Language::Vue);
        assert_eq!(component.start_line, 1);
        assert_eq!(component.end_line, 4);
        assert_eq!(component.start_column, 0);
        assert_eq!(component.end_column, 0);
        // No template refs: template/div are native HTML elements.
        assert!(result.unresolved_references.is_empty());
    }

    /// Mirrors "should create containment edges from component to script nodes".
    #[test]
    fn creates_containment_edges_from_component_to_script_nodes() {
        let source = "<template>\n  <div>{{ value }}</div>\n</template>\n\n<script setup lang=\"ts\">\nconst value = 42;\n</script>\n";
        let result = extract("Contained.vue", source);

        let component = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .expect("component node");

        let contain_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.source == component.id && e.kind == EdgeKind::Contains)
            .collect();
        assert!(!contain_edges.is_empty());
    }

    #[test]
    fn kebab_to_pascal_conversion() {
        assert_eq!(kebab_to_pascal("my-button"), "MyButton");
        assert_eq!(kebab_to_pascal("user-card-x"), "UserCardX");
        assert_eq!(kebab_to_pascal("keep-alive"), "KeepAlive");
        assert_eq!(kebab_to_pascal("x"), "X");
        assert_eq!(kebab_to_pascal("a--b"), "AB"); // empty segment → ''
    }
}
