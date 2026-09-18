//! File touches, searched identifiers and build/test outcomes of a
//! (redacted) shell command line.

use std::path::{Path, PathBuf};

use super::outcome::{self, Outcome, OutcomeKind};
use super::{Activity, Op, idents_of_pattern, is_pathish};
use crate::history::event::CallResult;
use crate::history::repo::absolutize;
use crate::history::shell::command_segments;

/// Commands that print (read) the files they're given.
const READERS: &[&str] = &[
    "cat", "head", "tail", "less", "more", "bat", "batcat", "nl", "view", "xxd", "hexdump", "od",
];

/// Commands that search file contents for a pattern.
const GREPS: &[&str] = &["grep", "egrep", "fgrep", "zgrep", "rg", "ag", "ack"];

/// grep/rg options that take a value (the pattern is not that value).
const GREP_VALUE_OPTS: &[&str] = &[
    "-f",
    "-A",
    "-B",
    "-C",
    "-m",
    "--max-count",
    "-g",
    "--glob",
    "--iglob",
    "-t",
    "--type",
    "-T",
    "--type-not",
    "--include",
    "--exclude",
    "--exclude-dir",
    "-d",
    "-D",
    "-j",
    "--threads",
    "--max-depth",
    "--maxdepth",
    "-M",
    "--max-columns",
    "--sort",
    "--sortr",
    "--context",
    "--after-context",
    "--before-context",
    "--type-add",
    "--color",
    "--colour",
];

/// `head -n 5`, `tail -c 10`: options whose value is not a file.
const READER_VALUE_OPTS: &[&str] = &["-n", "-c", "--lines", "--bytes", "-s", "-l", "-r"];

/// Record what `command` (already redacted) did, run from `cwd`.
pub(super) fn analyze(
    command: &str,
    cwd: Option<&Path>,
    result: Option<&CallResult>,
    act: &mut Activity,
) {
    let mut cwd: Option<PathBuf> = cwd.map(Path::to_path_buf);
    let mut outcome: Option<Outcome> = None;
    let mut commit: Option<Outcome> = None;
    for (word, tokens) in command_segments(command) {
        let args = strip_redirects(&tokens, cwd.as_deref(), act);
        match word.as_str() {
            "cd" | "pushd" => {
                if let Some(dir) = args.first().and_then(|d| absolutize(d, cwd.as_deref())) {
                    cwd = Some(dir);
                }
            }
            w if READERS.contains(&w) => {
                for path in positional(&args, READER_VALUE_OPTS) {
                    if is_pathish(path) {
                        touch(act, path, cwd.as_deref(), Op::Read);
                    }
                }
            }
            "sed" => sed(&args, cwd.as_deref(), act),
            "tee" => {
                for path in positional(&args, &[]) {
                    if is_pathish(path) {
                        touch(act, path, cwd.as_deref(), Op::Edit);
                    }
                }
            }
            w if GREPS.contains(&w) => grep(&args, cwd.as_deref(), act),
            "git" if args.first().map(String::as_str) == Some("grep") => {
                grep(&args[1..], cwd.as_deref(), act);
            }
            _ => {}
        }
        // The last build/test of a chain is the one whose output (and exit
        // status) the call reports; a commit counts only when nothing built.
        match outcome::classify(&word, &args) {
            Some(o) if o.kind != OutcomeKind::Commit => outcome = Some(o),
            Some(o) if outcome.is_none() => commit = Some(o),
            _ => {}
        }
    }
    if let Some(mut found) = outcome.or(commit) {
        found.judge(result);
        act.outcome = Some(found);
    }
}

fn touch(act: &mut Activity, path: &str, cwd: Option<&Path>, op: Op) {
    act.touch(Some(path), cwd, op, None);
}

/// Arguments without redirections; `> file` / `>> file` targets are edits.
fn strip_redirects(tokens: &[String], cwd: Option<&Path>, act: &mut Activity) -> Vec<String> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i].as_str();
        let op_len = redirect_prefix(tok);
        if op_len > 0 {
            let is_input = tok[..op_len].contains('<');
            let target = if tok.len() > op_len {
                i += 1;
                &tok[op_len..]
            } else {
                i += 2;
                tokens.get(i - 1).map_or("", String::as_str)
            };
            let edits = !is_input && !target.starts_with('&') && !target.starts_with("/dev/");
            if edits && is_pathish(target) {
                touch(act, target, cwd, Op::Edit);
            }
            continue;
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    out
}

/// Byte length of a leading redirection operator (`>`, `>>`, `2>`, `&>`, `<`), else 0.
fn redirect_prefix(tok: &str) -> usize {
    let b = tok.as_bytes();
    let mut i = 0;
    if b.first().is_some_and(|c| c.is_ascii_digit() || *c == b'&') {
        i = 1;
    }
    match (b.get(i), b.get(i + 1)) {
        (Some(b'>'), Some(b'>')) => i + 2,
        (Some(b'>' | b'<'), _) if !(i == 0 && b.get(1) == Some(&b'(')) => i + 1,
        _ => 0,
    }
}

/// Positional arguments, skipping options (and the value of `value_opts`).
fn positional<'a>(args: &'a [String], value_opts: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if value_opts.contains(&a) {
            i += 2;
            continue;
        }
        if !a.starts_with('-') || a == "-" {
            out.push(a);
        }
        i += 1;
    }
    out
}

fn sed(args: &[String], cwd: Option<&Path>, act: &mut Activity) {
    let in_place = args.iter().any(|a| {
        a.starts_with("-i")
            || a == "--in-place"
            || (a.starts_with('-') && !a.starts_with("--") && a.contains('i') && a.len() <= 4)
    });
    let op = if in_place { Op::Edit } else { Op::Read };
    let explicit_script = args.iter().any(|a| a == "-e" || a == "-f");
    let mut script_seen = explicit_script;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-e" || a == "-f" {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        if !script_seen {
            script_seen = true;
        } else if is_pathish(a) {
            touch(act, a, cwd, op);
        }
        i += 1;
    }
}

fn grep(args: &[String], cwd: Option<&Path>, act: &mut Activity) {
    let mut patterns: Vec<&str> = Vec::new();
    let mut paths: Vec<&str> = Vec::new();
    let mut explicit = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-e" || a == "--regexp" {
            if let Some(p) = args.get(i + 1) {
                patterns.push(p);
            }
            explicit = true;
            i += 2;
            continue;
        }
        if let Some(p) = a.strip_prefix("--regexp=") {
            patterns.push(p);
            explicit = true;
        } else if GREP_VALUE_OPTS.contains(&a) {
            i += 2;
            continue;
        } else if a.starts_with('-') && a.len() > 1 {
        } else if patterns.is_empty() && !explicit {
            patterns.push(a);
        } else {
            paths.push(a);
        }
        i += 1;
    }
    for p in patterns {
        act.idents.extend(idents_of_pattern(p));
    }
    for p in paths {
        if is_pathish(p) && super::has_extension(p) {
            touch(act, p, cwd, Op::Search);
        }
    }
}
