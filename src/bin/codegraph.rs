//! CodeGraph CLI
//!
//! Command-line interface for CodeGraph code intelligence.
//! Port of `src/bin/codegraph.ts` (commander → clap derive).
//!
//! Usage:
//!   codegraph                    Run interactive installer (when no args)
//!   codegraph install            Run interactive installer
//!   codegraph uninstall          Remove CodeGraph from your agents
//!   codegraph init [path]        Initialize CodeGraph in a project
//!   codegraph uninit [path]      Remove CodeGraph from a project
//!   codegraph index [path]       Index all files in the project
//!   codegraph sync [path]        Sync changes since last index
//!   codegraph status [path]      Show index status
//!   codegraph query <search>     Search for symbols
//!   codegraph files [options]    Show project file structure
//!   codegraph callers <symbol>   Find what calls a function/method
//!   codegraph callees <symbol>   Find what a function/method calls
//!   codegraph impact <symbol>    Analyze what code is affected by changing a symbol
//!   codegraph affected [files]   Find test files affected by changes
//!   codegraph serve --mcp        Run as an MCP server over stdio
//!   codegraph unlock [path]      Remove a stale lock file
//!
//! Node-ecosystem-only pieces of the TS entry point that have no Rust
//! equivalent (documented in notes/cli.md): the Node 25.x hard block +
//! minimum-Node-version banner (`node-version-check.ts`), the
//! `--liftoff-only` WASM re-exec (`wasm-runtime-flags.ts`), and the npm
//! `preuninstall` script (`uninstall.ts`).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use codegraph::directory::{get_codegraph_dir, is_initialized};
use codegraph::extraction::is_generated_file;
use codegraph::installer::targets::{Location, get_target, list_target_ids};
use codegraph::installer::{
    RunInstallerOptions,
    RunUninstallerOptions,
    offer_watch_fallback,
    run_installer,
    run_installer_with_options,
    run_uninstaller,
};
use codegraph::sync::{
    DEFAULT_SYNC_HOOKS,
    detect_worktree_index_mismatch,
    remove_git_sync_hook,
    worktree_mismatch_warning,
};
use codegraph::ui::{IndexProgress as UiIndexProgress, create_shimmer_progress, get_glyphs};
use codegraph::utils::lexical_resolve;
use codegraph::{
    CodeGraph,
    ExtractionError,
    FileRecord,
    IndexOptions,
    IndexProgress,
    IndexResult,
    InitOptions,
    MCPServer,
    NodeKind,
    OpenOptions,
    SearchOptions,
    Severity,
};

// =============================================================================
// ANSI Color Helpers (avoid chalk ESM issues — TS kept raw escapes; so do we)
// =============================================================================

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const CYAN: &str = "\x1b[36m";
const WHITE: &str = "\x1b[37m";
#[allow(dead_code)]
const GRAY: &str = "\x1b[90m";

fn bold(s: &str) -> String {
    format!("{BOLD}{s}{RESET}")
}
fn dim(s: &str) -> String {
    format!("{DIM}{s}{RESET}")
}
fn red(s: &str) -> String {
    format!("{RED}{s}{RESET}")
}
fn green(s: &str) -> String {
    format!("{GREEN}{s}{RESET}")
}
fn yellow(s: &str) -> String {
    format!("{YELLOW}{s}{RESET}")
}
fn blue(s: &str) -> String {
    format!("{BLUE}{s}{RESET}")
}
fn cyan(s: &str) -> String {
    format!("{CYAN}{s}{RESET}")
}
fn white(s: &str) -> String {
    format!("{WHITE}{s}{RESET}")
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Resolve project path from argument or current directory.
/// Walks up parent directories to find nearest initialized CodeGraph project
/// (must have .codegraph/codegraph.db, not just .codegraph/lessons.db).
fn resolve_project_path(path_arg: Option<&str>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let absolute = match path_arg {
        Some(p) if !p.is_empty() => lexical_resolve(&cwd, p),
        _ => cwd,
    };

    // If exact path is initialized (has codegraph.db), use it
    if is_initialized(&absolute) {
        return absolute;
    }

    // Walk up to find nearest parent with CodeGraph initialized (the TS loop
    // checks every parent up to and including the filesystem root).
    for ancestor in absolute.ancestors().skip(1) {
        if is_initialized(ancestor) {
            return ancestor.to_path_buf();
        }
    }

    // Not found - return original path (will fail later with helpful error)
    absolute
}

/// `path.resolve(pathArg || process.cwd())` parity (no walk-up).
fn resolve_absolute(path_arg: Option<&str>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    match path_arg {
        Some(p) if !p.is_empty() => lexical_resolve(&cwd, p),
        _ => cwd,
    }
}

/// Format a number with commas (`n.toLocaleString()` — en-US grouping, the
/// published CLI's default locale).
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// JS `Number.prototype.toFixed` approximation (round half away from zero).
fn js_to_fixed(value: f64, digits: u32) -> String {
    let factor = 10f64.powi(digits as i32);
    let rounded = (value * factor).round() / factor;
    format!("{:.*}", digits as usize, rounded)
}

/// Format duration in milliseconds to human readable.
fn format_duration(ms: i64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let seconds = ms as f64 / 1000.0;
    if seconds < 60.0 {
        return format!("{}s", js_to_fixed(seconds, 1));
    }
    let minutes = (seconds / 60.0).floor() as i64;
    let remaining_seconds = seconds % 60.0;
    format!("{minutes}m {}s", js_to_fixed(remaining_seconds, 0))
}

/// `parseInt(s, 10)` parity: optional sign + leading digit run; None == NaN.
fn parse_int_js(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let (negative, rest) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits
        .parse::<i64>()
        .ok()
        .map(|v| if negative { -v } else { v })
}

/// Epoch milliseconds (`Date.now()` parity).
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Howard Hinnant's `civil_from_days` — days since 1970-01-01 → (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `new Date(ms).toISOString()` parity: `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC).
fn iso_from_epoch_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

/// Print success message (TS `success()` — stdout).
fn success(message: &str) {
    println!("{} {message}", green(get_glyphs().ok));
}

/// Print error message (TS `error()` — `console.error`, stderr).
fn error_msg(message: &str) {
    eprintln!("{} {message}", red(get_glyphs().err));
}

/// Print info message (TS `info()` — stdout).
fn info(message: &str) {
    println!("{} {message}", blue(get_glyphs().info));
}

/// Print warning message (TS `warn()` — stdout).
fn warn(message: &str) {
    println!("{} {message}", yellow(get_glyphs().warn));
}

// =============================================================================
// @clack/prompts replacements (same flow & wording; plain stdout rendering,
// matching the installer module's clack adaptation — see notes/cli.md)
// =============================================================================

fn clack_intro(msg: &str) {
    println!("{msg}");
}
fn clack_outro(msg: &str) {
    println!("{msg}");
}
fn clack_log_success(msg: &str) {
    println!("{} {msg}", green(get_glyphs().ok));
}
fn clack_log_info(msg: &str) {
    println!("{} {msg}", blue(get_glyphs().info));
}
fn clack_log_warn(msg: &str) {
    println!("{} {msg}", yellow(get_glyphs().warn));
}
fn clack_log_error(msg: &str) {
    println!("{} {msg}", red(get_glyphs().err));
}
fn clack_note(body: &str, title: &str) {
    println!("{title}:");
    for line in body.lines() {
        println!("  {line}");
    }
}

// =============================================================================
// Progress rendering
// =============================================================================

/// Create a plain-text progress callback for --verbose mode.
/// No animations, no ANSI tricks — just timestamped lines to stdout.
fn create_verbose_progress() -> impl Fn(&IndexProgress) {
    let last_phase = RefCell::new(String::new());
    let last_pct = Cell::new(-1i64);
    let start_time = now_ms();

    move |progress: &IndexProgress| {
        let elapsed = js_to_fixed((now_ms() - start_time) as f64 / 1000.0, 1);
        let phase = progress.phase.as_str();

        if phase != last_phase.borrow().as_str() {
            *last_phase.borrow_mut() = phase.to_string();
            last_pct.set(-1);
            println!("[{elapsed}s] Phase: {phase}");
        }

        if progress.total > 0 {
            let pct = ((progress.current as f64 / progress.total as f64) * 100.0).floor() as i64;
            // Log every 5% to keep output manageable
            if pct >= last_pct.get() + 5 || progress.current == progress.total {
                last_pct.set(pct);
                let file_suffix = match &progress.current_file {
                    Some(f) => format!(" {} {f}", get_glyphs().dash),
                    None => String::new(),
                };
                println!(
                    "[{elapsed}s]   {}/{} ({pct}%){file_suffix}",
                    progress.current, progress.total
                );
            }
        } else if progress.current > 0 {
            // Scanning phase (no total yet) — log periodically
            if progress.current.is_multiple_of(1000) || progress.current == 1 {
                println!(
                    "[{elapsed}s]   {} files found",
                    format_number(progress.current as u64)
                );
            }
        }
    }
}

/// Run `indexAll` with either the verbose line logger or the shimmer renderer
/// (TS init/index command bodies share this exact branch).
fn run_index_all(cg: &CodeGraph, verbose: bool) -> codegraph::Result<IndexResult> {
    if verbose {
        let cb = create_verbose_progress();
        let cb_ref: &dyn Fn(&IndexProgress) = &cb;
        cg.index_all(&IndexOptions {
            on_progress: Some(cb_ref),
            signal: None,
            verbose: true,
        })
    } else {
        println!("{DIM}{}{RESET}", get_glyphs().rail);
        let _ = io::stdout().flush();
        let progress = RefCell::new(create_shimmer_progress());
        let result = {
            let cb = |p: &IndexProgress| {
                progress.borrow_mut().on_progress(&UiIndexProgress {
                    phase: p.phase.as_str().to_string(),
                    current: p.current as u64,
                    total: p.total as u64,
                });
            };
            let cb_ref: &dyn Fn(&IndexProgress) = &cb;
            cg.index_all(&IndexOptions {
                on_progress: Some(cb_ref),
                signal: None,
                verbose: false,
            })
        };
        progress.into_inner().stop();
        result
    }
}

/// Print indexing results using clack log methods.
fn print_index_result(result: &IndexResult, project_path: Option<&Path>) {
    let has_errors = result.files_errored > 0;

    // Surface non-file-level failures (e.g. lock-acquisition failure
    // when another indexer is running) before the file-count branches.
    // Without this the CLI falls through to "No files found to index",
    // which is actively misleading — the index DID run, it just couldn't
    // get the lock.
    if !result.success && !has_errors && result.files_indexed == 0 {
        let generic = result.errors.iter().find(|e| e.severity == Severity::Error);
        clack_log_error(&generic.map(|e| e.message.clone()).unwrap_or_else(|| {
            format!(
                "Indexing failed {} no further details available",
                get_glyphs().dash
            )
        }));
        return;
    }

    if result.files_indexed > 0 {
        if has_errors {
            clack_log_success(&format!(
                "Indexed {} files ({} could not be parsed)",
                format_number(result.files_indexed as u64),
                format_number(result.files_errored as u64)
            ));
        } else {
            clack_log_success(&format!(
                "Indexed {} files",
                format_number(result.files_indexed as u64)
            ));
        }
        clack_log_info(&format!(
            "{} nodes, {} edges in {}",
            format_number(result.nodes_created as u64),
            format_number(result.edges_created as u64),
            format_duration(result.duration_ms)
        ));
    } else if has_errors {
        clack_log_error(&format!(
            "Indexing failed {} all {} files had errors",
            get_glyphs().dash,
            format_number(result.files_errored as u64)
        ));
    } else {
        clack_log_warn("No files found to index");
    }

    if has_errors {
        // Insertion-ordered code → count map (TS `Map`).
        let mut errors_by_code: Vec<(String, u64)> = Vec::new();
        for err in &result.errors {
            if err.severity == Severity::Error {
                let code = err.code.clone().unwrap_or_else(|| "unknown".to_string());
                match errors_by_code.iter_mut().find(|(c, _)| *c == code) {
                    Some((_, count)) => *count += 1,
                    None => errors_by_code.push((code, 1)),
                }
            }
        }

        let code_label = |code: &str| -> Option<&'static str> {
            match code {
                "parse_error" => Some("files failed to parse"),
                "read_error" => Some("files could not be read"),
                "size_exceeded" => Some("files exceeded size limit"),
                "path_traversal" => Some("blocked paths"),
                "unsupported_language" => Some("unsupported language"),
                "parser_error" => Some("parser initialization failures"),
                _ => None,
            }
        };

        let breakdown = errors_by_code
            .iter()
            .map(|(code, count)| {
                format!(
                    "{} {}",
                    format_number(*count),
                    code_label(code).unwrap_or(code.as_str())
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        clack_note(&breakdown, "Error breakdown");

        if let Some(pp) = project_path {
            write_error_log(pp, &result.errors);
            clack_log_info("See .codegraph/errors.log for details");
        }

        if result.files_indexed > 0 {
            clack_log_info(&format!(
                "The index is fully usable {} only the failed files are missing.",
                get_glyphs().dash
            ));
        }
    } else if let Some(pp) = project_path {
        let log_path = pp.join(".codegraph").join("errors.log");
        if log_path.exists() {
            let _ = std::fs::remove_file(&log_path);
        }
    }
}

/// Write detailed error log to .codegraph/errors.log
fn write_error_log(project_path: &Path, errors: &[ExtractionError]) {
    let cg_dir = project_path.join(".codegraph");
    if !cg_dir.exists() {
        return;
    }

    let log_path = cg_dir.join("errors.log");

    // Group errors by file path (insertion order, TS `Map`).
    let mut errors_by_file: Vec<(String, Vec<String>)> = Vec::new();
    let mut no_file_errors: Vec<String> = Vec::new();

    for err in errors {
        if err.severity != Severity::Error {
            continue;
        }
        match &err.file_path {
            Some(fp) => match errors_by_file.iter_mut().find(|(f, _)| f == fp) {
                Some((_, list)) => list.push(err.message.clone()),
                None => errors_by_file.push((fp.clone(), vec![err.message.clone()])),
            },
            None => no_file_errors.push(err.message.clone()),
        }
    }

    let mut lines: Vec<String> = vec![
        format!("CodeGraph Error Log - {}", iso_from_epoch_ms(now_ms())),
        format!("{} files with errors", errors_by_file.len()),
        String::new(),
    ];

    for (file_path, file_errors) in &errors_by_file {
        for message in file_errors {
            lines.push(format!("{file_path}: {message}"));
        }
    }

    for message in &no_file_errors {
        lines.push(message.clone());
    }

    let _ = std::fs::write(&log_path, lines.join("\n") + "\n");
}

// =============================================================================
// CLI definition
// =============================================================================

#[derive(Parser)]
#[command(
    name = "codegraph",
    about = "Code intelligence and knowledge graph for any codebase",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize CodeGraph in a project directory and build the initial index
    Init {
        #[arg(value_name = "path")]
        path: Option<String>,
        /// Deprecated: indexing now runs by default; flag accepted for backward compatibility
        #[arg(short = 'i', long)]
        index: bool,
        /// Show detailed worker lifecycle and memory info
        #[arg(short = 'v', long)]
        verbose: bool,
    },
    /// Remove CodeGraph from a project (deletes .codegraph/ directory)
    Uninit {
        #[arg(value_name = "path")]
        path: Option<String>,
        /// Skip confirmation prompt
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Index all files in the project
    Index {
        #[arg(value_name = "path")]
        path: Option<String>,
        /// Force full re-index even if already indexed
        #[arg(short = 'f', long)]
        force: bool,
        /// Suppress progress output
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Show detailed worker lifecycle and memory info
        #[arg(short = 'v', long)]
        verbose: bool,
    },
    /// Sync changes since last index
    Sync {
        #[arg(value_name = "path")]
        path: Option<String>,
        /// Suppress output (for git hooks)
        #[arg(short = 'q', long)]
        quiet: bool,
    },
    /// Show index status and statistics
    Status {
        #[arg(value_name = "path")]
        path: Option<String>,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Search for symbols in the codebase
    Query {
        #[arg(value_name = "search")]
        search: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum results
        #[arg(short = 'l', long, value_name = "number", default_value = "10")]
        limit: String,
        /// Filter by node kind (function, class, etc.)
        #[arg(short = 'k', long, value_name = "kind")]
        kind: Option<String>,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Show project file structure from the index
    Files {
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Filter to files under this directory
        #[arg(long, value_name = "dir")]
        filter: Option<String>,
        /// Filter files matching this glob pattern
        #[arg(long, value_name = "glob")]
        pattern: Option<String>,
        /// Output format (tree, flat, grouped)
        #[arg(long, value_name = "format", default_value = "tree")]
        format: String,
        /// Maximum directory depth for tree format
        #[arg(long = "max-depth", value_name = "number")]
        max_depth: Option<String>,
        /// Hide file metadata (language, symbol count)
        #[arg(long = "no-metadata")]
        no_metadata: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Start CodeGraph as an MCP server for AI assistants
    Serve {
        /// Project path (optional for MCP mode, uses rootUri from client)
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Run as MCP server (stdio transport)
        #[arg(long)]
        mcp: bool,
        /// Disable the file watcher (no auto-sync; useful on slow filesystems like WSL2 /mnt drives)
        #[arg(long = "no-watch")]
        no_watch: bool,
    },
    /// Remove a stale lock file that is blocking indexing
    Unlock {
        #[arg(value_name = "path")]
        path: Option<String>,
    },
    /// Find all functions/methods that call a specific symbol
    Callers {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum results
        #[arg(short = 'l', long, value_name = "number", default_value = "20")]
        limit: String,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Find all functions/methods that a specific symbol calls
    Callees {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum results
        #[arg(short = 'l', long, value_name = "number", default_value = "20")]
        limit: String,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Analyze what code is affected by changing a symbol
    Impact {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Traversal depth
        #[arg(short = 'd', long, value_name = "number", default_value = "2")]
        depth: String,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Find test files affected by changed source files
    Affected {
        #[arg(value_name = "files")]
        files: Vec<String>,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Read file list from stdin (one per line)
        #[arg(long)]
        stdin: bool,
        /// Max dependency traversal depth
        #[arg(short = 'd', long, value_name = "number", default_value = "5")]
        depth: String,
        /// Custom glob filter for test files (e.g. "e2e/*.spec.ts")
        #[arg(short = 'f', long, value_name = "glob")]
        filter: Option<String>,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
        /// Only output file paths, no decoration
        #[arg(short = 'q', long)]
        quiet: bool,
    },
    /// Install codegraph MCP server into one or more agents (Claude Code, Cursor, Codex CLI, opencode, Hermes Agent)
    Install {
        /// Target agent(s): comma-separated ids, or "auto"|"all"|"none". Default: prompt
        #[arg(short = 't', long, value_name = "ids")]
        target: Option<String>,
        /// Install location: "global" or "local". Default: prompt
        #[arg(short = 'l', long, value_name = "where")]
        location: Option<String>,
        /// Non-interactive: defaults to --location=global --target=auto, auto-allow on
        #[arg(short = 'y', long)]
        yes: bool,
        /// Skip writing the auto-allow permissions list (Claude Code only)
        #[arg(long = "no-permissions")]
        no_permissions: bool,
        /// Print MCP config snippet for the named agent and exit (no file writes)
        #[arg(long = "print-config", value_name = "id")]
        print_config: Option<String>,
    },
    /// Remove codegraph from your agents (Claude Code, Cursor, Codex CLI, opencode, Hermes Agent)
    Uninstall {
        /// Target agent(s): comma-separated ids, or "all". Default: all
        #[arg(short = 't', long, value_name = "ids")]
        target: Option<String>,
        /// Uninstall location: "global" or "local". Default: prompt
        #[arg(short = 'l', long, value_name = "where")]
        location: Option<String>,
        /// Non-interactive: defaults to --location=global --target=all
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();

    // Check if running with no arguments - run installer (TS argv.length === 2)
    if argv.len() == 1 {
        if let Err(err) = run_installer() {
            // console.error('Installation failed:', msg)
            eprintln!("Installation failed: {err}");
            process::exit(1);
        }
        return;
    }

    // commander's `.version()` prints the bare version string; clap would
    // prefix the binary name — intercept for byte parity.
    if argv.len() == 2 && (argv[1] == "--version" || argv[1] == "-V") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            use clap::error::ErrorKind;
            let code = match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => 0,
                // commander exits 1 on usage errors (clap's default is 2)
                _ => 1,
            };
            let _ = err.print();
            process::exit(code);
        }
    };

    match cli.command {
        Commands::Init {
            path,
            index,
            verbose,
        } => cmd_init(path.as_deref(), index, verbose),
        Commands::Uninit { path, force } => cmd_uninit(path.as_deref(), force),
        Commands::Index {
            path,
            force,
            quiet,
            verbose,
        } => cmd_index(path.as_deref(), force, quiet, verbose),
        Commands::Sync { path, quiet } => cmd_sync(path.as_deref(), quiet),
        Commands::Status { path, json } => cmd_status(path.as_deref(), json),
        Commands::Query {
            search,
            path,
            limit,
            kind,
            json,
        } => cmd_query(&search, path.as_deref(), &limit, kind.as_deref(), json),
        Commands::Files {
            path,
            filter,
            pattern,
            format,
            max_depth,
            no_metadata,
            json,
        } => cmd_files(
            path.as_deref(),
            filter.as_deref(),
            pattern.as_deref(),
            &format,
            max_depth.as_deref(),
            !no_metadata,
            json,
        ),
        Commands::Serve {
            path,
            mcp,
            no_watch,
        } => cmd_serve(path.as_deref(), mcp, no_watch),
        Commands::Unlock { path } => cmd_unlock(path.as_deref()),
        Commands::Callers {
            symbol,
            path,
            limit,
            json,
        } => cmd_call_graph(
            CallDirection::Callers,
            &symbol,
            path.as_deref(),
            &limit,
            json,
        ),
        Commands::Callees {
            symbol,
            path,
            limit,
            json,
        } => cmd_call_graph(
            CallDirection::Callees,
            &symbol,
            path.as_deref(),
            &limit,
            json,
        ),
        Commands::Impact {
            symbol,
            path,
            depth,
            json,
        } => cmd_impact(&symbol, path.as_deref(), &depth, json),
        Commands::Affected {
            files,
            path,
            stdin,
            depth,
            filter,
            json,
            quiet,
        } => cmd_affected(
            files,
            path.as_deref(),
            stdin,
            &depth,
            filter.as_deref(),
            json,
            quiet,
        ),
        Commands::Install {
            target,
            location,
            yes,
            no_permissions,
            print_config,
        } => cmd_install(target, location, yes, no_permissions, print_config),
        Commands::Uninstall {
            target,
            location,
            yes,
        } => cmd_uninstall(target, location, yes),
    }
}

// =============================================================================
// Commands
// =============================================================================

/// codegraph init [path]
fn cmd_init(path_arg: Option<&str>, _index: bool, verbose: bool) {
    let project_path = resolve_absolute(path_arg);

    clack_intro("Initializing CodeGraph");

    let body = || -> Result<(), String> {
        if is_initialized(&project_path) {
            clack_log_warn(&format!(
                "Already initialized in {}",
                project_path.display()
            ));
            clack_log_info("Use \"codegraph index\" to re-index or \"codegraph sync\" to update");
            // try { offerWatchFallback } catch { /* non-fatal */ }
            offer_watch_fallback(&project_path, false);
            clack_outro("");
            return Ok(());
        }

        let cg = CodeGraph::init(
            &project_path,
            &InitOptions {
                index: false,
                on_progress: None,
            },
        )
        .map_err(|e| e.to_string())?;
        clack_log_success(&format!("Initialized in {}", project_path.display()));

        // Indexing runs by default now. The legacy -i/--index flag is still
        // accepted (so existing muscle memory and scripts don't break) but is a
        // no-op — initializing always builds the initial index.
        let result = run_index_all(&cg, verbose).map_err(|e| e.to_string())?;
        print_index_result(&result, Some(&project_path));

        // try { offerWatchFallback } catch { /* non-fatal */ }
        offer_watch_fallback(&project_path, false);

        clack_outro("Done");
        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        clack_log_error(&format!("Failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph uninit [path]
fn cmd_uninit(path_arg: Option<&str>, force: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            warn(&format!(
                "CodeGraph is not initialized in {}",
                project_path.display()
            ));
            return Ok(());
        }

        if !force {
            // Confirm with user
            print!(
                "{}",
                yellow(&format!(
                    "{} This will permanently delete all CodeGraph data. Continue? (y/N) ",
                    get_glyphs().warn
                ))
            );
            let _ = io::stdout().flush();
            let mut answer = String::new();
            let _ = io::stdin().lock().read_line(&mut answer);
            if answer.trim().to_lowercase() != "y" {
                info("Cancelled");
                return Ok(());
            }
        }

        let cg = CodeGraph::open_sync(&project_path).map_err(|e| e.to_string())?;
        cg.uninitialize().map_err(|e| e.to_string())?;

        // Clean up any git sync hooks we installed (no-op if none / not a repo).
        let removed = remove_git_sync_hook(&project_path, &DEFAULT_SYNC_HOOKS);
        if !removed.installed.is_empty() {
            let names: Vec<&str> = removed.installed.iter().map(|h| h.as_str()).collect();
            info(&format!(
                "Removed git {} sync hook{}",
                names.join(", "),
                if names.len() > 1 { "s" } else { "" }
            ));
        }

        success(&format!(
            "Removed CodeGraph from {}",
            project_path.display()
        ));
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Failed to uninitialize: {msg}"));
        process::exit(1);
    }
}

/// codegraph index [path]
fn cmd_index(path_arg: Option<&str>, force: bool, quiet: bool, verbose: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            info("Run \"codegraph init\" first");
            process::exit(1);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;

        if quiet {
            // Quiet mode: no UI, just run
            if force {
                cg.clear().map_err(|e| e.to_string())?;
            }
            let result = cg
                .index_all(&IndexOptions::default())
                .map_err(|e| e.to_string())?;
            if !result.success {
                process::exit(1);
            }
            cg.close();
            return Ok(());
        }

        clack_intro("Indexing project");

        if force {
            cg.clear().map_err(|e| e.to_string())?;
            clack_log_info("Cleared existing index");
        }

        let result = run_index_all(&cg, verbose).map_err(|e| e.to_string())?;

        print_index_result(&result, Some(&project_path));

        if !result.success {
            process::exit(1);
        }

        clack_outro("Done");
        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Failed to index: {msg}"));
        process::exit(1);
    }
}

/// codegraph sync [path]
fn cmd_sync(path_arg: Option<&str>, quiet: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            if !quiet {
                error_msg(&format!(
                    "CodeGraph not initialized in {}",
                    project_path.display()
                ));
            }
            process::exit(1);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;

        if quiet {
            cg.sync(&IndexOptions::default())
                .map_err(|e| e.to_string())?;
            cg.close();
            return Ok(());
        }

        clack_intro("Syncing CodeGraph");

        println!("{DIM}{}{RESET}", get_glyphs().rail);
        let _ = io::stdout().flush();
        let progress = RefCell::new(create_shimmer_progress());

        let result = {
            let cb = |p: &IndexProgress| {
                progress.borrow_mut().on_progress(&UiIndexProgress {
                    phase: p.phase.as_str().to_string(),
                    current: p.current as u64,
                    total: p.total as u64,
                });
            };
            let cb_ref: &dyn Fn(&IndexProgress) = &cb;
            cg.sync(&IndexOptions {
                on_progress: Some(cb_ref),
                signal: None,
                verbose: false,
            })
        };

        progress.into_inner().stop();
        let result = result.map_err(|e| e.to_string())?;

        let total_changes = result.files_added + result.files_modified + result.files_removed;

        if total_changes == 0 {
            clack_log_info("Already up to date");
        } else {
            clack_log_success(&format!(
                "Synced {} changed files",
                format_number(total_changes as u64)
            ));
            let mut details: Vec<String> = Vec::new();
            if result.files_added > 0 {
                details.push(format!("Added: {}", result.files_added));
            }
            if result.files_modified > 0 {
                details.push(format!("Modified: {}", result.files_modified));
            }
            if result.files_removed > 0 {
                details.push(format!("Removed: {}", result.files_removed));
            }
            clack_log_info(&format!(
                "{} {} {} nodes in {}",
                details.join(", "),
                get_glyphs().dash,
                format_number(result.nodes_updated as u64),
                format_duration(result.duration_ms)
            ));
        }

        clack_outro("Done");
        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        if !quiet {
            error_msg(&format!("Failed to sync: {msg}"));
        }
        process::exit(1);
    }
}

/// codegraph status [path]
fn cmd_status(path_arg: Option<&str>, json: bool) {
    let project_path = resolve_project_path(path_arg);
    // The directory the user actually ran from, before walking up to the index
    // root. Used to detect when the resolved index lives in a different git
    // working tree (e.g. a nested worktree borrowing the main checkout's index).
    let start_path = resolve_absolute(path_arg);
    let worktree_mismatch = detect_worktree_index_mismatch(&start_path, &project_path);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "initialized": false,
                        "version": env!("CARGO_PKG_VERSION"),
                        "projectPath": project_path.to_string_lossy(),
                        "indexPath": get_codegraph_dir(&project_path).to_string_lossy(),
                        "lastIndexed": null,
                    })
                );
                return Ok(());
            }
            println!("{}", bold("\nCodeGraph Status\n"));
            info(&format!("Project: {}", project_path.display()));
            warn("Not initialized");
            info("Run \"codegraph init\" to initialize");
            return Ok(());
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let stats = cg.get_stats().map_err(|e| e.to_string())?;
        let changes = cg.get_changed_files().map_err(|e| e.to_string())?;
        let backend = cg.get_backend();
        let journal_mode = cg.get_journal_mode().map_err(|e| e.to_string())?;

        // JSON output mode
        if json {
            let last_indexed_ms = cg.get_last_indexed_at().map_err(|e| e.to_string())?;

            // nodesByKind / languages: HashMap iteration order is
            // nondeterministic, so keys are emitted alphabetically (the TS
            // object insertion order is itself data-dependent).
            let mut kind_entries: Vec<(&String, &u64)> = stats.nodes_by_kind.iter().collect();
            kind_entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut nodes_by_kind = serde_json::Map::new();
            for (k, v) in kind_entries {
                nodes_by_kind.insert(k.clone(), serde_json::json!(v));
            }

            let mut languages: Vec<&String> = stats
                .files_by_language
                .iter()
                .filter(|(_, count)| **count > 0)
                .map(|(lang, _)| lang)
                .collect();
            languages.sort();

            println!(
                "{}",
                serde_json::json!({
                    "initialized": true,
                    "version": env!("CARGO_PKG_VERSION"),
                    "projectPath": project_path.to_string_lossy(),
                    "indexPath": get_codegraph_dir(&project_path).to_string_lossy(),
                    "lastIndexed": last_indexed_ms.map(iso_from_epoch_ms),
                    "fileCount": stats.file_count,
                    "nodeCount": stats.node_count,
                    "edgeCount": stats.edge_count,
                    "dbSizeBytes": stats.db_size_bytes,
                    "backend": backend.as_str(),
                    "journalMode": journal_mode,
                    "nodesByKind": nodes_by_kind,
                    "languages": languages,
                    "pendingChanges": {
                        "added": changes.added.len(),
                        "modified": changes.modified.len(),
                        "removed": changes.removed.len(),
                    },
                    "worktreeMismatch": worktree_mismatch.as_ref().map(|m| serde_json::json!({
                        "worktreeRoot": m.worktree_root.to_string_lossy(),
                        "indexRoot": m.index_root.to_string_lossy(),
                    })),
                })
            );
            cg.close();
            return Ok(());
        }

        println!("{}", bold("\nCodeGraph Status\n"));

        // Project info
        println!("{} {}", cyan("Project:"), project_path.display());
        if let Some(m) = &worktree_mismatch {
            warn(&worktree_mismatch_warning(m));
        }
        println!();

        // Index stats
        println!("{}", bold("Index Statistics:"));
        println!("  Files:     {}", format_number(stats.file_count));
        println!("  Nodes:     {}", format_number(stats.node_count));
        println!("  Edges:     {}", format_number(stats.edge_count));
        println!(
            "  DB Size:   {} MB",
            js_to_fixed(stats.db_size_bytes as f64 / 1024.0 / 1024.0, 2)
        );
        // Surface the active SQLite backend. (TS labels its node:sqlite
        // backend; the Rust port reports "native" per the porting contract.)
        let backend_label = green(&format!(
            "{} {} built-in (full WAL)",
            backend.as_str(),
            get_glyphs().dash
        ));
        println!("  Backend:   {backend_label}");
        // Effective journal mode: 'wal' means concurrent reads never block on a
        // writer; anything else means they can ("database is locked"). A non-wal
        // mode means the filesystem can't support it (network mounts, WSL2
        // /mnt). See issue #238.
        let journal_label = if journal_mode == "wal" {
            green("wal")
        } else {
            yellow(&format!(
                "{} {} WAL inactive; reads can block on writes",
                if journal_mode.is_empty() {
                    "unknown"
                } else {
                    journal_mode.as_str()
                },
                get_glyphs().dash
            ))
        };
        println!("  Journal:   {journal_label}");
        println!();

        // Node breakdown (count desc; key asc tie-break for determinism)
        println!("{}", bold("Nodes by Kind:"));
        let mut nodes_by_kind: Vec<(&String, &u64)> = stats
            .nodes_by_kind
            .iter()
            .filter(|(_, count)| **count > 0)
            .collect();
        nodes_by_kind.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (kind, count) in &nodes_by_kind {
            println!("  {:<15} {}", kind, format_number(**count));
        }
        println!();

        // Language breakdown
        println!("{}", bold("Files by Language:"));
        let mut files_by_lang: Vec<(&String, &u64)> = stats
            .files_by_language
            .iter()
            .filter(|(_, count)| **count > 0)
            .collect();
        files_by_lang.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (lang, count) in &files_by_lang {
            println!("  {:<15} {}", lang, format_number(**count));
        }
        println!();

        // Pending changes
        let total_changes = changes.added.len() + changes.modified.len() + changes.removed.len();
        if total_changes > 0 {
            println!("{}", bold("Pending Changes:"));
            if !changes.added.is_empty() {
                println!("  Added:     {} files", changes.added.len());
            }
            if !changes.modified.is_empty() {
                println!("  Modified:  {} files", changes.modified.len());
            }
            if !changes.removed.is_empty() {
                println!("  Removed:   {} files", changes.removed.len());
            }
            info("Run \"codegraph sync\" to update the index");
        } else {
            success("Index is up to date");
        }
        println!();

        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Failed to get status: {msg}"));
        process::exit(1);
    }
}

/// codegraph query <search>
fn cmd_query(
    search: &str,
    path_arg: Option<&str>,
    limit_arg: &str,
    kind: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            process::exit(1);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;

        let limit = parse_int_js(limit_arg).unwrap_or(10).max(0) as usize;
        // TS passes an unvalidated kind string straight to the SQL filter; an
        // unknown kind matches no rows. NodeKind is an enum here, so an
        // unparseable kind short-circuits to the same empty result set.
        let (kinds, kind_invalid) = match kind {
            Some(k) => match k.parse::<NodeKind>() {
                Ok(nk) => (Some(vec![nk]), false),
                Err(_) => (None, true),
            },
            None => (None, false),
        };
        let raw_results = if kind_invalid {
            Vec::new()
        } else {
            cg.search_nodes(
                search,
                Some(&SearchOptions {
                    limit: Some(limit),
                    kinds,
                    ..Default::default()
                }),
            )
            .map_err(|e| e.to_string())?
        };

        // Mirror the MCP search down-rank so the CLI also surfaces the
        // hand-written implementation before protobuf/gRPC scaffolding
        // when both share a name. See extraction/generated-detection.
        let mut results = raw_results;
        results.sort_by_key(|r| {
            if is_generated_file(&r.node.file_path) {
                1
            } else {
                0
            }
        });

        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&results).map_err(|e| e.to_string())?
            );
        } else if results.is_empty() {
            info(&format!("No results found for \"{search}\""));
        } else {
            println!("{}", bold(&format!("\nSearch Results for \"{search}\":\n")));

            for result in &results {
                let node = &result.node;
                let location = format!("{}:{}", node.file_path, node.start_line);
                let score = dim(&format!("({}%)", js_to_fixed(result.score * 100.0, 0)));

                println!(
                    "{}{} {score}",
                    cyan(&format!("{:<12}", node.kind.as_str())),
                    white(&node.name)
                );
                println!("{}", dim(&format!("  {location}")));
                if let Some(signature) = &node.signature {
                    println!("{}", dim(&format!("  {signature}")));
                }
                println!();
            }
        }

        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Search failed: {msg}"));
        process::exit(1);
    }
}

// =============================================================================
// files command
// =============================================================================

/// Convert glob pattern to regex (TS `globToRegex`).
fn glob_to_regex_str(pattern: &str) -> String {
    // .replace(/[.+^${}()|[\]\\]/g, '\\$&')
    let mut escaped = String::new();
    for c in pattern.chars() {
        if ".+^${}()|[]\\".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    let escaped = escaped.replace("**", "{{GLOBSTAR}}");
    let escaped = escaped.replace('*', "[^/]*");
    let escaped = escaped.replace('?', "[^/]");
    escaped.replace("{{GLOBSTAR}}", ".*")
}

/// codegraph files
fn cmd_files(
    path_arg: Option<&str>,
    filter: Option<&str>,
    pattern: Option<&str>,
    format: &str,
    max_depth_arg: Option<&str>,
    include_metadata: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            process::exit(1);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let mut files = cg.get_files().map_err(|e| e.to_string())?;

        if files.is_empty() {
            info("No files indexed. Run \"codegraph index\" first.");
            cg.close();
            return Ok(());
        }

        // Filter by path prefix
        if let Some(filter) = filter {
            let dotted = format!("./{filter}");
            files.retain(|f| f.path.starts_with(filter) || f.path.starts_with(&dotted));
        }

        // Filter by glob pattern
        if let Some(pattern) = pattern {
            let regex =
                regex::Regex::new(&glob_to_regex_str(pattern)).map_err(|e| e.to_string())?;
            files.retain(|f| regex.is_match(&f.path));
        }

        if files.is_empty() {
            info("No files found matching the criteria.");
            cg.close();
            return Ok(());
        }

        // JSON output
        if json {
            let output: Vec<serde_json::Value> = files
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "path": f.path,
                        "language": f.language.as_str(),
                        "nodeCount": f.node_count,
                        "size": f.size,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?
            );
            cg.close();
            return Ok(());
        }

        let max_depth = max_depth_arg.and_then(parse_int_js);

        // Format output
        match format {
            "flat" => {
                println!("{}", bold(&format!("\nFiles ({}):\n", files.len())));
                let mut sorted = files.clone();
                sorted.sort_by(|a, b| a.path.cmp(&b.path));
                for file in &sorted {
                    if include_metadata {
                        println!(
                            "  {} {}",
                            file.path,
                            dim(&format!(
                                "({}, {} symbols)",
                                file.language.as_str(),
                                file.node_count
                            ))
                        );
                    } else {
                        println!("  {}", file.path);
                    }
                }
            }
            "grouped" => {
                println!(
                    "{}",
                    bold(&format!("\nFiles by Language ({} total):\n", files.len()))
                );
                // Insertion-ordered Map<lang, files> (TS `Map`).
                let mut by_lang: Vec<(String, Vec<&FileRecord>)> = Vec::new();
                for file in &files {
                    let lang = file.language.as_str().to_string();
                    match by_lang.iter_mut().find(|(l, _)| *l == lang) {
                        Some((_, list)) => list.push(file),
                        None => by_lang.push((lang, vec![file])),
                    }
                }
                by_lang.sort_by_key(|(_, list)| std::cmp::Reverse(list.len()));
                for (lang, mut lang_files) in by_lang {
                    println!("{}", cyan(&format!("{lang} ({}):", lang_files.len())));
                    lang_files.sort_by(|a, b| a.path.cmp(&b.path));
                    for file in lang_files {
                        if include_metadata {
                            println!(
                                "  {} {}",
                                file.path,
                                dim(&format!("({} symbols)", file.node_count))
                            );
                        } else {
                            println!("  {}", file.path);
                        }
                    }
                    println!();
                }
            }
            _ => {
                // "tree" and unknown formats fall through to tree (TS default)
                println!(
                    "{}",
                    bold(&format!("\nProject Structure ({} files):\n", files.len()))
                );
                print_file_tree(&files, include_metadata, max_depth);
            }
        }

        println!();
        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Failed to list files: {msg}"));
        process::exit(1);
    }
}

/// Tree node for `printFileTree`.
struct TreeNode {
    name: String,
    /// Insertion-ordered children (TS `Map`); render sorts dirs-first by name.
    children: Vec<TreeNode>,
    file: Option<(String, u32)>, // (language, node_count)
}

impl TreeNode {
    fn child_index(&mut self, name: &str) -> usize {
        if let Some(i) = self.children.iter().position(|c| c.name == name) {
            return i;
        }
        self.children.push(TreeNode {
            name: name.to_string(),
            children: Vec::new(),
            file: None,
        });
        self.children.len() - 1
    }
}

/// Print files as a tree (TS `printFileTree`).
fn print_file_tree(files: &[FileRecord], include_metadata: bool, max_depth: Option<i64>) {
    let mut root = TreeNode {
        name: String::new(),
        children: Vec::new(),
        file: None,
    };

    for file in files {
        let parts: Vec<&str> = file.path.split('/').collect();
        let mut current = &mut root;

        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            let idx = current.child_index(part);
            current = &mut current.children[idx];
            if i == parts.len() - 1 {
                current.file = Some((file.language.as_str().to_string(), file.node_count));
            }
        }
    }

    fn render_node(
        node: &TreeNode,
        prefix: &str,
        is_last: bool,
        depth: i64,
        include_metadata: bool,
        max_depth: Option<i64>,
    ) {
        if let Some(max) = max_depth {
            if depth > max {
                return;
            }
        }

        let glyphs = get_glyphs();
        let connector = if is_last {
            glyphs.tree_last
        } else {
            glyphs.tree_branch
        };
        let child_prefix = if is_last { "    " } else { glyphs.tree_pipe };

        if !node.name.is_empty() {
            let mut line = format!("{prefix}{connector}{}", node.name);
            if include_metadata {
                if let Some((language, node_count)) = &node.file {
                    line.push_str(&dim(&format!(" ({language}, {node_count} symbols)")));
                }
            }
            println!("{line}");
        }

        let mut children: Vec<&TreeNode> = node.children.iter().collect();
        children.sort_by(|a, b| {
            let a_is_dir = !a.children.is_empty() && a.file.is_none();
            let b_is_dir = !b.children.is_empty() && b.file.is_none();
            if a_is_dir != b_is_dir {
                return if a_is_dir {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            a.name.cmp(&b.name)
        });

        for (i, child) in children.iter().enumerate() {
            let next_prefix = if !node.name.is_empty() {
                format!("{prefix}{child_prefix}")
            } else {
                prefix.to_string()
            };
            render_node(
                child,
                &next_prefix,
                i == children.len() - 1,
                depth + 1,
                include_metadata,
                max_depth,
            );
        }
    }

    render_node(&root, "", true, 0, include_metadata, max_depth);
}

/// codegraph serve
fn cmd_serve(path_arg: Option<&str>, mcp: bool, no_watch: bool) {
    let project_path = path_arg.map(|p| resolve_project_path(Some(p)));

    // Commander sets watch=false when --no-watch is passed. Route it through
    // the same env-var chokepoint the watcher and MCP server already honor.
    if no_watch {
        std::env::set_var("CODEGRAPH_NO_WATCH", "1");
    }

    if mcp {
        // Start MCP server - it handles initialization lazily based on rootUri
        // from client
        let server = MCPServer::new(project_path.map(|p| p.to_string_lossy().to_string()));
        if let Err(err) = server.start() {
            error_msg(&format!("Failed to start server: {err}"));
            process::exit(1);
        }
        // Server will run until terminated
    } else {
        // Default: show info about MCP mode.
        // Use stderr so stdout stays clean for any piped/stdio usage.
        eprintln!("{}", bold("\nCodeGraph MCP Server\n"));
        eprintln!(
            "{} Use --mcp flag to start the MCP server",
            blue(get_glyphs().info)
        );
        eprintln!("\nTo use with Claude Code, add to your MCP configuration:");
        eprintln!(
            "{}",
            dim(
                "\n{\n  \"mcpServers\": {\n    \"codegraph\": {\n      \"command\": \"codegraph\",\n      \"args\": [\"serve\", \"--mcp\"]\n    }\n  }\n}\n"
            )
        );
        eprintln!("Available tools:");
        eprintln!(
            "{}   - Primary: source of the relevant symbols for any question",
            cyan("  codegraph_explore")
        );
        eprintln!(
            "{}    - Search for code symbols",
            cyan("  codegraph_search")
        );
        eprintln!(
            "{}   - Find callers of a symbol",
            cyan("  codegraph_callers")
        );
        eprintln!(
            "{}   - Find what a symbol calls",
            cyan("  codegraph_callees")
        );
        eprintln!(
            "{}    - Analyze impact of changes",
            cyan("  codegraph_impact")
        );
        eprintln!("{}      - Get symbol details", cyan("  codegraph_node"));
        eprintln!(
            "{}     - Get project file structure",
            cyan("  codegraph_files")
        );
        eprintln!("{}    - Get index status", cyan("  codegraph_status"));
    }
}

/// codegraph unlock [path]
fn cmd_unlock(path_arg: Option<&str>) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            return Ok(());
        }

        let lock_path = get_codegraph_dir(&project_path).join("codegraph.lock");

        if !lock_path.exists() {
            info(&format!(
                "No lock file found {} nothing to do",
                get_glyphs().dash
            ));
            return Ok(());
        }

        std::fs::remove_file(&lock_path).map_err(|e| e.to_string())?;
        success("Removed lock file. You can now run indexing again.");
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Failed to remove lock: {msg}"));
        process::exit(1);
    }
}

// =============================================================================
// callers / callees
//
// CLI parity with the MCP graph tools (codegraph_callers/callees/impact) so
// the traversal queries work in scripts, CI, and git hooks without a running
// MCP server.
// =============================================================================

#[derive(Clone, Copy, PartialEq)]
enum CallDirection {
    Callers,
    Callees,
}

impl CallDirection {
    fn noun(self) -> &'static str {
        match self {
            CallDirection::Callers => "callers",
            CallDirection::Callees => "callees",
        }
    }
    fn heading(self) -> &'static str {
        match self {
            CallDirection::Callers => "Callers",
            CallDirection::Callees => "Callees",
        }
    }
}

/// Is `name` an exact match for `symbol` (allowing `.`/`::` qualification)?
fn is_exact_symbol_match(name: &str, symbol: &str) -> bool {
    name == symbol
        || name.ends_with(&format!(".{symbol}"))
        || name.ends_with(&format!("::{symbol}"))
}

/// codegraph callers <symbol> / codegraph callees <symbol>
fn cmd_call_graph(
    direction: CallDirection,
    symbol: &str,
    path_arg: Option<&str>,
    limit_arg: &str,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            process::exit(1);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let limit = parse_int_js(limit_arg).unwrap_or(20).max(0) as usize;

        let matches = cg
            .search_nodes(
                symbol,
                Some(&SearchOptions {
                    limit: Some(50),
                    ..Default::default()
                }),
            )
            .map_err(|e| e.to_string())?;
        if matches.is_empty() {
            info(&format!("Symbol \"{symbol}\" not found"));
            cg.close();
            return Ok(());
        }

        let fetch = |node_id: &str| -> Result<Vec<codegraph::NodeRef>, String> {
            match direction {
                CallDirection::Callers => cg.get_callers(node_id, None),
                CallDirection::Callees => cg.get_callees(node_id, None),
            }
            .map_err(|e| e.to_string())
        };

        let mut seen: HashSet<String> = HashSet::new();
        let mut all: Vec<(String, String, String, u32)> = Vec::new(); // (name, kind, filePath, startLine)

        for m in &matches {
            let exact_match = is_exact_symbol_match(&m.node.name, symbol);
            if !exact_match && matches.len() > 1 {
                continue;
            }
            for c in fetch(&m.node.id)? {
                if seen.insert(c.node.id.clone()) {
                    all.push((
                        c.node.name.clone(),
                        c.node.kind.as_str().to_string(),
                        c.node.file_path.clone(),
                        c.node.start_line,
                    ));
                }
            }
        }

        // Fallback: if exact filter removed everything, use the top match
        if all.is_empty() {
            if let Some(first) = matches.first() {
                for c in fetch(&first.node.id)? {
                    if seen.insert(c.node.id.clone()) {
                        all.push((
                            c.node.name.clone(),
                            c.node.kind.as_str().to_string(),
                            c.node.file_path.clone(),
                            c.node.start_line,
                        ));
                    }
                }
            }
        }

        let limited = &all[..all.len().min(limit)];

        if json {
            let entries: Vec<serde_json::Value> = limited
                .iter()
                .map(|(name, kind, file_path, start_line)| {
                    serde_json::json!({
                        "name": name,
                        "kind": kind,
                        "filePath": file_path,
                        "startLine": start_line,
                    })
                })
                .collect();
            // `{ symbol, callers }` / `{ symbol, callees }` — the key name
            // follows the command, so build the object manually.
            let mut obj = serde_json::Map::new();
            obj.insert("symbol".to_string(), serde_json::json!(symbol));
            obj.insert(
                direction.noun().to_string(),
                serde_json::Value::Array(entries),
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::Value::Object(obj))
                    .map_err(|e| e.to_string())?
            );
        } else if limited.is_empty() {
            info(&format!("No {} found for \"{symbol}\"", direction.noun()));
        } else {
            println!(
                "{}",
                bold(&format!(
                    "\n{} of \"{symbol}\" ({}):\n",
                    direction.heading(),
                    limited.len()
                ))
            );
            for (name, kind, file_path, start_line) in limited {
                let loc = if *start_line != 0 {
                    format!(":{start_line}")
                } else {
                    String::new()
                };
                println!("{}{}", cyan(&format!("{kind:<12}")), white(name));
                println!("{}", dim(&format!("  {file_path}{loc}")));
                println!();
            }
        }

        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("{} failed: {msg}", direction.noun()));
        process::exit(1);
    }
}

/// codegraph impact <symbol>
fn cmd_impact(symbol: &str, path_arg: Option<&str>, depth_arg: &str, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            process::exit(1);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let depth = parse_int_js(depth_arg).unwrap_or(2).clamp(1, 10) as u32;

        let matches = cg
            .search_nodes(
                symbol,
                Some(&SearchOptions {
                    limit: Some(50),
                    ..Default::default()
                }),
            )
            .map_err(|e| e.to_string())?;
        if matches.is_empty() {
            info(&format!("Symbol \"{symbol}\" not found"));
            cg.close();
            return Ok(());
        }

        // Merge impact subgraphs across all exact-matching symbols
        let mut merged_nodes: HashMap<String, (String, String, String, u32)> = HashMap::new();
        let mut seen_edges: HashSet<String> = HashSet::new();
        let mut edge_count = 0usize;

        for m in &matches {
            let exact_match = is_exact_symbol_match(&m.node.name, symbol);
            if !exact_match && matches.len() > 1 {
                continue;
            }
            let impact = cg
                .get_impact_radius(&m.node.id, Some(depth))
                .map_err(|e| e.to_string())?;
            for (id, n) in &impact.nodes {
                merged_nodes.insert(
                    id.clone(),
                    (
                        n.name.clone(),
                        n.kind.as_str().to_string(),
                        n.file_path.clone(),
                        n.start_line,
                    ),
                );
            }
            for e in &impact.edges {
                let key = format!("{}->{}:{}", e.source, e.target, e.kind.as_str());
                if seen_edges.insert(key) {
                    edge_count += 1;
                }
            }
        }

        // Fallback to top match if exact filter removed everything
        if merged_nodes.is_empty() {
            if let Some(first) = matches.first() {
                let impact = cg
                    .get_impact_radius(&first.node.id, Some(depth))
                    .map_err(|e| e.to_string())?;
                for (id, n) in &impact.nodes {
                    merged_nodes.insert(
                        id.clone(),
                        (
                            n.name.clone(),
                            n.kind.as_str().to_string(),
                            n.file_path.clone(),
                            n.start_line,
                        ),
                    );
                }
                edge_count = impact.edges.len();
            }
        }

        // The TS Map preserved BFS insertion order; the subgraph's HashMap
        // loses it, so emit a deterministic (filePath, startLine, name) order.
        let mut affected: Vec<(String, String, String, u32)> = merged_nodes.into_values().collect();
        affected.sort_by(|a, b| a.2.cmp(&b.2).then(a.3.cmp(&b.3)).then(a.0.cmp(&b.0)));

        if json {
            let entries: Vec<serde_json::Value> = affected
                .iter()
                .map(|(name, kind, file_path, start_line)| {
                    serde_json::json!({
                        "name": name,
                        "kind": kind,
                        "filePath": file_path,
                        "startLine": start_line,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "symbol": symbol,
                    "depth": depth,
                    "nodeCount": affected.len(),
                    "edgeCount": edge_count,
                    "affected": entries,
                }))
                .map_err(|e| e.to_string())?
            );
        } else if affected.is_empty() {
            info(&format!("No affected symbols found for \"{symbol}\""));
        } else {
            println!(
                "{}",
                bold(&format!(
                    "\nImpact of changing \"{symbol}\" — {} affected symbols:\n",
                    affected.len()
                ))
            );

            // Group by file (insertion order over the sorted affected list)
            let mut by_file: Vec<(String, Vec<(String, String, u32)>)> = Vec::new();
            for (name, kind, file_path, start_line) in &affected {
                match by_file.iter_mut().find(|(f, _)| f == file_path) {
                    Some((_, list)) => list.push((name.clone(), kind.clone(), *start_line)),
                    None => by_file.push((
                        file_path.clone(),
                        vec![(name.clone(), kind.clone(), *start_line)],
                    )),
                }
            }

            for (file, nodes) in &by_file {
                println!("{}", cyan(file));
                for (name, kind, start_line) in nodes {
                    let loc = if *start_line != 0 {
                        format!(":{start_line}")
                    } else {
                        String::new()
                    };
                    println!("  {}{name}{}", dim(&format!("{kind:<12}")), dim(&loc));
                }
                println!();
            }
        }

        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("impact failed: {msg}"));
        process::exit(1);
    }
}

// =============================================================================
// affected command
// =============================================================================

/// Convert the `--filter` glob to a regex (TS inline converter:
/// `** → .+`, `* → [^/]*`, `. → \.`).
fn affected_filter_to_regex_str(filter: &str) -> String {
    // .replace(/[+[\]{}()^$|\\]/g, '\\$&')
    let mut escaped = String::new();
    for c in filter.chars() {
        if "+[]{}()^$|\\".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    let escaped = escaped.replace('.', "\\.");
    let escaped = escaped.replace("**", ".+");
    escaped.replace('*', "[^/]*")
}

/// codegraph affected [files...]
///
/// Find test files affected by the given source files.
/// Traces dependency edges transitively to find test files that depend on
/// changed code.
///
/// Usage:
///   git diff --name-only | codegraph affected --stdin
///   codegraph affected src/lib/components/Editor.svelte src/routes/+page.svelte
fn cmd_affected(
    file_args: Vec<String>,
    path_arg: Option<&str>,
    stdin: bool,
    depth_arg: &str,
    filter: Option<&str>,
    json: bool,
    quiet: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            process::exit(1);
        }

        // Collect changed files from args or stdin
        let mut changed_files: Vec<String> = file_args.clone();

        if stdin {
            let stdin_data = io::read_to_string(io::stdin()).map_err(|e| e.to_string())?;
            changed_files.extend(
                stdin_data
                    .split('\n')
                    .map(|f| f.trim().to_string())
                    .filter(|f| !f.is_empty()),
            );
        }

        if changed_files.is_empty() {
            if !quiet {
                info("No files provided. Use file arguments or --stdin.");
            }
            process::exit(0);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let max_depth = parse_int_js(depth_arg).unwrap_or(5);

        // Common test file patterns
        let default_test_patterns: Vec<regex::Regex> = [
            r"\.spec\.",
            r"\.test\.",
            r"/__tests__/",
            r"/tests?/",
            r"/e2e/",
            r"/spec/",
        ]
        .iter()
        .map(|p| regex::Regex::new(p).expect("static pattern"))
        .collect();

        // Custom filter pattern
        let custom_filter: Option<regex::Regex> = match filter {
            Some(f) => Some(
                regex::Regex::new(&affected_filter_to_regex_str(f)).map_err(|e| e.to_string())?,
            ),
            None => None,
        };

        let is_test_file = |file_path: &str| -> bool {
            if let Some(cf) = &custom_filter {
                return cf.is_match(file_path);
            }
            default_test_patterns.iter().any(|p| p.is_match(file_path))
        };

        // BFS to find all transitive dependents of changed files, filtered to
        // test files
        let mut affected_tests: HashSet<String> = HashSet::new();
        let mut all_dependents: HashSet<String> = HashSet::new();

        for file in &changed_files {
            // If the changed file is itself a test file, include it
            if is_test_file(file) {
                affected_tests.insert(file.clone());
                continue;
            }

            // BFS through dependents
            let mut queue: VecDeque<(String, i64)> = VecDeque::new();
            queue.push_back((file.clone(), 0));
            let mut visited: HashSet<String> = HashSet::new();
            visited.insert(file.clone());

            while let Some((current, depth)) = queue.pop_front() {
                if depth >= max_depth {
                    continue;
                }

                let dependents = cg
                    .get_file_dependents(&current)
                    .map_err(|e| e.to_string())?;
                for dep in dependents {
                    if visited.contains(&dep) {
                        continue;
                    }
                    visited.insert(dep.clone());
                    all_dependents.insert(dep.clone());

                    if is_test_file(&dep) {
                        affected_tests.insert(dep);
                    } else {
                        queue.push_back((dep, depth + 1));
                    }
                }
            }
        }

        let mut sorted_tests: Vec<String> = affected_tests.into_iter().collect();
        sorted_tests.sort();

        // Output
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "changedFiles": changed_files,
                    "affectedTests": sorted_tests,
                    "totalDependentsTraversed": all_dependents.len(),
                }))
                .map_err(|e| e.to_string())?
            );
        } else if quiet {
            for t in &sorted_tests {
                println!("{t}");
            }
        } else if sorted_tests.is_empty() {
            info("No test files affected by the changed files.");
        } else {
            println!(
                "{}",
                bold(&format!(
                    "\nAffected test files ({}):\n",
                    sorted_tests.len()
                ))
            );
            for t in &sorted_tests {
                println!("  {}", cyan(t));
            }
            println!();
        }

        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Affected analysis failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph install
fn cmd_install(
    target: Option<String>,
    location: Option<String>,
    yes: bool,
    no_permissions: bool,
    print_config: Option<String>,
) {
    if let Some(id) = print_config {
        let Some(target) = get_target(&id) else {
            let known = list_target_ids()
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            error_msg(&format!("Unknown target \"{id}\". Known: {known}."));
            process::exit(1);
        };
        let loc = if location.as_deref() == Some("local") {
            Location::Local
        } else {
            Location::Global
        };
        print!("{}", target.print_config(loc));
        let _ = io::stdout().flush();
        return;
    }

    if let Some(loc) = &location {
        if loc != "global" && loc != "local" {
            error_msg(&format!(
                "--location must be \"global\" or \"local\" (got \"{loc}\")."
            ));
            process::exit(1);
        }
    }

    // Commander's `--no-permissions` makes `opts.permissions === false`;
    // omitting the flag leaves it `true` (the positive-form default).
    // We MUST treat the default-true as "user did not override — let
    // the orchestrator prompt" and only forward an explicit `false`
    // (or `true` when --yes implies it). Otherwise the auto-allow
    // prompt is silently skipped on every interactive run.
    let auto_allow: Option<bool> = if no_permissions {
        Some(false)
    } else if yes {
        Some(true)
    } else {
        None
    };

    let opts = RunInstallerOptions {
        target,
        location: location.as_deref().map(|l| {
            if l == "local" {
                Location::Local
            } else {
                Location::Global
            }
        }),
        auto_allow,
        yes,
    };

    if let Err(err) = run_installer_with_options(&opts) {
        error_msg(&err.to_string());
        process::exit(1);
    }
}

/// codegraph uninstall
///
/// Inverse of `install`. Removes the codegraph MCP server entry,
/// instructions block, and permissions from every agent (or a
/// `--target` subset). Prompts global-vs-local when not given. Does NOT
/// delete the `.codegraph/` index — that's `codegraph uninit`.
fn cmd_uninstall(target: Option<String>, location: Option<String>, yes: bool) {
    if let Some(loc) = &location {
        if loc != "global" && loc != "local" {
            error_msg(&format!(
                "--location must be \"global\" or \"local\" (got \"{loc}\")."
            ));
            process::exit(1);
        }
    }

    let opts = RunUninstallerOptions {
        target,
        location: location.as_deref().map(|l| {
            if l == "local" {
                Location::Local
            } else {
                Location::Global
            }
        }),
        yes,
    };

    if let Err(err) = run_uninstaller(&opts) {
        error_msg(&err.to_string());
        process::exit(1);
    }
}
