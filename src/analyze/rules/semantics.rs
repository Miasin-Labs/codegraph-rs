//! What the semantic predicates know about calls and functions, from two
//! sources behind one trait:
//!
//! - [`IndexSemantics`]: the index. A call resolves to the callees the
//!   indexer resolved at that call site (matched by the call's start line
//!   and column, the way `calls` edges record it), including calls into
//!   dependency and linked-project graphs (`external_edges`, named
//!   `<package>::<qualified name>` as well). A function's calls are what
//!   its calls resolve to, and — for calls the index could not resolve
//!   (library APIs it holds no node for) — the callee as written.
//! - [`SyntaxSemantics`]: no index (`--check` examples). A call resolves to
//!   its callee as written (`client.send`, `send`), plus what the example's
//!   `resolves` map says; a function's calls are the calls in its body.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use codegraph_analysis::taint_flow::CallResolution;
use tree_sitter::Node;

use super::engine::FileInput;
use super::lang::{self, LangRules};
use crate::analyze::bugs::{FnSpan, Project};
use crate::codegraph::CodeGraph;

/// A project function a call resolves to, as the index records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CalleeRef {
    pub file: String,
    /// 1-based line where it starts.
    pub line: u32,
    pub name: String,
    pub qualified_name: String,
    /// The index's node kind (`function`, `method`, `struct`, `enum_member`…).
    pub kind: String,
}

/// The function a match sits in.
#[derive(Debug, Clone)]
pub(super) struct FunctionFacts {
    pub name: String,
    pub qualified_name: String,
    pub is_test: bool,
    /// Names its calls resolve to.
    pub calls: Vec<String>,
}

pub(super) trait Semantics {
    /// Names the call node `call` resolves to (empty when unresolved).
    fn call_targets(&self, file: &FileInput, rules: &LangRules, call: Node) -> Vec<String>;
    /// Whether some graph resolves the call node `call` (an example: its
    /// `resolves` map names it).
    fn is_resolved(&self, file: &FileInput, rules: &LangRules, call: Node) -> bool;
    /// What the call starting at `line` (1-based) and `col` whose callee
    /// is written `callee` resolves to: the join of an IR call op (which
    /// carries its call's start and callee text) with the index. Chained
    /// calls share a start, so the callee's last name picks among them.
    fn call_at(&self, file: &FileInput, line: u32, col: u32, callee: &str) -> CallResolution;
    /// The project functions the call [`Semantics::call_at`] joins resolves
    /// to (none without an index).
    fn callee_functions(
        &self,
        _file: &FileInput,
        _line: u32,
        _col: u32,
        _callee: &str,
    ) -> Vec<CalleeRef> {
        Vec::new()
    }
    /// Facts about the function node `function`.
    fn function(&self, file: &FileInput, rules: &LangRules, function: Node) -> FunctionFacts;
    /// Calls resolve through an index (else as written, `--check`).
    fn has_index(&self) -> bool {
        true
    }
}

/// Calls as written, for rule examples.
pub(super) struct SyntaxSemantics<'a> {
    pub resolves: &'a BTreeMap<String, String>,
}

impl SyntaxSemantics<'_> {
    fn targets(&self, text: String, name: String) -> Vec<String> {
        let mut targets = Vec::with_capacity(4);
        for key in [&text, &name] {
            if let Some(mapped) = self.resolves.get(key.as_str()) {
                if !targets.contains(mapped) {
                    targets.push(mapped.clone());
                }
            }
        }
        if !name.is_empty() && !targets.contains(&name) {
            targets.push(name);
        }
        if !text.is_empty() && !targets.contains(&text) {
            targets.push(text);
        }
        targets
    }
}

impl Semantics for SyntaxSemantics<'_> {
    fn call_targets(&self, file: &FileInput, rules: &LangRules, call: Node) -> Vec<String> {
        let (text, name) = lang::callee(rules, call, file.source);
        self.targets(text, name)
    }

    fn is_resolved(&self, file: &FileInput, rules: &LangRules, call: Node) -> bool {
        let (text, name) = lang::callee(rules, call, file.source);
        self.resolves.contains_key(&text) || self.resolves.contains_key(&name)
    }

    /// As written; an example's `resolves` map stands for the index (a
    /// callee it names is project code).
    fn call_at(&self, _: &FileInput, _: u32, _: u32, callee: &str) -> CallResolution {
        let name = lang::last_name(callee);
        let mapped: Vec<String> = [callee, name]
            .iter()
            .filter_map(|key| self.resolves.get(*key).cloned())
            .collect();
        CallResolution {
            in_project: !mapped.is_empty(),
            names: mapped,
            summary: None,
        }
    }

    fn function(&self, file: &FileInput, rules: &LangRules, function: Node) -> FunctionFacts {
        syntax_facts(file, rules, function, |call| {
            self.call_targets(file, rules, call)
        })
    }

    fn has_index(&self) -> bool {
        false
    }
}

/// Facts from the syntax alone, with `targets` naming each call.
fn syntax_facts<'t>(
    file: &FileInput,
    rules: &LangRules,
    function: Node<'t>,
    targets: impl Fn(Node<'t>) -> Vec<String>,
) -> FunctionFacts {
    let name = lang::function_name(function, file.source);
    let mut calls = Vec::new();
    // Iterative walk of the body: depth is bounded by the input.
    let mut stack = vec![function];
    while let Some(node) = stack.pop() {
        if node.id() != function.id() && rules.calls.contains(&node.kind()) {
            for target in targets(node) {
                if !calls.contains(&target) {
                    calls.push(target);
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    FunctionFacts {
        is_test: lang::is_test_function(rules, function, file.source)
            || crate::search::is_test_symbol(file.path, &name),
        qualified_name: name.clone(),
        name,
        calls,
    }
}

/// One resolved callee of a call site.
struct Target {
    name: String,
    names: Vec<String>,
    /// A project function (else code in another graph).
    in_project: bool,
    /// Where it is, for a project function.
    callee: Option<CalleeRef>,
}

/// Calls and functions from the index.
pub(crate) struct IndexSemantics<'p> {
    project: &'p Project,
    /// (file, line, col) of a call's start → its resolved callees.
    calls_at: HashMap<(String, u32, u32), Vec<Target>>,
    /// Caller node id → names its calls resolve to.
    calls_by_caller: HashMap<String, Vec<String>>,
    /// Per file: line → 1 + index into `project.functions_in(file)` of the
    /// innermost function spanning it (0: none). Built on first use.
    line_functions: RefCell<HashMap<String, Vec<u32>>>,
}

impl<'p> IndexSemantics<'p> {
    pub(crate) fn new(cg: &CodeGraph, project: &'p Project) -> Result<Self, String> {
        Ok(Self::with_external(project, load_external_calls(cg)?))
    }

    /// From the project's call sites plus calls resolved into other graphs.
    fn with_external(project: &'p Project, external: Vec<ExternalCall>) -> Self {
        let mut calls_at: HashMap<(String, u32, u32), Vec<Target>> = HashMap::new();
        let mut calls_by_caller: HashMap<String, Vec<String>> = HashMap::new();
        for site in project.call_sites() {
            let names = vec![site.callee_qualified.clone()];
            push_unique(
                calls_by_caller.entry(site.caller_id.clone()).or_default(),
                &names,
            );
            calls_at
                .entry((site.file.clone(), site.line, site.col))
                .or_default()
                .push(Target {
                    name: site.callee_name.clone(),
                    names,
                    in_project: true,
                    callee: Some(CalleeRef {
                        file: site.callee_file.clone(),
                        line: site.callee_line,
                        name: site.callee_name.clone(),
                        qualified_name: site.callee_qualified.clone(),
                        kind: site.callee_kind.clone(),
                    }),
                });
        }
        for external in external {
            push_unique(
                calls_by_caller.entry(external.caller_id).or_default(),
                &external.target.names,
            );
            calls_at
                .entry((external.file, external.line, external.col))
                .or_default()
                .push(external.target);
        }
        Self {
            project,
            calls_at,
            calls_by_caller,
            line_functions: RefCell::new(HashMap::new()),
        }
    }

    /// For tests: the project's call sites only.
    #[cfg(test)]
    pub(crate) fn for_tests(project: &'p Project) -> Self {
        Self::with_external(project, Vec::new())
    }

    /// The innermost indexed function of `file` spanning `line`.
    pub(crate) fn function_at(&self, file: &str, line: u32) -> Option<&'p FnSpan> {
        let spans = self.project.functions_in(file);
        if spans.is_empty() {
            return None;
        }
        let mut cache = self.line_functions.borrow_mut();
        let table = cache.entry(file.to_string()).or_insert_with(|| {
            let last = spans
                .iter()
                .map(|span| span.end_line.max(span.start_line))
                .max()
                .unwrap_or(0) as usize;
            let mut table = vec![0u32; last + 2];
            // Outer spans first (sorted by start, longer first), so inner
            // ones overwrite them.
            for (index, span) in spans.iter().enumerate() {
                let end = span.end_line.max(span.start_line) as usize;
                for slot in &mut table[span.start_line as usize..=end] {
                    *slot = index as u32 + 1;
                }
            }
            table
        });
        let index = *table.get(line as usize)?;
        (index > 0).then(|| &spans[index as usize - 1])
    }
}

fn push_unique(list: &mut Vec<String>, names: &[String]) {
    for name in names {
        if !list.contains(name) {
            list.push(name.clone());
        }
    }
}

impl IndexSemantics<'_> {
    /// The targets of the call at `file:line:col` named `name` (the ones
    /// named like it when chained calls share the start).
    /// The resolved targets of the call at `file:line:col` whose callee is
    /// written `callee`. Chained calls (`a.b().c()`) share a start, so the
    /// target named like the call wins. A plain call named otherwise (`baz()`
    /// for `use m::bar as baz`) takes what resolved there; a member call never
    /// does — in `client_builder().no_proxy()` the unresolved library call
    /// `no_proxy` is not a call to `client_builder`.
    fn chosen(&self, file: &str, line: u32, col: u32, callee: &str) -> Vec<&Target> {
        let Some(targets) = self.calls_at.get(&(file.to_string(), line, col)) else {
            return Vec::new();
        };
        let name = lang::last_name(callee);
        let named: Vec<&Target> = targets.iter().filter(|t| t.name == name).collect();
        let member = callee.contains('.') || callee.contains("->");
        if named.is_empty() && !member {
            targets.iter().collect()
        } else {
            named
        }
    }

    /// The resolved callees of the call at `file:line:col` written `callee`.
    fn targets_at(&self, file: &str, line: u32, col: u32, callee: &str) -> Vec<String> {
        self.resolve_at(file, line, col, callee).names
    }

    /// [`Self::targets_at`], and whether a target is project code.
    fn resolve_at(&self, file: &str, line: u32, col: u32, callee: &str) -> CallResolution {
        let chosen = self.chosen(file, line, col, callee);
        let mut names = Vec::new();
        for target in &chosen {
            push_unique(&mut names, &target.names);
        }
        CallResolution {
            names,
            in_project: chosen.iter().any(|target| target.in_project),
            summary: None,
        }
    }
}

impl Semantics for IndexSemantics<'_> {
    fn call_targets(&self, file: &FileInput, rules: &LangRules, call: Node) -> Vec<String> {
        let start = call.start_position();
        let (text, _) = lang::callee(rules, call, file.source);
        self.targets_at(file.path, start.row as u32 + 1, start.column as u32, &text)
    }

    fn is_resolved(&self, file: &FileInput, rules: &LangRules, call: Node) -> bool {
        !self.call_targets(file, rules, call).is_empty()
    }

    fn call_at(&self, file: &FileInput, line: u32, col: u32, callee: &str) -> CallResolution {
        self.resolve_at(file.path, line, col, callee)
    }

    fn callee_functions(
        &self,
        file: &FileInput,
        line: u32,
        col: u32,
        callee: &str,
    ) -> Vec<CalleeRef> {
        self.chosen(file.path, line, col, callee)
            .into_iter()
            .filter_map(|target| target.callee.clone())
            .collect()
    }

    fn function(&self, file: &FileInput, rules: &LangRules, function: Node) -> FunctionFacts {
        // Resolved calls by their targets; unresolved ones (library calls
        // the index has no node for) as written.
        let mut facts = syntax_facts(file, rules, function, |call| {
            let targets = self.call_targets(file, rules, call);
            if targets.is_empty() {
                let (text, name) = lang::callee(rules, call, file.source);
                [name, text].into_iter().filter(|s| !s.is_empty()).collect()
            } else {
                targets
            }
        });
        let line = function.start_position().row as u32 + 1;
        if let Some(span) = self.function_at(file.path, line) {
            facts.name = span.name.clone();
            facts.qualified_name = span.qualified_name.clone();
            facts.is_test |= span.is_test;
            if let Some(calls) = self.calls_by_caller.get(&span.id) {
                push_unique(&mut facts.calls, calls);
            }
        }
        facts
    }
}

struct ExternalCall {
    caller_id: String,
    file: String,
    line: u32,
    col: u32,
    target: Target,
}

/// Calls the external pass resolved into dependency or linked graphs.
fn load_external_calls(cg: &CodeGraph) -> Result<Vec<ExternalCall>, String> {
    let conn = cg.query_builder().db().conn();
    let has_table: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'external_edges'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
        .map_err(|e| e.to_string())?;
    if !has_table {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT x.source, n.file_path, x.line, IFNULL(x.col, 0), x.target_name, \
                    x.target_qualified_name, x.target_graph_key \
             FROM external_edges x JOIN nodes n ON n.id = x.source \
             WHERE x.kind = 'calls' AND x.line IS NOT NULL",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let qualified: String = row.get(5)?;
            let key: String = row.get(6)?;
            let mut names = vec![qualified.clone()];
            if let Some(package) = package_of(&key) {
                let prefixed = format!("{package}::{qualified}");
                if !qualified.starts_with(&format!("{package}::")) {
                    names.push(prefixed);
                }
            }
            Ok(ExternalCall {
                caller_id: row.get(0)?,
                file: row.get(1)?,
                line: row.get(2)?,
                col: row.get(3)?,
                target: Target {
                    name: row.get(4)?,
                    names,
                    in_project: false,
                    callee: None,
                },
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// `crates/reqwest-0.12.4` → `reqwest`; a linked project's root → its
/// directory name.
fn package_of(graph_key: &str) -> Option<String> {
    let last = graph_key.trim_end_matches('/').rsplit('/').next()?;
    let name = match last.rfind('-') {
        Some(dash)
            if last[dash + 1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit()) =>
        {
            &last[..dash]
        }
        _ => last,
    };
    (!name.is_empty()).then(|| name.replace('-', "_"))
}

#[cfg(test)]
mod tests {
    use super::package_of;

    #[test]
    fn graph_keys_name_their_package() {
        assert_eq!(
            package_of("crates/reqwest-0.12.4").as_deref(),
            Some("reqwest")
        );
        assert_eq!(
            package_of("crates/serde_json-1.0.1+git.abc").as_deref(),
            Some("serde_json")
        );
        assert_eq!(
            package_of("crates/tokio-util-0.7.0").as_deref(),
            Some("tokio_util")
        );
        assert_eq!(package_of("/home/u/linked").as_deref(), Some("linked"));
    }
}
