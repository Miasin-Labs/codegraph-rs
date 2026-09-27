//! Deviance mining after Engler et al., "Bugs as Deviant Behavior" (SOSP
//! '01): beliefs learned from the code itself, and the sites that depart
//! from a strong one, with the agreeing sites as evidence.
//!
//! Three templates:
//!
//! - `arm-result-deviance` ([`arms`]): in a `match`/`switch` whose value is
//!   the function's result, most arms return what a project call produced;
//!   an arm that makes a project call and then returns a constant (`None`,
//!   `()`, `null`) dropped the work it did.
//! - `result-discarded` ([`results`]): most call sites of a function use its
//!   result; one that discards it.
//! - `missing-companion-call` ([`companions`]): most callers of `A` also call
//!   `B`; one that calls `A` without `B`.
//!
//! The first two read syntax (per-language [`rules`] tables; languages
//! without one are skipped) and share one walk per file; the third reads
//! only the index's resolved calls.

mod arms;
mod companions;
mod results;
mod returns;
mod rules;
mod syntax;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use self::arms::ArmDeviance;
use self::results::Use;
use self::rules::Rules;
use super::{CallSite, Finding, FnSpan, Project};
use crate::ensure_sufficient_stack;
use crate::extraction::detect_language;

pub(super) fn detect(project: &mut Project) -> Vec<Finding> {
    let scanned = scan(project);
    let discarded = results::findings(project, &scanned.uses);
    let mut findings = arms::merge(scanned.arms, discarded);
    findings.extend(companions::findings(
        project,
        &scanned.test_regions,
        &scanned.called_names,
    ));
    findings
}

/// Line ranges of test items per file (`#[cfg(test)] mod …`, `#[test] fn
/// …`) that the index does not mark as tests by name.
pub(super) type TestRegions = HashMap<String, Vec<(u32, u32)>>;

/// What the syntactic pass found.
struct Scanned {
    /// How each call site (by index) uses its result, where classified.
    uses: Vec<Option<Use>>,
    arms: Vec<ArmDeviance>,
    test_regions: TestRegions,
    called_names: CalledNames,
}

/// The names each function calls as written (by function id), resolved or
/// not: a call the index left unresolved still counts as made.
pub(super) type CalledNames = HashMap<String, HashSet<String>>;

/// The syntactic pass: every file in a supported language parsed once and
/// walked once — how each resolved call uses its result (by site index),
/// and the deviant arms.
fn scan(project: &mut Project) -> Scanned {
    let sites = SiteIndex::build(project.call_sites());
    let mut uses: Vec<Option<Use>> = vec![None; project.call_sites().len()];
    let mut arm_deviances = Vec::new();
    let mut test_regions = TestRegions::new();
    let mut called_names = CalledNames::new();

    for file in syntax_files(project, &sites) {
        let Some(rules) = rules::for_language(detect_language(&file, None)) else {
            continue;
        };
        if project.parsed(&file).is_none() {
            continue;
        }
        let project = &*project;
        let Some(parsed) = project.parsed_cached(&file) else {
            continue;
        };
        let mut scan = FileScan {
            project,
            file: &file,
            rules,
            source: &parsed.source,
            sites: sites.in_file(&file),
            uses: &mut uses,
            arms: &mut arm_deviances,
            seen_matches: HashSet::new(),
            in_test: 0,
            test_regions: Vec::new(),
            functions: project.functions_in(&file),
            fn_stack: Vec::new(),
            called_names: &mut called_names,
        };
        let mut ancestors = Vec::new();
        scan.visit(parsed.tree.root_node(), &mut ancestors);
        if !scan.test_regions.is_empty() {
            test_regions.insert(file.clone(), scan.test_regions);
        }
    }
    Scanned {
        uses,
        arms: arm_deviances,
        test_regions,
        called_names,
    }
}

/// Files the syntactic templates read: those with call sites or functions,
/// in a language with rules, path-ordered.
fn syntax_files(project: &Project, sites: &SiteIndex) -> Vec<String> {
    let mut files: Vec<String> = project
        .files()
        .iter()
        .filter(|file| {
            sites.by_file.contains_key(file.as_str()) || !project.functions_in(file).is_empty()
        })
        .filter(|file| rules::for_language(detect_language(file, None)).is_some())
        .cloned()
        .collect();
    files.sort();
    files.dedup();
    files
}

/// A call site's position and its index in [`Project::call_sites`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SitePos {
    pub line: u32,
    pub col: u32,
    pub site: usize,
}

/// The call sites of each file, position-ordered, for point and range
/// lookups by binary search.
struct SiteIndex {
    by_file: HashMap<String, Vec<SitePos>>,
}

impl SiteIndex {
    fn build(call_sites: &[CallSite]) -> Self {
        let mut by_file: HashMap<String, Vec<SitePos>> = HashMap::new();
        for (site, call) in call_sites.iter().enumerate() {
            by_file.entry(call.file.clone()).or_default().push(SitePos {
                line: call.line,
                col: call.col,
                site,
            });
        }
        for positions in by_file.values_mut() {
            positions.sort_unstable();
        }
        Self { by_file }
    }

    fn in_file(&self, file: &str) -> &[SitePos] {
        self.by_file.get(file).map_or(&[], Vec::as_slice)
    }
}

/// The sites of `sites` (one file's, ordered) from `from` to `to` inclusive.
pub(super) fn sites_between(sites: &[SitePos], from: (u32, u32), to: (u32, u32)) -> &[SitePos] {
    let lo = sites.partition_point(|pos| (pos.line, pos.col) < from);
    let hi = sites.partition_point(|pos| (pos.line, pos.col) <= to);
    &sites[lo..hi.max(lo)]
}

/// One file's walk: classifies each resolved call's use of its result
/// (`result-discarded`) and checks each match in result position
/// (`arm-result-deviance`).
struct FileScan<'a> {
    project: &'a Project,
    file: &'a str,
    rules: &'static Rules,
    source: &'a str,
    sites: &'a [SitePos],
    uses: &'a mut [Option<Use>],
    arms: &'a mut Vec<ArmDeviance>,
    seen_matches: HashSet<usize>,
    /// Test items the walk is inside.
    in_test: usize,
    /// This file's outermost test items.
    test_regions: Vec<(u32, u32)>,
    /// This file's functions (by start line), the ones the walk is inside,
    /// and the names each calls.
    functions: &'a [FnSpan],
    fn_stack: Vec<usize>,
    called_names: &'a mut CalledNames,
}

impl<'a> FileScan<'a> {
    fn visit<'t>(&mut self, node: Node<'t>, ancestors: &mut Vec<Node<'t>>) {
        ensure_sufficient_stack(|| {
            let kind = node.kind();
            if self.rules.calls.contains(&kind) {
                self.on_call(node, ancestors);
            }
            if self.rules.match_must_be_result {
                if self.rules.functions.contains(&kind) {
                    if let Some(body) = node.child_by_field_name("body") {
                        let mut matches = Vec::new();
                        arms::result_matches(self.rules, self.source, body, &mut matches);
                        for found in matches {
                            self.on_match(found);
                        }
                    }
                } else if self.rules.returns.contains(&kind) {
                    let mut matches = Vec::new();
                    arms::result_matches(self.rules, self.source, node, &mut matches);
                    for found in matches {
                        self.on_match(found);
                    }
                }
            } else if self.rules.matches.contains(&kind) {
                self.on_match(node);
            }

            let entered = self
                .rules
                .functions
                .contains(&kind)
                .then(|| self.function_at(node))
                .flatten();
            if let Some(function) = entered {
                self.fn_stack.push(function);
            }
            ancestors.push(node);
            let mut cursor = node.walk();
            let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
            let mut test_item = false;
            for child in children {
                let kind = child.kind();
                if self.rules.attributes.contains(&kind) {
                    let attribute = syntax::text(child, self.source);
                    test_item |= self
                        .rules
                        .test_markers
                        .iter()
                        .any(|m| attribute.contains(m));
                } else if test_item && !self.rules.comments.contains(&kind) {
                    test_item = false;
                    if self.in_test == 0 {
                        self.test_regions
                            .push((syntax::start(child).0, syntax::end(child).0));
                    }
                    self.in_test += 1;
                    self.visit(child, ancestors);
                    self.in_test -= 1;
                    continue;
                }
                self.visit(child, ancestors);
            }
            ancestors.pop();
            if entered.is_some() {
                self.fn_stack.pop();
            }
        });
    }

    /// The resolved project call made by `call` (a call node), if any: the
    /// site recorded at its start for the name it calls.
    pub(super) fn project_call(&self, call: Node<'_>) -> Option<usize> {
        if !self.rules.calls.contains(&call.kind()) {
            return None;
        }
        let name = syntax::call_name(self.rules, call, self.source)?;
        let at = syntax::start(call);
        sites_between(self.sites, at, at)
            .iter()
            .map(|pos| pos.site)
            .find(|&site| self.project.call_sites()[site].callee_name == name)
    }

    /// The indexed function a function node is, by its start line.
    fn function_at(&self, node: Node<'_>) -> Option<usize> {
        let line = syntax::start(node).0;
        let at = self
            .functions
            .partition_point(|span| span.start_line < line);
        (at < self.functions.len() && self.functions[at].start_line == line).then_some(at)
    }

    fn on_call<'t>(&mut self, call: Node<'t>, ancestors: &[Node<'t>]) {
        if self.in_test > 0 {
            return;
        }
        let Some(name) = syntax::call_name(self.rules, call, self.source) else {
            return;
        };
        if let Some(&function) = self.fn_stack.last() {
            self.called_names
                .entry(self.functions[function].id.clone())
                .or_default()
                .insert(name.to_string());
        }
        let at = syntax::start(call);
        let matching: Vec<usize> = sites_between(self.sites, at, at)
            .iter()
            .map(|pos| pos.site)
            .filter(|&site| self.project.call_sites()[site].callee_name == name)
            .collect();
        if matching.is_empty() {
            return;
        }
        let used = results::classify(self.rules, self.source, call, ancestors);
        for site in matching {
            self.uses[site] = Some(used);
        }
    }

    fn on_match(&mut self, node: Node<'_>) {
        if self.in_test > 0 || !self.seen_matches.insert(node.id()) {
            return;
        }
        let (line, _) = syntax::start(node);
        if self
            .project
            .enclosing_function(self.file, line)
            .is_some_and(|span| span.is_test)
        {
            return;
        }
        if let Some(deviance) = arms::check(self, node) {
            self.arms.extend(deviance);
        }
    }
}

/// Engler's z statistic for a belief followed at `agree` of `total` sites,
/// against the null hypothesis that it is a coin flip (p0 = 0.5):
/// `z = (agree/total - p0) / sqrt(p0 (1 - p0) / total) = (2 agree - total)
/// / sqrt(total)`, mapped onto 0..1 by `z / (z + 2)` and capped at 0.95.
/// More sites agreeing, fewer departing, rank higher: 3 of 4 → 0.33, 9 of
/// 10 → 0.56, 20 of 21 → 0.67, 50 of 51 → 0.77.
pub(super) fn z_confidence(agree: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let z = (2.0 * agree as f64 - total as f64) / (total as f64).sqrt();
    if z <= 0.0 {
        return 0.0;
    }
    (z / (z + 2.0)).min(0.95)
}

/// A snippet of source on one line, at most `max` chars.
pub(super) fn snippet(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max || text.lines().nth(1).is_some() {
        out.push('…');
    }
    out
}
