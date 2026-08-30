use std::sync::LazyLock;

use regex::Regex;

use super::common::{balanced_paren_end, blank_range, finish};

static CPP_EXPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:class|struct)\s+([A-Z][A-Z0-9_]+)(\s+[A-Za-z_]\w*(?:\s+final)?\s*[:{])")
        .expect("valid C++ export macro regex")
});

pub(super) fn blank_cpp_export_macros(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for captures in CPP_EXPORT_RE.captures_iter(source) {
        if let Some(name) = captures.get(1) {
            blank_range(&mut bytes, name.start(), name.end());
        }
    }
    finish(bytes)
}

const CPP_INLINE_MACROS: &[&str] = &[
    "FORCEINLINE_DEBUGGABLE",
    "FORCENOINLINE",
    "FORCEINLINE",
    "PUGI__FN_NO_INLINE",
    "PUGI__FN",
    "PUGIXML_FUNCTION",
    "_ALWAYS_INLINE_",
    "_FORCE_INLINE_",
    "BOOST_FORCEINLINE",
    "BOOST_NOINLINE",
    "Q_INVOKABLE",
    "Q_SCRIPTABLE",
    "Q_ALWAYS_INLINE",
    "Q_SLOT",
    "Q_SIGNAL",
    "FOLLY_ALWAYS_INLINE",
    "FOLLY_NOINLINE",
    "ABSL_ATTRIBUTE_ALWAYS_INLINE",
    "ABSL_ATTRIBUTE_NOINLINE",
    "LLVM_ATTRIBUTE_ALWAYS_INLINE",
    "LLVM_ATTRIBUTE_NOINLINE",
    "V8_INLINE",
    "V8_NOINLINE",
    "EIGEN_STRONG_INLINE",
    "EIGEN_ALWAYS_INLINE",
    "EIGEN_DEVICE_FUNC",
    "RAPIDJSON_FORCEINLINE",
    "MOZ_ALWAYS_INLINE",
    "MOZ_NEVER_INLINE",
    "PROTOBUF_ALWAYS_INLINE",
    "PROTOBUF_NOINLINE",
    "FMT_CONSTEXPR20",
    "FMT_CONSTEXPR",
    "FMT_INLINE",
    "JSON_HEDLEY_ALWAYS_INLINE",
    "JSON_HEDLEY_NEVER_INLINE",
    "HEDLEY_ALWAYS_INLINE",
    "HEDLEY_NEVER_INLINE",
    "GLM_FUNC_QUALIFIER",
    "GLM_FUNC_DECL",
    "GLM_CONSTEXPR",
    "GLM_INLINE",
    "SIMD_FORCE_INLINE",
    "SK_ALWAYS_INLINE",
    "CV_ALWAYS_INLINE",
    "CV_INLINE",
    "EA_FORCE_INLINE",
    "EA_NOINLINE",
    "CC_INLINE",
    "NEVER_INLINE",
    "G_INLINE_FUNC",
    "SQLITE_PRIVATE",
    "SQLITE_API",
    "STDMETHODCALLTYPE",
    "WINAPIV",
    "WINAPI",
    "APIENTRY",
    "ALWAYS_INLINE",
    "FORCE_INLINE",
    "NOINLINE",
];

static CPP_INLINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    let mut names = CPP_INLINE_MACROS.to_vec();
    names.sort_unstable_by_key(|name| std::cmp::Reverse(name.len()));
    Regex::new(&format!(r"\b({})\b([ \t\r\n]+[A-Za-z_])", names.join("|")))
        .expect("valid C++ inline macro regex")
});

pub(super) fn blank_cpp_inline_macros(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for captures in CPP_INLINE_RE.captures_iter(source) {
        if let Some(name) = captures.get(1) {
            blank_range(&mut bytes, name.start(), name.end());
        }
    }
    finish(bytes)
}

static CPP_API_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([A-Z][A-Z0-9_]*(?:_API|_EXPORT|_ABI))\b(\s+[A-Za-z_])")
        .expect("valid C++ API macro regex")
});

pub(super) fn blank_cpp_api_prefix_macros(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for captures in CPP_API_RE.captures_iter(source) {
        if let Some(name) = captures.get(1) {
            blank_range(&mut bytes, name.start(), name.end());
        }
    }
    finish(bytes)
}

static CPP_INLINE_ANNOTATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:UMETA|UPARAM|UE_DEPRECATED\w*)\s*\(")
        .expect("valid C++ inline annotation regex")
});

pub(super) fn blank_cpp_inline_annotation_macros(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for matched in CPP_INLINE_ANNOTATION_RE.find_iter(source) {
        let open = matched.end() - 1;
        if let Some(end) = balanced_paren_end(source, open) {
            blank_range(&mut bytes, matched.start(), end);
        }
    }
    finish(bytes)
}

static CPP_COM_INTERFACE_RE: LazyLock<Regex> = LazyLock::new(|| {
    // MSVC COM headers alias the `interface` keyword with `#define interface
    // struct`. tree-sitter-cpp doesn't know the alias, so it reads `interface`
    // as a declaration's return type and the class vanishes — the type surfaces
    // as a phantom `function` instead of a `struct` (#1519). Match the keyword
    // only where a class/struct definition follows: `interface Name {` or
    // `interface Name : Base…{`, so a variable named `interface` or a member
    // access can never match.
    Regex::new(r"(?m)\binterface\b([ \t]+[A-Za-z_]\w*[ \t]*[:{])")
        .expect("valid C++ COM interface regex")
});

/// Rewrite the MSVC COM `interface` keyword to `struct` so tree-sitter-cpp
/// parses the definition as a `struct_specifier` instead of misparsing it as a
/// function (#1519). `interface` is nine bytes and `struct` is six, so the
/// keyword is replaced with `struct` plus three trailing spaces — every
/// following byte offset is preserved, exactly as the blanking passes do.
pub(super) fn rewrite_cpp_com_interface_keyword(source: &str) -> String {
    if !source.contains("interface") {
        return source.to_string();
    }
    let mut bytes = source.as_bytes().to_vec();
    for captures in CPP_COM_INTERFACE_RE.captures_iter(source) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        // The keyword occupies the first nine bytes of the match.
        let start = whole.start();
        bytes[start..start + 6].copy_from_slice(b"struct");
        for byte in &mut bytes[start + 6..start + 9] {
            *byte = b' ';
        }
    }
    finish(bytes)
}
