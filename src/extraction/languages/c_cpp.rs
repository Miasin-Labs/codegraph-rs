//! C and C++ language extraction configs.
//!
//! Ported from `src/extraction/languages/c-cpp.ts`.

use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::sync::LazyLock;

use regex::Regex;

mod preprocess;

use preprocess::{pre_parse_c_source, pre_parse_cpp_source};

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::{NodeKind, Visibility};

/// One-pass, memoized C/C++ access-specifier resolution.
///
/// Returns the parent's first `access_specifier` (TS `getVisibility` parity:
/// the TS loop returns on the first specifier it finds and applies it to every
/// member). The naive port scanned the parent's children *per member*, and
/// `Node::child(i)` re-walks the child list from the start each call, so a
/// class/struct body was O(members^2). On a macro-confused parse or a giant
/// generated aggregate (tens of thousands of siblings under one node) that
/// quadratic blows up into minutes of pure-CPU spin — the codegraph indexing
/// hang. Here the parent's verdict is computed once via a single O(children)
/// cursor pass and memoized per parent in a thread-local, so the first member
/// pays O(children) and the rest are O(1). Measured ~20,000x faster than the
/// per-member scan on a 25k-member aggregate.
///
/// Correctness: `Node::id()` is a pointer, unique only within one live tree, so
/// the memo is scoped to the current tree. Extraction runs one tree at a time
/// per worker thread (`parse_batch` parses then extracts each file
/// sequentially in its blocking task), so a change in the tree's root id means
/// a new file — the memo is dropped and rebuilt, preventing any cross-file id
/// collision.
fn cpp_visibility_for(
    parent: SyntaxNode<'_>,
    _node: SyntaxNode<'_>,
    source: &str,
) -> Option<Visibility> {
    thread_local! {
        // (current tree root id, parent id -> first-access-specifier verdict).
        // `None` verdict is cached too, so a specifier-less aggregate is not
        // rescanned once per member.
        static VISIBILITY_CACHE: std::cell::RefCell<(
            usize,
            std::collections::HashMap<usize, Option<Visibility>>,
        )> = std::cell::RefCell::new((0, std::collections::HashMap::new()));
    }

    // Root of the tree this node belongs to: walk up to the top.
    let mut root = parent;
    while let Some(up) = root.parent() {
        root = up;
    }
    let root_id = root.id();
    let parent_id = parent.id();

    VISIBILITY_CACHE.with(|cell| {
        {
            let borrowed = cell.borrow();
            if borrowed.0 == root_id {
                if let Some(verdict) = borrowed.1.get(&parent_id) {
                    return *verdict;
                }
            }
        }

        // Single O(children) pass: the first `access_specifier` wins (TS parity).
        let mut verdict: Option<Visibility> = None;
        let mut cursor = parent.walk();
        for child in parent.children(&mut cursor) {
            if child.kind() == "access_specifier" {
                let text = get_node_text(child, source);
                if text.contains("public") {
                    verdict = Some(Visibility::Public);
                    break;
                }
                if text.contains("private") {
                    verdict = Some(Visibility::Private);
                    break;
                }
                if text.contains("protected") {
                    verdict = Some(Visibility::Protected);
                    break;
                }
            }
        }

        let mut borrowed = cell.borrow_mut();
        if borrowed.0 != root_id {
            // New tree (new file) on this worker thread — drop stale entries.
            borrowed.0 = root_id;
            borrowed.1.clear();
        }
        borrowed.1.insert(parent_id, verdict);
        verdict
    })
}

fn find_declarator_qualified_id(declarator: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut queue: VecDeque<SyntaxNode<'_>> = VecDeque::from([declarator]);
    while let Some(current) = queue.pop_front() {
        if current.kind() == "qualified_identifier" {
            return Some(current);
        }
        for i in 0..current.named_child_count() as u32 {
            let Some(child) = current.named_child(i) else {
                continue;
            };
            if !matches!(child.kind(), "parameter_list" | "trailing_return_type") {
                queue.push_back(child);
            }
        }
    }
    None
}

fn extract_cpp_qualified_method_name(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let declarator = get_child_by_field(node, "declarator")?;
    let qualified = find_declarator_qualified_id(declarator)?;
    get_node_text(qualified, source)
        .trim()
        .split("::")
        .filter(|part| !part.is_empty())
        .last()
        .map(str::to_string)
}

fn extract_cpp_receiver_type(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let declarator = get_child_by_field(node, "declarator")?;

    let qualified = find_declarator_qualified_id(declarator)?;
    let text = get_node_text(qualified, source).trim();
    let parts: Vec<&str> = text.split("::").filter(|part| !part.is_empty()).collect();
    if parts.len() > 1 {
        Some(parts[..parts.len() - 1].join("::"))
    } else {
        None
    }
}

static NON_CLASS_RETURN_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "void",
        "bool",
        "char",
        "short",
        "int",
        "long",
        "float",
        "double",
        "unsigned",
        "signed",
        "size_t",
        "ssize_t",
        "auto",
        "wchar_t",
        "char8_t",
        "char16_t",
        "char32_t",
        "int8_t",
        "int16_t",
        "int32_t",
        "int64_t",
        "uint8_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        "intptr_t",
        "uintptr_t",
        "nullptr_t",
    ]
    .into_iter()
    .collect()
});
static CPP_WRAPPER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:(?:std\s*::\s*)?)(?:unique_ptr|shared_ptr|weak_ptr|optional)\s*<\s*([^,>]+?)\s*>",
    )
    .expect("valid C++ wrapper regex")
});
static CPP_QUALIFIER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:const|volatile|typename|struct|class|enum)\b")
        .expect("valid C++ qualifier regex")
});
static CPP_TEMPLATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]*>").expect("valid C++ template regex"));

/// Normalize a result type to the class name that receives a chained call.
pub fn normalize_cpp_return_type(raw: &str) -> Option<String> {
    let mut value = raw.trim().to_string();
    if let Some(inner) = CPP_WRAPPER_RE
        .captures(&value)
        .and_then(|captures| captures.get(1))
    {
        value = inner.as_str().to_string();
    }
    value = CPP_QUALIFIER_RE.replace_all(&value, " ").into_owned();
    value = CPP_TEMPLATE_RE.replace_all(&value, " ").into_owned();
    value = value.replace(['*', '&'], " ");
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let last = value.split("::").filter(|part| !part.is_empty()).last()?;
    if NON_CLASS_RETURN_TYPES.contains(last)
        || !last
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        || !last
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(last.to_string())
}

fn extract_cpp_return_type(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    normalize_cpp_return_type(get_node_text(get_child_by_field(node, "type")?, source))
}

/// Remove balanced template argument groups from an inheritance reference.
pub fn strip_cpp_template_args(name: &str) -> String {
    if !name.contains('<') {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut depth = 0usize;
    for ch in name.chars() {
        match ch {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn blank_matches(source: &str, regex: &Regex) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for matched in regex.find_iter(source) {
        for byte in &mut bytes[matched.start()..matched.end()] {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).expect("offset-preserving blanking must remain UTF-8")
}

static METAL_ATTRIBUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\[\[\s*[A-Za-z_]\w*(?:\s*\([^()\n]*\))?(?:\s*,\s*[A-Za-z_]\w*(?:\s*\([^()\n]*\))?)*\s*\]\]",
    )
    .expect("valid Metal attribute regex")
});
static CUDA_LAUNCH_BOUNDS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b__launch_bounds__\s*\([^()\n]*\)").expect("valid CUDA bounds regex")
});
static CUDA_SPECIFIER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b__(?:global|device|host|constant|shared|managed|grid_constant|forceinline|noinline|launch_bounds)__\b")
        .expect("valid CUDA specifier regex")
});
static CUDA_LAUNCH_CONFIG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<<<[^;]{0,400}?>>>").expect("valid CUDA launch regex"));

pub fn blank_metal_attributes(source: &str) -> String {
    if source.contains("[[") {
        blank_matches(source, &METAL_ATTRIBUTE_RE)
    } else {
        source.to_string()
    }
}

pub fn blank_cuda_constructs(source: &str) -> String {
    let mut output = source.to_string();
    if output.contains("__") {
        output = blank_matches(&output, &CUDA_LAUNCH_BOUNDS_RE);
        output = blank_matches(&output, &CUDA_SPECIFIER_RE);
    }
    if output.contains("<<<") {
        let mut bytes = output.as_bytes().to_vec();
        for matched in CUDA_LAUNCH_CONFIG_RE.find_iter(&output) {
            let mut depth = 0i32;
            let mut balanced = true;
            for byte in matched.as_str().bytes() {
                if byte == b'{' {
                    depth += 1;
                } else if byte == b'}' {
                    depth -= 1;
                    if depth < 0 {
                        balanced = false;
                        break;
                    }
                }
            }
            if balanced && depth == 0 {
                for byte in &mut bytes[matched.start()..matched.end()] {
                    if !matches!(*byte, b'\n' | b'\r') {
                        *byte = b' ';
                    }
                }
            }
        }
        output = String::from_utf8(bytes).expect("offset-preserving blanking must remain UTF-8");
    }
    output
}

fn looks_like_cuda_source(source: &str) -> bool {
    ["__global__", "__device__", "__constant__", "cudaStream_t"]
        .iter()
        .any(|marker| source.contains(marker))
}

/// Shared `extractImport` body for C / C++ / (also reused by ObjC in TS shape):
/// `#include <stdio.h>` / `#include "myheader.h"`.
fn extract_include_import(node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
    let import_text = get_node_text(node, source).trim();
    if let Some(system_lib) = named_children(node)
        .into_iter()
        .find(|c| c.kind() == "system_lib_string")
    {
        // TS: .replace(/^<|>$/g, '')
        let text = get_node_text(system_lib, source);
        let text = text.strip_prefix('<').unwrap_or(text);
        let text = text.strip_suffix('>').unwrap_or(text);
        return ImportOutcome::Info(ImportInfo::new(text, import_text));
    }
    if let Some(string_literal) = named_children(node)
        .into_iter()
        .find(|c| c.kind() == "string_literal")
    {
        if let Some(string_content) = named_children(string_literal)
            .into_iter()
            .find(|c| c.kind() == "string_content")
        {
            return ImportOutcome::Info(ImportInfo::new(
                get_node_text(string_content, source),
                import_text,
            ));
        }
    }
    ImportOutcome::Declined
}

fn resolve_typedef_kind(node: SyntaxNode<'_>) -> Option<NodeKind> {
    for i in 0..node.named_child_count() as u32 {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if child.kind() == "enum_specifier" && get_child_by_field(child, "body").is_some() {
            return Some(NodeKind::Enum);
        }
        if child.kind() == "struct_specifier" && get_child_by_field(child, "body").is_some() {
            return Some(NodeKind::Struct);
        }
        if child.kind() == "union_specifier" && get_child_by_field(child, "body").is_some() {
            return Some(NodeKind::Union);
        }
    }
    None
}

pub struct CExtractor;

impl LanguageExtractor for CExtractor {
    fn pre_parse<'a>(&self, source: &'a str, _file_path: &str) -> Cow<'a, str> {
        pre_parse_c_source(source)
    }

    fn function_types(&self) -> &[&str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &[&str] {
        &[]
    }
    fn method_types(&self) -> &[&str] {
        &[]
    }
    fn interface_types(&self) -> &[&str] {
        &[]
    }
    fn struct_types(&self) -> &[&str] {
        &["struct_specifier"]
    }
    fn union_types(&self) -> &[&str] {
        &["union_specifier"]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_specifier"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enumerator"]
    }
    fn type_alias_types(&self) -> &[&str] {
        // typedef
        &["type_definition"]
    }
    fn import_types(&self) -> &[&str] {
        &["preproc_include"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &["declaration"]
    }
    fn field_types(&self) -> &[&str] {
        &["field_declaration"]
    }
    fn name_field(&self) -> &str {
        "declarator"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    fn params_field(&self) -> &str {
        "parameters"
    }

    fn get_return_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        extract_cpp_return_type(node, source)
    }

    fn resolve_type_alias_kind(&self, node: SyntaxNode<'_>, _source: &str) -> Option<NodeKind> {
        resolve_typedef_kind(node)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        extract_include_import(node, source)
    }
}

pub struct CppExtractor;

impl LanguageExtractor for CppExtractor {
    fn pre_parse<'a>(&self, source: &'a str, file_path: &str) -> Cow<'a, str> {
        pre_parse_cpp_source(source, file_path)
    }

    fn function_types(&self) -> &[&str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &[&str] {
        &["class_specifier"]
    }
    fn method_types(&self) -> &[&str] {
        &["function_definition"]
    }
    fn interface_types(&self) -> &[&str] {
        &[]
    }
    fn struct_types(&self) -> &[&str] {
        &["struct_specifier"]
    }
    fn union_types(&self) -> &[&str] {
        &["union_specifier"]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_specifier"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enumerator"]
    }
    fn type_alias_types(&self) -> &[&str] {
        // typedef and using
        &["type_definition", "alias_declaration"]
    }
    fn import_types(&self) -> &[&str] {
        &["preproc_include"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &["declaration"]
    }
    fn field_types(&self) -> &[&str] {
        &["field_declaration"]
    }
    fn name_field(&self) -> &str {
        "declarator"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    fn params_field(&self) -> &str {
        "parameters"
    }

    fn resolve_name(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        extract_cpp_qualified_method_name(node, source)
    }

    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        extract_cpp_receiver_type(node, source)
    }

    fn get_return_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        extract_cpp_return_type(node, source)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        // Delegates to the memoized one-pass resolver. The naive port scanned
        // the parent's children per member with `parent.child(i)` (which
        // re-walks from child 0 each call), making a class/struct body
        // O(members^2) and hanging the indexer on huge/misparsed aggregates.
        // See `cpp_visibility_for` for the O(children)-per-parent fix.
        let parent = node.parent()?;
        cpp_visibility_for(parent, node, source)
    }

    fn resolve_type_alias_kind(&self, node: SyntaxNode<'_>, _source: &str) -> Option<NodeKind> {
        // C++ typedef: `typedef enum { ... } name;` or `typedef struct { ... } name;`
        resolve_typedef_kind(node)
    }

    fn is_misparsed_function(&self, name: &str, _node: SyntaxNode<'_>) -> bool {
        // C++ macros like NLOHMANN_JSON_NAMESPACE_BEGIN cause tree-sitter to misparse
        // namespace blocks as function_definitions (e.g. name = "namespace detail").
        // Also filter C++ keywords that tree-sitter occasionally misinterprets as
        // function/method names (e.g. switch statements inside macro-confused scopes).
        if name.starts_with("namespace") {
            return true;
        }
        const CPP_KEYWORDS: [&str; 7] = ["switch", "if", "for", "while", "do", "case", "return"];
        CPP_KEYWORDS.contains(&name)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        extract_include_import(node, source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn cpp_visibility_public_method_in_class() {
        // A public: method in a class resolves to Public via the one-pass
        // access-specifier resolver.
        let source = "class Widget {
public:
    void show();
    void hide();
};
";
        let result = TreeSitterExtractor::new(
            "src/widget.cpp",
            source,
            Some(Language::Cpp),
            Some(&CppExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let show = result
            .nodes
            .iter()
            .find(|n| n.name == "show")
            .expect("show method extracted");
        assert_eq!(show.visibility, Some(Visibility::Public));
        let hide = result
            .nodes
            .iter()
            .find(|n| n.name == "hide")
            .expect("hide method extracted");
        // TS parity: the first access specifier applies to every member.
        assert_eq!(hide.visibility, Some(Visibility::Public));
    }

    #[test]
    fn cpp_visibility_none_without_specifier() {
        // A struct body with no access specifier yields no explicit visibility
        // (matches the TS getVisibility which returns undefined). This is the
        // path that used to run the full O(n^2) scan.
        let source = "struct Bag {
    int a();
    int b();
    int c();
};
";
        let result = TreeSitterExtractor::new(
            "src/bag.cpp",
            source,
            Some(Language::Cpp),
            Some(&CppExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        for name in ["a", "b", "c"] {
            let m = result
                .nodes
                .iter()
                .find(|n| n.name == name)
                .unwrap_or_else(|| panic!("member {name} extracted"));
            assert_eq!(m.visibility, None, "member {name} visibility");
        }
    }

    #[test]
    fn cpp_visibility_large_aggregate_terminates_fast() {
        // Regression for the indexing hang: a struct with thousands of members
        // and no access specifier used to be O(members^2). The one-pass
        // resolver makes this trivial; the test simply asserts it completes and
        // every member has no explicit visibility.
        let mut source = String::from(
            "struct Big {
",
        );
        for i in 0..4000 {
            source.push_str(&format!(
                "    int field_{i}();
"
            ));
        }
        source.push_str(
            "};
",
        );
        let result = TreeSitterExtractor::new(
            "src/big.cpp",
            &source,
            Some(Language::Cpp),
            Some(&CppExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let members = result
            .nodes
            .iter()
            .filter(|n| n.name.starts_with("field_"))
            .count();
        assert!(members >= 3990, "expected ~4000 members, got {members}");
    }

    #[test]
    fn c_smoke_extraction() {
        let source = "#include <stdio.h>\n#include \"local.h\"\n\nstruct Point {\n    int x;\n    int y;\n};\n\ntypedef enum { RED, GREEN } Color;\n\nint main(void) {\n    printf(\"hi\");\n    return 0;\n}\n";
        let result =
            TreeSitterExtractor::new("src/main.c", source, Some(Language::C), Some(&CExtractor))
                .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let point = result.nodes.iter().find(|n| n.name == "Point").unwrap();
        assert_eq!(point.kind, NodeKind::Struct);

        let color = result.nodes.iter().find(|n| n.name == "Color").unwrap();
        // typedef enum resolves to enum kind via resolve_type_alias_kind
        assert_eq!(color.kind, NodeKind::Enum);

        let main = result.nodes.iter().find(|n| n.name == "main").unwrap();
        assert_eq!(main.kind, NodeKind::Function);

        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Import)
            .collect();
        assert!(imports.iter().any(|n| n.name == "stdio.h"));
        assert!(imports.iter().any(|n| n.name == "local.h"));
    }

    #[test]
    fn cpp_smoke_extraction() {
        let source = "#include <iostream>\n\nclass Engine {\npublic:\n    void start();\n};\n\nvoid Engine::start() {\n    helper();\n}\n\nvoid helper() {}\n";
        let result = TreeSitterExtractor::new(
            "src/engine.cpp",
            source,
            Some(Language::Cpp),
            Some(&CppExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result.nodes.iter().find(|n| n.name == "Engine").unwrap();
        assert_eq!(class.kind, NodeKind::Class);

        // Out-of-line definition resolves to the unqualified name via resolve_name,
        // with the receiver from the qualified identifier.
        let start = result
            .nodes
            .iter()
            .find(|n| n.name == "start" && n.kind != NodeKind::File)
            .expect("start method");
        assert!(
            start.qualified_name.contains("Engine"),
            "expected receiver in qualified name, got {:?}",
            start.qualified_name
        );

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .unwrap();
        assert_eq!(import.name, "iostream");
    }

    #[test]
    fn cpp_misparsed_function_filter() {
        let ext = CppExtractor;
        let source = "void helper() {}\n";
        let mut parser = crate::extraction::grammars::create_parser(Language::Cpp).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let node = tree.root_node().named_child(0).unwrap();
        assert!(ext.is_misparsed_function("namespace detail", node));
        assert!(ext.is_misparsed_function("switch", node));
        assert!(ext.is_misparsed_function("return", node));
        assert!(!ext.is_misparsed_function("helper", node));
    }

    #[test]
    fn normalizes_return_types_and_template_bases() {
        assert_eq!(
            normalize_cpp_return_type("const std::unique_ptr<Widget>&"),
            Some("Widget".into())
        );
        assert_eq!(normalize_cpp_return_type("unsigned int"), None);
        assert_eq!(
            strip_cpp_template_args("ns::Base<Foo<int>>::Inner"),
            "ns::Base::Inner"
        );
    }

    #[test]
    fn metal_and_cuda_blanking_is_offset_preserving() {
        let metal = "float4 position [[position]];\nconstant U &u [[buffer(0)]];";
        let blanked = blank_metal_attributes(metal);
        assert_eq!(blanked.len(), metal.len());
        assert_eq!(blanked.lines().count(), metal.lines().count());
        assert!(!blanked.contains("[["));

        let cuda = "__global__ void step() {}\nstep<<<dim3{1,1,1}, 256>>>();";
        let blanked = blank_cuda_constructs(cuda);
        assert_eq!(blanked.len(), cuda.len());
        assert_eq!(blanked.lines().count(), cuda.lines().count());
        assert!(!blanked.contains("__global__"));
        assert!(!blanked.contains("<<<"));
    }

    #[test]
    fn cpp_extraction_records_factory_return_type() {
        let source = "std::unique_ptr<Widget> make_widget() { return {}; }\n";
        let result = TreeSitterExtractor::new(
            "src/factory.cpp",
            source,
            Some(Language::Cpp),
            Some(&CppExtractor),
        )
        .extract();
        let function = result
            .nodes
            .iter()
            .find(|node| node.name == "make_widget")
            .expect("factory function");
        assert_eq!(function.return_type.as_deref(), Some("Widget"));
    }

    #[test]
    fn c_extracts_named_and_typedef_unions_but_skips_forward_declarations() {
        let source = r#"
union packet_hdr {
    unsigned int raw;
    unsigned short port;
};
union opaque_hdr;
typedef union {
    unsigned int u;
    float f;
} word_t;
struct envelope {
    union {
        int code;
        float value;
    };
};
"#;
        let result =
            TreeSitterExtractor::new("src/packet.c", source, Some(Language::C), Some(&CExtractor))
                .extract();

        let packet = result
            .nodes
            .iter()
            .find(|node| node.name == "packet_hdr")
            .expect("named union");
        assert_eq!(packet.kind, NodeKind::Union);
        assert_eq!(packet.start_line, 2);
        assert!(result.nodes.iter().any(|node| {
            node.kind == NodeKind::Field
                && node.name == "raw"
                && node.qualified_name == "packet_hdr::raw"
        }));
        assert!(!result.nodes.iter().any(|node| node.name == "opaque_hdr"));

        let word = result
            .nodes
            .iter()
            .find(|node| node.name == "word_t")
            .expect("typedef union");
        assert_eq!(word.kind, NodeKind::Union);

        let envelope = result
            .nodes
            .iter()
            .find(|node| node.name == "envelope")
            .expect("containing struct");
        let anonymous = result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Union && node.name == "<anonymous>")
            .expect("anonymous member union");
        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::Union && node.name == "<anonymous>")
                .count(),
            1
        );
        assert_eq!(anonymous.start_line, 12);
        assert!(result.edges.iter().any(|edge| {
            edge.kind == crate::types::EdgeKind::Contains
                && edge.source == envelope.id
                && edge.target == anonymous.id
        }));
        assert!(result.nodes.iter().any(|node| {
            node.kind == NodeKind::Field
                && node.name == "code"
                && node.qualified_name == "envelope::<anonymous>::code"
        }));
    }

    #[test]
    fn cpp_union_contains_member_function() {
        let source = r#"
union Value {
    int i;
    double d;
    int as_int() const { return i; }
};
"#;
        let result = TreeSitterExtractor::new(
            "src/value.cpp",
            source,
            Some(Language::Cpp),
            Some(&CppExtractor),
        )
        .extract();
        let value = result
            .nodes
            .iter()
            .find(|node| node.name == "Value")
            .expect("union");
        let method = result
            .nodes
            .iter()
            .find(|node| node.name == "as_int")
            .expect("member function");

        assert_eq!(value.kind, NodeKind::Union);
        assert!(result.edges.iter().any(|edge| {
            edge.kind == crate::types::EdgeKind::Contains
                && edge.source == value.id
                && edge.target == method.id
        }));
    }
}
