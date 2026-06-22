use crate::extraction_test::fixture::*;

// describe('Luau Extraction')
// =============================================================================

#[test]
fn luau_detects_luau_files() {
    assert_eq!(detect_language("init.luau", None), Language::Luau);
    assert_eq!(detect_language("src/Client.luau", None), Language::Luau);
}

#[test]
fn luau_is_reported_as_supported() {
    assert!(is_language_supported(Language::Luau));
    assert!(get_supported_languages().contains(&Language::Luau));
}

#[test]
fn luau_extracts_type_and_export_type_definitions() {
    let code = "
export type Vector = { x: number, y: number }
type Handler = (msg: string) -> boolean
";
    let result = extract("types.luau", code);
    let aliases = filter_kind(&result, NodeKind::TypeAlias);
    let vector = aliases.iter().find(|a| a.name == "Vector").expect("Vector");
    assert_eq!(vector.is_exported, Some(true));
    let handler = aliases
        .iter()
        .find(|a| a.name == "Handler")
        .expect("Handler");
    assert_eq!(handler.is_exported, Some(false));
}

#[test]
fn luau_captures_typed_signatures_and_splits_methods_by_receiver() {
    let code = "
function configure(opts: { debug: boolean }): boolean
\treturn opts.debug
end
function Client:fetch(path: string): Response
\treturn path
end
";
    let result = extract("client.luau", code);
    let configure = find_named(&result, NodeKind::Function, "configure").expect("configure");
    assert_eq!(configure.language, Language::Luau);
    assert_eq!(
        configure.signature.as_deref(),
        Some("(opts: { debug: boolean }): boolean")
    );
    let fetch = find_named(&result, NodeKind::Method, "fetch").expect("fetch");
    assert_eq!(fetch.qualified_name, "Client::fetch");
}

#[test]
fn luau_extracts_string_and_roblox_instance_path_require_imports() {
    let code = "
local http = require(\"http\")
local Signal = require(script.Parent.Signal)
local count = 0
";
    let result = extract("mod.luau", code);
    let imports = names(&import_nodes(&result));
    assert!(imports.contains(&"http".to_string())); // string require
    assert!(imports.contains(&"Signal".to_string())); // Roblox instance-path require
    let vars = names(&filter_kind(&result, NodeKind::Variable));
    assert!(vars.contains(&"count".to_string()));
}

// =============================================================================
