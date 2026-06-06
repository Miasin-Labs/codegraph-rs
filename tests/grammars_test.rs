//! Grammar-loading smoke test: every Language in the native grammar registry
//! must construct a parser and parse a hello-world snippet without errors.
//!
//! This is the native-port equivalent of the TS wasm-grammar loading checks —
//! it guards against grammar-crate ABI mismatches (`set_language` failures)
//! and broken `LanguageFn` exports, which would otherwise only surface as
//! silent `parser_error` extraction results at runtime.

use codegraph::extraction::grammars::{
    create_parser,
    get_supported_languages,
    grammar_language,
    has_grammar,
    is_language_supported,
};
use codegraph::types::Language;

/// One minimal, syntactically valid snippet per grammar language.
fn hello_world_snippet(language: Language) -> &'static str {
    match language {
        Language::Typescript => {
            "const x: number = 1;\nfunction f(a: string): number { return x; }\n"
        }
        Language::Tsx => "const el = <div className=\"a\">hi</div>;\n",
        Language::Javascript => "function f() { return 1; }\nf();\n",
        Language::Jsx => "function App() { return <div>hi</div>; }\n",
        Language::Python => "def f():\n    return 1\n",
        Language::Go => "package main\n\nfunc main() {\n}\n",
        Language::Rust => "fn main() { let x = 1; }\n",
        Language::Java => "class A { void m() { } }\n",
        Language::C => "int main(void) { return 0; }\n",
        Language::Cpp => "namespace n { class A { public: int x; }; }\n",
        Language::Csharp => "class A { void M() { } }\n",
        Language::Php => "<?php\nfunction f() { return 1; }\n",
        Language::Ruby => "def f\n  1\nend\n",
        Language::Swift => "func f() -> Int { return 1 }\n",
        Language::Kotlin => "fun f(): Int = 1\n",
        Language::Dart => "int f() {\n  return 1;\n}\n",
        Language::Pascal => "program Hello;\nbegin\nend.\n",
        Language::Scala => "object A { def f: Int = 1 }\n",
        Language::Lua => "local function f() return 1 end\n",
        Language::Luau => "local function f(): number return 1 end\n",
        Language::Objc => "@interface Foo : NSObject\n@end\n@implementation Foo\n@end\n",
        _ => panic!("no snippet for non-grammar language {language}"),
    }
}

const GRAMMAR_LANGUAGES: &[Language] = &[
    Language::Typescript,
    Language::Tsx,
    Language::Javascript,
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
];

#[test]
fn every_grammar_language_parses_a_hello_world_snippet() {
    for &language in GRAMMAR_LANGUAGES {
        assert!(
            has_grammar(language),
            "{language} should be registered as a grammar language"
        );
        let lang = grammar_language(language);
        assert!(lang.is_some(), "{language} grammar should load");

        let mut parser = create_parser(language)
            .unwrap_or_else(|| panic!("{language}: create_parser returned None (ABI mismatch?)"));

        let snippet = hello_world_snippet(language);
        let tree = parser
            .parse(snippet, None)
            .unwrap_or_else(|| panic!("{language}: parse returned no tree"));
        let root = tree.root_node();

        assert!(
            root.named_child_count() > 0,
            "{language}: expected a non-empty parse tree for snippet {snippet:?}, got {}",
            root.to_sexp()
        );
        assert!(
            !root.has_error(),
            "{language}: snippet should parse without errors; tree: {}",
            root.to_sexp()
        );
    }
}

#[test]
fn non_grammar_languages_have_no_parser() {
    for language in [
        Language::Svelte,
        Language::Vue,
        Language::Liquid,
        Language::Yaml,
        Language::Twig,
        Language::Xml,
        Language::Properties,
        Language::Unknown,
    ] {
        assert!(!has_grammar(language), "{language} should have no grammar");
        assert!(
            grammar_language(language).is_none(),
            "{language} should not yield a tree-sitter Language"
        );
        assert!(
            create_parser(language).is_none(),
            "{language} should not yield a parser"
        );
    }
}

#[test]
fn supported_languages_cover_all_grammars_plus_custom_extractors() {
    let supported = get_supported_languages();
    for &language in GRAMMAR_LANGUAGES {
        assert!(
            supported.contains(&language),
            "{language} missing from get_supported_languages()"
        );
        assert!(is_language_supported(language));
    }
    for language in [Language::Svelte, Language::Vue, Language::Liquid] {
        assert!(supported.contains(&language));
    }
    assert!(!supported.contains(&Language::Unknown));
}
