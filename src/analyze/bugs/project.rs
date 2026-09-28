//! What the detectors read: the index's functions and resolved call sites
//! (loaded once, in bulk) and each source file parsed on first use.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tree_sitter::Tree;

use super::Finding;
use crate::codegraph::CodeGraph;
use crate::extraction::{create_parser, detect_language};
use crate::search::{is_test_source_file, is_test_symbol};
use crate::types::Language;

/// A function or method as the index records it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FnSpan {
    #[serde(skip)]
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_col: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    pub is_test: bool,
}

impl FnSpan {
    pub fn contains(&self, line: u32) -> bool {
        self.start_line <= line && line <= self.end_line.max(self.start_line)
    }
}

/// One resolved call: which function calls which, and where.
#[derive(Debug, Clone)]
pub struct CallSite {
    pub caller_id: String,
    pub caller: String,
    pub file: String,
    /// 1-based line and 0-based column of the call expression.
    pub line: u32,
    pub col: u32,
    pub callee_id: String,
    pub callee_name: String,
    pub callee_qualified: String,
    pub callee_kind: String,
    pub callee_signature: Option<String>,
    pub callee_return_type: Option<String>,
    pub callee_file: String,
    /// 1-based line where the callee starts.
    pub callee_line: u32,
    /// The caller is test code.
    pub in_test: bool,
}

/// A function a framework route dispatches to (`route` node → `references`
/// edge → function): the index's axum/actix/rocket/express/… routes.
#[derive(Debug, Clone)]
pub struct RouteHandler {
    pub handler_id: String,
    /// The route as the index names it (`GET /upload`).
    pub route: String,
    /// Where the route is registered.
    pub file: String,
    pub line: u32,
}

/// A source file parsed for the syntactic checks.
pub struct ParsedFile {
    pub language: Language,
    pub source: String,
    pub tree: Tree,
    /// Line ranges of test items the syntax marks (`#[cfg(test)] mod …`,
    /// `#[test] fn …`), outermost only, by start line.
    pub test_regions: Vec<(u32, u32)>,
}

/// The indexed project, as the detectors see it.
pub struct Project {
    root: PathBuf,
    files: Vec<String>,
    functions: HashMap<String, Vec<FnSpan>>,
    function_count: usize,
    call_sites: Vec<CallSite>,
    parsed: HashMap<String, Option<ParsedFile>>,
    skipped: BTreeMap<String, usize>,
    /// Every symbol name the index holds (generated files included).
    symbol_names: HashSet<String>,
    routes: Vec<RouteHandler>,
    /// Ids of the functions and methods the index records as public.
    public_fns: HashSet<String>,
}

impl Project {
    /// Read the functions and resolved call sites of `cg`'s index.
    pub fn load(cg: &CodeGraph, root: &Path) -> Result<Self, String> {
        let conn = cg.query_builder().db().conn();
        let err = |e: rusqlite::Error| e.to_string();

        let files: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT path FROM files WHERE IFNULL(generated, 0) = 0 ORDER BY path")
                .map_err(err)?;
            let rows = stmt.query_map([], |row| row.get(0)).map_err(err)?;
            rows.collect::<Result<_, _>>().map_err(err)?
        };

        let mut functions: HashMap<String, Vec<FnSpan>> = HashMap::new();
        let mut function_count = 0;
        {
            let mut stmt = conn
                .prepare(
                    "SELECT id, name, qualified_name, kind, file_path, start_line, end_line, \
                            IFNULL(start_column, 0), signature, return_type \
                     FROM nodes WHERE kind IN ('function', 'method')",
                )
                .map_err(err)?;
            let rows = stmt
                .query_map([], |row| {
                    let file: String = row.get(4)?;
                    let qualified_name: String = row.get(2)?;
                    Ok(FnSpan {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        is_test: is_test_symbol(&file, &qualified_name),
                        qualified_name,
                        kind: row.get(3)?,
                        start_line: row.get(5)?,
                        end_line: row.get(6)?,
                        start_col: row.get(7)?,
                        signature: row.get(8)?,
                        return_type: row.get(9)?,
                        file,
                    })
                })
                .map_err(err)?;
            for span in rows {
                let span = span.map_err(err)?;
                function_count += 1;
                functions.entry(span.file.clone()).or_default().push(span);
            }
        }
        for spans in functions.values_mut() {
            spans.sort_by_key(|span| (span.start_line, std::cmp::Reverse(span.end_line)));
        }

        let call_sites = {
            let mut stmt = conn
                .prepare(
                    "SELECT e.source, s.qualified_name, s.file_path, e.line, IFNULL(e.col, 0), \
                            e.target, t.name, t.qualified_name, t.kind, t.signature, \
                            t.return_type, t.file_path, IFNULL(t.start_line, 0) \
                     FROM edges e \
                     JOIN nodes s ON s.id = e.source \
                     JOIN nodes t ON t.id = e.target \
                     WHERE e.kind = 'calls' AND e.line IS NOT NULL",
                )
                .map_err(err)?;
            let rows = stmt
                .query_map([], |row| {
                    let caller: String = row.get(1)?;
                    let file: String = row.get(2)?;
                    Ok(CallSite {
                        caller_id: row.get(0)?,
                        in_test: is_test_symbol(&file, &caller),
                        caller,
                        file,
                        line: row.get(3)?,
                        col: row.get(4)?,
                        callee_id: row.get(5)?,
                        callee_name: row.get(6)?,
                        callee_qualified: row.get(7)?,
                        callee_kind: row.get(8)?,
                        callee_signature: row.get(9)?,
                        callee_return_type: row.get(10)?,
                        callee_file: row.get(11)?,
                        callee_line: row.get(12)?,
                    })
                })
                .map_err(err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(err)?
        };

        let symbol_names: HashSet<String> = {
            let mut stmt = conn
                .prepare("SELECT DISTINCT name FROM nodes")
                .map_err(err)?;
            let rows = stmt.query_map([], |row| row.get(0)).map_err(err)?;
            rows.collect::<Result<_, _>>().map_err(err)?
        };

        let routes: Vec<RouteHandler> = {
            let mut stmt = conn
                .prepare(
                    "SELECT e.target, r.name, r.file_path, r.start_line \
                     FROM nodes r \
                     JOIN edges e ON e.source = r.id AND e.kind = 'references' \
                     JOIN nodes t ON t.id = e.target \
                     WHERE r.kind = 'route' AND t.kind IN ('function', 'method') \
                     ORDER BY r.file_path, r.start_line",
                )
                .map_err(err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(RouteHandler {
                        handler_id: row.get(0)?,
                        route: row.get(1)?,
                        file: row.get(2)?,
                        line: row.get(3)?,
                    })
                })
                .map_err(err)?;
            rows.collect::<Result<_, _>>().map_err(err)?
        };

        let public_fns: HashSet<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM nodes WHERE kind IN ('function', 'method') \
                     AND (visibility = 'public' OR IFNULL(is_exported, 0) = 1)",
                )
                .map_err(err)?;
            let rows = stmt.query_map([], |row| row.get(0)).map_err(err)?;
            rows.collect::<Result<_, _>>().map_err(err)?
        };

        Ok(Self {
            root: root.to_path_buf(),
            files,
            functions,
            function_count,
            call_sites,
            parsed: HashMap::new(),
            skipped: BTreeMap::new(),
            symbol_names,
            routes,
            public_fns,
        })
    }

    /// Route registrations and the functions they dispatch to.
    pub fn routes(&self) -> &[RouteHandler] {
        &self.routes
    }

    /// Whether the index records function `id` as public.
    pub fn is_public(&self, id: &str) -> bool {
        self.public_fns.contains(id)
    }

    /// Set routes and public functions, for unit tests of [`Self::from_parts`].
    #[cfg(test)]
    pub(crate) fn with_entries(mut self, routes: Vec<RouteHandler>, public: &[&str]) -> Self {
        self.routes = routes;
        self.public_fns = public.iter().map(|id| id.to_string()).collect();
        self
    }

    /// A project from parts, for detector unit tests: sources under `root`,
    /// with the functions and call sites an index would hold.
    #[cfg(test)]
    pub(crate) fn from_parts(
        root: &Path,
        files: Vec<String>,
        functions: Vec<FnSpan>,
        call_sites: Vec<CallSite>,
    ) -> Self {
        let function_count = functions.len();
        let mut by_file: HashMap<String, Vec<FnSpan>> = HashMap::new();
        for span in functions {
            by_file.entry(span.file.clone()).or_default().push(span);
        }
        for spans in by_file.values_mut() {
            spans.sort_by_key(|span| (span.start_line, std::cmp::Reverse(span.end_line)));
        }
        Self {
            root: root.to_path_buf(),
            files,
            functions: by_file,
            function_count,
            call_sites,
            parsed: HashMap::new(),
            skipped: BTreeMap::new(),
            symbol_names: HashSet::new(),
            routes: Vec::new(),
            public_fns: HashSet::new(),
        }
    }

    /// The project's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the index holds a symbol named `name` (a constant, variant,
    /// function… anywhere in the project, generated code included).
    pub fn has_symbol_named(&self, name: &str) -> bool {
        self.symbol_names.contains(name)
    }

    /// Indexed source files (generated ones excluded), path-ordered.
    pub fn files(&self) -> &[String] {
        &self.files
    }

    pub fn call_sites(&self) -> &[CallSite] {
        &self.call_sites
    }

    pub fn function_count(&self) -> usize {
        self.function_count
    }

    /// The functions and methods of `file`, by start line (outer first).
    pub fn functions_in(&self, file: &str) -> &[FnSpan] {
        self.functions.get(file).map_or(&[], Vec::as_slice)
    }

    /// The innermost function or method of `file` containing `line`.
    pub fn enclosing_function(&self, file: &str, line: u32) -> Option<&FnSpan> {
        self.functions_in(file)
            .iter()
            .filter(|span| span.contains(line))
            .max_by_key(|span| span.start_line)
    }

    /// `file` parsed (cached; `None` if unreadable or unsupported, counted
    /// in [`Self::skipped`]).
    pub fn parsed(&mut self, file: &str) -> Option<&ParsedFile> {
        if !self.parsed.contains_key(file) {
            let parsed = self.parse(file);
            self.parsed.insert(file.to_string(), parsed);
        }
        self.parsed.get(file).and_then(Option::as_ref)
    }

    /// `file` as [`Self::parsed`] left it, through a shared borrow so the
    /// index can be read alongside (`None` if not parsed yet or unparsable).
    pub fn parsed_cached(&self, file: &str) -> Option<&ParsedFile> {
        self.parsed.get(file).and_then(Option::as_ref)
    }

    fn parse(&mut self, file: &str) -> Option<ParsedFile> {
        let language = detect_language(file, None);
        let Ok(source) = std::fs::read_to_string(self.root.join(file)) else {
            *self.skipped.entry("unreadable".into()).or_default() += 1;
            return None;
        };
        let Some(mut parser) = create_parser(language) else {
            *self
                .skipped
                .entry(format!("no parser ({})", language.as_str()))
                .or_default() += 1;
            return None;
        };
        let tree = parser.parse(&source, None)?;
        let test_regions = test_regions(language, &source, &tree);
        Some(ParsedFile {
            language,
            source,
            tree,
            test_regions,
        })
    }

    /// Files parsed so far.
    pub fn files_parsed(&self) -> usize {
        self.parsed
            .values()
            .filter(|parsed| parsed.is_some())
            .count()
    }

    pub fn skipped(&self) -> &BTreeMap<String, usize> {
        &self.skipped
    }

    /// Record a file a detector could not analyse.
    pub fn skip(&mut self, reason: &str) {
        *self.skipped.entry(reason.to_string()).or_default() += 1;
    }

    /// The finding sits in test code: a test file, a function the index
    /// names as a test, or an item the syntax marks as one (`#[cfg(test)]
    /// mod …`, which the index does not see). The syntax is read only for a
    /// file already [parsed](Self::parsed).
    pub fn is_test_location(&self, finding: &Finding) -> bool {
        is_test_source_file(&finding.file)
            || self
                .enclosing_function(&finding.file, finding.line)
                .is_some_and(|span| span.is_test)
            || self.parsed_cached(&finding.file).is_some_and(|parsed| {
                parsed
                    .test_regions
                    .iter()
                    .any(|&(start, end)| start <= finding.line && finding.line <= end)
            })
    }
}

/// Outermost line ranges of the items `language`'s test attributes mark
/// (`#[cfg(test)]`, `#[test]`, `#[tokio::test]`…), from the deviance rules
/// table. One walk over the tree; a marked item's insides are not walked.
fn test_regions(language: Language, source: &str, tree: &Tree) -> Vec<(u32, u32)> {
    let Some(rules) = super::deviance::rules::for_language(language) else {
        return Vec::new();
    };
    if rules.test_markers.is_empty() {
        return Vec::new();
    }
    let mut regions = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        let mut marked = false;
        for child in node.named_children(&mut cursor) {
            let kind = child.kind();
            if rules.attributes.contains(&kind) {
                let text = child.utf8_text(source.as_bytes()).unwrap_or("");
                marked |= rules.test_markers.iter().any(|m| text.contains(m));
            } else if rules.comments.contains(&kind) {
                // Doc comments may sit between an attribute and its item.
            } else if marked {
                marked = false;
                regions.push((
                    child.start_position().row as u32 + 1,
                    child.end_position().row as u32 + 1,
                ));
            } else {
                stack.push(child);
            }
        }
    }
    regions.sort_unstable();
    regions
}

#[cfg(test)]
mod test_region_tests {
    use super::*;

    #[test]
    fn cfg_test_modules_and_test_fns_are_test_regions() {
        let source = "fn live() {}\n\
                      #[cfg(test)]\n\
                      mod checks {\n    struct Mock;\n    fn helper() {}\n}\n\
                      /// doc\n\
                      #[tokio::test]\n\
                      async fn smoke() {\n    live();\n}\n\
                      #[derive(Debug)]\n\
                      struct Real;\n";
        let mut parser = create_parser(Language::Rust).unwrap();
        let tree = parser.parse(source, None).unwrap();
        // The module (lines 3–6) and the attributed fn (lines 9–11); a
        // non-test attribute marks nothing.
        assert_eq!(
            test_regions(Language::Rust, source, &tree),
            vec![(3, 6), (9, 11)]
        );
    }
}
