//! Per-language extraction configurations.
//!
//! Each file exposes a [`LanguageExtractor`] implementation (a stateless unit
//! struct mirroring the TS config object). This barrel builds the
//! `EXTRACTORS` map consumed by `TreeSitterExtractor`, expressed as the
//! [`extractor_for`] lookup function.
//!
//! Ported from `src/extraction/languages/index.ts`.

pub mod apex;
pub mod bash;
pub mod c_cpp;
pub mod csharp;
pub mod dart;
pub mod go;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod lua;
pub mod luau;
pub mod objc;
pub mod pascal;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod scala;
pub mod swift;
pub mod typescript;

pub use apex::ApexExtractor;
pub use bash::BashExtractor;
pub use c_cpp::{CExtractor, CppExtractor};
pub use csharp::CsharpExtractor;
pub use dart::DartExtractor;
pub use go::GoExtractor;
pub use java::JavaExtractor;
pub use javascript::JavascriptExtractor;
pub use kotlin::KotlinExtractor;
pub use lua::LuaExtractor;
pub use luau::LuauExtractor;
pub use objc::ObjcExtractor;
pub use pascal::PascalExtractor;
pub use php::PhpExtractor;
pub use python::PythonExtractor;
pub use ruby::RubyExtractor;
pub use rust::RustExtractor;
pub use scala::ScalaExtractor;
pub use swift::SwiftExtractor;
pub use typescript::TypescriptExtractor;

use crate::extraction::tree_sitter_types::{LanguageExtractor, SyntaxNode};
use crate::types::Language;

/// Collect a node's named children (TS `node.namedChildren`).
pub(crate) fn named_children<'t>(node: SyntaxNode<'t>) -> Vec<SyntaxNode<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// First named child with the given kind
/// (TS `node.namedChildren.find(c => c.type === kind)`).
pub(crate) fn find_named_child<'t>(node: SyntaxNode<'t>, kind: &str) -> Option<SyntaxNode<'t>> {
    named_children(node).into_iter().find(|c| c.kind() == kind)
}

/// The TS `EXTRACTORS` map: language → per-language extraction config.
/// Languages without an entry (svelte, vue, liquid, yaml, …) return `None`.
pub fn extractor_for(language: Language) -> Option<&'static dyn LanguageExtractor> {
    match language {
        Language::Typescript | Language::Tsx => Some(&TypescriptExtractor),
        Language::Javascript | Language::Jsx => Some(&JavascriptExtractor),
        Language::Python => Some(&PythonExtractor),
        Language::Go => Some(&GoExtractor),
        Language::Rust => Some(&RustExtractor),
        Language::Java => Some(&JavaExtractor),
        Language::C => Some(&CExtractor),
        Language::Cpp => Some(&CppExtractor),
        Language::Csharp => Some(&CsharpExtractor),
        Language::Php => Some(&PhpExtractor),
        Language::Ruby => Some(&RubyExtractor),
        Language::Swift => Some(&SwiftExtractor),
        Language::Kotlin => Some(&KotlinExtractor),
        Language::Dart => Some(&DartExtractor),
        Language::Pascal => Some(&PascalExtractor),
        Language::Scala => Some(&ScalaExtractor),
        Language::Lua => Some(&LuaExtractor),
        Language::Luau => Some(&LuauExtractor),
        Language::Objc => Some(&ObjcExtractor),
        Language::Apex => Some(&ApexExtractor),
        Language::Bash => Some(&BashExtractor),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LANGUAGES;

    #[test]
    fn extractor_map_matches_ts_extractors() {
        // The 21 keys of the TS EXTRACTORS map, plus Rust-native Apex.
        let mapped: Vec<Language> = LANGUAGES
            .iter()
            .copied()
            .filter(|l| extractor_for(*l).is_some())
            .collect();
        assert_eq!(
            mapped,
            vec![
                Language::Typescript,
                Language::Javascript,
                Language::Tsx,
                Language::Jsx,
                Language::Python,
                Language::Go,
                Language::Rust,
                Language::Java,
                Language::C,
                Language::Cpp,
                Language::Csharp,
                Language::Php,
                Language::Ruby,
                Language::Swift,
                Language::Kotlin,
                Language::Dart,
                Language::Pascal,
                Language::Scala,
                Language::Lua,
                Language::Luau,
                Language::Objc,
                Language::Apex,
                Language::Bash,
            ]
        );
        // tsx/jsx share the typescript/javascript extractor configs.
        assert_eq!(
            extractor_for(Language::Tsx).unwrap().method_types(),
            extractor_for(Language::Typescript).unwrap().method_types()
        );
        assert_eq!(
            extractor_for(Language::Jsx).unwrap().method_types(),
            extractor_for(Language::Javascript).unwrap().method_types()
        );
        // No extractor for non-tree-sitter / file-level-only languages.
        for lang in [
            Language::Svelte,
            Language::Vue,
            Language::Liquid,
            Language::Html,
            Language::Visualforce,
            Language::Aura,
            Language::Yaml,
            Language::Twig,
            Language::Xml,
            Language::Properties,
            Language::Unknown,
        ] {
            assert!(
                extractor_for(lang).is_none(),
                "{lang:?} should have no extractor"
            );
        }
    }
}
