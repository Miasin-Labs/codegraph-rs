//! The recall answer as the MCP tool sends it (and the CLI renders it),
//! bounded to [`RECALL_BUDGET`] bytes of compact JSON.

use serde::Serialize;

use super::recall::About;

/// Default bound on a recall answer (bytes of compact JSON).
pub const RECALL_BUDGET: usize = 2048;

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The answer, as the MCP tool sends it.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallReport {
    pub schema_version: u32,
    pub kind: &'static str,
    pub about: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub episodes: Vec<EpisodeRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<SymbolRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<FailureRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cochange: Vec<PairRow>,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One earlier episode (a human prompt and the work that followed).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeRow {
    /// How long ago it started (`3h`, `2d`).
    pub ago: String,
    /// Agent (`claude-code`, `opencode`, `jfc`).
    pub source: String,
    pub calls: i64,
    /// `commit`, `tests-pass`, `tests-fail`, `build-ok`, `build-fail`, `edited`, `explored`.
    pub outcome: String,
    pub files: Vec<FileTouch>,
    #[serde(skip_serializing_if = "is_zero")]
    pub more_files: usize,
}

/// A file an episode touched.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTouch {
    pub path: String,
    /// `edit`, `read` or `search`.
    pub op: &'static str,
    /// Times.
    pub n: i64,
    /// The file is (`true`) or isn't (`false`) what that session last saw,
    /// by the index's content hash. Absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unchanged: Option<bool>,
}

/// Where an identifier was looked up and found.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolRow {
    pub name: String,
    pub lookups: i64,
    pub episodes: i64,
    /// `path:line` (or `path`) read right after looking it up, most common first.
    pub found: Vec<String>,
    pub ago: String,
}

/// A failing build/test run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureRow {
    pub ago: String,
    pub kind: String,
    /// Masked command template (`cargo test -p … --lib`).
    pub command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub codes: Vec<String>,
    /// Runs that failed the same way (same error lines).
    pub repeats: i64,
    /// A later run of the same command passed.
    pub fixed_later: bool,
}

/// Two files edited together.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRow {
    pub a: String,
    pub b: String,
    /// Episodes that edited both.
    pub episodes: i64,
    /// Recent commits that changed both.
    pub commits: i64,
}

impl RecallReport {
    pub(crate) fn new(about: &About) -> Self {
        Self {
            schema_version: 1,
            kind: "recall",
            about: about.label(),
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.episodes.is_empty()
            && self.symbols.is_empty()
            && self.failures.is_empty()
            && self.cochange.is_empty()
    }

    /// Drop the least important detail until the JSON fits `budget`.
    pub fn fit(&mut self, budget: usize) {
        while json_len(self) > budget {
            self.truncated = true;
            let widest = self
                .episodes
                .iter_mut()
                .filter(|e| e.files.len() > 1)
                .max_by_key(|e| e.files.len());
            if let Some(e) = widest {
                e.files.pop();
                e.more_files += 1;
                continue;
            }
            let lists = [
                self.cochange.len(),
                self.failures.len(),
                self.symbols.len(),
                self.episodes.len(),
            ];
            match lists.iter().enumerate().max_by_key(|(_, n)| **n) {
                Some((_, 0)) | None => break,
                Some((0, _)) => {
                    self.cochange.pop();
                }
                Some((1, _)) => {
                    self.failures.pop();
                }
                Some((2, _)) => {
                    self.symbols.pop();
                }
                Some(_) => {
                    self.episodes.pop();
                }
            }
        }
    }

    /// Plain-text rendering (the CLI).
    pub fn render_text(&self) -> String {
        let mut out = format!("Recall `{}`", self.about);
        if let Some(note) = &self.note {
            out.push_str(&format!(" — {note}"));
        }
        out.push('\n');
        for e in &self.episodes {
            out.push_str(&format!(
                "- {} ago · {} · {} calls · {}\n",
                e.ago, e.source, e.calls, e.outcome
            ));
            for f in &e.files {
                let same = match f.unchanged {
                    Some(true) => " (unchanged since)",
                    Some(false) => " (changed since)",
                    None => "",
                };
                out.push_str(&format!("    {} {} ×{}{same}\n", f.op, f.path, f.n));
            }
            if e.more_files > 0 {
                out.push_str(&format!("    … {} more file(s)\n", e.more_files));
            }
        }
        for s in &self.symbols {
            out.push_str(&format!(
                "- {}: looked up {}× in {} episode(s), last {} ago; found in {}\n",
                s.name,
                s.lookups,
                s.episodes,
                s.ago,
                if s.found.is_empty() {
                    "—".to_owned()
                } else {
                    s.found.join(", ")
                }
            ));
        }
        for f in &self.failures {
            out.push_str(&format!(
                "- {} ago · {} `{}`{} · {}× · {}\n",
                f.ago,
                f.kind,
                f.command,
                if f.codes.is_empty() {
                    String::new()
                } else {
                    format!(" {}", f.codes.join(","))
                },
                f.repeats,
                if f.fixed_later {
                    "fixed later"
                } else {
                    "not fixed since"
                }
            ));
        }
        for p in &self.cochange {
            out.push_str(&format!(
                "- {} + {} ({} episode(s), {} commit(s))\n",
                p.a, p.b, p.episodes, p.commits
            ));
        }
        if self.truncated {
            out.push_str("(truncated)\n");
        }
        out
    }
}

pub(crate) fn json_len<T: Serialize>(v: &T) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.len())
}
