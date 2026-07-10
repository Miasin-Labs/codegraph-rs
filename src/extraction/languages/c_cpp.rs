//! C and C++ language extraction configs.
//!
//! Ported from `src/extraction/languages/c-cpp.ts`.

use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::sync::LazyLock;

use regex::Regex;

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};
use crate::types::{NodeKind, Visibility};

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

fn pre_parse_cpp_source<'a>(source: &'a str, file_path: &str) -> Cow<'a, str> {
    let lower = file_path.to_ascii_lowercase();
    if lower.ends_with(".metal") {
        Cow::Owned(blank_metal_attributes(source))
    } else if lower.ends_with(".cu") || lower.ends_with(".cuh") || looks_like_cuda_source(source) {
        Cow::Owned(blank_cuda_constructs(source))
    } else {
        Cow::Borrowed(source)
    }
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

/// C typedef: `typedef enum { ... } name;` or `typedef struct { ... } name;`
/// The inner enum_specifier/struct_specifier is anonymous, but we want the
/// typedef name to become the enum/struct node name.
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
    }
    None
}

pub struct CExtractor;

impl LanguageExtractor for CExtractor {
    fn pre_parse<'a>(&self, source: &'a str, _file_path: &str) -> Cow<'a, str> {
        if looks_like_cuda_source(source) {
            Cow::Owned(blank_cuda_constructs(source))
        } else {
            Cow::Borrowed(source)
        }
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
        // Check for access specifier in parent
        if let Some(parent) = node.parent() {
            for i in 0..parent.child_count() as u32 {
                if let Some(child) = parent.child(i) {
                    if child.kind() == "access_specifier" {
                        let text = get_node_text(child, source);
                        if text.contains("public") {
                            return Some(Visibility::Public);
                        }
                        if text.contains("private") {
                            return Some(Visibility::Private);
                        }
                        if text.contains("protected") {
                            return Some(Visibility::Protected);
                        }
                    }
                }
            }
        }
        None
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
}
