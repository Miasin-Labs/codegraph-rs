//! Import-resolution integration tests.
//!
//! Ports the import-resolution-targeted cases of `__tests__/resolution.test.ts`
//! — the ones that call `resolveImportPath` / `extractImportMappings` /
//! `resolveJvmImport` / `loadCppIncludeDirs` / `clearCppIncludeDirCache`
//! directly with a stub `ResolutionContext`:
//!
//! - "Import Resolver" (4 cases)
//! - "JVM FQN Import Resolution" (9 cases)
//! - "C/C++ Import Resolution" (all direct cases)
//!
//! The full-pipeline cases in that file (everything driven through
//! `CodeGraph.init` — Integration Tests, tsconfig path aliases,
//! re-export chain following, the C/C++ `connects #include … end-to-end`
//! case) belong to the resolver-stitch agent (see
//! `notes/resolution-import.md`).

use std::collections::HashMap;
use std::fs;

use codegraph::resolution::import_resolver::{
    clear_cpp_include_dir_cache,
    extract_import_mappings,
    load_cpp_include_dirs,
    resolve_import_path,
    resolve_jvm_import,
};
use codegraph::resolution::types::{ImportMapping, ResolutionContext, ResolvedBy, UnresolvedRef};
use codegraph::types::{EdgeKind, Language, Node, NodeKind};

/// Stub context mirroring the TS test mocks (closure-built objects).
#[derive(Default)]
struct MockContext {
    /// Paths for which `file_exists` returns true.
    existing: Vec<String>,
    /// TS `fileExists: () => true` fixtures.
    all_exist: bool,
    by_qualified_name: HashMap<String, Vec<Node>>,
    cpp_include_dirs: Option<Vec<String>>,
    project_root: String,
    all_files: Vec<String>,
}

impl ResolutionContext for MockContext {
    fn get_nodes_in_file(&self, _file_path: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_name(&self, _name: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.by_qualified_name
            .get(qualified_name)
            .cloned()
            .unwrap_or_default()
    }
    fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
        Vec::new()
    }
    fn file_exists(&self, file_path: &str) -> bool {
        self.all_exist || self.existing.iter().any(|f| f == file_path)
    }
    fn read_file(&self, _file_path: &str) -> Option<String> {
        None
    }
    fn get_project_root(&self) -> &str {
        &self.project_root
    }
    fn get_all_files(&self) -> Vec<String> {
        self.all_files.clone()
    }
    fn get_nodes_by_lower_name(&self, _lower_name: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
        Vec::new()
    }
    fn get_cpp_include_dirs(&self) -> Vec<String> {
        self.cpp_include_dirs.clone().unwrap_or_default()
    }
}

// =============================================================================
// Import Resolver
// =============================================================================

#[test]
fn should_resolve_relative_import_paths() {
    let context = MockContext {
        existing: vec![
            "src/components/utils.ts".into(),
            "src/components/utils/index.ts".into(),
        ],
        all_files: vec![
            "src/components/utils.ts".into(),
            "src/components/utils/index.ts".into(),
        ],
        ..Default::default()
    };

    let result = resolve_import_path(
        "./utils",
        "src/components/Button.ts",
        Language::Typescript,
        &context,
    );

    assert_eq!(result.as_deref(), Some("src/components/utils.ts"));
}

#[test]
fn should_resolve_parent_directory_imports() {
    let context = MockContext {
        existing: vec!["src/helpers.ts".into(), "src/helpers/index.ts".into()],
        all_files: vec!["src/helpers.ts".into(), "src/helpers/index.ts".into()],
        ..Default::default()
    };

    let result = resolve_import_path(
        "../helpers",
        "src/components/Button.ts",
        Language::Typescript,
        &context,
    );

    assert_eq!(result.as_deref(), Some("src/helpers.ts"));
}

#[test]
fn should_extract_js_ts_import_mappings() {
    let content = r#"
import { foo } from './foo';
import bar from '../bar';
import * as utils from './utils';
import { baz, qux } from './baz';
"#;

    let mappings = extract_import_mappings("src/index.ts", content, Language::Typescript);

    assert!(!mappings.is_empty());
    assert!(mappings.iter().any(|m| m.local_name == "foo"));
    assert!(mappings.iter().any(|m| m.local_name == "bar"));
}

#[test]
fn should_extract_python_import_mappings() {
    let content = r#"
from utils import helper
from .models import User
import os
from ..services import auth_service
"#;

    let mappings = extract_import_mappings("src/main.py", content, Language::Python);

    assert!(!mappings.is_empty());
    assert!(mappings.iter().any(|m| m.local_name == "helper"));
    assert!(mappings.iter().any(|m| m.local_name == "User"));
}

// =============================================================================
// JVM FQN Import Resolution
// =============================================================================

/// Build a ResolutionContext stub whose getNodesByQualifiedName answers
/// from a fixed table — the only context method resolveJvmImport touches.
fn make_context(by_qname: &[(&str, Vec<Node>)]) -> MockContext {
    MockContext {
        by_qualified_name: by_qname
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        ..Default::default()
    }
}

fn node(id: &str, name: &str, qualified_name: &str, kind: NodeKind, language: Language) -> Node {
    Node::new(id, kind, name, qualified_name, "Models.kt", language, 1, 1)
}

fn kt_class(id: &str, name: &str, qualified_name: &str) -> Node {
    node(id, name, qualified_name, NodeKind::Class, Language::Kotlin)
}

fn import_ref(reference_name: &str, language: Language) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: "caller".into(),
        reference_name: reference_name.into(),
        reference_kind: EdgeKind::Imports,
        line: 1,
        column: 0,
        file_path: "Caller.kt".into(),
        language,
        candidates: None,
    }
}

#[test]
fn resolves_a_kotlin_class_import_by_fqn_regardless_of_filename() {
    let target = kt_class("n1", "Bar", "com.example.foo::Bar");
    let ctx = make_context(&[("com.example.foo::Bar", vec![target])]);
    let result = resolve_jvm_import(&import_ref("com.example.foo.Bar", Language::Kotlin), &ctx)
        .expect("resolved");
    assert_eq!(result.target_node_id, "n1");
    assert_eq!(result.resolved_by, ResolvedBy::Import);
}

#[test]
fn resolves_a_kotlin_top_level_function_import_by_fqn() {
    let util = node(
        "n2",
        "util",
        "com.example.foo::util",
        NodeKind::Function,
        Language::Kotlin,
    );
    let ctx = make_context(&[("com.example.foo::util", vec![util])]);
    let result = resolve_jvm_import(&import_ref("com.example.foo.util", Language::Kotlin), &ctx)
        .expect("resolved");
    assert_eq!(result.target_node_id, "n2");
}

#[test]
fn resolves_a_java_import_by_fqn() {
    let target = node(
        "n3",
        "Bar",
        "com.example.foo::Bar",
        NodeKind::Class,
        Language::Java,
    );
    let ctx = make_context(&[("com.example.foo::Bar", vec![target])]);
    let result = resolve_jvm_import(&import_ref("com.example.foo.Bar", Language::Java), &ctx)
        .expect("resolved");
    assert_eq!(result.target_node_id, "n3");
}

#[test]
fn resolves_cross_language_kotlin_importing_a_java_class() {
    // The Kotlin file declares `import com.example.JavaBar` — the target is
    // a Java class node. JVM interop means the resolver doesn't care about
    // the source language of the target, only that the FQN matches.
    let target = node(
        "n4",
        "JavaBar",
        "com.example::JavaBar",
        NodeKind::Class,
        Language::Java,
    );
    let ctx = make_context(&[("com.example::JavaBar", vec![target])]);
    let result = resolve_jvm_import(&import_ref("com.example.JavaBar", Language::Kotlin), &ctx)
        .expect("resolved");
    assert_eq!(result.target_node_id, "n4");
}

#[test]
fn disambiguates_a_name_collision_across_packages() {
    // Two classes named `Bar` in different packages. Each import resolves
    // to the one whose FQN matches — not to "whichever was found first".
    let bar_a = kt_class("n5a", "Bar", "com.example.alpha::Bar");
    let bar_b = kt_class("n5b", "Bar", "com.example.beta::Bar");
    let ctx = make_context(&[
        ("com.example.alpha::Bar", vec![bar_a]),
        ("com.example.beta::Bar", vec![bar_b]),
    ]);
    assert_eq!(
        resolve_jvm_import(&import_ref("com.example.alpha.Bar", Language::Kotlin), &ctx)
            .unwrap()
            .target_node_id,
        "n5a"
    );
    assert_eq!(
        resolve_jvm_import(&import_ref("com.example.beta.Bar", Language::Kotlin), &ctx)
            .unwrap()
            .target_node_id,
        "n5b"
    );
}

#[test]
fn returns_null_for_wildcard_imports() {
    let ctx = make_context(&[]);
    assert!(resolve_jvm_import(&import_ref("com.example.foo.*", Language::Kotlin), &ctx).is_none());
}

#[test]
fn returns_null_for_unqualified_names() {
    // A single-segment name has no package; nothing to look up by FQN.
    let ctx = make_context(&[("Bar", vec![kt_class("n6", "Bar", "Bar")])]);
    assert!(resolve_jvm_import(&import_ref("Bar", Language::Kotlin), &ctx).is_none());
}

#[test]
fn returns_null_for_non_jvm_languages() {
    let target = kt_class("n7", "Bar", "com.example::Bar");
    let ctx = make_context(&[("com.example::Bar", vec![target])]);
    assert!(
        resolve_jvm_import(&import_ref("com.example.Bar", Language::Typescript), &ctx).is_none()
    );
}

#[test]
fn returns_null_for_non_imports_reference_kinds() {
    // The resolver intentionally only acts on `imports` refs; ordinary
    // `calls`/`extends` refs fall through to the framework + name-matcher
    // strategies.
    let target = kt_class("n8", "Bar", "com.example::Bar");
    let ctx = make_context(&[("com.example::Bar", vec![target])]);
    let reference = UnresolvedRef {
        from_node_id: "caller".into(),
        reference_name: "com.example.Bar".into(),
        reference_kind: EdgeKind::Calls,
        line: 1,
        column: 0,
        file_path: "Caller.kt".into(),
        language: Language::Kotlin,
        candidates: None,
    };
    assert!(resolve_jvm_import(&reference, &ctx).is_none());
}

#[test]
fn returns_null_when_the_fqn_is_not_in_the_index() {
    let ctx = make_context(&[]);
    assert!(
        resolve_jvm_import(&import_ref("com.example.Unknown", Language::Kotlin), &ctx).is_none()
    );
}

// =============================================================================
// C/C++ Import Resolution
// =============================================================================

#[test]
fn should_resolve_c_include_to_header_in_same_directory() {
    let context = MockContext {
        existing: vec!["utils.h".into()],
        all_files: vec!["utils.h".into(), "main.c".into()],
        ..Default::default()
    };

    let result = resolve_import_path("utils.h", "main.c", Language::C, &context);

    assert_eq!(result.as_deref(), Some("utils.h"));
}

#[test]
fn should_resolve_cpp_include_with_hpp_extension() {
    let context = MockContext {
        existing: vec!["include/myclass.hpp".into()],
        all_files: vec!["include/myclass.hpp".into(), "src/main.cpp".into()],
        cpp_include_dirs: Some(vec!["include".into()]),
        ..Default::default()
    };

    let result = resolve_import_path("myclass.hpp", "src/main.cpp", Language::Cpp, &context);

    assert_eq!(result.as_deref(), Some("include/myclass.hpp"));
}

#[test]
fn should_resolve_include_with_subdirectory_path() {
    let context = MockContext {
        existing: vec!["utils/helpers.h".into()],
        all_files: vec!["utils/helpers.h".into(), "main.c".into()],
        ..Default::default()
    };

    let result = resolve_import_path("utils/helpers.h", "main.c", Language::C, &context);

    assert_eq!(result.as_deref(), Some("utils/helpers.h"));
}

#[test]
fn should_resolve_include_via_include_directories() {
    let context = MockContext {
        existing: vec!["include/myheader.h".into()],
        all_files: vec!["include/myheader.h".into(), "src/main.cpp".into()],
        cpp_include_dirs: Some(vec!["include".into()]),
        ..Default::default()
    };

    let result = resolve_import_path("myheader.h", "src/main.cpp", Language::Cpp, &context);

    assert_eq!(result.as_deref(), Some("include/myheader.h"));
}

#[test]
fn should_resolve_include_trying_multiple_extensions() {
    let context = MockContext {
        // myclass.h does not exist, but myclass.hpp does
        existing: vec!["include/myclass.hpp".into()],
        all_files: vec!["include/myclass.hpp".into(), "src/main.cpp".into()],
        cpp_include_dirs: Some(vec!["include".into()]),
        ..Default::default()
    };

    let result = resolve_import_path("myclass", "src/main.cpp", Language::Cpp, &context);

    assert_eq!(result.as_deref(), Some("include/myclass.hpp"));
}

#[test]
fn should_return_null_for_system_headers() {
    let context = MockContext {
        all_exist: true,
        ..Default::default()
    };

    // C standard library header
    assert!(resolve_import_path("stdio.h", "main.c", Language::C, &context).is_none());
    // C++ standard library header
    assert!(resolve_import_path("vector", "main.cpp", Language::Cpp, &context).is_none());
    // C++ C-wrapper header
    assert!(resolve_import_path("cstdio", "main.cpp", Language::Cpp, &context).is_none());
}

#[test]
fn should_return_null_for_single_component_third_party_paths_that_cannot_be_resolved() {
    let context = MockContext {
        cpp_include_dirs: Some(vec![]),
        ..Default::default()
    };

    // Third-party bare header without path — not resolvable, returns null
    let result = resolve_import_path("openssl/ssl.h", "main.cpp", Language::Cpp, &context);

    assert!(result.is_none());
}

#[test]
fn should_not_filter_project_headers_with_path_separators() {
    let context = MockContext {
        existing: vec!["mylib/utils.h".into()],
        all_files: vec!["mylib/utils.h".into()],
        ..Default::default()
    };

    // Path with separator should NOT be filtered as external
    let result = resolve_import_path("mylib/utils.h", "main.c", Language::C, &context);

    assert_eq!(result.as_deref(), Some("mylib/utils.h"));
}

#[test]
fn should_extract_c_cpp_import_mappings_from_include_directives() {
    let code = "#include <iostream>\n#include \"myheader.h\"\n#include \"utils/helpers.hpp\"";

    let mappings = extract_import_mappings("main.cpp", code, Language::Cpp);

    assert_eq!(mappings.len(), 3);
    assert_eq!(
        mappings[0],
        ImportMapping {
            local_name: "iostream".into(),
            exported_name: "*".into(),
            source: "iostream".into(),
            is_default: false,
            is_namespace: true,
            resolved_path: None,
        }
    );
    assert_eq!(
        mappings[1],
        ImportMapping {
            local_name: "myheader".into(),
            exported_name: "*".into(),
            source: "myheader.h".into(),
            is_default: false,
            is_namespace: true,
            resolved_path: None,
        }
    );
    assert_eq!(
        mappings[2],
        ImportMapping {
            local_name: "helpers".into(),
            exported_name: "*".into(),
            source: "utils/helpers.hpp".into(),
            is_default: false,
            is_namespace: true,
            resolved_path: None,
        }
    );
}

#[test]
fn should_discover_include_directories_from_compile_commands_json() {
    // Create a temp project with compile_commands.json
    let temp_project = tempfile::tempdir().unwrap();
    let root = temp_project.path();
    let compile_db = serde_json::json!([
        {
            "directory": root.to_string_lossy(),
            "command": "g++ -Iinclude -Isrc/lib -isystem /usr/include -c src/main.cpp",
            "file": "src/main.cpp",
        }
    ]);
    fs::write(
        root.join("compile_commands.json"),
        serde_json::to_string(&compile_db).unwrap(),
    )
    .unwrap();
    // Create the include dirs so they exist
    fs::create_dir_all(root.join("include")).unwrap();
    fs::create_dir_all(root.join("src").join("lib")).unwrap();

    clear_cpp_include_dir_cache();
    let dirs = load_cpp_include_dirs(root.to_str().unwrap());

    // Should find include and src/lib (relative to project root)
    // /usr/include is absolute and outside project, should be excluded
    assert!(dirs.contains(&"include".to_string()));
    assert!(dirs.contains(&"src/lib".to_string()));
    assert!(!dirs.iter().any(|d| d.contains("usr")));
}

#[test]
fn should_fall_back_to_heuristic_include_dirs_when_no_compile_commands_json() {
    let temp_project = tempfile::tempdir().unwrap();
    let root = temp_project.path();
    // Create include/ and src/ directories with headers
    fs::create_dir_all(root.join("include")).unwrap();
    fs::write(root.join("include").join("types.h"), "").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("main.cpp"), "").unwrap();
    // Create a directory without headers — should not be included
    fs::create_dir_all(root.join("docs")).unwrap();

    clear_cpp_include_dir_cache();
    let dirs = load_cpp_include_dirs(root.to_str().unwrap());

    assert!(dirs.contains(&"include".to_string()));
    assert!(dirs.contains(&"src".to_string()));
    assert!(!dirs.contains(&"docs".to_string()));
}

// Documents the cross-language `.h` behavior. Objective-C and C++ share
// the `.h` extension, so in a mixed iOS-style project an Obj-C header
// dir gets claimed as a C/C++ include dir too. That's intentional — a
// C++ file legitimately can `#include "Foo.h"` against an Obj-C header
// (Obj-C++ / .mm callers), and false-positive inclusion is far cheaper
// than missing real resolutions. The test pins this so a later
// "exclude objc dirs" refactor breaks loudly and reviewers see the
// trade-off explicitly.
#[test]
fn heuristic_claims_any_top_level_dir_containing_h_files_including_obj_c() {
    let temp_project = tempfile::tempdir().unwrap();
    let root = temp_project.path();
    // C++ side: a `cppmod` dir with a .hpp (C++-only extension)
    fs::create_dir_all(root.join("cppmod")).unwrap();
    fs::write(root.join("cppmod").join("shared.hpp"), "").unwrap();
    // Obj-C side: an `iosmod` dir with .h + .m (no .cpp/.hpp).
    fs::create_dir_all(root.join("iosmod")).unwrap();
    fs::write(root.join("iosmod").join("View.h"), "").unwrap();
    fs::write(root.join("iosmod").join("View.m"), "").unwrap();

    clear_cpp_include_dir_cache();
    let dirs = load_cpp_include_dirs(root.to_str().unwrap());

    // Both included — Obj-C dirs are intentionally allowed.
    assert!(dirs.contains(&"cppmod".to_string()));
    assert!(dirs.contains(&"iosmod".to_string()));
}

#[test]
fn load_cpp_include_dirs_is_cached_per_project_root() {
    let temp_project = tempfile::tempdir().unwrap();
    let root = temp_project.path();
    fs::create_dir_all(root.join("include")).unwrap();
    fs::write(root.join("include").join("a.h"), "").unwrap();

    clear_cpp_include_dir_cache();
    let first = load_cpp_include_dirs(root.to_str().unwrap());
    assert!(first.contains(&"include".to_string()));

    // Add a new header dir AFTER the first load: the cached answer sticks
    // until the cache is cleared (mirrors the TS per-run cache).
    fs::create_dir_all(root.join("extra")).unwrap();
    fs::write(root.join("extra").join("b.h"), "").unwrap();
    let second = load_cpp_include_dirs(root.to_str().unwrap());
    assert_eq!(first, second);

    clear_cpp_include_dir_cache();
    let third = load_cpp_include_dirs(root.to_str().unwrap());
    assert!(third.contains(&"extra".to_string()));
}

// =============================================================================
// Path-helper parity with an absolute project root (the TS direct-call
// fixtures all use projectRoot: '' — this pins the absolute-root path too)
// =============================================================================

#[test]
fn relative_import_with_absolute_project_root() {
    let temp_project = tempfile::tempdir().unwrap();
    let root = temp_project.path().to_str().unwrap().to_string();
    let context = MockContext {
        existing: vec!["src/helpers.ts".into()],
        project_root: root,
        ..Default::default()
    };
    let result = resolve_import_path(
        "../helpers",
        "src/components/Button.ts",
        Language::Typescript,
        &context,
    );
    assert_eq!(result.as_deref(), Some("src/helpers.ts"));
}

#[test]
fn relative_import_escaping_project_root_returns_none() {
    let temp_project = tempfile::tempdir().unwrap();
    let root = temp_project.path().to_str().unwrap().to_string();
    let context = MockContext {
        all_exist: false,
        project_root: root,
        ..Default::default()
    };
    let result = resolve_import_path(
        "../../../outside",
        "src/a.ts",
        Language::Typescript,
        &context,
    );
    assert!(result.is_none());
}
