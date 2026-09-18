//! `codegraph_grep` argument handling and the search → page pipeline.

use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::{mcp_output_budget, num_or};
use super::super::schema::ToolResult;
use super::cursor::{Cursor, query_key};
use super::ledger::SentLines;
use super::matcher::{PatternFlags, compile};
use super::page::{Mode, Page};
use super::render::Window;
use super::scan::{ScanLimits, ScanRequest, StopReason, scan};
use super::scope::{GlobFilter, Scope, candidates};
use super::symbols::LiveSource;
use crate::error::Result;
use crate::utils::clamp;

/// Default wall-clock budget of one call. `CODEGRAPH_GREP_DEADLINE_MS`
/// overrides it.
const DEFAULT_DEADLINE: Duration = Duration::from_millis(3_000);
/// A page handed out earlier re-searches a range that already finished once
/// within the deadline (now from a warm cache); it gets this many deadlines,
/// so a range that took nearly the whole budget cannot stall on the same
/// limit call after call.
const RESEARCH_DEADLINES: u32 = 4;
/// Default bytes read by one call. `CODEGRAPH_GREP_MAX_BYTES` overrides it.
const DEFAULT_MAX_BYTES: u64 = 2 << 30;
/// Files larger than this are skipped (and counted in `skippedFiles`).
const MAX_FILE_BYTES: u64 = 64 << 20;
const DEFAULT_MAX_PER_FILE: f64 = 3.0;
const DEFAULT_LIMIT: f64 = 30.0;
/// Longest `before`/`after` context.
const MAX_CONTEXT_LINES: f64 = 50.0;
/// Scanner threads; searching is I/O and memory bound past a handful.
const MAX_THREADS: usize = 8;

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

fn deadline_budget() -> Duration {
    env_number("CODEGRAPH_GREP_DEADLINE_MS").map_or(DEFAULT_DEADLINE, Duration::from_millis)
}

fn byte_budget() -> u64 {
    env_number("CODEGRAPH_GREP_MAX_BYTES")
        .filter(|bytes| *bytes > 0)
        .unwrap_or(DEFAULT_MAX_BYTES)
}

fn flag(args: &Map<String, Value>, name: &str) -> bool {
    matches!(args.get(name), Some(Value::Bool(true)))
        || args.get(name).and_then(Value::as_str) == Some("true")
}

/// A non-negative line count; unlike `num_or`, an explicit 0 stays 0.
fn lines_arg(args: &Map<String, Value>, name: &str) -> Option<u32> {
    let value = match args.get(name)? {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    Some(clamp(value, 0.0, MAX_CONTEXT_LINES) as u32)
}

/// grep's `-B`/`-A`, with `context` (`-C`) filling whichever is unset.
fn window(args: &Map<String, Value>) -> Window {
    let both = lines_arg(args, "context").unwrap_or(0);
    Window {
        before: lines_arg(args, "before").unwrap_or(both),
        after: lines_arg(args, "after").unwrap_or(both),
    }
}

fn optional_string<'a>(args: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_grep(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let started = Instant::now();
        let pattern = match self.validate_string(args.get("pattern"), "pattern") {
            Ok(pattern) => pattern,
            Err(result) => return Ok(result),
        };
        let flags = PatternFlags {
            literal: flag(args, "literal"),
            case_insensitive: flag(args, "caseInsensitive"),
            word: flag(args, "word"),
        };
        let Some(mode) = Mode::parse(optional_string(args, "mode")) else {
            return Ok(self.validation_error_result(
                "mode",
                "mode must be `lines` (numbered hit lines), `count` (per-file counts), or \
                 `files` (file names only)",
                "lines | count | files",
                Some("string"),
            ));
        };
        let regex = match compile(&pattern, flags) {
            Ok(regex) => regex,
            Err(message) => {
                return Ok(self.validation_error_result(
                    "pattern",
                    &format!(
                        "pattern is not a valid regex ({message}). It uses Rust/ripgrep syntax \
                         (`\\|` also alternates, as in grep); pass `literal: true` to search for \
                         the text as written."
                    ),
                    "regex (Rust/ripgrep syntax)",
                    Some("string"),
                ));
            }
        };
        let max_per_file =
            clamp(num_or(args, "maxPerFile", DEFAULT_MAX_PER_FILE), 1.0, 50.0) as usize;
        let limit = clamp(num_or(args, "limit", DEFAULT_LIMIT), 1.0, 500.0) as usize;

        let cg = self.get_code_graph(args.get("projectPath").and_then(Value::as_str))?;
        let root = cg.get_project_root().to_path_buf();
        let Scope { prefix, glob } = Scope::resolve(
            &root,
            optional_string(args, "path"),
            optional_string(args, "glob"),
        );
        let filter = match glob.as_deref().map(GlobFilter::parse) {
            None => None,
            Some(Ok(filter)) => Some(filter),
            Some(Err(message)) => {
                return Ok(self.validation_error_result(
                    "glob",
                    &format!("glob is invalid: {message}"),
                    "glob such as `*.rs` or `src/**/*.{ts,tsx}`",
                    Some("string"),
                ));
            }
        };
        let candidates = candidates(cg.get_file_languages_under(&prefix)?, filter.as_ref());
        if candidates.is_empty() {
            let scope = match (prefix.is_empty(), glob.as_deref()) {
                (true, Some(glob)) => format!("matches glob `{glob}`"),
                (false, Some(glob)) => format!("is under `{prefix}` and matches glob `{glob}`"),
                (false, None) => format!("is under `{prefix}`"),
                (true, None) => "is in this project".to_string(),
            };
            return Ok(self.validation_error_result(
                if glob.is_some() { "glob" } else { "path" },
                &format!(
                    "No indexed file {scope}. codegraph_grep searches the files the index holds \
                     (see codegraph_files); for other files use your own tools."
                ),
                "a path or glob that covers indexed files",
                Some("string"),
            ));
        }

        let key = query_key(
            &pattern,
            flags,
            &prefix,
            glob.as_deref().unwrap_or_default(),
        );
        let cursor = match optional_string(args, "cursor") {
            None => Cursor::FIRST,
            Some(raw) => match Cursor::decode(raw, &key) {
                Some(cursor) => cursor,
                None => {
                    return Ok(self.validation_error_result(
                        "cursor",
                        "cursor does not belong to this search — pass the `nextCursor` of a \
                         previous call with the same pattern, literal, caseInsensitive, path, \
                         and glob, or drop it to start over",
                        "the nextCursor of a previous page",
                        Some("unknown string"),
                    ));
                }
            },
        };

        let sent = SentLines::from_args(args);
        let cancel = self.call_context.cancel_flag();
        let threads = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(MAX_THREADS);
        let deadline = match cursor.end {
            Some(_) => deadline_budget() * RESEARCH_DEADLINES,
            None => deadline_budget(),
        };
        let outcome = scan(&ScanRequest {
            root: &root,
            candidates: &candidates,
            start: cursor.start,
            end: cursor.end,
            regex: &regex,
            // Counts and file names need no hit lines.
            max_per_file: if mode == Mode::Lines { max_per_file } else { 0 },
            sent: &sent,
            limits: ScanLimits {
                deadline: started + deadline,
                max_bytes: byte_budget(),
                max_file_bytes: MAX_FILE_BYTES,
                threads,
            },
            cancel: cancel.as_deref(),
        });
        if outcome.stopped == Some(StopReason::Cancelled) {
            return Ok(self.error_result("Request cancelled by client"));
        }

        let matched: Vec<String> = outcome
            .files
            .iter()
            .map(|hits| candidates[hits.index].path.clone())
            .collect();
        let generated = cg.generated_file_predicate(&matched)?;
        let mut source = LiveSource::new(&cg, &root, MAX_FILE_BYTES);
        let page = Page {
            candidates: &candidates,
            cursor,
            mode,
            limit,
            max_per_file,
            window: window(args),
            regex: &regex,
            budget: mcp_output_budget(),
            key: &key,
        };
        let payload = page.assemble(outcome, |path| generated.is_generated(path), &mut source);
        self.structured_result(&self.truncate_output(&payload.render()), &payload)
    }
}
