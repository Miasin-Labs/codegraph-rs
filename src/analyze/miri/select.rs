//! What to run under Miri: the project's own `#[test]`s that reach the
//! aimed functions through the call graph, nearest first.
//!
//! Tests are found in the syntax (`#[test]`, `#[tokio::test]`,
//! `#[rstest]`…: an attribute the index does not keep) and matched to the
//! indexed function they are; reach comes from the index's resolved call
//! edges ([`CallGraph::reverse_reach`]). A test is named the way libtest
//! knows it — its module path inside its test binary — so `cargo miri test
//! -- --exact <path>` runs exactly that test.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Serialize;
use tree_sitter::Node;

use crate::analyze::bugs::Project;
use crate::analyze::fuzz::graph::CallGraph;
use crate::analyze::fuzz::rust::api::{CrateInfo, RustApi};

/// Which test binary of a package holds a test.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "name")]
pub enum TestTarget {
    /// The library's unit tests (`--lib`).
    Lib,
    /// `tests/<name>.rs` or `tests/<name>/main.rs` (`--test <name>`).
    Integration(String),
}

impl TestTarget {
    /// Cargo's arguments that pick the binary.
    pub fn cargo_args(&self) -> Vec<String> {
        match self {
            Self::Lib => vec!["--lib".to_string()],
            Self::Integration(name) => vec!["--test".to_string(), name.clone()],
        }
    }
}

/// A `#[test]` function as cargo and libtest see it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestFn {
    /// Dense index into the call graph.
    #[serde(skip)]
    pub index: usize,
    /// The libtest path inside its binary (`codec::tests::roundtrip`).
    pub path: String,
    pub file: String,
    pub line: u32,
    /// Project-relative directory of the package's `Cargo.toml`.
    pub crate_dir: String,
    pub package: String,
    pub target: TestTarget,
    /// Why Miri will not run it (`#[ignore]`, `cfg_attr(miri, ignore)`),
    /// when its attributes say so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip: Option<String>,
}

/// Attributes that make a function a test (the last path segment of the
/// attribute, or its whole path).
const TEST_ATTRIBUTES: &[&str] = &[
    "test",
    "rstest",
    "test_case",
    "quickcheck",
    "proptest",
    "wasm_bindgen_test",
];

/// Every test function in the project's library crates and their
/// integration tests, mapped to the call graph.
pub fn discover_tests(project: &mut Project, graph: &CallGraph, api: &RustApi) -> Vec<TestFn> {
    let files: Vec<String> = project
        .files()
        .iter()
        .filter(|file| file.ends_with(".rs"))
        .cloned()
        .collect();
    let mut tests = Vec::new();
    for file in files {
        let Some((krate, target, base)) = test_home(api, &file) else {
            continue;
        };
        let found = {
            let Some(parsed) = project.parsed(&file) else {
                continue;
            };
            test_items(parsed.tree.root_node(), parsed.source.as_bytes())
        };
        for item in found {
            let Some(span) = project
                .enclosing_function(&file, item.line)
                .filter(|span| span.name == item.name)
            else {
                continue;
            };
            let Some(index) = graph.index_of(&span.id) else {
                continue;
            };
            let mut path = base.clone();
            path.extend(inline_modules(api, &krate, &file, item.line));
            path.push(item.name.clone());
            tests.push(TestFn {
                index,
                path: path.join("::"),
                file: file.clone(),
                line: span.start_line,
                crate_dir: krate.dir.clone(),
                package: krate.package.clone(),
                target: target.clone(),
                skip: item.skip,
            });
        }
    }
    tests.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    tests
}

/// The library crate, test binary and module path prefix of `file`, when
/// it can hold tests cargo runs.
fn test_home(api: &RustApi, file: &str) -> Option<(CrateInfo, TestTarget, Vec<String>)> {
    if let Some(krate) = api.crate_of(file) {
        let module = api.file_module(krate, file);
        return Some((krate.clone(), TestTarget::Lib, module));
    }
    // `<crate>/tests/<name>.rs`, `<crate>/tests/<name>/main.rs`, and the
    // modules `<crate>/tests/<name>/<m>.rs` it declares.
    let krate = api
        .crates()
        .iter()
        .filter(|krate| {
            let tests = join(&krate.dir, "tests/");
            file.starts_with(&tests)
        })
        .max_by_key(|krate| krate.dir.len())?;
    let relative = file.strip_prefix(&join(&krate.dir, "tests/"))?;
    let segments: Vec<&str> = relative.trim_end_matches(".rs").split('/').collect();
    let (name, module): (&str, Vec<String>) = match segments.as_slice() {
        [name] => (name, Vec::new()),
        [name, "main"] => (name, Vec::new()),
        [name, rest @ ..] => {
            let mut module: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
            if module.last().is_some_and(|last| last == "mod") {
                module.pop();
            }
            (name, module)
        }
        [] => return None,
    };
    Some((
        krate.clone(),
        TestTarget::Integration(name.to_string()),
        module,
    ))
}

fn join(dir: &str, rest: &str) -> String {
    if dir.is_empty() {
        rest.to_string()
    } else {
        format!("{dir}/{rest}")
    }
}

/// The inline `mod x { … }` blocks enclosing `line` (the file's own module
/// path is the caller's).
fn inline_modules(api: &RustApi, krate: &CrateInfo, file: &str, line: u32) -> Vec<String> {
    // `module_at` is the file's module plus the inline ones; asked as if
    // the file were the crate root, it is the inline ones alone.
    let as_root = CrateInfo {
        lib_root: file.to_string(),
        ..krate.clone()
    };
    api.module_at(&as_root, file, line)
}

/// A test function found in the syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestItem {
    pub name: String,
    /// 1-based line of the `fn`.
    pub line: u32,
    pub skip: Option<String>,
}

/// The test functions of a Rust syntax tree: `function_item`s whose
/// attributes (the `attribute_item` siblings right before them) include a
/// test attribute. One walk; test functions' bodies are not entered.
pub fn test_items(root: Node<'_>, source: &[u8]) -> Vec<TestItem> {
    let mut items = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        let mut attributes: Vec<String> = Vec::new();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "attribute_item" => {
                    attributes.push(child.utf8_text(source).unwrap_or("").to_string());
                }
                "line_comment" | "block_comment" => {}
                "function_item" => {
                    if attributes.iter().any(|a| is_test_attribute(a)) {
                        let name = child
                            .child_by_field_name("name")
                            .and_then(|n| n.utf8_text(source).ok())
                            .unwrap_or("")
                            .to_string();
                        items.push(TestItem {
                            name,
                            line: child.start_position().row as u32 + 1,
                            skip: skip_reason(&attributes),
                        });
                    }
                    attributes.clear();
                }
                _ => {
                    attributes.clear();
                    if child.named_child_count() > 0 {
                        stack.push(child);
                    }
                }
            }
        }
    }
    items.sort_by_key(|item| item.line);
    items
}

/// `#[test]`, `#[tokio::test(flavor = …)]`, `#[rstest]`…
fn is_test_attribute(attribute: &str) -> bool {
    let inner = attribute
        .trim()
        .trim_start_matches("#[")
        .trim_end_matches(']')
        .trim();
    let path = inner
        .split(|c: char| c == '(' || c == '=' || c.is_whitespace())
        .next()
        .unwrap_or("");
    let last = path.rsplit("::").next().unwrap_or(path);
    TEST_ATTRIBUTES.contains(&last)
}

/// Why libtest (under Miri) will not run a test with these attributes.
fn skip_reason(attributes: &[String]) -> Option<String> {
    for attribute in attributes {
        let compact: String = attribute.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("cfg_attr(miri,ignore") {
            return Some(
                "marked #[cfg_attr(miri, ignore)]: its author says Miri cannot run it".into(),
            );
        }
        if compact.contains("cfg(not(miri))") {
            return Some("marked #[cfg(not(miri))]: not compiled under Miri".into());
        }
        if compact.starts_with("#[ignore") {
            return Some("marked #[ignore]".into());
        }
    }
    None
}

/// A test chosen for the aimed functions.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Selected {
    pub test: TestFn,
    /// Aimed functions (qualified names) it reaches.
    pub reaches: Vec<String>,
    /// Calls from the test to the nearest aimed function.
    pub distance: u32,
}

/// Bounds of the reverse search from an aimed function.
const REACH_DEPTH: u32 = 10;
const REACH_CAP: usize = 20_000;

/// The tests to run for `aimed` (dense indices), at most `max_tests`: first
/// the nearest test of each aimed function (so every reachable one is
/// covered), then the rest nearest first. Also returns the aimed functions
/// no runnable test reaches.
pub fn select_tests(
    graph: &CallGraph,
    tests: &[TestFn],
    aimed: &[usize],
    max_tests: usize,
) -> (Vec<Selected>, Vec<usize>) {
    let by_index: HashMap<usize, usize> = tests
        .iter()
        .enumerate()
        .filter(|(_, test)| test.skip.is_none())
        .map(|(i, test)| (test.index, i))
        .collect();
    // test (position in `tests`) → aimed function → distance.
    let mut reach: BTreeMap<usize, BTreeMap<usize, u32>> = BTreeMap::new();
    let mut nearest: Vec<Option<(u32, usize)>> = vec![None; aimed.len()];
    for (slot, &function) in aimed.iter().enumerate() {
        for (caller, depth) in graph.reverse_reach(function, REACH_DEPTH, REACH_CAP) {
            let Some(&test) = by_index.get(&caller) else {
                continue;
            };
            reach.entry(test).or_default().insert(function, depth);
            let better = nearest[slot].is_none_or(|(d, t)| {
                (depth, &tests[test].file, tests[test].line) < (d, &tests[t].file, tests[t].line)
            });
            if better {
                nearest[slot] = Some((depth, test));
            }
        }
    }
    let uncovered: Vec<usize> = aimed
        .iter()
        .zip(&nearest)
        .filter(|(_, n)| n.is_none())
        .map(|(&f, _)| f)
        .collect();

    let mut order: Vec<usize> = Vec::new();
    for (_, test) in nearest.iter().flatten() {
        if !order.contains(test) {
            order.push(*test);
        }
    }
    let mut rest: Vec<(u32, usize)> = reach
        .iter()
        .filter(|(test, _)| !order.contains(test))
        .map(|(&test, functions)| (functions.values().copied().min().unwrap_or(0), test))
        .collect();
    rest.sort_by(|a, b| {
        (a.0, &tests[a.1].file, tests[a.1].line).cmp(&(b.0, &tests[b.1].file, tests[b.1].line))
    });
    order.extend(rest.into_iter().map(|(_, test)| test));
    order.truncate(max_tests);

    let selected = order
        .into_iter()
        .map(|test| {
            let functions = &reach[&test];
            Selected {
                test: tests[test].clone(),
                reaches: functions
                    .keys()
                    .map(|&f| graph.functions[f].qualified_name.clone())
                    .collect(),
                distance: functions.values().copied().min().unwrap_or(0),
            }
        })
        .collect();
    (selected, uncovered)
}

/// The directory of the package that owns `test`, absolute.
pub fn manifest_dir(root: &Path, crate_dir: &str) -> std::path::PathBuf {
    if crate_dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(crate_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::create_parser;
    use crate::types::Language;

    #[test]
    fn test_attributes_are_read_from_the_syntax() {
        let source = "fn live() {}\n\
                      #[cfg(test)]\n\
                      mod tests {\n\
                      \x20   use super::*;\n\
                      \x20   #[test]\n\
                      \x20   fn plain() { live(); }\n\
                      \x20   /// doc\n\
                      \x20   #[tokio::test(flavor = \"multi_thread\")]\n\
                      \x20   async fn runtime() {}\n\
                      \x20   #[test]\n\
                      \x20   #[cfg_attr(miri, ignore)]\n\
                      \x20   fn too_slow() {}\n\
                      \x20   #[test]\n\
                      \x20   #[ignore = \"flaky\"]\n\
                      \x20   fn ignored() {}\n\
                      \x20   #[derive(Debug)]\n\
                      \x20   struct Helper;\n\
                      \x20   fn helper() {}\n\
                      }\n";
        let mut parser = create_parser(Language::Rust).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let items = test_items(tree.root_node(), source.as_bytes());
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["plain", "runtime", "too_slow", "ignored"]);
        assert_eq!(items[0].line, 6);
        assert!(items[0].skip.is_none() && items[1].skip.is_none());
        assert!(items[2].skip.as_deref().unwrap().contains("miri"));
        assert_eq!(items[3].skip.as_deref(), Some("marked #[ignore]"));
    }

    #[test]
    fn attribute_paths_decide() {
        assert!(is_test_attribute("#[test]"));
        assert!(is_test_attribute("#[tokio::test]"));
        assert!(is_test_attribute("#[async_std::test]"));
        assert!(is_test_attribute("#[rstest]"));
        assert!(!is_test_attribute("#[cfg(test)]"));
        assert!(!is_test_attribute("#[testing]"));
        assert!(!is_test_attribute("#[inline]"));
    }
}
