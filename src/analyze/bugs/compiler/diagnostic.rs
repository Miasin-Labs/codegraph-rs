//! `cargo clippy --message-format=json` lines → the curated lints'
//! diagnostics, each at a place in the project.
//!
//! A diagnostic's primary span can sit in a macro's body — the project's own
//! `macro_rules!`, or `vec!`/`format!` in std — so the place reported is the
//! outermost expansion call site inside the project (the line someone wrote
//! in a function), with the macro named in a note. Child diagnostics (notes,
//! help, suggestions) and labelled secondary spans become notes; the
//! boilerplate every lint carries (where its level came from, the docs link)
//! is dropped.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::lints::{self, LintInfo};

/// A note attached to a diagnostic: a child message or a labelled span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Project-relative; the diagnostic's own place when the note has none.
    pub file: String,
    pub line: u32,
    pub text: String,
}

/// One curated lint's diagnostic, located in the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilerDiagnostic {
    /// The lint as reported: `clippy::indexing_slicing`, `unused_must_use`.
    pub code: String,
    pub message: String,
    pub file: String,
    pub line: u32,
    /// 1-based, as rustc reports it.
    pub column: u32,
    /// Where the span ends (line, 1-based column, exclusive), for reading
    /// the flagged code's text.
    pub end_line: u32,
    pub end_column: u32,
    pub notes: Vec<Note>,
}

impl CompilerDiagnostic {
    pub fn lint(&self) -> Option<LintInfo> {
        lints::lookup(&self.code)
    }
}

/// What a run's output held.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parsed {
    pub diagnostics: Vec<CompilerDiagnostic>,
    /// Compile errors (the crates they are in were not fully linted).
    pub errors: usize,
    /// The first few compile errors, `file:line: message`.
    pub error_samples: Vec<String>,
}

/// Children every lint carries, which say nothing about the code.
const BOILERPLATE: &[&str] = &[
    "for further information visit",
    "requested on the command line",
    "to override `-",
    "the lint level is defined here",
    "`#[warn(",
    "`#[deny(",
    "`-W ",
    "`-D ",
];

/// Parse cargo's JSON lines. `resolve` maps a span's `file_name` to a
/// project-relative path, or `None` for a file outside the project
/// (dependencies, std).
pub fn parse(stdout: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Parsed {
    let mut parsed = Parsed::default();
    let mut seen = HashSet::new();
    for line in stdout.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["reason"] != "compiler-message" {
            continue;
        }
        let message = &record["message"];
        let code = message["code"]["code"].as_str().unwrap_or_default();
        let level = message["level"].as_str().unwrap_or_default();
        if level.starts_with("error") && (code.is_empty() || code.starts_with('E')) {
            parsed.errors += 1;
            if parsed.error_samples.len() < 5 {
                let place = primary(message)
                    .and_then(|span| locate(span, resolve))
                    .map(|at| format!("{}:{}: ", at.file, at.line))
                    .unwrap_or_default();
                parsed.error_samples.push(format!(
                    "{place}{}",
                    message["message"].as_str().unwrap_or_default()
                ));
            }
            continue;
        }
        if lints::lookup(code).is_none() {
            continue;
        }
        let Some(at) = primary(message).and_then(|span| locate(span, resolve)) else {
            continue;
        };
        if !seen.insert((code.to_string(), at.file.clone(), at.line, at.column)) {
            // `--workspace` checks a file once per target that includes it.
            continue;
        }
        let mut notes = Vec::new();
        if let Some(name) = &at.macro_name {
            notes.push(Note {
                file: at.file.clone(),
                line: at.line,
                text: format!("in the expansion of `{name}`"),
            });
        }
        for span in spans(message) {
            if span["is_primary"] == true {
                continue;
            }
            if let (Some(label), Some(place)) = (span["label"].as_str(), locate(span, resolve)) {
                notes.push(Note {
                    file: place.file,
                    line: place.line,
                    text: label.to_string(),
                });
            }
        }
        for child in message["children"].as_array().into_iter().flatten() {
            let text = child["message"].as_str().unwrap_or_default();
            if text.is_empty() || BOILERPLATE.iter().any(|b| text.starts_with(b)) {
                continue;
            }
            let level = child["level"].as_str().unwrap_or("note");
            let child_span = primary(child).or_else(|| spans(child).next());
            let place = child_span.and_then(|span| locate(span, resolve));
            let suggestion = child_span
                .and_then(|span| span["suggested_replacement"].as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty() && s.len() <= 120)
                .map(|s| format!(": `{}`", s.split_whitespace().collect::<Vec<_>>().join(" ")))
                .unwrap_or_default();
            let (file, line) = place.map_or((at.file.clone(), at.line), |p| (p.file, p.line));
            notes.push(Note {
                file,
                line,
                text: format!("{level}: {text}{suggestion}"),
            });
        }
        parsed.diagnostics.push(CompilerDiagnostic {
            code: code.to_string(),
            message: message["message"].as_str().unwrap_or_default().to_string(),
            file: at.file,
            line: at.line,
            column: at.column,
            end_line: at.end_line,
            end_column: at.end_column,
            notes,
        });
    }
    parsed.diagnostics.sort_by(|a, b| {
        (&a.file, a.line, a.column, &a.code).cmp(&(&b.file, b.line, b.column, &b.code))
    });
    parsed
}

fn spans(message: &Value) -> impl Iterator<Item = &Value> {
    message["spans"].as_array().into_iter().flatten()
}

fn primary(message: &Value) -> Option<&Value> {
    spans(message).find(|span| span["is_primary"] == true)
}

struct Place {
    file: String,
    line: u32,
    column: u32,
    end_line: u32,
    end_column: u32,
    macro_name: Option<String>,
}

/// The outermost place in the project on `span`'s expansion chain: the
/// span itself, or the macro call site that produced it.
fn locate(span: &Value, resolve: &dyn Fn(&str) -> Option<String>) -> Option<Place> {
    let mut best = None;
    let mut macro_name = None;
    let mut at = span;
    for _ in 0..64 {
        if let Some(file) = at["file_name"].as_str().and_then(resolve) {
            best = Some(Place {
                file,
                line: at["line_start"].as_u64().unwrap_or(0) as u32,
                column: at["column_start"].as_u64().unwrap_or(0) as u32,
                end_line: at["line_end"].as_u64().unwrap_or(0) as u32,
                end_column: at["column_end"].as_u64().unwrap_or(0) as u32,
                macro_name: macro_name.clone(),
            });
        }
        let expansion = &at["expansion"];
        if expansion.is_null() {
            break;
        }
        macro_name = expansion["macro_decl_name"].as_str().map(str::to_string);
        at = &expansion["span"];
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINES: &str = include_str!("testdata/clippy.jsonl");

    /// Relative paths are the project's; the registry is not.
    fn resolve(file: &str) -> Option<String> {
        (!file.starts_with('/')).then(|| file.to_string())
    }

    #[test]
    fn keeps_curated_lints_in_the_project_and_counts_errors() {
        let parsed = parse(LINES, &resolve);
        let codes: Vec<(&str, &str, u32)> = parsed
            .diagnostics
            .iter()
            .map(|d| (d.code.as_str(), d.file.as_str(), d.line))
            .collect();
        assert_eq!(
            codes,
            vec![
                (
                    "clippy::significant_drop_in_scrutinee",
                    "src/calendar_api.rs",
                    823
                ),
                ("clippy::unwrap_in_result", "src/email.rs", 684),
                ("unused_must_use", "src/main.rs", 15),
                ("clippy::indexing_slicing", "src/main.rs", 17),
                ("clippy::eq_op", "src/main.rs", 19),
                ("clippy::indexing_slicing", "src/server.rs", 35),
                ("clippy::indexing_slicing", "src/server.rs", 39),
                ("clippy::indexing_slicing", "src/server.rs", 44),
            ],
            "the dependency's eq_op, the style lint and the error are not findings"
        );
        assert_eq!(parsed.errors, 1);
        assert_eq!(parsed.error_samples, ["src/server.rs:4: mismatched types"]);
    }

    #[test]
    fn a_macro_finding_lands_on_its_call_site_and_names_the_macro() {
        let parsed = parse(LINES, &resolve);
        let pick = parsed
            .diagnostics
            .iter()
            .find(|d| d.file == "src/main.rs" && d.code == "clippy::indexing_slicing")
            .unwrap();
        // `pick!(v, 5)` on line 17, not `$v[$i]` in the macro body (line 5).
        assert_eq!((pick.line, pick.column), (17, 13));
        assert!(
            pick.notes
                .iter()
                .any(|n| n.text == "in the expansion of `pick!`"),
            "{:?}",
            pick.notes
        );
    }

    #[test]
    fn notes_keep_help_and_labelled_spans_and_drop_boilerplate() {
        let parsed = parse(LINES, &resolve);
        let index = parsed
            .diagnostics
            .iter()
            .find(|d| d.file == "src/server.rs" && d.line == 35)
            .unwrap();
        assert_eq!(index.column, 5);
        assert_eq!(
            index.notes,
            vec![Note {
                file: "src/server.rs".into(),
                line: 35,
                text: "help: consider using `.get(n)` or `.get_mut(n)` instead".into(),
            }],
            "the docs link and the level's origin are dropped"
        );
        let unwrap = parsed
            .diagnostics
            .iter()
            .find(|d| d.code == "clippy::unwrap_in_result")
            .unwrap();
        assert!(
            unwrap
                .notes
                .iter()
                .any(|n| n.text == "note: in this function signature" && n.line == 683),
            "a child's own span places its note: {:?}",
            unwrap.notes
        );
        let scrutinee = parsed
            .diagnostics
            .iter()
            .find(|d| d.code == "clippy::significant_drop_in_scrutinee")
            .unwrap();
        assert!(
            scrutinee.notes.iter().any(|n| n
                .text
                .starts_with("help: try moving the temporary above the match: `let value")),
            "a suggestion is quoted: {:?}",
            scrutinee.notes
        );
        assert!(
            scrutinee.notes.iter().any(|n| n.text.contains("deadlocks")),
            "{:?}",
            scrutinee.notes
        );
    }

    #[test]
    fn a_diagnostic_reported_twice_is_kept_once() {
        let line = LINES
            .lines()
            .find(|l| l.contains("clippy::eq_op") && l.contains("src/main.rs"))
            .unwrap();
        let twice = format!("{line}\n{line}\n");
        assert_eq!(parse(&twice, &resolve).diagnostics.len(), 1);
    }
}
