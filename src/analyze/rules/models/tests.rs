//! The importer on sample MaD files of every language, the model files'
//! round trip, call matching per language and tier, and one taint rule per
//! language family driven only by the vendored models.

use std::path::PathBuf;

use tree_sitter::Node;

use super::mad::{ImportOptions, import_dir};
use super::matcher::facts::FileFacts;
use super::matcher::{CallFacts, FileContext, Tier};
use super::{LanguageModels, Model, ModelDb, ModelLanguage, Pos, Role, parse_models, write_models};
use crate::analyze::rules::check::check_rules;
use crate::analyze::rules::compile::RuleSet;
use crate::extraction::create_parser;
use crate::types::Language;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/codeql_models_fixture")
}

fn find<'m>(models: &'m [Model], role: Role, name: &str) -> Vec<&'m Model> {
    models
        .iter()
        .filter(|m| m.role == role && m.name == name)
        .collect()
}

#[test]
fn java_tuples_import_with_their_drops_counted() {
    let import = import_dir(&fixture(), &ImportOptions::default()).expect("imports");
    let java = &import.models[&ModelLanguage::Java];
    let source = find(java, Role::Source, "getParameter");
    assert_eq!(source.len(), 1);
    assert_eq!(source[0].namespace, "javax.servlet");
    assert_eq!(source[0].type_name, "ServletRequest");
    assert!(source[0].subtypes);
    assert_eq!(source[0].arity, Some(1));
    assert_eq!(source[0].output, Some(Pos::Ret));
    assert_eq!(source[0].kind, "remote");
    let sink = find(java, Role::Sink, "executeQuery");
    assert_eq!(sink[0].input, Some(Pos::arg(0)));
    // `Argument[0..1]`: one model per argument.
    assert_eq!(find(java, Role::Sink, "File").len(), 2);
    // `path-injection[read]`: the base kind, marked approximate.
    let read = find(java, Role::Sink, "newInputStream");
    assert_eq!(read[0].kind, "path-injection");
    assert!(read[0].approximate);
    // `Argument[this]` → `Argument[0]`: the stream fills the buffer.
    let summary = find(java, Role::Summary, "read");
    assert_eq!(summary[0].input, Some(Pos::Recv));
    assert_eq!(summary[0].output, Some(Pos::arg(0)));
    // `Argument[this].MapValue` collapses onto the receiver.
    let get = find(java, Role::Summary, "get");
    assert_eq!(get[0].input, Some(Pos::Recv));
    assert!(get[0].approximate);
    assert_eq!(find(java, Role::Neutral, "length").len(), 1);
    assert!(find(java, Role::Neutral, "trim").is_empty());
    let barrier = find(java, Role::Barrier, "encodeForSQL");
    assert_eq!(barrier[0].output, Some(Pos::Ret));
    let guard = find(java, Role::Guard, "isAbsolute");
    assert_eq!(guard[0].input, Some(Pos::Recv));
    assert_eq!(guard[0].accepting, Some(false));
    // Generated summaries stay out by default; test dirs never count.
    assert!(find(java, Role::Summary, "toString").is_empty());
    assert!(java.iter().all(|m| m.namespace != "test"));
    let dropped = |reason: &str| {
        import
            .dropped
            .iter()
            .filter(|((lang, r), _)| lang == "java" && r.contains(reason))
            .map(|(_, n)| *n)
            .sum::<usize>()
    };
    assert_eq!(dropped("overriding method"), 1);
    assert_eq!(dropped("`ext`"), 1);
    assert_eq!(dropped("callback's"), 1);
    assert_eq!(dropped("generated"), 1);
    assert_eq!(dropped("neutral of kind `sink`"), 1);
    assert_eq!(dropped("experimentalSinkModel"), 1);
    let with_generated = import_dir(&fixture(), &ImportOptions { generated: true }).unwrap();
    let java = &with_generated.models[&ModelLanguage::Java];
    assert!(find(java, Role::Summary, "toString")[0].generated);
}

#[test]
fn cpp_tuples_import_indirections_as_their_argument() {
    let import = import_dir(&fixture(), &ImportOptions::default()).unwrap();
    let cpp = &import.models[&ModelLanguage::Cpp];
    assert_eq!(find(cpp, Role::Source, "getc")[0].output, Some(Pos::Ret));
    assert_eq!(
        find(cpp, Role::Source, "ReadFile")[0].output,
        Some(Pos::arg(1))
    );
    assert_eq!(find(cpp, Role::Sink, "PQexec")[0].input, Some(Pos::arg(1)));
    let set_host = find(cpp, Role::Summary, "SetHost");
    assert_eq!(set_host[0].namespace, "Azure::Core");
    assert_eq!(set_host[0].type_name, "Url");
    assert_eq!(set_host[0].output, Some(Pos::Recv));
    // `Argument[@0]`: any indirection of the argument.
    assert_eq!(
        find(cpp, Role::Summary, "strdup")[0].input,
        Some(Pos::arg(0))
    );
    assert!(
        import
            .dropped
            .keys()
            .any(|(l, r)| l == "cpp" && r.contains("allocationFunctionModel"))
    );
}

#[test]
fn python_api_graph_paths_import_as_module_members() {
    let import = import_dir(&fixture(), &ImportOptions::default()).unwrap();
    let py = &import.models[&ModelLanguage::Python];
    let getenv = find(py, Role::Source, "getenv");
    assert_eq!(getenv[0].namespace, "os");
    assert_eq!(getenv[0].output, Some(Pos::Ret));
    // A member read is a source too.
    assert_eq!(find(py, Role::Source, "environ")[0].output, Some(Pos::Read));
    // `Member[execute,fetch]` names two callables of the named type.
    let execute = find(py, Role::Sink, "execute");
    assert_eq!(execute[0].type_name, "Connection");
    assert_eq!(execute[0].namespace, "asyncpg");
    assert_eq!(
        execute[0].input,
        Some(Pos::Arg {
            n: 0,
            keyword: Some("query".into())
        })
    );
    assert_eq!(find(py, Role::Sink, "fetch").len(), 1);
    // `X!` + `Subclass.Call`: the class constructed.
    let header = find(py, Role::Summary, "Header");
    assert_eq!(header[0].type_name, "Header");
    assert!(header[0].subtypes);
    assert_eq!(
        find(py, Role::Summary, "unquote")[0].namespace,
        "urllib.parse"
    );
    assert_eq!(
        find(py, Role::Guard, "url_has_allowed_host_and_scheme")[0].accepting,
        Some(true)
    );
    assert!(
        import
            .dropped
            .keys()
            .any(|(l, r)| l == "python" && r.contains("API-graph chain"))
    );
    assert!(
        import
            .dropped
            .keys()
            .any(|(l, r)| l == "python" && r.contains("typeModel"))
    );
}

#[test]
fn javascript_api_graph_paths_import() {
    let import = import_dir(&fixture(), &ImportOptions::default()).unwrap();
    let js = &import.models[&ModelLanguage::JavaScript];
    let read = find(js, Role::Source, "read");
    assert_eq!(read[0].namespace, "global.process.stdin");
    assert_eq!(find(js, Role::Sink, "exec")[0].namespace, "shelljs");
    // A module that is itself a function.
    let open: Vec<&Model> = js
        .iter()
        .filter(|m| m.role == Role::Sink && m.namespace == "open")
        .collect();
    assert_eq!(open[0].name, "");
    assert_eq!(
        find(js, Role::Sink, "sync")[0].input,
        Some(Pos::ArgsFrom(0))
    );
    assert_eq!(
        find(js, Role::Summary, "slugify")[0].namespace,
        "underscore.string"
    );
    // A promise callback's parameter is not a call position.
    assert!(find(js, Role::Source, "readFile").is_empty());
}

#[test]
fn rust_canonical_paths_import() {
    let import = import_dir(&fixture(), &ImportOptions::default()).unwrap();
    let rust = &import.models[&ModelLanguage::Rust];
    let var = find(rust, Role::Source, "var");
    assert_eq!(var[0].namespace, "std::env");
    assert!(var[0].approximate);
    let new = find(rust, Role::Sink, "new");
    assert_eq!(new[0].namespace, "std::process");
    assert_eq!(new[0].type_name, "Command");
    let from = find(rust, Role::Summary, "from");
    assert_eq!(from[0].type_name, "String");
    assert!(
        import
            .dropped
            .keys()
            .any(|(l, r)| l == "rust" && r.contains("non-path type"))
    );
    assert!(
        import
            .dropped
            .keys()
            .any(|(l, r)| l == "go" && r.contains("does not lower"))
    );
}

#[test]
fn import_reports_and_writes_attributed_files() {
    let import = import_dir(&fixture(), &ImportOptions::default()).unwrap();
    let report = import.report();
    assert!(report.contains("Dropped"));
    assert!(report.contains("Approximated"));
    let out = std::env::temp_dir().join(format!("cg-models-import-{}", std::process::id()));
    let written = super::mad::write_import(&import, &fixture(), &out).unwrap();
    assert_eq!(written.len(), 6);
    let notice = std::fs::read_to_string(out.join("NOTICE")).unwrap();
    assert!(notice.contains("MIT License"));
    let db = ModelDb::from_dir(&out).unwrap();
    assert_eq!(
        db.language(ModelLanguage::Java).unwrap().models,
        import.models[&ModelLanguage::Java]
    );
    let _ = std::fs::remove_dir_all(&out);
}

/// Every vendored model survives write → parse unchanged.
#[test]
fn model_files_round_trip() {
    let db = ModelDb::vendored();
    for language in ModelLanguage::ALL {
        let models = &db.language(language).expect("vendored").models;
        assert!(!models.is_empty(), "{language:?} has models");
        let text = write_models("preamble\nsecond line", models);
        assert_eq!(&parse_models(&text), models, "{language:?}");
    }
    assert!(super::NOTICE.contains("MIT"));
}

/// A parsed snippet and its call nodes.
struct Snippet {
    source: String,
    tree: tree_sitter::Tree,
    language: Language,
}

impl Snippet {
    fn new(language: Language, source: &str) -> Self {
        let tree = create_parser(language)
            .unwrap()
            .parse(source, None)
            .unwrap();
        Snippet {
            source: source.to_string(),
            tree,
            language,
        }
    }

    /// The first call node whose callee's last name is `name`.
    fn call(&self, name: &str) -> Node<'_> {
        let mut stack = vec![self.tree.root_node()];
        let mut found = Vec::new();
        while let Some(node) = stack.pop() {
            if let Some(shape) = super::matcher::shape::shape(node, &self.source) {
                if shape.name == name {
                    found.push(node);
                }
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        found.sort_by_key(|n| n.start_byte());
        *found.first().unwrap_or_else(|| panic!("no call {name}"))
    }

    /// How the call named `name` matches `models`: the tier and the
    /// matched models' roles.
    fn matched(
        &self,
        models: &LanguageModels,
        name: &str,
        facts: CallFacts,
    ) -> Option<(Tier, Vec<Role>)> {
        let file_facts = FileFacts::read(self.language, self.tree.root_node(), &self.source);
        let context = FileContext {
            models,
            model_language: ModelLanguage::of(self.language).unwrap(),
            language: self.language,
            source: &self.source,
            facts: &file_facts,
        };
        context
            .match_call(self.call(name), &facts)
            .map(|m| (m.tier, m.models.iter().map(|m| m.role).collect()))
    }
}

fn model(role: Role, namespace: &str, type_name: &str, name: &str, pos: Pos) -> Model {
    let (input, output) = match role {
        Role::Source | Role::Barrier => (None, Some(pos)),
        _ => (Some(pos), None),
    };
    Model {
        role,
        namespace: namespace.into(),
        type_name: type_name.into(),
        subtypes: true,
        name: name.into(),
        arity: None,
        input,
        output,
        kind: "k".into(),
        accepting: None,
        generated: false,
        approximate: false,
    }
}

fn library() -> CallFacts {
    CallFacts::default()
}

#[test]
fn java_calls_match_by_type_import_supertype_and_name() {
    let models = LanguageModels::new(vec![
        model(
            Role::Sink,
            "java.sql",
            "Statement",
            "executeQuery",
            Pos::arg(0),
        ),
        model(
            Role::Source,
            "javax.servlet",
            "ServletRequest",
            "getParameter",
            Pos::Ret,
        ),
        model(
            Role::Sink,
            "java.sql",
            "DriverManager",
            "getConnection",
            Pos::arg(0),
        ),
        model(Role::Sink, "java.io", "File", "File", Pos::arg(0)),
        model(Role::Sink, "java.lang", "Runtime", "exec", Pos::arg(0)),
        model(Role::Sink, "org.other", "Statement", "execute", Pos::arg(0)),
        model(Role::Sink, "java.sql", "Statement", "execute", Pos::arg(0)),
    ]);
    let code = r#"
import java.sql.*;
import java.io.File;
import javax.servlet.http.HttpServletRequest;
class A {
  private Statement field;
  void f(HttpServletRequest request, Statement st, java.util.List<String> list) throws Exception {
    String p = request.getParameter("id");
    st.executeQuery(p);
    this.field.execute(p);
    DriverManager.getConnection(p);
    new File(p);
    Runtime.getRuntime().exec(p);
    list.executeQuery(p);
    other().execute(p);
  }
}"#;
    let s = Snippet::new(Language::Java, code);
    assert_eq!(
        s.matched(&models, "executeQuery", library()),
        Some((Tier::Typed, vec![Role::Sink]))
    );
    // `HttpServletRequest` by name is a `javax.servlet.ServletRequest`.
    assert_eq!(
        s.matched(&models, "getParameter", library()),
        Some((Tier::Typed, vec![Role::Source]))
    );
    // A field's declared type, wildcard-imported.
    assert_eq!(
        s.matched(&models, "execute", library()).map(|m| m.0),
        Some(Tier::Typed)
    );
    assert_eq!(
        s.matched(&models, "getConnection", library()).map(|m| m.0),
        Some(Tier::Imported)
    );
    assert_eq!(
        s.matched(&models, "File", library()).map(|m| m.0),
        Some(Tier::Typed)
    );
    // `exec` on a call's result: only the name, and one type has it.
    assert_eq!(
        s.matched(&models, "exec", library()).map(|m| m.0),
        Some(Tier::Name)
    );
    // Code the index resolves into the project runs that code.
    assert_eq!(
        s.matched(
            &models,
            "executeQuery",
            CallFacts {
                in_project: true,
                ..Default::default()
            }
        ),
        None
    );
}

#[test]
fn java_declared_type_that_is_not_the_model_type_does_not_match() {
    let models = LanguageModels::new(vec![model(
        Role::Sink,
        "java.sql",
        "Statement",
        "executeQuery",
        Pos::arg(0),
    )]);
    let s = Snippet::new(
        Language::Java,
        "import java.util.List;\nclass A { void f(List<String> list, String p) { list.executeQuery(p); } }",
    );
    // Typed as something else: no fallback to the name.
    assert_eq!(s.matched(&models, "executeQuery", library()), None);
}

#[test]
fn python_calls_match_through_imports() {
    let models = LanguageModels::new(vec![
        model(Role::Sink, "os", "", "system", Pos::arg(0)),
        model(Role::Source, "os", "", "getenv", Pos::Ret),
        model(Role::Source, "builtins", "", "input", Pos::Ret),
        model(Role::Sink, "zipfile", "ZipFile", "extractall", Pos::arg(0)),
        model(Role::Summary, "urllib.parse", "", "unquote", Pos::arg(0)),
    ]);
    let code = "import os as o\nfrom os import getenv\nfrom urllib import parse\nimport zipfile\n\ndef f(p):\n    o.system(p)\n    getenv('x')\n    input()\n    z = zipfile.ZipFile(p)\n    z.extractall(p)\n    parse.unquote(p)\n";
    let s = Snippet::new(Language::Python, code);
    assert_eq!(
        s.matched(&models, "system", library()).map(|m| m.0),
        Some(Tier::Imported)
    );
    assert_eq!(
        s.matched(&models, "getenv", library()).map(|m| m.0),
        Some(Tier::Imported)
    );
    assert_eq!(
        s.matched(&models, "input", library()).map(|m| m.0),
        Some(Tier::Imported)
    );
    assert_eq!(
        s.matched(&models, "extractall", library()).map(|m| m.0),
        Some(Tier::Typed)
    );
    assert_eq!(
        s.matched(&models, "unquote", library()).map(|m| m.0),
        Some(Tier::Imported)
    );
    // A project function named `input` is not the builtin.
    assert_eq!(
        s.matched(
            &models,
            "input",
            CallFacts {
                defined_here: true,
                ..Default::default()
            }
        ),
        None
    );
}

#[test]
fn javascript_calls_match_through_require_import_and_globals() {
    let mut open = model(Role::Sink, "open", "", "", Pos::arg(0));
    open.subtypes = false;
    let models = LanguageModels::new(vec![
        model(Role::Sink, "shelljs", "", "exec", Pos::arg(0)),
        model(Role::Source, "global.process.stdin", "", "read", Pos::Ret),
        open,
        model(Role::Sink, "child_process", "", "execSync", Pos::arg(0)),
    ]);
    let code = "const sh = require('shelljs');\nconst { execSync } = require('child_process');\nimport open from 'open';\nfunction f(x) {\n  sh.exec(x);\n  process.stdin.read();\n  open(x);\n  execSync(x);\n}\n";
    let s = Snippet::new(Language::Javascript, code);
    for name in ["exec", "read", "open", "execSync"] {
        assert_eq!(
            s.matched(&models, name, library()).map(|m| m.0),
            Some(Tier::Imported),
            "{name}"
        );
    }
}

#[test]
fn rust_calls_match_by_resolution_use_and_declared_type() {
    let models = LanguageModels::new(vec![
        model(Role::Sink, "std::process", "Command", "new", Pos::arg(0)),
        model(Role::Sink, "std::process", "Command", "arg", Pos::arg(0)),
        model(Role::Source, "std::env", "", "var", Pos::Ret),
        model(
            Role::Summary,
            "alloc::string",
            "String",
            "push_str",
            Pos::arg(0),
        ),
    ]);
    let code = "use std::process::Command;\nuse std::env;\nfn f(s: &mut String) {\n    let v = env::var(\"X\").unwrap();\n    let mut c = Command::new(&v);\n    c.arg(&v);\n    s.push_str(&v);\n}\n";
    let s = Snippet::new(Language::Rust, code);
    assert_eq!(
        s.matched(&models, "var", library()).map(|m| m.0),
        Some(Tier::Imported)
    );
    assert_eq!(
        s.matched(&models, "new", library()).map(|m| m.0),
        Some(Tier::Imported)
    );
    // `let mut c = Command::new(…)` types `c`.
    assert_eq!(
        s.matched(&models, "arg", library()).map(|m| m.0),
        Some(Tier::Typed)
    );
    // `String` is the prelude's: alloc's model, by its simple name.
    assert_eq!(
        s.matched(&models, "push_str", library()).map(|m| m.0),
        Some(Tier::Typed)
    );
    // What the index resolved (the std graph's names) comes first.
    assert_eq!(
        s.matched(
            &models,
            "new",
            CallFacts {
                names: vec!["std::process::Command::new".into()],
                ..Default::default()
            }
        )
        .map(|m| m.0),
        Some(Tier::Resolved)
    );
}

#[test]
fn c_free_functions_match_unless_the_project_defines_them() {
    let mut pq = model(Role::Sink, "", "", "PQexec", Pos::arg(1));
    pq.subtypes = false;
    let models = LanguageModels::new(vec![pq]);
    let s = Snippet::new(Language::C, "void f(void *c, char *q) { PQexec(c, q); }");
    assert_eq!(
        s.matched(&models, "PQexec", library()).map(|m| m.0),
        Some(Tier::Imported)
    );
    assert_eq!(
        s.matched(
            &models,
            "PQexec",
            CallFacts {
                defined_here: true,
                ..Default::default()
            }
        ),
        None
    );
}

/// `yaml` (one rule) passes its examples, run against the vendored
/// models only (no query patterns).
fn passes(yaml: &str) {
    let set = RuleSet::load(&[], &[("t.yaml".to_string(), yaml.to_string())], false);
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    let report = check_rules(&set);
    let failures: Vec<String> = report.rules[0]
        .examples
        .iter()
        .filter(|e| !e.passed)
        .map(|e| e.explain())
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn java_taint_from_models_alone_with_a_library_summary() {
    passes(
        r#"
- id: t
  language: java
  taint:
    sources: [{model: remote}]
    sinks: [{model: sql}]
  examples:
    bad:
      - |
        import java.sql.Statement;
        import javax.servlet.http.HttpServletRequest;
        class A { void f(HttpServletRequest request, Statement st) throws Exception {
          st.executeQuery("SELECT * FROM t WHERE id='" + request.getParameter("id") + "'");
        } }
      # `InputStream.read(byte[])` fills its argument: a MaD summary.
      - |
        import java.io.InputStream;
        import java.sql.Statement;
        import javax.servlet.http.HttpServletRequest;
        class A { void f(HttpServletRequest request, Statement st) throws Exception {
          InputStream in = request.getInputStream();
          byte[] buf = new byte[64];
          in.read(buf);
          st.executeQuery(new String(buf));
        } }
    good:
      # `length()` carries nothing (the propagation table).
      - |
        import java.sql.Statement;
        import javax.servlet.http.HttpServletRequest;
        class A { void f(HttpServletRequest request, Statement st) throws Exception {
          String p = request.getParameter("id");
          st.executeQuery("SELECT * FROM t LIMIT " + p.length());
        } }
      # The statement's own type decides: a List's method is no sink.
      - |
        import java.util.List;
        import javax.servlet.http.HttpServletRequest;
        class A { void f(HttpServletRequest request, List<String> st) {
          st.add(request.getParameter("id"));
        } }
"#,
    );
}

#[test]
fn python_taint_from_models_alone() {
    passes(
        r#"
- id: t
  language: python
  taint:
    sources: [{model: environment}]
    sinks: [{model: sql-injection}]
  examples:
    bad:
      - |
        import os
        import asyncpg
        async def f(conn: asyncpg.Connection):
            name = os.getenv("NAME")
            conn = asyncpg.Connection()
            await conn.execute("SELECT * FROM t WHERE n = '%s'" % name)
      - |
        import os
        import asyncpg
        def f():
            conn = asyncpg.Connection()
            conn.fetch("SELECT " + os.environ["X"])
    good:
      - |
        import os
        import asyncpg
        async def f():
            conn = asyncpg.Connection()
            await conn.execute("SELECT * FROM t WHERE n = $1", "fixed")
"#,
    );
}

#[test]
fn javascript_taint_from_models_alone() {
    passes(
        r#"
- id: t
  language: javascript
  taint:
    sources: [{model: stdin}]
    sinks: [{model: path}]
  examples:
    bad:
      # `mkdirp` is a module called as a function.
      - |
        const mkdirp = require('mkdirp');
        function f() {
          const input = process.stdin.read();
          mkdirp('/srv/' + input);
        }
      - |
        import { sync } from 'rimraf';
        function f() {
          sync(process.stdin.read());
        }
    good:
      - |
        const mkdirp = require('mkdirp');
        function f() {
          const input = process.stdin.read();
          mkdirp('/srv/tmp');
        }
"#,
    );
}

#[test]
fn c_taint_from_models_alone() {
    passes(
        r#"
- id: t
  language: [c, cpp]
  taint:
    sources: [{model: local}]
    sinks: [{model: sql}]
  examples:
    bad:
      - |
        void f(void *file, void *conn) {
            char buf[256];
            ReadFile(file, buf, 255, 0, 0);
            PQexec(conn, buf);
        }
    good:
      - |
        void f(void *file, void *conn) {
            char buf[256];
            ReadFile(file, buf, 255, 0, 0);
            PQexec(conn, "SELECT 1");
        }
"#,
    );
}

#[test]
fn rust_taint_from_models_alone() {
    passes(
        r#"
- id: t
  language: rust
  taint:
    sources: [{model: environment}]
    sinks: [{model: command-injection}]
  examples:
    bad:
      - |
        use std::process::Command;
        fn f() {
            let v = std::env::var("CMD").unwrap();
            Command::new(v).status().unwrap();
        }
      - |
        use std::env;
        use std::process::Command;
        fn f() {
            let mut c = Command::new("sh");
            c.arg(env::var("ARG").unwrap());
        }
    good:
      - |
        use std::process::Command;
        fn f() {
            let v = std::env::var("CMD").unwrap();
            Command::new("ls").status().unwrap();
        }
"#,
    );
}

#[test]
fn model_patterns_are_checked_when_rules_load() {
    let set = RuleSet::load(
        &[],
        &[(
            "t.yaml".to_string(),
            "- id: t\n  language: java\n  taint:\n    sources: [{model: remote}]\n    sinks: [{model: no-such-kind}]\n"
                .to_string(),
        )],
        false,
    );
    assert!(
        set.errors
            .iter()
            .any(|e| e.to_string().contains("no-such-kind")),
        "{:?}",
        set.errors
    );
    let set = RuleSet::load(
        &[],
        &[(
            "t.yaml".to_string(),
            "- id: t\n  language: php\n  taint:\n    sources: [{model: remote}]\n    sinks: [{model: sql}]\n"
                .to_string(),
        )],
        false,
    );
    assert!(
        set.errors
            .iter()
            .any(|e| e.to_string().contains("no library models"))
    );
}
