//! Integration smoke tests for the per-language extraction configs
//! (`src/extraction/languages/`), driven end-to-end through the public
//! `TreeSitterExtractor` API. Mirrors the per-file `#[cfg(test)]` smoke
//! tests but exercises the crate surface the orchestrator wiring will use
//! (`languages::extractor_for`).

use codegraph::extraction::languages::extractor_for;
use codegraph::extraction::tree_sitter_wrapper::TreeSitterExtractor;
use codegraph::types::{EdgeKind, Language, Node, NodeKind};

fn extract(file_path: &str, source: &str, language: Language) -> Vec<Node> {
    let extractor = extractor_for(language).expect("language has an extractor");
    let result =
        TreeSitterExtractor::new(file_path, source, Some(language), Some(extractor)).extract();
    assert!(
        result.errors.is_empty(),
        "{language:?} errors: {:?}",
        result.errors
    );
    result.nodes
}

fn kind_of(nodes: &[Node], name: &str) -> Option<NodeKind> {
    nodes.iter().find(|n| n.name == name).map(|n| n.kind)
}

#[test]
fn every_extractor_language_round_trips() {
    // (language, file, snippet, [(symbol, kind)])
    let cases: Vec<(Language, &str, &str, Vec<(&str, NodeKind)>)> = vec![
        (
            Language::Typescript,
            "a.ts",
            "export class A { run(): void {} }\nexport function f(): number { return 1; }\n",
            vec![
                ("A", NodeKind::Class),
                ("run", NodeKind::Method),
                ("f", NodeKind::Function),
            ],
        ),
        (
            Language::Tsx,
            "a.tsx",
            "export function App(): JSX.Element { return <div />; }\n",
            vec![("App", NodeKind::Function)],
        ),
        (
            Language::Javascript,
            "a.js",
            "class B { go() {} }\nfunction g() {}\n",
            vec![
                ("B", NodeKind::Class),
                ("go", NodeKind::Method),
                ("g", NodeKind::Function),
            ],
        ),
        (
            Language::Jsx,
            "a.jsx",
            "export function App() { return <div />; }\n",
            vec![("App", NodeKind::Function)],
        ),
        (
            Language::Python,
            "a.py",
            "class C:\n    def m(self):\n        pass\n\ndef f():\n    pass\n",
            vec![
                ("C", NodeKind::Class),
                ("m", NodeKind::Method),
                ("f", NodeKind::Function),
            ],
        ),
        (
            Language::Go,
            "a.go",
            "package p\n\ntype S struct{}\n\nfunc (s *S) M() {}\n\nfunc F() {}\n",
            vec![
                ("S", NodeKind::Struct),
                ("M", NodeKind::Method),
                ("F", NodeKind::Function),
            ],
        ),
        (
            Language::Rust,
            "a.rs",
            "pub struct S { pub x: i32 }\npub trait T { fn t(&self); }\nimpl S { pub fn m(&self) {} }\nfn f() {}\n",
            vec![
                ("S", NodeKind::Struct),
                ("T", NodeKind::Trait),
                ("f", NodeKind::Function),
            ],
        ),
        (
            Language::Java,
            "A.java",
            "public class A { void m() {} }\ninterface I {}\n",
            vec![
                ("A", NodeKind::Class),
                ("m", NodeKind::Method),
                ("I", NodeKind::Interface),
            ],
        ),
        (
            Language::C,
            "a.c",
            "struct P { int x; };\nint main(void) { return 0; }\n",
            vec![("P", NodeKind::Struct), ("main", NodeKind::Function)],
        ),
        (
            Language::Cpp,
            "a.cpp",
            "class K { public: void m(); };\nvoid f() {}\n",
            vec![("K", NodeKind::Class), ("f", NodeKind::Function)],
        ),
        (
            Language::Csharp,
            "A.cs",
            "public class A { public void M() {} }\nstruct V {}\nenum E { X }\n",
            vec![
                ("A", NodeKind::Class),
                ("M", NodeKind::Method),
                ("V", NodeKind::Struct),
                ("E", NodeKind::Enum),
            ],
        ),
        (
            Language::Php,
            "a.php",
            "<?php\nclass A { public function m() {} }\nfunction f() {}\n",
            vec![
                ("A", NodeKind::Class),
                ("m", NodeKind::Method),
                ("f", NodeKind::Function),
            ],
        ),
        (
            Language::Ruby,
            "a.rb",
            "class A\n  def m\n  end\nend\n\ndef f\nend\n",
            vec![
                ("A", NodeKind::Class),
                ("m", NodeKind::Method),
                ("f", NodeKind::Function),
            ],
        ),
        (
            Language::Swift,
            "a.swift",
            "class A {\n    func m() {}\n}\nstruct S {}\nfunc f() {}\n",
            vec![
                ("A", NodeKind::Class),
                ("m", NodeKind::Method),
                ("S", NodeKind::Struct),
                ("f", NodeKind::Function),
            ],
        ),
        (
            Language::Kotlin,
            "A.kt",
            "class A {\n    fun m() {}\n}\n\nfun f() {}\n",
            vec![
                ("A", NodeKind::Class),
                ("m", NodeKind::Method),
                ("f", NodeKind::Function),
            ],
        ),
        (
            Language::Dart,
            "a.dart",
            "class A {\n  void m() {}\n}\n\nvoid f() {}\n",
            vec![
                ("A", NodeKind::Class),
                ("m", NodeKind::Method),
                ("f", NodeKind::Function),
            ],
        ),
        (
            Language::Pascal,
            "a.pas",
            // F must be declared in the interface section: an implementation
            // `defProc` alone deliberately creates no node (TS parity — the
            // declaration is the node; the definition only contributes calls).
            "unit U;\ninterface\nprocedure F;\nimplementation\nprocedure F;\nbegin\nend;\nend.\n",
            vec![("F", NodeKind::Function)],
        ),
        (
            Language::Scala,
            "A.scala",
            "class A {\n  def m(): Unit = {}\n}\n\ntrait T\n",
            vec![
                ("A", NodeKind::Class),
                ("m", NodeKind::Method),
                ("T", NodeKind::Trait),
            ],
        ),
        (
            Language::Lua,
            "a.lua",
            "function f()\nend\n",
            vec![("f", NodeKind::Function)],
        ),
        (
            Language::Luau,
            "a.luau",
            "export type P = { x: number }\nfunction f()\nend\n",
            vec![("P", NodeKind::TypeAlias), ("f", NodeKind::Function)],
        ),
        (
            Language::Objc,
            "a.m",
            "@interface A : NSObject\n@end\n@implementation A\n- (void)m {\n}\n@end\n",
            vec![("A", NodeKind::Class), ("m", NodeKind::Method)],
        ),
    ];

    for (language, file, source, expectations) in cases {
        let nodes = extract(file, source, language);
        for (symbol, expected_kind) in expectations {
            assert_eq!(
                kind_of(&nodes, symbol),
                Some(expected_kind),
                "{language:?}: expected {symbol:?} as {expected_kind:?}; nodes: {:?}",
                nodes
                    .iter()
                    .map(|n| (n.kind, n.name.clone()))
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn import_extraction_per_language() {
    let cases: Vec<(Language, &str, &str, &str)> = vec![
        (
            Language::Typescript,
            "a.ts",
            "import { x } from './mod';\n",
            "./mod",
        ),
        (
            Language::Javascript,
            "a.js",
            "import y from './lib.js';\n",
            "./lib.js",
        ),
        (Language::Python, "a.py", "from os import path\n", "os"),
        (Language::Rust, "a.rs", "use std::fmt::Debug;\n", "std"),
        (
            Language::Java,
            "A.java",
            "import java.util.List;\n",
            "java.util.List",
        ),
        (Language::C, "a.c", "#include <stdio.h>\n", "stdio.h"),
        (Language::Cpp, "a.cpp", "#include <vector>\n", "vector"),
        (Language::Csharp, "A.cs", "using System;\n", "System"),
        (
            Language::Php,
            "a.php",
            "<?php\nuse App\\Service;\n",
            "App\\Service",
        ),
        (Language::Ruby, "a.rb", "require 'json'\n", "json"),
        (
            Language::Swift,
            "a.swift",
            "import Foundation\n",
            "Foundation",
        ),
        (
            Language::Kotlin,
            "A.kt",
            "import java.io.IOException\n",
            "java.io.IOException",
        ),
        (
            Language::Dart,
            "a.dart",
            "import 'dart:async';\n",
            "dart:async",
        ),
        (
            Language::Lua,
            "a.lua",
            "local j = require('json')\n",
            "json",
        ),
        (
            Language::Luau,
            "a.luau",
            "local s = require(script.Parent.Signal)\n",
            "Signal",
        ),
        (
            Language::Objc,
            "a.m",
            "#import <Foundation/Foundation.h>\n",
            "Foundation/Foundation.h",
        ),
    ];
    for (language, file, source, expected) in cases {
        let nodes = extract(file, source, language);
        let import = nodes.iter().find(|n| n.kind == NodeKind::Import);
        assert_eq!(
            import.map(|n| n.name.as_str()),
            Some(expected),
            "{language:?} import; nodes: {:?}",
            nodes
                .iter()
                .map(|n| (n.kind, n.name.clone()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn call_references_are_recorded() {
    let extractor = extractor_for(Language::Typescript).unwrap();
    let result = TreeSitterExtractor::new(
        "a.ts",
        "function callee() {}\nexport function caller() { callee(); }\n",
        Some(Language::Typescript),
        Some(extractor),
    )
    .extract();
    let caller = result.nodes.iter().find(|n| n.name == "caller").unwrap();
    let call = result
        .unresolved_references
        .iter()
        .find(|r| r.reference_name == "callee")
        .expect("call reference");
    assert_eq!(call.reference_kind, EdgeKind::Calls);
    assert_eq!(call.from_node_id, caller.id);
}
