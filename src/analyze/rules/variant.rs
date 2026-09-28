//! `variant`: from one bug to a rule that finds its variants. Given a
//! `file:line`, gather what a model needs to write a rule for the bug there:
//! the enclosing function, the syntax tree of the statement at that line (a
//! compact tree-sitter s-expression with field names — the vocabulary of a
//! `query`), the calls in it and what the index resolves them to, a
//! skeleton rule (a query plus `resolves-to`/`inside` predicates pre-filled
//! from those calls, and a `bad` example pre-filled with the code) that
//! already passes `check`, and any detector finding there with its evidence.

use std::path::Path;

use serde::Serialize;
use tree_sitter::{Node, Tree};

use super::check::check_rules;
use super::compile::RuleSet;
use super::engine::{FileInput, one_line};
use super::lang::{self, LangRules};
use super::semantics::{IndexSemantics, Semantics};
use crate::analyze::bugs::{self, BugsOptions, Finding, Project};
use crate::codegraph::CodeGraph;
use crate::extraction::{create_parser, detect_language};
use crate::types::Language;

#[cfg(test)]
mod tests;

/// Longest function quoted whole; longer ones are windowed on the line.
const MAX_FUNCTION_LINES: u32 = 120;
/// Depth of the syntax tree printed below the statement.
const MAX_TREE_DEPTH: usize = 7;
/// Lines of the printed tree.
const MAX_TREE_LINES: usize = 70;
/// Children of one node printed before the middle ones are elided.
const MAX_CHILDREN: usize = 6;
/// Calls listed.
const MAX_CALLS: usize = 16;
/// Longest snippet put in the skeleton's `bad` example.
const MAX_EXAMPLE_LINES: usize = 150;

/// The enclosing function, numbered (`N\ttext`) as a Read shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantFunction {
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub source: String,
}

/// The statement at the line and its syntax tree.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantNode {
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
    /// Compact s-expression: named nodes with their field names, leaves
    /// annotated `; text`, deep or long parts elided.
    pub tree: String,
}

/// A call inside the statement.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantCall {
    pub line: u32,
    /// The callee as written (`self.download_file`), what an example's
    /// `resolves` map keys on.
    pub callee: String,
    /// What the index resolves it to (qualified names; dependency calls
    /// also as `<package>::<name>`) — what `resolves-to` matches.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resolves_to: Vec<String>,
}

/// Whether the skeleton passes `check` as generated.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkeletonCheck {
    pub passed: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
}

/// Everything [`variant`] gathers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantReport {
    pub file: String,
    pub line: u32,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<VariantFunction>,
    pub node: VariantNode,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<VariantCall>,
    /// A rule to generalize: YAML that `check` accepts.
    pub skeleton: String,
    pub skeleton_check: SkeletonCheck,
    /// Detector findings (deviance, lint) in the statement.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
}

/// Gather the material for a rule about the bug at `file:line` (`file`
/// project-relative) of the project indexed at `root`.
pub fn variant(
    cg: &CodeGraph,
    root: &Path,
    file: &str,
    line: u32,
) -> Result<VariantReport, String> {
    let file = file.trim_start_matches("./").replace('\\', "/");
    let relative = Path::new(&file);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "{file}: give a path inside the project, relative to its root"
        ));
    }
    let source = std::fs::read_to_string(root.join(&file))
        .map_err(|e| format!("cannot read {file}: {e}"))?;
    let language = detect_language(&file, Some(&source));
    let tree = create_parser(language)
        .and_then(|mut parser| parser.parse(&source, None))
        .ok_or_else(|| format!("no parser for {file} ({})", language.as_str()))?;
    let line_count = source.lines().count() as u32;
    if line == 0 || line > line_count {
        return Err(format!(
            "{file} has {line_count} lines; line {line} is outside it"
        ));
    }
    let rules = lang::for_language(language);
    let node =
        statement_at(&tree, &source, line).ok_or_else(|| format!("{file}:{line} is blank"))?;

    let project = Project::load(cg, root)?;
    let function = project.enclosing_function(&file, line).map(|span| {
        let end = span.end_line.max(span.start_line).min(line_count);
        let (from, to) = if end - span.start_line < MAX_FUNCTION_LINES {
            (span.start_line, end)
        } else {
            let from = line
                .saturating_sub(MAX_FUNCTION_LINES / 2)
                .max(span.start_line);
            (from, (from + MAX_FUNCTION_LINES).min(end))
        };
        VariantFunction {
            name: span.qualified_name.clone(),
            start_line: from,
            end_line: to,
            source: numbered(&source, from, to),
        }
    });

    let input = FileInput::new(&file, language, &source, &tree);
    let semantics = IndexSemantics::new(cg, &project)?;
    let calls = calls_in(node, &input, rules, &semantics);

    let snippet = example_code(node, &source, language, rules);
    let skeleton = skeleton(&file, line, language, node, &calls, snippet.as_deref());
    let skeleton_check = check_skeleton(&skeleton);

    let (start_line, end_line) = lines_of(node);
    let findings = detector_findings(cg, root, &file, start_line, end_line)?;
    Ok(VariantReport {
        file,
        line,
        language: language.as_str().to_string(),
        function,
        node: VariantNode {
            kind: node.kind().to_string(),
            start_line,
            end_line,
            tree: sexp(node, &source),
        },
        calls: calls.into_iter().map(|(call, _)| call).collect(),
        skeleton,
        skeleton_check,
        findings,
    })
}

fn lines_of(node: Node) -> (u32, u32) {
    (
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
    )
}

fn numbered(source: &str, from: u32, to: u32) -> String {
    source
        .lines()
        .enumerate()
        .skip(from as usize - 1)
        .take((to - from + 1) as usize)
        .map(|(index, text)| format!("{}\t{text}", index + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The statement or expression at `line`: the outermost node starting at
/// the line's first non-blank character (short of the root).
fn statement_at<'t>(tree: &'t Tree, source: &str, line: u32) -> Option<Node<'t>> {
    let line_start: usize = source
        .split_inclusive('\n')
        .take(line as usize - 1)
        .map(str::len)
        .sum();
    let text = source[line_start..].lines().next().unwrap_or("");
    let indent = text.len() - text.trim_start().len();
    if text.trim().is_empty() {
        return None;
    }
    let at = line_start + indent;
    let root = tree.root_node();
    let mut node = root.descendant_for_byte_range(at, at)?;
    while let Some(parent) = node.parent() {
        if parent.start_byte() != node.start_byte() || parent.id() == root.id() {
            break;
        }
        node = parent;
    }
    // A closing `}` or keyword alone: the named node it belongs to.
    while !node.is_named() {
        match node.parent() {
            Some(parent) if parent.id() != root.id() => node = parent,
            _ => break,
        }
    }
    Some(node)
}

/// The calls inside `node` with what each resolves to (and the call node).
fn calls_in<'t>(
    node: Node<'t>,
    input: &FileInput,
    rules: &LangRules,
    semantics: &IndexSemantics,
) -> Vec<(VariantCall, Node<'t>)> {
    let mut calls = Vec::new();
    // Iterative, in source order: depth is bounded by the input.
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if rules.calls.contains(&current.kind()) && calls.len() < MAX_CALLS {
            let (callee, _) = lang::callee(rules, current, input.source);
            let resolves_to = semantics.call_targets(input, rules, current);
            calls.push((
                VariantCall {
                    line: current.start_position().row as u32 + 1,
                    callee,
                    resolves_to,
                },
                current,
            ));
        }
        let mut cursor = current.walk();
        let children: Vec<Node> = current.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    calls
}

/// The code of the statement holding `node`, made a file that parses (the
/// statement wrapped in a function, or the whole function), if it fits.
fn example_code(node: Node, source: &str, language: Language, rules: &LangRules) -> Option<String> {
    let function = lang::enclosing_function(rules, node);
    let is_function = function.is_some_and(|f| f.id() == node.id());
    let mut statement = node;
    if !is_function {
        while let Some(parent) = statement.parent() {
            if lang::STATEMENT_CONTAINERS.contains(&parent.kind()) {
                break;
            }
            if function.is_some_and(|f| f.id() == parent.id()) {
                break;
            }
            statement = parent;
        }
    }
    let wrapped = if is_function || function.is_some_and(|f| f.id() == statement.id()) {
        None
    } else {
        lang::example_wrapper(language).map(|wrapper| {
            let body = dedent(&source[statement.byte_range()], wrapper.indent);
            format!("{}{body}\n{}", wrapper.prefix, wrapper.suffix)
        })
    };
    let candidates = wrapped.into_iter().chain(
        function
            .or(is_function.then_some(node))
            .map(|f| format!("{}\n", dedent(&source[f.byte_range()], ""))),
    );
    candidates
        .filter(|code| code.lines().count() <= MAX_EXAMPLE_LINES)
        .find(|code| parses(code, language))
}

fn parses(code: &str, language: Language) -> bool {
    create_parser(language)
        .and_then(|mut parser| parser.parse(code, None))
        .is_some_and(|tree| !tree.root_node().has_error())
}

/// `text` (whose first line starts at a node, its indentation cut off)
/// re-indented by `indent`: the common indentation of the later lines
/// removed.
fn dedent(text: &str, indent: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("").trim_start().to_string();
    let rest: Vec<&str> = lines.collect();
    let common = rest
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let last_indent = rest.last().map_or(0, |l| l.len() - l.trim_start().len());
    // The closing line (`}`) sits at the statement's own indentation.
    let base = common.min(last_indent);
    let mut out = format!("{indent}{first}");
    for line in rest {
        out.push('\n');
        if !line.trim().is_empty() {
            out.push_str(indent);
            out.push_str(line.get(base..).unwrap_or(line.trim_start()));
        }
    }
    out
}

/// YAML single-quoted.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A YAML block scalar body, each line indented by `indent`.
fn block(text: &str, indent: &str) -> String {
    text.trim_end_matches('\n')
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The skeleton rule: a query for the first resolved call in the statement,
/// pinned by `resolves-to` and `inside` the statement's kind, with the
/// statement as its `bad` example.
fn skeleton(
    file: &str,
    line: u32,
    language: Language,
    node: Node,
    calls: &[(VariantCall, Node)],
    snippet: Option<&str>,
) -> String {
    let stem = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let mut yaml = format!(
        "id: variant-{stem}-{line}\n\
         description: >-\n  TODO: the bug class — what goes wrong, and why it is a bug wherever it occurs.\n\
         severity: medium\n\
         language: {}\n",
        language.as_str()
    );
    let resolved: Vec<&(VariantCall, Node)> = calls
        .iter()
        .filter(|(call, _)| !call.resolves_to.is_empty())
        .collect();
    let mut resolves = Vec::new();
    match resolved.first() {
        Some((call, call_node)) => {
            let target = &call.resolves_to[0];
            yaml.push_str(&format!(
                "message: \"TODO: what is wrong with `{{call}}` in {{function}}\"\n\
                 check-patterns:\n  - name: {}\n    query: |\n{}\n    where:\n\
                 \x20     - capture: call\n        resolves-to: {}\n",
                call_node.kind().replace('_', "-"),
                block(&format!("({}) @call", call_node.kind()), "      "),
                quoted(&format!("^{}$", regex::escape(target)))
            ));
            if call_node.id() != node.id() {
                yaml.push_str(&format!(
                    "      - capture: call\n        inside: |\n{}\n",
                    block(&format!("({})", node.kind()), "          ")
                ));
            }
            let others: Vec<String> = resolved
                .iter()
                .skip(1)
                .map(|(other, _)| format!("{} → {}", other.callee, other.resolves_to[0]))
                .collect();
            if !others.is_empty() {
                yaml.push_str(&format!(
                    "    # Other resolved calls here: {}\n",
                    one_line(&others.join("; "), 300)
                ));
            }
            resolves.push((call.callee.clone(), target.clone()));
        }
        None => {
            yaml.push_str(&format!(
                "message: \"TODO: what is wrong in {{function}}\"\n\
                 check-patterns:\n  - name: {}\n    # TODO: this matches every `{}`; narrow it \
                 with fields, #match? and `where`.\n    query: |\n{}\n",
                node.kind().replace('_', "-"),
                node.kind(),
                block(&format!("({}) @stmt", node.kind()), "      ")
            ));
        }
    }
    yaml.push_str("examples:\n  bad:\n");
    match snippet {
        Some(code) => {
            yaml.push_str(&format!("    - code: |\n{}\n", block(code, "        ")));
            if !resolves.is_empty() {
                yaml.push_str("      resolves:\n");
                for (callee, target) in &resolves {
                    yaml.push_str(&format!("        {}: {}\n", quoted(callee), quoted(target)));
                }
            }
        }
        None => yaml.push_str("    # TODO: the buggy code, as a snippet that parses\n    - ''\n"),
    }
    yaml.push_str("  good:\n    # TODO: the fixed code — the rule must stay silent on it\n");
    let empty = lang::example_wrapper(language)
        .map(|wrapper| {
            let body = if wrapper.empty.is_empty() {
                String::new()
            } else {
                format!("{}{}\n", wrapper.indent, wrapper.empty)
            };
            format!("{}{body}{}", wrapper.prefix, wrapper.suffix)
        })
        .unwrap_or_default();
    yaml.push_str(&format!("    - |\n{}\n", block(&empty, "      ")));
    yaml
}

fn check_skeleton(yaml: &str) -> SkeletonCheck {
    let mut rules = RuleSet::default();
    rules.add_text("<skeleton>", yaml);
    let report = check_rules(&rules);
    let mut failures: Vec<String> = report.errors.clone();
    for rule in &report.rules {
        if let Some(error) = &rule.error {
            failures.push(error.clone());
        }
        failures.extend(
            rule.examples
                .iter()
                .filter(|e| !e.passed)
                .map(|e| e.explain()),
        );
    }
    SkeletonCheck {
        passed: report.ok(),
        failures,
    }
}

/// Detector findings (deviance, lint) reported in lines `from..=to`.
fn detector_findings(
    cg: &CodeGraph,
    root: &Path,
    file: &str,
    from: u32,
    to: u32,
) -> Result<Vec<Finding>, String> {
    let options = BugsOptions {
        detectors: Vec::new(),
        only_files: Some(vec![file.to_string()]),
        include_tests: true,
        taint_budget: None,
        dependency_summaries: None,
    };
    let report = bugs::bugs_report(cg, root, &options)?;
    Ok(report
        .findings
        .into_iter()
        .filter(|f| from <= f.line && f.line <= to)
        .collect())
}

/// A compact s-expression of `node`: named nodes with field names, a leaf's
/// text as a `;` comment, anonymous tokens only where a field names them.
pub(super) fn sexp(node: Node, source: &str) -> String {
    let mut lines = Vec::new();
    write_sexp(node, None, 0, source, &mut lines);
    if lines.len() > MAX_TREE_LINES {
        lines.truncate(MAX_TREE_LINES);
        lines.push("; … (trimmed)".to_string());
    }
    lines.join("\n")
}

fn write_sexp(
    node: Node,
    field: Option<&str>,
    depth: usize,
    source: &str,
    lines: &mut Vec<String>,
) {
    crate::ensure_sufficient_stack(|| {
        if lines.len() > MAX_TREE_LINES {
            return;
        }
        let pad = "  ".repeat(depth);
        let label = field.map(|f| format!("{f}: ")).unwrap_or_default();
        if !node.is_named() {
            lines.push(format!("{pad}{label}\"{}\"", node.kind()));
            return;
        }
        let mut children: Vec<(Option<&'static str>, Node)> = Vec::new();
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                let name = cursor.field_name();
                if child.is_named() || name.is_some() {
                    children.push((name, child));
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        let text = one_line(&source[node.byte_range()], 48);
        if children.is_empty() {
            lines.push(format!("{pad}{label}({})  ; {text}", node.kind()));
            return;
        }
        if depth >= MAX_TREE_DEPTH {
            lines.push(format!("{pad}{label}({} …)  ; {text}", node.kind()));
            return;
        }
        lines.push(format!("{pad}{label}({}", node.kind()));
        let count = children.len();
        for (index, (name, child)) in children.into_iter().enumerate() {
            if count > MAX_CHILDREN && index == 3 {
                lines.push(format!("{pad}  ; … {} more", count - 5));
            }
            if count > MAX_CHILDREN && (3..count - 2).contains(&index) {
                continue;
            }
            write_sexp(child, name, depth + 1, source, lines);
        }
        if let Some(last) = lines.last_mut() {
            // Close after the last child, before its `; text` comment.
            match last.find("  ; ") {
                Some(at) if !last.trim_start().starts_with(';') => last.insert(at, ')'),
                _ => last.push(')'),
            }
        }
    });
}
