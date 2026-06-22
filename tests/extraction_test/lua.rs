use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Lua Extraction')
// =============================================================================

#[test]
fn lua_detects_lua_files() {
    assert_eq!(detect_language("init.lua", None), Language::Lua);
    assert_eq!(detect_language("src/util.lua", None), Language::Lua);
}

#[test]
fn lua_is_reported_as_supported() {
    assert!(is_language_supported(Language::Lua));
    assert!(get_supported_languages().contains(&Language::Lua));
}

#[test]
fn lua_extracts_global_and_local_functions() {
    let code = "
function configure(opts) return opts end
local function helper(x) return x * 2 end
";
    let result = extract("init.lua", code);
    let funcs = names(&filter_kind(&result, NodeKind::Function));
    assert!(funcs.contains(&"configure".to_string()));
    assert!(funcs.contains(&"helper".to_string()));
    let configure = result
        .nodes
        .iter()
        .find(|n| n.name == "configure")
        .expect("configure");
    assert_eq!(configure.language, Language::Lua);
    assert_eq!(configure.signature.as_deref(), Some("(opts)"));
}

#[test]
fn lua_splits_table_method_functions_into_a_receiver_and_method_name() {
    let code = "
function M.connect(host, port) return host end
function M:send(data) return self end
";
    let result = extract("init.lua", code);
    let methods = filter_kind(&result, NodeKind::Method);
    let connect = methods
        .iter()
        .find(|m| m.name == "connect")
        .expect("connect");
    assert_eq!(connect.qualified_name, "M::connect");
    let send = methods.iter().find(|m| m.name == "send").expect("send");
    assert_eq!(send.qualified_name, "M::send");
}

#[test]
fn lua_extracts_local_variable_declarations() {
    let code = "
local M = {}
local count = 0
";
    let result = extract("mod.lua", code);
    let vars = names(&filter_kind(&result, NodeKind::Variable));
    assert!(vars.contains(&"M".to_string()));
    assert!(vars.contains(&"count".to_string()));
}

#[test]
fn lua_extracts_require_in_local_declarations_and_bare_calls() {
    let code = "
local socket = require(\"socket\")
local http = require \"resty.http\"
require(\"side.effect\")
";
    let result = extract("net.lua", code);
    let imports = names(&import_nodes(&result));
    assert!(imports.contains(&"socket".to_string()));
    assert!(imports.contains(&"resty.http".to_string()));
    assert!(imports.contains(&"side.effect".to_string()));

    assert!(find_ref(&result, EdgeKind::Imports, "socket").is_some());
}

#[test]
fn lua_keeps_extracting_require_across_many_sequential_parses() {
    // Regression guard ported from TS (the WASM-heap-corruption scenario can't
    // happen natively, but the loop is kept as a parity check).
    let mut last = None;
    for i in 0..8 {
        last = Some(extract(
            &format!("f{i}.lua"),
            &format!("local m = require(\"module.{i}\")\nreturn m\n"),
        ));
    }
    let last = last.unwrap();
    let imports = names(&import_nodes(&last));
    assert!(imports.contains(&"module.7".to_string()));
}

#[test]
fn lua_records_intra_file_calls_as_resolvable_references() {
    let code = "
local function helper(x) return x end
local function run(y) return helper(y) end
";
    let result = extract("calls.lua", code);
    assert!(find_ref(&result, EdgeKind::Calls, "helper").is_some());
}

// =============================================================================
