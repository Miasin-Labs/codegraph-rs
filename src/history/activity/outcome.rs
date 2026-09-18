//! Build, test and commit outcomes of shell commands: what ran (a masked
//! command template), whether it passed, the first compiler error codes and
//! a hash of its first error lines — never the command's literals or its
//! output.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::history::event::CallResult;
use crate::history::shell::command_segments;

/// Longest template kept.
const MAX_TEMPLATE: usize = 80;
/// Error codes kept per outcome.
const MAX_CODES: usize = 5;
/// Error lines hashed into a signature.
const SIGNATURE_LINES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutcomeKind {
    Build,
    Test,
    Commit,
}

impl OutcomeKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Test => "test",
            Self::Commit => "commit",
        }
    }
}

/// A build/test/commit a shell call ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub kind: OutcomeKind,
    /// Command word, subcommand and option names; values masked (`cargo test -p …`).
    pub template: String,
    /// Passed (`Some(true)`), failed, or unknown.
    pub ok: Option<bool>,
    /// First compiler error codes (`E0425`, `TS2339`).
    pub codes: Vec<String>,
    /// Hash of the normalized first error lines of a failure.
    pub signature: Option<String>,
}

/// Whether an adapter should keep an output excerpt for this command: it
/// builds, tests or commits.
pub(crate) fn keeps_excerpt(command: &str) -> bool {
    command_segments(command)
        .iter()
        .any(|(word, args)| classify(word, args).is_some_and(|o| o.kind != OutcomeKind::Commit))
}

/// The outcome a command segment produces, if it builds, tests or commits.
pub(crate) fn classify(word: &str, args: &[String]) -> Option<Outcome> {
    let first = first_positional(args);
    let second = args
        .iter()
        .filter(|a| !a.starts_with('-') && !a.starts_with('+'))
        .nth(1)
        .map(String::as_str);
    let has = |needle: &str| first.is_some_and(|f| f.contains(needle));
    let kind = match word {
        "cargo" => match first? {
            "test" | "t" | "nextest" | "bench" => OutcomeKind::Test,
            "build" | "b" | "check" | "c" | "clippy" | "doc" | "fmt" => OutcomeKind::Build,
            _ => return None,
        },
        "pytest" | "py.test" | "vitest" | "jest" | "mocha" | "ctest" | "phpunit" | "rspec" => {
            OutcomeKind::Test
        }
        "python" | "python3" if args.iter().any(|a| a == "pytest" || a == "unittest") => {
            OutcomeKind::Test
        }
        "uv" | "poetry" | "npx" | "bunx" | "pnpx" => match (first, second) {
            (Some("run"), Some(tool)) | (Some(tool), _) => tool_kind(tool)?,
            _ => return None,
        },
        "npm" | "pnpm" | "yarn" | "bun" => match (first?, second) {
            ("test" | "t", _) => OutcomeKind::Test,
            ("run" | "run-script", Some(script)) => script_kind(script)?,
            ("build", _) => OutcomeKind::Build,
            (other, _) if word != "npm" => script_kind(other)?,
            _ => return None,
        },
        "go" => match first? {
            "test" => OutcomeKind::Test,
            "build" | "vet" => OutcomeKind::Build,
            _ => return None,
        },
        "make" | "gmake" | "ninja" | "just" => {
            if has("test") || has("check") {
                OutcomeKind::Test
            } else {
                OutcomeKind::Build
            }
        }
        "tsc" | "vue-tsc" | "cmake" | "meson" => OutcomeKind::Build,
        "mvn" | "gradle" | "gradlew" | "dotnet" | "swift" | "zig" => {
            if args.iter().any(|a| a.contains("test")) {
                OutcomeKind::Test
            } else if args
                .iter()
                .any(|a| a.contains("build") || a == "compile" || a == "package")
            {
                OutcomeKind::Build
            } else {
                return None;
            }
        }
        "git" if git_subcommand(args) == Some("commit") => OutcomeKind::Commit,
        _ => return None,
    };
    Some(Outcome {
        kind,
        template: template(word, args),
        ok: None,
        codes: Vec::new(),
        signature: None,
    })
}

/// `git -C dir -c k=v commit …` → `commit`.
fn git_subcommand(args: &[String]) -> Option<&str> {
    let mut it = args.iter().map(String::as_str);
    while let Some(a) = it.next() {
        match a {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" => {
                it.next();
            }
            a if a.starts_with('-') => {}
            a => return Some(a),
        }
    }
    None
}

fn tool_kind(tool: &str) -> Option<OutcomeKind> {
    match tool {
        "pytest" | "vitest" | "jest" | "mocha" => Some(OutcomeKind::Test),
        "tsc" | "vue-tsc" => Some(OutcomeKind::Build),
        _ => None,
    }
}

fn script_kind(script: &str) -> Option<OutcomeKind> {
    if script.contains("test") {
        Some(OutcomeKind::Test)
    } else if ["build", "lint", "typecheck", "type-check", "check", "tsc"]
        .iter()
        .any(|s| script.contains(s))
    {
        Some(OutcomeKind::Build)
    } else {
        None
    }
}

fn first_positional(args: &[String]) -> Option<&str> {
    args.iter()
        .map(String::as_str)
        .find(|a| !a.starts_with('-') && !a.starts_with('+'))
}

/// `cargo test -p codegraph-rs --lib foo` → `cargo test -p … --lib …`: the
/// command word, its subcommand (and a `run` script name), option names;
/// every other literal masked.
pub(crate) fn template(word: &str, args: &[String]) -> String {
    let mut out = String::from(word);
    let mut positionals = 0;
    let mut after_run = false;
    let mut masked = false;
    for arg in args {
        if out.len() >= MAX_TEMPLATE {
            break;
        }
        let keep: Option<String> = if arg.starts_with('+') {
            Some(arg.clone())
        } else if arg.starts_with('-') {
            match arg.split_once('=') {
                Some((flag, _)) => Some(format!("{flag}=…")),
                None => Some(arg.clone()),
            }
        } else {
            positionals += 1;
            let wordy = arg.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | ':')
            }) && arg.len() <= 24;
            let keep = wordy && (positionals == 1 || after_run);
            after_run = positionals == 1 && matches!(arg.as_str(), "run" | "run-script" | "exec");
            keep.then(|| arg.clone())
        };
        match keep {
            Some(tok) => {
                out.push(' ');
                out.push_str(&tok);
                masked = false;
            }
            None if !masked => {
                out.push_str(" …");
                masked = true;
            }
            None => {}
        }
    }
    if out.len() > MAX_TEMPLATE {
        let mut end = MAX_TEMPLATE;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

impl Outcome {
    /// Fill in pass/fail, error codes and the failure signature from the
    /// call's result.
    pub(crate) fn judge(&mut self, result: Option<&CallResult>) {
        let Some(result) = result else {
            return;
        };
        let text = result.excerpt.as_deref().unwrap_or("");
        let reported_failure =
            result.is_error == Some(true) || result.exit_code.is_some_and(|c| c != 0);
        let reported_success = result.exit_code == Some(0) || result.is_error == Some(false);
        let output_failure = self.kind != OutcomeKind::Commit && failure_marked(text);
        self.ok = if reported_failure || output_failure {
            Some(false)
        } else if reported_success {
            Some(true)
        } else {
            None
        };
        if self.ok == Some(false) {
            self.codes = error_codes(text);
            self.signature = signature(text);
        }
    }
}

/// Output that says a build or test failed even when the exit status (a
/// pipe into `tail`) says otherwise.
fn failure_marked(text: &str) -> bool {
    [
        "test result: FAILED",
        "error[E",
        "error: could not compile",
        "npm ERR!",
        "error TS",
        "--- FAIL:",
        "FAILED (failures=",
        "short test summary info",
    ]
    .iter()
    .any(|m| text.contains(m))
}

/// First distinct compiler error codes: rustc `error[E0425]`, tsc `error TS2339`.
pub(crate) fn error_codes(text: &str) -> Vec<String> {
    let mut codes: Vec<String> = Vec::new();
    let mut push = |code: String| {
        if !codes.contains(&code) && codes.len() < MAX_CODES {
            codes.push(code);
        }
    };
    let bytes = text.as_bytes();
    for (at, _) in text.match_indices("error") {
        let rest = &bytes[at + 5..];
        let code = match rest {
            [b'[', b'E', d @ ..]
                if d.len() >= 5 && d[..4].iter().all(u8::is_ascii_digit) && d[4] == b']' =>
            {
                Some(format!(
                    "E{}",
                    std::str::from_utf8(&d[..4]).unwrap_or_default()
                ))
            }
            [b' ', b'T', b'S', d @ ..] => {
                let n = d.iter().take_while(|b| b.is_ascii_digit()).count();
                (4..=5)
                    .contains(&n)
                    .then(|| format!("TS{}", std::str::from_utf8(&d[..n]).unwrap_or_default()))
            }
            _ => None,
        };
        if let Some(code) = code {
            push(code);
        }
    }
    codes
}

/// Hash of the first error lines, numbers masked, so the same failure in
/// another session (other line numbers, timings, addresses) matches.
pub(crate) fn signature(text: &str) -> Option<String> {
    let lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| is_error_line(l))
        .take(SIGNATURE_LINES)
        .map(normalize_line)
        .collect();
    if lines.is_empty() {
        return None;
    }
    let digest = Sha256::digest(lines.join("\n").as_bytes());
    let mut hex = String::with_capacity(16);
    for b in &digest[..8] {
        let _ = write!(hex, "{b:02x}");
    }
    Some(hex)
}

fn is_error_line(line: &str) -> bool {
    line.starts_with("error")
        || line.starts_with("FAILED")
        || line.starts_with("--- FAIL")
        || line.starts_with("E   ")
        || line.contains("panicked at")
        || line.contains(" error TS")
        || line.starts_with("npm ERR!")
}

/// Digits → `#`, whitespace collapsed.
fn normalize_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last_hash = false;
    let mut last_space = false;
    for c in line.chars() {
        if c.is_ascii_digit() {
            if !last_hash {
                out.push('#');
            }
            last_hash = true;
            last_space = false;
        } else if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
            last_hash = false;
        } else {
            out.push(c);
            last_hash = false;
            last_space = false;
        }
    }
    out
}
