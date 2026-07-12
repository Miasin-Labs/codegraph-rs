//! Grammar Loading and Language Detection
//!
//! Ported from `src/extraction/grammars.ts`. The TS implementation loads
//! WASM grammars lazily through web-tree-sitter; here every grammar is a
//! native crate linked at build time, so the whole async-init/lazy-load/
//! cache/reset machinery collapses:
//!
//! - `initGrammars` / `loadGrammarsForLanguages` / `loadAllGrammars` /
//!   `isGrammarsInitialized` — no-ops kept for call-site parity.
//! - `getParser` → [`create_parser`] (constructs a fresh `Parser`; native
//!   parser construction is cheap and `Parser` is not `Sync`, so the TS
//!   per-language parser cache is dropped).
//! - `resetParser` / `clearParserCache` — no-ops (no WASM heap to reclaim).
//! - `getUnavailableGrammarErrors` — always empty (grammars are compiled in
//!   and cannot fail to load at runtime).

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::Parser;

use crate::types::Language;

/// File extension to Language mapping.
///
/// Kept as an ordered slice (TS object-literal insertion order) plus the
/// [`language_for_extension`] lookup. Keys are lowercase extensions
/// including the leading dot.
pub const EXTENSION_MAP: &[(&str, Language)] = &[
    (".ts", Language::Typescript),
    (".tsx", Language::Tsx),
    // ESM/CJS TypeScript module extensions — parsed as TS (no JSX). (#366)
    (".mts", Language::Typescript),
    (".cts", Language::Typescript),
    // ArkTS (HarmonyOS / OpenHarmony) is a TypeScript superset with its own
    // grammar for `@Component struct` declarations and the ArkUI build DSL.
    (".ets", Language::Arkts),
    (".js", Language::Javascript),
    (".mjs", Language::Javascript),
    (".cjs", Language::Javascript),
    // SAP HANA XS Classic server-side JavaScript. (#556)
    (".xsjs", Language::Javascript),
    (".xsjslib", Language::Javascript),
    (".jsx", Language::Jsx),
    (".py", Language::Python),
    (".pyw", Language::Python),
    (".go", Language::Go),
    (".rs", Language::Rust),
    (".java", Language::Java),
    (".c", Language::C),
    (".h", Language::C), // Could also be C++, defaulting to C
    (".cpp", Language::Cpp),
    (".cc", Language::Cpp),
    (".cxx", Language::Cpp),
    (".hpp", Language::Cpp),
    (".hxx", Language::Cpp),
    (".metal", Language::Cpp),
    (".cu", Language::Cpp),
    (".cuh", Language::Cpp),
    (".cs", Language::Csharp),
    // ASP.NET Razor / Blazor markup is handled by the custom Razor extractor.
    (".cshtml", Language::Razor),
    (".razor", Language::Razor),
    (".php", Language::Php),
    // Drupal-specific PHP file extensions
    (".module", Language::Php),
    (".install", Language::Php),
    (".theme", Language::Php),
    (".inc", Language::Php),
    // YAML (used for Drupal routing files; no symbol extraction, file-level tracking only)
    (".yml", Language::Yaml),
    (".yaml", Language::Yaml),
    // Twig templates (file-level tracking only, no symbol extraction)
    (".twig", Language::Twig),
    (".rb", Language::Ruby),
    (".rake", Language::Ruby),
    (".swift", Language::Swift),
    (".kt", Language::Kotlin),
    (".kts", Language::Kotlin),
    (".dart", Language::Dart),
    (".liquid", Language::Liquid),
    (".svelte", Language::Svelte),
    (".vue", Language::Vue),
    (".astro", Language::Astro),
    (".r", Language::R),
    (".pas", Language::Pascal),
    (".dpr", Language::Pascal),
    (".dpk", Language::Pascal),
    (".lpr", Language::Pascal),
    (".dfm", Language::Pascal),
    (".fmx", Language::Pascal),
    (".scala", Language::Scala),
    (".sc", Language::Scala),
    (".lua", Language::Lua),
    (".luau", Language::Luau),
    (".m", Language::Objc),
    (".mm", Language::Objc),
    (".sol", Language::Solidity),
    (".vy", Language::Vyper),
    (".move", Language::Move),
    (".cairo", Language::Cairo),
    (".sw", Language::Sway),
    (".fe", Language::Fe),
    // ColdFusion markup/components and standalone CFScript.
    (".cfc", Language::Cfml),
    (".cfm", Language::Cfml),
    (".cfs", Language::Cfscript),
    (".nix", Language::Nix),
    // COBOL programs and copybooks.
    (".cbl", Language::Cobol),
    (".cob", Language::Cobol),
    (".cobol", Language::Cobol),
    (".cpy", Language::Cobol),
    (".vb", Language::Vbnet),
    (".erl", Language::Erlang),
    (".hrl", Language::Erlang),
    (".escript", Language::Erlang),
    // Salesforce Apex: classes, triggers, and anonymous-execute scripts.
    // `.cls` is claimed for Apex unconditionally — a stray LaTeX/VB `.cls`
    // parses with errors and yields ~no symbols, which extraction tolerates.
    (".cls", Language::Apex),
    (".trigger", Language::Apex),
    (".apex", Language::Apex),
    // Salesforce markup: Visualforce pages/components and Aura bundles.
    // Custom extractors (no grammar) — controller/extension attributes and
    // `{!...}` expression bindings become references.
    (".page", Language::Visualforce),
    (".component", Language::Visualforce),
    (".cmp", Language::Aura),
    (".app", Language::Aura),
    (".evt", Language::Aura),
    // HTML: file-level tracking; LWC templates (under `lwc/`) additionally
    // emit `{binding}` references to their component JS class members.
    (".html", Language::Html),
    (".htm", Language::Html),
    // Shell scripts (tree-sitter-bash).
    (".sh", Language::Bash),
    (".bash", Language::Bash),
    // XML: file-level tracking; the MyBatis extractor matches `<mapper namespace="...">`
    // shape and emits SQL-statement nodes (other XML returns empty).
    (".xml", Language::Xml),
    // Spring config: `application.properties` / `application-*.properties`. Same
    // shape as the `.yml` variants — the YAML/properties extractor emits one node
    // per leaf key, and the Spring resolver links `@Value("${k}")` references.
    (".properties", Language::Properties),
    // Terraform, OpenTofu, and variable files share the HCL grammar.
    (".tf", Language::Terraform),
    (".tfvars", Language::Terraform),
    (".tofu", Language::Terraform),
];

/// Look up the language for a (lowercase, dot-prefixed) file extension.
pub fn language_for_extension(ext: &str) -> Option<Language> {
    EXTENSION_MAP
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, l)| *l)
}

/// Look up an extension after applying project-scoped overrides.
pub fn language_for_extension_with_overrides(
    ext: &str,
    overrides: &HashMap<String, Language>,
) -> Option<Language> {
    overrides
        .get(ext)
        .copied()
        .or_else(|| language_for_extension(ext))
}

/// Whether a file is one CodeGraph can parse, based purely on its extension.
/// This is the single source of truth for "should we index this file" — derived
/// from EXTENSION_MAP so parser support and indexing selection never drift.
pub fn is_source_file(file_path: &str) -> bool {
    is_source_file_with_overrides(file_path, &HashMap::new())
}

/// Project-aware source-file check. Custom mappings override the built-in map.
pub fn is_source_file_with_overrides(
    file_path: &str,
    overrides: &HashMap<String, Language>,
) -> bool {
    if is_play_routes_file(file_path) {
        return true; // Play `conf/routes` is extensionless
    }
    if is_erlang_app_file(file_path) {
        return true;
    }
    match file_path.rfind('.') {
        Some(dot) => {
            language_for_extension_with_overrides(&file_path[dot..].to_lowercase(), overrides)
                .is_some()
        }
        None => false,
    }
}

/// Play Framework routes file: the extensionless `conf/routes` (and included
/// `conf/*.routes`). No grammar — route extraction is done by the Play framework
/// resolver, so it's processed through the no-grammar (`yaml`-style) path.
pub fn is_play_routes_file(file_path: &str) -> bool {
    file_path == "conf/routes"
        || file_path.ends_with("/conf/routes")
        || file_path.ends_with(".routes")
}

/// OTP application resource files contain Erlang terms but use a compound
/// suffix whose final extension (`.src`) is too generic to register globally.
pub fn is_erlang_app_file(file_path: &str) -> bool {
    let lower = file_path.to_ascii_lowercase();
    lower.ends_with(".app") || lower.ends_with(".app.src")
}

/// Whether `language` has a tree-sitter grammar compiled into this binary.
/// Mirrors the TS `GrammarLanguage` type (every `Language` except the
/// custom-extractor and file-level-only ones).
pub fn has_grammar(language: Language) -> bool {
    !matches!(
        language,
        Language::Svelte
            | Language::Vue
            | Language::Astro
            | Language::Liquid
            | Language::Razor
            | Language::Html
            | Language::Visualforce
            | Language::Aura
            | Language::Yaml
            | Language::Twig
            | Language::Xml
            | Language::Properties
            | Language::Unknown
    )
}

/// Grammar-language list in the TS `WASM_GRAMMAR_FILES` key order.
const GRAMMAR_LANGUAGES: &[Language] = &[
    Language::Typescript,
    Language::Tsx,
    Language::Arkts,
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
    Language::R,
    Language::Solidity,
    Language::Vyper,
    Language::Move,
    Language::Cairo,
    Language::Sway,
    Language::Fe,
    Language::Nix,
    Language::Cfml,
    Language::Cfscript,
    Language::Cfquery,
    Language::Cobol,
    Language::Vbnet,
    Language::Erlang,
    Language::Terraform,
    Language::Apex,
    Language::Bash,
];

/// The native tree-sitter grammar for a language, or `None` when the
/// language has no grammar (custom extractors / file-level-only formats).
pub fn grammar_language(language: Language) -> Option<tree_sitter::Language> {
    match language {
        Language::Cobol => return Some(tree_sitter_cobol::language()),
        Language::Move => return Some(tree_sitter_move::language()),
        Language::Cairo => return Some(tree_sitter_cairo::language()),
        Language::Sway => return Some(tree_sitter_sway::language()),
        _ => {}
    }
    let lang_fn = match language {
        Language::Typescript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
        Language::Arkts => tree_sitter_arkts::LANGUAGE,
        // `javascript` covers `jsx` (same grammar).
        Language::Javascript | Language::Jsx => tree_sitter_javascript::LANGUAGE,
        Language::Python => tree_sitter_python::LANGUAGE,
        Language::Go => tree_sitter_go::LANGUAGE,
        Language::Rust => tree_sitter_rust::LANGUAGE,
        Language::Java => tree_sitter_java::LANGUAGE,
        Language::C => tree_sitter_c::LANGUAGE,
        Language::Cpp => tree_sitter_cpp::LANGUAGE,
        Language::Csharp => tree_sitter_c_sharp::LANGUAGE,
        // PHP grammar with embedded HTML support (matches the TS wasm build).
        Language::Php => tree_sitter_php::LANGUAGE_PHP,
        Language::Ruby => tree_sitter_ruby::LANGUAGE,
        Language::Swift => tree_sitter_swift::LANGUAGE,
        Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE,
        Language::Dart => tree_sitter_dart::LANGUAGE,
        Language::Pascal => tree_sitter_pascal::LANGUAGE,
        Language::Scala => tree_sitter_scala::LANGUAGE,
        Language::Lua => tree_sitter_lua::LANGUAGE,
        Language::Luau => tree_sitter_luau::LANGUAGE,
        Language::Objc => tree_sitter_objc::LANGUAGE,
        Language::R => tree_sitter_r::LANGUAGE,
        Language::Solidity => tree_sitter_solidity::LANGUAGE,
        Language::Vyper => tree_sitter_vyper::LANGUAGE,
        Language::Fe => tree_sitter_fe::LANGUAGE,
        Language::Nix => tree_sitter_nix::LANGUAGE,
        Language::Cfml => tree_sitter_cfml::LANGUAGE_CFML,
        Language::Cfscript => tree_sitter_cfml::LANGUAGE_CFSCRIPT,
        Language::Cfquery => tree_sitter_cfml::LANGUAGE_CFQUERY,
        Language::Move | Language::Cairo | Language::Sway | Language::Cobol => {
            unreachable!("handled before LanguageFn dispatch")
        }
        Language::Vbnet => tree_sitter_vb_dotnet::LANGUAGE,
        Language::Erlang => tree_sitter_erlang::LANGUAGE,
        Language::Terraform => tree_sitter_hcl::LANGUAGE,
        Language::Apex => tree_sitter_sfapex::apex::LANGUAGE,
        Language::Bash => tree_sitter_bash::LANGUAGE,
        Language::Svelte
        | Language::Vue
        | Language::Astro
        | Language::Liquid
        | Language::Razor
        | Language::Html
        | Language::Visualforce
        | Language::Aura
        | Language::Yaml
        | Language::Twig
        | Language::Xml
        | Language::Properties
        | Language::Unknown => return None,
    };
    Some(tree_sitter::Language::new(lang_fn))
}

/// Get a parser for the specified language (TS `getParser`).
///
/// Native deviation: returns a fresh `Parser` per call instead of a cached
/// instance — `tree_sitter::Parser` is cheap to construct and not `Sync`,
/// so the TS per-language cache (a WASM-heap optimization) is unnecessary.
pub fn create_parser(language: Language) -> Option<Parser> {
    let lang = grammar_language(language)?;
    let mut parser = Parser::new();
    parser.set_language(&lang).ok()?;
    Some(parser)
}

static CPP_HEURISTIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\bnamespace\b|\bclass\s+\w+\s*[:{]|\btemplate\s*<|\b(?:public|private|protected)\s*:|\bvirtual\b|\busing\s+(?:namespace\b|\w+\s*=)",
    )
    .expect("valid regex")
});

static OBJC_HEURISTIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@(?:interface|implementation|protocol|synthesize)\b").expect("valid regex")
});

/// First ~8KB of the source, clamped to a char boundary.
fn sample(source: &str) -> &str {
    let mut end = source.len().min(8192);
    while end < source.len() && !source.is_char_boundary(end) {
        end -= 1;
    }
    &source[..end]
}

/// Heuristic: does a .h file contain C++ constructs?
/// Checks the first ~8KB for patterns that are unique to C++ and never valid C.
fn looks_like_cpp(source: &str) -> bool {
    CPP_HEURISTIC.is_match(sample(source))
}

/// Heuristic: does a .h file contain Objective-C constructs?
fn looks_like_objc(source: &str) -> bool {
    OBJC_HEURISTIC.is_match(sample(source))
}

/// Salesforce Aura applications also use `.app`; distinguish their XML root
/// from OTP application resource terms when source is available.
fn looks_like_aura_app(source: &str) -> bool {
    sample(source)
        .to_ascii_lowercase()
        .contains("<aura:application")
}

/// Detect language from file extension
pub fn detect_language(file_path: &str, source: Option<&str>) -> Language {
    detect_language_with_overrides(file_path, source, &HashMap::new())
}

/// Detect a language with project-scoped extension mappings taking precedence.
pub fn detect_language_with_overrides(
    file_path: &str,
    source: Option<&str>,
    overrides: &HashMap<String, Language>,
) -> Language {
    // Play `conf/routes` has no grammar — route through the no-symbol path; the
    // Play framework resolver extracts route nodes from it.
    if is_play_routes_file(file_path) {
        return Language::Yaml;
    }
    if is_erlang_app_file(file_path) {
        if file_path.to_ascii_lowercase().ends_with(".app")
            && source.is_some_and(looks_like_aura_app)
        {
            return Language::Aura;
        }
        return Language::Erlang;
    }
    // TS `filePath.substring(filePath.lastIndexOf('.'))` — when there is no
    // dot, JS clamps -1 to 0, so the "extension" is the whole path (which
    // never matches the map → unknown).
    let ext = match file_path.rfind('.') {
        Some(dot) => file_path[dot..].to_lowercase(),
        None => file_path.to_lowercase(),
    };
    let lang = language_for_extension_with_overrides(&ext, overrides).unwrap_or(Language::Unknown);

    // .h files could be C, C++, or Objective-C — check source content
    if lang == Language::C && ext == ".h" {
        if let Some(source) = source {
            if looks_like_cpp(source) {
                return Language::Cpp;
            }
            if looks_like_objc(source) {
                return Language::Objc;
            }
        }
    }

    lang
}

/// Check if a language is supported (has a grammar defined).
/// Returns true if the grammar exists, even if not yet loaded.
pub fn is_language_supported(language: Language) -> bool {
    match language {
        Language::Svelte => true,      // custom extractor (script block delegation)
        Language::Vue => true,         // custom extractor (script block delegation)
        Language::Astro => true,       // custom extractor (frontmatter/script delegation)
        Language::Liquid => true,      // custom regex extractor
        Language::Razor => true,       // custom extractor (C# block delegation)
        Language::Html => true,        // file node + LWC template bindings
        Language::Visualforce => true, // custom regex extractor (controller/bindings)
        Language::Aura => true,        // custom regex extractor (controller/actions)
        Language::Yaml => true, // file-level tracking only; Drupal routing extraction via framework resolver
        Language::Twig => true, // file-level tracking only
        Language::Xml => true,  // MyBatis mapper extractor
        Language::Properties => true, // Spring config keys
        Language::Unknown => false,
        _ => has_grammar(language),
    }
}

/// Check if a grammar has been loaded and is ready for parsing.
/// Native: grammars are compiled in, so this is equivalent to having one
/// (plus the custom-extractor/file-level languages, mirroring TS).
pub fn is_grammar_loaded(language: Language) -> bool {
    match language {
        Language::Svelte | Language::Vue | Language::Astro | Language::Liquid | Language::Razor => {
            true
        }
        Language::Yaml | Language::Twig => true, // no grammar needed
        Language::Xml | Language::Properties => true, // no grammar needed
        _ => has_grammar(language),
    }
}

/// Languages tracked at the file-record level only: parsing emits zero symbol
/// nodes, but the file is still stored (and framework resolvers may add per-file
/// references later, e.g. Drupal routing yml, Spring `@Value` against
/// application.properties). This is the canonical set behind the no-symbol
/// branch of the extraction dispatcher; `xml` is intentionally excluded because
/// its MyBatis extractor emits a file node. Callers use this to count such files
/// as indexed rather than skipped, so it must stay in sync with that branch.
pub fn is_file_level_only_language(language: Language) -> bool {
    matches!(
        language,
        Language::Yaml | Language::Twig | Language::Properties
    )
}

/// Get all supported languages (those with grammar definitions).
pub fn get_supported_languages() -> Vec<Language> {
    let mut out: Vec<Language> = GRAMMAR_LANGUAGES.to_vec();
    out.extend([
        Language::Svelte,
        Language::Vue,
        Language::Astro,
        Language::Liquid,
        Language::Razor,
        Language::Html,
        Language::Visualforce,
        Language::Aura,
    ]);
    out
}

/// Initialize the tree-sitter runtime (TS `initGrammars`).
/// Native no-op — grammars are compiled in. Kept for call-site parity.
pub fn init_grammars() {}

/// Load grammars for specific languages (TS `loadGrammarsForLanguages`).
/// Native no-op — grammars are compiled in. Kept for call-site parity.
pub fn load_grammars_for_languages(_languages: &[Language]) {}

/// Load ALL grammars (TS `loadAllGrammars`). Native no-op.
pub fn load_all_grammars() {}

/// Check if grammars have been initialized. Native: always true.
pub fn is_grammars_initialized() -> bool {
    true
}

/// Reset the cached parser for a language (TS `resetParser`).
/// Native no-op — there is no parser cache or WASM heap to reclaim.
pub fn reset_parser(_language: Language) {}

/// Clear parser/grammar caches (TS `clearParserCache`). Native no-op.
pub fn clear_parser_cache() {}

/// Report grammars that failed to load (TS `getUnavailableGrammarErrors`).
/// Native: grammars are compiled in and cannot fail to load — always empty.
pub fn get_unavailable_grammar_errors() -> HashMap<Language, String> {
    HashMap::new()
}

/// Get language display name
pub fn get_language_display_name(language: Language) -> &'static str {
    match language {
        Language::Typescript => "TypeScript",
        Language::Javascript => "JavaScript",
        Language::Tsx => "TypeScript (TSX)",
        Language::Jsx => "JavaScript (JSX)",
        Language::Arkts => "ArkTS",
        Language::Python => "Python",
        Language::Go => "Go",
        Language::Rust => "Rust",
        Language::Java => "Java",
        Language::C => "C",
        Language::Cpp => "C++",
        Language::Csharp => "C#",
        Language::Razor => "Razor / Blazor",
        Language::Php => "PHP",
        Language::Ruby => "Ruby",
        Language::Swift => "Swift",
        Language::Kotlin => "Kotlin",
        Language::Dart => "Dart",
        Language::Svelte => "Svelte",
        Language::Vue => "Vue",
        Language::Astro => "Astro",
        Language::Liquid => "Liquid",
        Language::Pascal => "Pascal / Delphi",
        Language::Scala => "Scala",
        Language::Lua => "Lua",
        Language::Luau => "Luau",
        Language::Objc => "Objective-C",
        Language::R => "R",
        Language::Solidity => "Solidity",
        Language::Vyper => "Vyper",
        Language::Move => "Move",
        Language::Cairo => "Cairo",
        Language::Sway => "Sway",
        Language::Fe => "Fe",
        Language::Nix => "Nix",
        Language::Apex => "Apex",
        Language::Bash => "Shell (Bash)",
        Language::Html => "HTML",
        Language::Visualforce => "Visualforce",
        Language::Aura => "Aura",
        Language::Yaml => "YAML",
        Language::Twig => "Twig",
        Language::Xml => "XML",
        Language::Properties => "Java properties",
        Language::Cfml => "CFML",
        Language::Cfscript => "CFScript",
        Language::Cfquery => "CFQuery (SQL)",
        Language::Cobol => "COBOL",
        Language::Vbnet => "Visual Basic .NET",
        Language::Erlang => "Erlang",
        Language::Terraform => "Terraform / OpenTofu",
        Language::Unknown => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_language_from_extension() {
        assert_eq!(detect_language("src/app.ts", None), Language::Typescript);
        assert_eq!(detect_language("src/App.TSX", None), Language::Tsx);
        assert_eq!(detect_language("lib/util.mjs", None), Language::Javascript);
        assert_eq!(detect_language("main.go", None), Language::Go);
        assert_eq!(detect_language("noextension", None), Language::Unknown);
        assert_eq!(detect_language("conf/routes", None), Language::Yaml);
        assert_eq!(detect_language("app/conf/routes", None), Language::Yaml);
        assert_eq!(detect_language("conf/dev.routes", None), Language::Yaml);
        assert_eq!(detect_language("ebin/demo.app", None), Language::Erlang);
        assert_eq!(
            detect_language(
                "force-app/main/default/aura/Demo/Demo.app",
                Some("<aura:application extends=\"force:slds\" />")
            ),
            Language::Aura
        );
    }

    #[test]
    fn dot_h_heuristics_pick_cpp_and_objc() {
        assert_eq!(
            detect_language("foo.h", Some("namespace foo { class Bar {}; }")),
            Language::Cpp
        );
        assert_eq!(
            detect_language("foo.h", Some("@interface Foo : NSObject\n@end")),
            Language::Objc
        );
        assert_eq!(
            detect_language("foo.h", Some("int add(int a, int b);")),
            Language::C
        );
        assert_eq!(detect_language("foo.h", None), Language::C);
    }

    #[test]
    fn source_file_detection_follows_extension_map() {
        assert!(is_source_file("src/index.ts"));
        assert!(is_source_file("a/b/c.PAS"));
        assert!(is_source_file("conf/routes"));
        assert!(is_source_file("application.properties"));
        assert!(!is_source_file("README.md"));
        assert!(!is_source_file("Makefile"));
    }

    #[test]
    fn supported_and_file_level_languages() {
        assert!(is_language_supported(Language::Typescript));
        assert!(is_language_supported(Language::Svelte));
        assert!(is_language_supported(Language::Yaml));
        assert!(!is_language_supported(Language::Unknown));
        assert!(is_file_level_only_language(Language::Yaml));
        assert!(is_file_level_only_language(Language::Twig));
        assert!(is_file_level_only_language(Language::Properties));
        assert!(!is_file_level_only_language(Language::Xml));
        assert!(is_language_supported(Language::Apex));
        assert!(is_language_supported(Language::Bash));
        assert!(is_language_supported(Language::Html));
        assert!(is_language_supported(Language::Visualforce));
        assert!(is_language_supported(Language::Aura));
        assert_eq!(get_supported_languages().len(), 47);
    }
}
