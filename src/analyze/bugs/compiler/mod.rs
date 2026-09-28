//! The compiler as a detector family: rustc's and clippy's lints, which see
//! real types, trait impls and macro expansion, brought into `analyze bugs`
//! where they gain the graph's context.
//!
//! `cargo clippy --offline --message-format=json` runs through the
//! diagnostics runner (`crate::diagnostics::clippy_lints_or_poll`: detached,
//! waited on at most [`BugsOptions::compiler_wait`], picked up by the next
//! call while still going) with the curated lint set of [`lints`]. A
//! finished run is cached under a fingerprint of the lockfile, the sources
//! and the lint arguments ([`cache`]), so asking again with nothing changed
//! does not start cargo.
//!
//! Each diagnostic becomes a [`Finding`] at its primary span (a macro's
//! call site when the span is in a macro body), in the enclosing indexed
//! function, as rule `clippy::<lint>` / `rustc::<lint>`, with the compiler's
//! notes as evidence. Its confidence is the lint's measured precision class,
//! moved by reachability ([`super::reach`], shared with the rules engine): a lint that is only a bug on untrusted
//! input (`indexing_slicing`, `unwrap_used`…) rises toward its class's
//! ceiling (2.5× its base, ≤0.9) in a function a route handler reaches and
//! halves where no entry point reaches it, and
//! the path from the handler is part of the evidence. Test code, `codegraph:
//! ignore` comments and `#[allow(…)]` (clippy's own) filter as for every
//! detector.

mod cache;
mod diagnostic;
pub mod lints;
mod shapes;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use self::diagnostic::{CompilerDiagnostic, Parsed};
use self::lints::{Exposure, LintInfo};
use self::shapes::Shape;
use super::reach::{EntryKind, Reach, Reached};
use super::{BugsOptions, Detector, Evidence, Finding, Project};
use crate::diagnostics::{RunStatus, clippy_lints_or_poll, diagnostics_dir};

/// How long `analyze bugs --detector compiler` waits for cargo when the
/// caller sets no limit. The run keeps going past it; the next call picks
/// it up.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(300);

/// The runner's state-file name for these runs (`.codegraph/diagnostics/`).
const RUN_NAME: &str = "compiler";

/// Where the compiler run stands, reported beside the findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CompilerState {
    /// Finished (now or earlier: see `cached`); findings are complete.
    Complete,
    /// Still building in the background; call again for the findings.
    Running,
    /// Not a Cargo project, or cargo could not be started.
    Unavailable,
}

/// The compiler run behind a report's compiler findings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompilerStatus {
    pub state: CompilerState,
    /// Answered from the result cache (nothing changed since that run).
    pub cached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<u64>,
    /// Sources changed after the run started; its findings may be out of
    /// date and it was not cached.
    pub stale: bool,
    /// Curated lint diagnostics the run reported (before any filtering).
    pub diagnostics: usize,
    /// Compile errors: the crates they are in were not fully linted.
    pub compile_errors: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub error_samples: Vec<String>,
    /// Why cargo failed (stderr tail), or why nothing ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// Entry points reachability was measured from, and their kind
    /// (`routes`, `request extractors`, `public API`, or `none`).
    pub entry_points: usize,
    pub entry_kind: &'static str,
}

impl CompilerStatus {
    fn unavailable(why: String) -> Self {
        Self {
            state: CompilerState::Unavailable,
            cached: false,
            started_ms: None,
            finished_ms: None,
            stale: false,
            diagnostics: 0,
            compile_errors: 0,
            error_samples: Vec::new(),
            failure: Some(why),
            entry_points: 0,
            entry_kind: "none",
        }
    }

    /// Whether the project built: the run finished with no compile error
    /// and no cargo failure.
    pub fn built(&self) -> bool {
        self.state == CompilerState::Complete && self.compile_errors == 0 && self.failure.is_none()
    }

    /// One line for a person.
    pub fn summary(&self) -> String {
        match self.state {
            CompilerState::Unavailable => format!(
                "compiler detector unavailable: {}",
                self.failure.as_deref().unwrap_or("unknown reason")
            ),
            CompilerState::Running => "cargo clippy is still running in the background; run \
                                       again to pick up its findings"
                .to_string(),
            CompilerState::Complete => {
                let mut line = format!(
                    "cargo clippy: {} lint diagnostic{}{}; reachability from {} entry point{} \
                     ({})",
                    self.diagnostics,
                    if self.diagnostics == 1 { "" } else { "s" },
                    if self.cached { " (cached)" } else { "" },
                    self.entry_points,
                    if self.entry_points == 1 { "" } else { "s" },
                    self.entry_kind
                );
                if self.compile_errors > 0 {
                    line.push_str(&format!(
                        "; {} compile error{} (those crates were not fully linted)",
                        self.compile_errors,
                        if self.compile_errors == 1 { "" } else { "s" }
                    ));
                } else if self.failure.is_some() {
                    line.push_str("; cargo failed (see `compiler.failure`)");
                }
                if self.stale {
                    line.push_str("; sources changed during the run — run again");
                }
                line
            }
        }
    }
}

/// Run (or pick up, or read from the cache) the compiler lints of the
/// project and turn them into findings.
pub(super) fn detect(project: &Project, options: &BugsOptions) -> (Vec<Finding>, CompilerStatus) {
    let root = project.root().to_path_buf();
    if !root.join("Cargo.toml").is_file() {
        return (
            Vec::new(),
            CompilerStatus::unavailable(format!("no Cargo.toml at {}", root.display())),
        );
    }
    let args = lints::driver_args();
    let fingerprint = cache::fingerprint(&root, project.files(), &args);
    let dir = diagnostics_dir(&root);

    let (parsed, mut status) = match cache::load(&dir, &fingerprint.key) {
        Some(cached) => {
            let status = CompilerStatus {
                state: CompilerState::Complete,
                cached: true,
                started_ms: Some(cached.started_ms),
                finished_ms: Some(cached.finished_ms),
                stale: false,
                diagnostics: 0,
                compile_errors: 0,
                error_samples: Vec::new(),
                failure: cached.failure,
                entry_points: 0,
                entry_kind: "none",
            };
            (cached.parsed, status)
        }
        None => {
            let wait = options.compiler_wait.unwrap_or(DEFAULT_WAIT);
            let run = match clippy_lints_or_poll(&root, RUN_NAME, &args, wait, &|| false) {
                Ok(run) => run,
                Err(error) => return (Vec::new(), CompilerStatus::unavailable(error)),
            };
            let mut status = CompilerStatus {
                state: CompilerState::Running,
                cached: false,
                started_ms: Some(run.started_ms),
                finished_ms: run.finished_ms,
                stale: false,
                diagnostics: 0,
                compile_errors: 0,
                error_samples: Vec::new(),
                failure: None,
                entry_points: 0,
                entry_kind: "none",
            };
            if run.status == RunStatus::Running {
                return (Vec::new(), status);
            }
            status.state = CompilerState::Complete;
            let indexed: HashSet<&str> = project.files().iter().map(String::as_str).collect();
            let resolve = |name: &str| resolve_file(&root, &indexed, name);
            let parsed = diagnostic::parse(&run.stdout, &resolve);
            // Keyed by the state after the run (cargo may have written the
            // lockfile); a run that started before the newest edit missed it
            // and is not cached.
            let after = cache::fingerprint(&root, project.files(), &args);
            status.stale = after.newest_ms > run.started_ms;
            status.failure = run.failure.clone();
            if !status.stale {
                cache::store(
                    &dir,
                    &after.key,
                    run.started_ms,
                    run.finished_ms.unwrap_or(run.started_ms),
                    &parsed,
                    run.failure.as_deref(),
                );
            }
            (parsed, status)
        }
    };
    status.diagnostics = parsed.diagnostics.len();
    status.compile_errors = parsed.errors;
    status.error_samples = parsed.error_samples.clone();

    let reach = project.reach();
    status.entry_points = reach.entries.len();
    status.entry_kind = match reach.strongest_kind() {
        Some(EntryKind::Route) => "routes",
        Some(EntryKind::Extractor) => "request extractors",
        Some(EntryKind::Listener) => "listeners",
        Some(EntryKind::Message) => "message handlers",
        Some(EntryKind::PublicApi) => "public API functions",
        None => "none found",
    };
    (findings(project, reach, &parsed), status)
}

/// A span's file as a project-relative path, when it is one of the files
/// the index holds (never a dependency, std, or build-script output under
/// `target/`). Cargo names workspace files relative to the workspace root,
/// which may be an ancestor of the project root.
fn resolve_file(root: &Path, indexed: &HashSet<&str>, name: &str) -> Option<String> {
    let path = Path::new(name);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.ancestors()
            .map(|ancestor| ancestor.join(path))
            .find(|candidate| candidate.is_file())?
    };
    let relative = full
        .strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .or_else(|| {
            let canonical = root.canonicalize().ok()?;
            let full = full.canonicalize().ok()?;
            full.strip_prefix(canonical)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
        })?;
    indexed.contains(relative.as_str()).then_some(relative)
}

/// The findings of `parsed`, ranked by reachability. Where clippy reports
/// both `unwrap_in_result` and `unwrap_used`/`expect_used` for one call, only
/// the first (which says the error could have been returned) is kept.
fn findings(project: &Project, reach: &Reach, parsed: &Parsed) -> Vec<Finding> {
    let in_result: HashSet<(&str, u32)> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.code == "clippy::unwrap_in_result")
        .map(|d| (d.file.as_str(), d.line))
        .collect();
    let mut sources: HashMap<&str, Option<String>> = HashMap::new();
    let mut out = Vec::new();
    for diagnostic in &parsed.diagnostics {
        let absorbed = matches!(
            diagnostic.code.as_str(),
            "clippy::unwrap_used" | "clippy::expect_used"
        ) && in_result.contains(&(diagnostic.file.as_str(), diagnostic.line));
        let Some(lint) = diagnostic.lint().filter(|_| !absorbed) else {
            continue;
        };
        let source = sources
            .entry(diagnostic.file.as_str())
            .or_insert_with(|| std::fs::read_to_string(project.root().join(&diagnostic.file)).ok());
        let text = source
            .as_deref()
            .map(|source| span_text(source, diagnostic))
            .unwrap_or_default();
        let shape = shapes::judge(lint.name, &diagnostic.message, &text);
        if matches!(shape, Shape::Drop(_)) {
            continue;
        }
        let function = project.enclosing_function(&diagnostic.file, diagnostic.line);
        let reached = function.and_then(|span| reach.reached(&span.id));
        let mut finding = finding(
            diagnostic,
            &lint,
            function.map(|span| span.qualified_name.clone()),
            reached.as_ref(),
        );
        if let Shape::Discount(factor, why) = shape {
            finding.confidence = ((finding.confidence * factor) * 1000.0).round() / 1000.0;
            finding.evidence.push(Evidence {
                file: finding.file.clone(),
                line: finding.line,
                note: format!("ranked lower: {why}"),
            });
        }
        out.push(finding);
    }
    out
}

/// The source text of a diagnostic's primary span (rustc's columns count
/// characters, 1-based, end exclusive).
fn span_text(source: &str, diagnostic: &CompilerDiagnostic) -> String {
    let (first, last) = (diagnostic.line, diagnostic.end_line.max(diagnostic.line));
    let mut out = String::new();
    for (index, text) in source
        .lines()
        .enumerate()
        .skip(first.saturating_sub(1) as usize)
        .take((last - first + 1).min(64) as usize)
    {
        let n = index as u32 + 1;
        let from = if n == first {
            diagnostic.column.saturating_sub(1) as usize
        } else {
            0
        };
        let to = if n == last && diagnostic.end_column > 0 {
            diagnostic.end_column.saturating_sub(1) as usize
        } else {
            usize::MAX
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.extend(text.chars().skip(from).take(to.saturating_sub(from)));
    }
    out
}

fn finding(
    diagnostic: &CompilerDiagnostic,
    lint: &LintInfo,
    function: Option<String>,
    reached: Option<&Reached<'_>>,
) -> Finding {
    let mut message = diagnostic.message.clone();
    let mut evidence: Vec<Evidence> = diagnostic
        .notes
        .iter()
        .map(|note| Evidence {
            file: note.file.clone(),
            line: note.line,
            note: note.text.clone(),
        })
        .collect();
    if let Some(reached) = reached {
        let entry = reached.entry;
        let (what, weight) = match entry.kind {
            EntryKind::Route => (format!("route `{}`", entry.label), "request"),
            EntryKind::Extractor => (
                format!("handler `{}` ({})", entry.name, entry.label),
                "request",
            ),
            EntryKind::Listener => (
                format!("listener `{}` ({})", entry.name, entry.label),
                "peer",
            ),
            EntryKind::Message => (
                format!("message handler `{}` ({})", entry.name, entry.label),
                "message",
            ),
            EntryKind::PublicApi => (format!("public `{}`", entry.name), "caller"),
        };
        if lint.exposure == Exposure::Input || entry.kind != EntryKind::PublicApi {
            message.push_str(&format!(
                " — reachable from {what}{}",
                match reached.depth {
                    0 => String::new(),
                    1 => ", 1 call away".to_string(),
                    n => format!(", {n} calls away"),
                }
            ));
        }
        evidence.push(Evidence {
            file: entry.file.clone(),
            line: entry.line,
            note: format!(
                "entry point: {what} → `{}`; a {weight} controls what reaches here",
                entry.name
            ),
        });
        for step in &reached.path {
            evidence.push(Evidence {
                file: step.file.clone(),
                line: step.line,
                note: format!("`{}` calls `{}`", step.caller, step.callee),
            });
        }
    }
    Finding {
        detector: Detector::Compiler,
        rule: lint.rule().into(),
        file: diagnostic.file.clone(),
        line: diagnostic.line,
        col: diagnostic.column.saturating_sub(1),
        function,
        message,
        confidence: confidence(lint, reached),
        evidence,
    }
}

/// The lint's precision class, moved by who can reach the code: an input
/// lint rises toward its [`Precision::reach_ceiling`] near a request
/// handler (halfway there near a library's public API), decaying ×0.85 per
/// call, and halves where nothing reaches it; any other lint gains a little
/// near a handler.
///
/// [`Precision::reach_ceiling`]: lints::Precision::reach_ceiling
fn confidence(lint: &LintInfo, reached: Option<&Reached<'_>>) -> f64 {
    let base = lint.precision.confidence();
    let value = match (lint.exposure, reached) {
        (Exposure::Input, Some(reached)) => {
            let full = lint.precision.reach_ceiling();
            let ceiling = match reached.entry.kind {
                EntryKind::Route
                | EntryKind::Extractor
                | EntryKind::Listener
                | EntryKind::Message => full,
                EntryKind::PublicApi => (base + full) / 2.0,
            };
            let decay = 0.85f64.powi(reached.depth as i32);
            base + (ceiling - base).max(0.0) * decay
        }
        (Exposure::Input, None) => base * 0.5,
        (Exposure::Any, Some(reached)) if reached.entry.kind != EntryKind::PublicApi => {
            (base + 0.05).min(0.95)
        }
        (Exposure::Any, _) => base,
    };
    (value * 1000.0).round() / 1000.0
}

/// The review checklist a compiler finding adds: its lint's questions.
pub fn review_questions(finding: &Finding) -> Vec<String> {
    if finding.detector != Detector::Compiler {
        return Vec::new();
    }
    let rule = finding.rule.as_ref();
    let code = rule.strip_prefix("rustc::").unwrap_or(rule);
    let Some(lint) = lints::lookup(code) else {
        return Vec::new();
    };
    let mut questions: Vec<String> = lint.questions.iter().map(|q| q.to_string()).collect();
    if lint.questions.is_empty() {
        questions.push(format!(
            "clippy's `{}` fires on code that is usually wrong. Is it wrong here, or is the \
             pattern deliberate (then `#[allow({rule})]` with the reason)?",
            lint.name
        ));
    }
    if let Some(entry) = finding
        .evidence
        .iter()
        .find(|e| e.note.starts_with("entry point: "))
    {
        questions.insert(
            0,
            format!(
                "Reachability: {} (see the call path). Does a value the caller controls reach \
                 this line unchecked?",
                entry.note.trim_start_matches("entry point: ")
            ),
        );
    }
    questions
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::diagnostic::Note;
    use super::*;
    use crate::analyze::bugs::project::RouteHandler;
    use crate::analyze::bugs::reach::tests::{call, span};

    fn project() -> Project {
        // handler → parse → index_at (indexing), and a tool nothing reaches.
        let functions = vec![
            span("handler", "src/api.rs", 1, 5, "(body: Vec<u8>) -> u8"),
            span("parse", "src/api.rs", 7, 12, "(b: &[u8]) -> u8"),
            span("tool", "src/tool.rs", 1, 9, "(t: &[u8]) -> u8"),
        ];
        let calls = vec![call("handler", "parse", "src/api.rs", 3)];
        Project::from_parts(
            Path::new("/p"),
            vec!["src/api.rs".into(), "src/tool.rs".into()],
            functions,
            calls,
        )
        .with_entries(
            vec![RouteHandler {
                handler_id: "handler".into(),
                route: "POST /items".into(),
                file: "src/main.rs".into(),
                line: 20,
            }],
            &[],
        )
    }

    fn diagnostic(code: &str, file: &str, line: u32) -> CompilerDiagnostic {
        CompilerDiagnostic {
            code: code.into(),
            message: "indexing may panic".into(),
            file: file.into(),
            line,
            column: 9,
            end_line: line,
            end_column: 13,
            notes: vec![Note {
                file: file.into(),
                line,
                text: "help: consider using `.get(n)` or `.get_mut(n)` instead".into(),
            }],
        }
    }

    #[test]
    fn a_diagnostic_maps_to_a_finding_in_its_function_with_the_path() {
        let project = project();
        let reach = project.reach();
        let parsed = Parsed {
            diagnostics: vec![diagnostic("clippy::indexing_slicing", "src/api.rs", 9)],
            ..Parsed::default()
        };
        let found = findings(&project, reach, &parsed);
        assert_eq!(found.len(), 1);
        let finding = &found[0];
        assert_eq!(finding.detector, Detector::Compiler);
        assert_eq!(finding.rule, "clippy::indexing_slicing");
        assert_eq!((finding.line, finding.col), (9, 8), "col is 0-based");
        assert_eq!(finding.function.as_deref(), Some("m::parse"));
        assert!(
            finding
                .message
                .ends_with("reachable from route `POST /items`, 1 call away"),
            "{}",
            finding.message
        );
        let notes: Vec<&str> = finding.evidence.iter().map(|e| e.note.as_str()).collect();
        assert_eq!(
            notes[0],
            "help: consider using `.get(n)` or `.get_mut(n)` instead"
        );
        assert!(notes[1].starts_with("entry point: route `POST /items` → `m::handler`"));
        assert_eq!(notes[2], "`m::handler` calls `m::parse`");
        assert_eq!(finding.evidence[2].line, 3);

        let questions = review_questions(finding);
        assert!(questions[0].starts_with("Reachability: route `POST /items`"));
        assert!(questions.iter().any(|q| q.contains("Trace it back")));
    }

    #[test]
    fn reachability_ranks_input_lints_and_barely_moves_the_rest() {
        let project = project();
        let reach = project.reach();
        let parsed = Parsed {
            diagnostics: vec![
                diagnostic("clippy::indexing_slicing", "src/api.rs", 3),
                diagnostic("clippy::indexing_slicing", "src/api.rs", 9),
                diagnostic("clippy::indexing_slicing", "src/tool.rs", 4),
                diagnostic("clippy::eq_op", "src/api.rs", 9),
                diagnostic("clippy::eq_op", "src/tool.rs", 4),
                diagnostic("clippy::needless_return", "src/api.rs", 9),
            ],
            ..Parsed::default()
        };
        let found = findings(&project, reach, &parsed);
        assert_eq!(found.len(), 5, "the style lint is not a finding");
        let at = |rule: &str, file: &str, line: u32| {
            found
                .iter()
                .find(|f| f.rule == rule && f.file == file && f.line == line)
                .unwrap()
                .confidence
        };
        let in_handler = at("clippy::indexing_slicing", "src/api.rs", 3);
        let one_call = at("clippy::indexing_slicing", "src/api.rs", 9);
        let unreached = at("clippy::indexing_slicing", "src/tool.rs", 4);
        assert_eq!(
            in_handler, 0.375,
            "a rare lint rises to 2.5x its base in a handler"
        );
        assert!(0.15 < one_call && one_call < in_handler, "{one_call}");
        assert_eq!(unreached, 0.075, "an input lint nothing reaches halves");
        assert!(
            at("clippy::eq_op", "src/tool.rs", 4) > in_handler,
            "correctness outranks reach"
        );
        assert_eq!(at("clippy::eq_op", "src/tool.rs", 4), 0.8);
        assert_eq!(at("clippy::eq_op", "src/api.rs", 9), 0.85);
    }

    #[test]
    fn unwrap_in_result_absorbs_unwrap_used_on_its_line() {
        let project = project();
        let reach = project.reach();
        let parsed = Parsed {
            diagnostics: vec![
                diagnostic("clippy::unwrap_used", "src/api.rs", 9),
                diagnostic("clippy::unwrap_in_result", "src/api.rs", 9),
                diagnostic("clippy::unwrap_used", "src/api.rs", 10),
            ],
            ..Parsed::default()
        };
        let rules: Vec<(String, u32)> = findings(&project, reach, &parsed)
            .into_iter()
            .map(|f| (f.rule.to_string(), f.line))
            .collect();
        assert_eq!(
            rules,
            [
                ("clippy::unwrap_in_result".to_string(), 9),
                ("clippy::unwrap_used".to_string(), 10)
            ]
        );
    }

    #[test]
    fn only_indexed_project_files_are_places() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("crate");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target/debug/build/x/out")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "").unwrap();
        std::fs::write(root.join("target/debug/build/x/out/gen.rs"), "").unwrap();
        let indexed: HashSet<&str> = ["src/lib.rs"].into_iter().collect();
        assert_eq!(
            resolve_file(&root, &indexed, "src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
        let absolute = root.join("src/lib.rs");
        assert_eq!(
            resolve_file(&root, &indexed, absolute.to_str().unwrap()).as_deref(),
            Some("src/lib.rs")
        );
        let generated = root.join("target/debug/build/x/out/gen.rs");
        assert_eq!(
            resolve_file(&root, &indexed, generated.to_str().unwrap()),
            None
        );
        assert_eq!(resolve_file(&root, &indexed, "/elsewhere/lib.rs"), None);
        // A workspace member reported relative to the workspace root above.
        assert_eq!(
            resolve_file(&root, &indexed, "crate/src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
    }
}
