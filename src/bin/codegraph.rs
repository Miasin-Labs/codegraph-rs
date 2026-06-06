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
//!   codegraph context <task>     Build ready-to-inject context for a task
//!                                (--budget <tokens>, --strategy classic|analysis)
//!   codegraph analyze <cmd>      Analysis engine over the bridged index
//!                                (query, complexity, communities, dominators,
//!                                slice, cycles, impact, taint, co-change,
//!                                coverage, validate, traits, centrality,
//!                                critical, export, types, generics,
//!                                boundaries, capabilities, schema, stats,
//!                                cfg, dataflow)
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
use codegraph::analysis_bridge::{
    BridgeOptions,
    BridgeResult,
    build_analysis_graph_cached,
    build_analysis_graph_cached_with_options,
    compute_index_fingerprint,
    load_auto_base_snapshot,
    load_explicit_base_snapshot,
    store_complexity_sidecar,
};
use codegraph::analyze::{self, SliceDirection};
use codegraph::context_analysis::{self, AnalysisContextOptions};
use codegraph::db::{DatabaseConnection, QueryBuilder, get_database_path};
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
    BuildContextOptions,
    CodeGraph,
    ContextFormat,
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
    TaskInput,
    analyze_ir,
};
use codegraph_analysis::nodes::NodeId as ANodeId;

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
    /// Build ready-to-inject context for a task description
    Context {
        #[arg(value_name = "task")]
        task: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Trim output to roughly this many tokens (~4 chars/token)
        #[arg(short = 'b', long, value_name = "tokens")]
        budget: Option<String>,
        /// Selection strategy: "classic" (FTS + graph expansion) or
        /// "analysis" (token-budgeted, dataflow-seeded engine)
        #[arg(
            short = 's',
            long,
            value_name = "classic|analysis",
            default_value = "classic"
        )]
        strategy: String,
        /// Carry field/property metadata through the analysis bridge so big
        /// structs render as field-level partial views (only the fields the
        /// selected symbols touch). Analysis strategy only; equivalent to
        /// CODEGRAPH_ANALYSIS_FIELDS=1. The analysis snapshot cache is keyed
        /// by this flag, so flipping it rebuilds the bridged graph.
        #[arg(long)]
        fields: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
        /// Print capability notes (seeding mode, gate decisions, trims) to stderr
        #[arg(short = 'v', long)]
        verbose: bool,
    },
    /// Run analysis-engine queries over the indexed project graph
    Analyze {
        #[command(subcommand)]
        command: AnalyzeCommands,
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

/// `codegraph analyze` — the analysis engine (`codegraph-analysis`) running
/// over the bridged SQLite index. Pure reads of the index itself; the
/// bridged graph is snapshotted under `.codegraph/analysis/` and reused
/// until the index changes (`--no-cache` forces a rebuild).
#[derive(Subcommand)]
enum AnalyzeCommands {
    /// Run a pipe-based DSL query over the project graph
    #[command(after_help = "Examples (taken from the engine's test suite):
  codegraph analyze query 'fn(\"main\") | callees | depth 3'
      Everything main() reaches within 3 call hops.
  codegraph analyze query 'path fn(\"main\") -> fn(\"helper\")'
      Shortest call path between two functions (hops listed in path order).
  codegraph analyze query 'scc'
      Strongly-connected components: every mutual-recursion cluster.

Grammar: seed with fn(\"name\"), type(\"Name\"), entrypoints, scc, or hot N;
pipe through callers, callees, depth N, filter kind=K, since N,
reachable via \"Calls+\" [incoming], ...; combine with set algebra
(union / intersect / \\), path patterns (path A -> B,
paths A -> B via Calls), and aggregations (count, exists, group_by, ...).
Use --explain to print the optimised plan without executing, --why to
include per-row provenance (which operators produced each result).
The `untested` operator needs coverage data: pass --lcov <path> to
annotate the graph before the query runs (see `analyze coverage`).")]
    Query {
        /// The DSL query, e.g. 'fn("main") | callees | depth 3'
        #[arg(value_name = "dsl")]
        query: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum result nodes
        #[arg(long = "max-nodes", value_name = "number", default_value = "50")]
        max_nodes: String,
        /// Include why-provenance: which operators/predecessors produced each row
        #[arg(long)]
        why: bool,
        /// Print the optimised query plan without executing it
        #[arg(long)]
        explain: bool,
        /// Annotate LCOV coverage before the query runs (enables `untested`)
        #[arg(long, value_name = "path")]
        lcov: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Per-function complexity metrics (cyclomatic, cognitive, nesting, maintainability)
    Complexity {
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Show the N most complex functions
        #[arg(short = 't', long, value_name = "number", default_value = "20")]
        top: String,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Detect call-graph communities/modules (Louvain)
    Communities {
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum members listed per community
        #[arg(long, value_name = "number", default_value = "8")]
        sample: String,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Dominator analysis from an entry symbol (what every path must pass through)
    Dominators {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum reachable nodes analyzed
        #[arg(short = 't', long, value_name = "number", default_value = "50")]
        top: String,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Program slice from a symbol (call-graph granularity by default)
    Slice {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Slice direction: "fwd" (what it influences) or "bwd" (what affects it)
        #[arg(long, value_name = "fwd|bwd", default_value = "fwd")]
        direction: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum slice depth (call hops)
        #[arg(long, value_name = "number", default_value = "10")]
        depth: String,
        /// Value-level precision via per-function dataflow IR (needs byte
        /// offsets in the index; re-index pre-v5 projects to enable)
        #[arg(long = "value-level")]
        value_level: bool,
        /// Compact source-annotated report (one "name (file:line)" entry per
        /// line) plus the direct data-dependency set, rendered by the
        /// analysis engine's CPG facade. Fixed engine depth; --depth ignored.
        #[arg(long = "source")]
        source_annotated: bool,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Strongly-connected components: mutual recursion and dependency cycles
    Cycles {
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Signature-edit cascade: direct call sites to update, grouped by file
    Impact {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Proposed new signature shown in the generated tasks
        #[arg(short = 's', long, value_name = "signature")]
        signature: Option<String>,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Find call-graph paths from a source symbol to a sink symbol
    Taint {
        /// Source symbol (omit together with sink for --suggest mode)
        #[arg(value_name = "source-symbol")]
        source: Option<String>,
        /// Sink symbol
        #[arg(value_name = "sink-symbol")]
        sink: Option<String>,
        /// Suggest source/sink candidates by identifier naming instead of
        /// tracing paths (the default when no symbols are given)
        #[arg(long)]
        suggest: bool,
        /// Value-level precision via per-function dataflow IR (needs byte
        /// offsets in the index; re-index pre-v5 projects to enable)
        #[arg(long = "value-level")]
        value_level: bool,
        /// Compact source-annotated flow report (each flow rendered hop by
        /// hop as "name (file:line)" with sanitizer status), rendered by the
        /// analysis engine's CPG facade
        #[arg(long = "source")]
        source_annotated: bool,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum intermediate nodes per path
        #[arg(long = "max-nodes", value_name = "number", default_value = "6")]
        max_nodes: String,
        /// Maximum suggested pairs (--suggest mode)
        #[arg(short = 't', long, value_name = "number", default_value = "20")]
        top: String,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Temporal coupling: symbols that change together across git history
    #[command(name = "co-change")]
    CoChange {
        /// Limit pairs to those touching this symbol
        #[arg(value_name = "symbol")]
        symbol: Option<String>,
        /// Minimum co-occurrence count for a pair to be reported
        #[arg(long = "min-support", value_name = "number", default_value = "2")]
        min_support: String,
        /// Maximum commits mined from git log
        #[arg(long = "max-commits", value_name = "number", default_value = "500")]
        max_commits: String,
        /// Show the N strongest pairs
        #[arg(short = 't', long, value_name = "number", default_value = "25")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Map LCOV coverage onto functions; find untested code
    #[command(
        after_help = "Annotating coverage also enables the analysis DSL's `untested` \
operator: `codegraph analyze query --lcov coverage/lcov.info 'entrypoints | untested'`."
    )]
    Coverage {
        /// Path to an LCOV-format coverage file (e.g. coverage/lcov.info)
        #[arg(long, value_name = "path")]
        lcov: String,
        /// List only untested functions
        #[arg(long)]
        untested: bool,
        /// Show at most N functions
        #[arg(short = 't', long, value_name = "number", default_value = "50")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Simulate a signature (arity) change before making it
    Validate {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Parameter count before the edit
        #[arg(long = "params-before", value_name = "number")]
        params_before: String,
        /// Parameter count after the edit
        #[arg(long = "params-after", value_name = "number")]
        params_after: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Trait/interface hierarchies, dispatch calls, and type clusters
    Traits {
        /// Limit output to this trait/type (exact name or qualified suffix)
        #[arg(value_name = "type")]
        type_name: Option<String>,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// PageRank centrality: the most depended-upon symbols
    Centrality {
        /// Show the N most central symbols
        #[arg(short = 't', long, value_name = "number", default_value = "20")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Articulation nodes and bridge edges: single points of failure
    Critical {
        /// Show at most N nodes and N edges
        #[arg(short = 't', long, value_name = "number", default_value = "25")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Export the graph as Graphviz DOT (pipe to `dot -Tsvg`)
    Export {
        /// Output format (only "dot" is supported)
        #[arg(short = 'f', long, value_name = "format", default_value = "dot")]
        format: String,
        /// Export only this symbol's neighborhood instead of the whole graph
        #[arg(short = 's', long, value_name = "symbol")]
        symbol: Option<String>,
        /// Neighborhood depth around --symbol (hops, both directions)
        #[arg(short = 'd', long, value_name = "number", default_value = "2")]
        depth: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Concrete types that can flow into/out of a function
    Types {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Generic definitions and callsite-supplied instantiations
    Generics {
        /// Limit output to this symbol (exact name or qualified suffix)
        #[arg(value_name = "symbol")]
        symbol: Option<String>,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Cross-language boundaries: HTTP routes, FFI and WASM exports
    Boundaries {
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Analysis-engine capability toggles and their env kill-switches
    Capabilities {
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Print the JSON Schema for an engine payload kind
    #[command(
        after_help = "Known kinds: query_result, entrypoint_summary, context_result, \
formatted_output."
    )]
    Schema {
        /// Payload kind (e.g. query_result)
        #[arg(value_name = "kind")]
        kind: String,
        /// Output as JSON (the schema is already JSON; printed identically)
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Bridged-graph statistics, optionally with reachability profiling
    Stats {
        /// Add per-node reachability counts (exact on small graphs,
        /// HyperLogLog estimates on large ones)
        #[arg(long = "estimate-reachability")]
        estimate_reachability: bool,
        /// Show the N widest-reaching nodes
        #[arg(short = 't', long, value_name = "number", default_value = "10")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Control-flow graph of one function: basic blocks + typed edges
    #[command(
        after_help = "Built by re-parsing the on-disk source with the host grammars and \
anchoring the function at its indexed position. CFG rules cover Rust, TypeScript/TSX, \
JavaScript/JSX, Python, Go, Java, C, C++, and PHP; other languages get an honest \
capability note."
    )]
    Cfg {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Per-function dataflow: params, returns, assignments, argument flows, mutations
    #[command(
        after_help = "Built by re-parsing the on-disk source with the host grammars and \
anchoring the function at its indexed position. Dataflow rules cover Rust, \
TypeScript/TSX, JavaScript/JSX, Python, and Go; other languages get an honest \
capability note."
    )]
    Dataflow {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Working-tree vs base: what changed since the last cached snapshot
    #[command(
        after_help = "Compares the current bridged graph against a base snapshot: \
nodes/edges added/removed/changed, complexity deltas for changed functions, \
newly-introduced cycles, and the impact set of the delta. --base auto (default) \
uses the last cached snapshot built before the current index state — every \
analyze command caches one, and one previous generation is kept under \
.codegraph/analysis/*.prev. The run also annotates the current snapshot with \
per-function complexity so the NEXT diff reports full before/after deltas."
    )]
    Diff {
        /// Base snapshot: "auto", or a path to a snapshot file / cache directory
        #[arg(long, value_name = "snapshot|auto", default_value = "auto")]
        base: String,
        /// Impact BFS depth from the changed/added/removed symbols
        #[arg(long, value_name = "number", default_value = "3")]
        depth: String,
        /// Maximum entries listed per section
        #[arg(short = 't', long, value_name = "number", default_value = "50")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
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
        Commands::Context {
            task,
            path,
            budget,
            strategy,
            fields,
            json,
            verbose,
        } => cmd_context(
            &task,
            path.as_deref(),
            budget.as_deref(),
            &strategy,
            fields,
            json,
            verbose,
        ),
        Commands::Analyze { command } => cmd_analyze(command),
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

            // Human display only: relevance relative to the best hit (top = 100%).
            // Raw scores stack FTS bm25 + name/kind/path bonuses and routinely
            // exceed 1.0, so an absolute percent reads as nonsense like "(11012%)".
            // JSON output keeps the raw parity-faithful score.
            let max_score = results.iter().map(|r| r.score).fold(f64::EPSILON, f64::max);

            for result in &results {
                let node = &result.node;
                let location = format!("{}:{}", node.file_path, node.start_line);
                let score = dim(&format!(
                    "({}%)",
                    js_to_fixed((result.score / max_score) * 100.0, 0)
                ));

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

// =============================================================================
// context command
// =============================================================================

/// codegraph context <task> [--budget tokens] [--strategy classic|analysis] [--fields]
///
/// `classic` (default) is the existing `ContextBuilder` pipeline (FTS entry
/// points + graph expansion) — its output is unchanged. `analysis` routes
/// through the analysis engine's context modules over the bridged index:
/// dataflow-seeded entry points, retrieval-gated expansion, clustered
/// per-file source, rendered to markdown and trimmed to the token budget.
/// `--fields` (analysis only) bridges with field/property carrying so the
/// engine's partial-struct views render — same effect as
/// `CODEGRAPH_ANALYSIS_FIELDS=1`, scoped to this invocation.
fn cmd_context(
    task: &str,
    path_arg: Option<&str>,
    budget_arg: Option<&str>,
    strategy: &str,
    fields: bool,
    json: bool,
    verbose: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let budget_tokens = match budget_arg {
        Some(raw) => match parse_int_js(raw) {
            Some(n) if n > 0 => Some(n as usize),
            _ => {
                error_msg(&format!(
                    "Invalid --budget \"{raw}\" — expected a positive token count"
                ));
                process::exit(1);
            }
        },
        None => None,
    };

    let body = || -> Result<(), String> {
        match strategy {
            "classic" => {
                if fields {
                    eprintln!(
                        "warning: --fields requires --strategy analysis (ignored for classic)"
                    );
                }
                cmd_context_classic(task, &project_path, budget_tokens, json, verbose)
            }
            "analysis" => {
                cmd_context_analysis(task, &project_path, budget_tokens, fields, json, verbose)
            }
            other => {
                error_msg(&format!(
                    "Invalid --strategy \"{other}\" — expected \"classic\" or \"analysis\""
                ));
                process::exit(1);
            }
        }
    };

    if let Err(msg) = body() {
        error_msg(&format!("Context build failed: {msg}"));
        process::exit(1);
    }
}

/// The pre-existing `ContextBuilder` path. Without `--budget` the printed
/// output is exactly what `CodeGraph::build_context` returns (regression-
/// pinned); `--budget` applies a plain output trim on top (markdown only —
/// trimming would corrupt the JSON shape).
fn cmd_context_classic(
    task: &str,
    project_path: &Path,
    budget_tokens: Option<usize>,
    json: bool,
    verbose: bool,
) -> Result<(), String> {
    if !is_initialized(project_path) {
        error_msg(&format!(
            "CodeGraph not initialized in {}",
            project_path.display()
        ));
        info("Run \"codegraph init\" first");
        process::exit(1);
    }

    let cg = CodeGraph::open(project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
    let options = BuildContextOptions {
        format: Some(if json {
            ContextFormat::Json
        } else {
            ContextFormat::Markdown
        }),
        ..Default::default()
    };
    let output = cg
        .build_context(&TaskInput::Text(task.to_string()), Some(&options))
        .map_err(|e| e.to_string())?;
    cg.close();

    let output = match (budget_tokens, json) {
        (Some(tokens), false) => {
            let (trimmed, truncated) = context_analysis::trim_to_token_budget(&output, tokens);
            if verbose && truncated {
                eprintln!(
                    "note: classic output trimmed to ~{tokens} tokens (use --strategy analysis \
                     for budget-aware selection)"
                );
            }
            trimmed
        }
        (Some(_), true) => {
            eprintln!("warning: --budget is ignored for classic JSON output");
            output
        }
        (None, _) => output,
    };
    println!("{output}");
    Ok(())
}

/// The analysis-engine path: bridge the index, run the engine's context
/// pipeline (`codegraph::context_analysis`), print markdown (or the full
/// JSON report). Capability notes go to stderr under `--verbose` and are
/// always present in the JSON report.
///
/// `fields` ORs into the environment's bridge options
/// (`CODEGRAPH_ANALYSIS_FIELDS=1`) — the flag can only ADD field carrying,
/// never strip it from an env-enabled run.
fn cmd_context_analysis(
    task: &str,
    project_path: &Path,
    budget_tokens: Option<usize>,
    fields: bool,
    json: bool,
    verbose: bool,
) -> Result<(), String> {
    let mut options = BridgeOptions::from_env();
    options.include_fields = options.include_fields || fields;
    let bridged = bridge_project_with_options(project_path, false, json, &options)?;
    let report = context_analysis::build_analysis_context(
        &bridged.graph,
        project_path,
        task,
        &AnalysisContextOptions {
            budget_tokens,
            ..Default::default()
        },
    );

    if verbose {
        for note in &report.notes {
            eprintln!("note: {note}");
        }
        eprintln!(
            "note: strategy=analysis seeding={} measured-tokens={}{}",
            match report.seeding {
                context_analysis::SeedingMode::Dataflow => "dataflow",
                context_analysis::SeedingMode::CallGraph => "call-graph",
            },
            report.measured_tokens,
            report
                .budget_tokens
                .map(|t| format!(" budget-tokens={t}"))
                .unwrap_or_default(),
        );
    }

    if json {
        print_json(&report)
    } else {
        println!("{}", report.markdown);
        Ok(())
    }
}

// =============================================================================
// analyze command family
//
// The analysis engine (`codegraph-analysis`) running over the
// project's bridged SQLite index (`analysis_bridge::build_analysis_graph`).
// All commands are pure reads of the index. Report shapes live in
// `codegraph::analyze`; this file only resolves symbols and renders.
//
// The bridged graph is snapshotted under `.codegraph/analysis/` keyed by an
// index fingerprint, so repeat invocations skip the full SQL re-read
// (`analysis_bridge::build_analysis_graph_cached`). `--no-cache` forces a
// rebuild; cache hits print a one-line "(cached graph)" notice in human
// output only — `--json` stays pure JSON.
// =============================================================================

/// Entry cap for the `analyze slice --source` annotated lists (slice +
/// data dependencies). The engine summarizes anything beyond the cap.
const SOURCE_REPORT_MAX_ENTRIES: usize = 50;

/// Rendered-flow cap for `analyze taint --source` — same cap the default
/// taint path rendering uses; the engine summarizes flows beyond it.
const SOURCE_TAINT_MAX_PATHS: usize = 25;

/// Bridge the project's index into the analysis engine, via the snapshot
/// cache unless `no_cache`. Exits (status 1) when the project is not
/// initialized — same contract as the other read commands.
///
/// Bridge options come from the process environment
/// (`CODEGRAPH_ANALYSIS_FIELDS=1` turns on field carrying for every
/// analyze command); [`bridge_project_with_options`] is the explicit-flag
/// variant `context --fields` uses.
fn bridge_project(project_path: &Path, no_cache: bool, json: bool) -> Result<BridgeResult, String> {
    bridge_project_with_options(project_path, no_cache, json, &BridgeOptions::from_env())
}

/// [`bridge_project`] with explicit [`BridgeOptions`]. The snapshot cache
/// is keyed by the options, so a graph bridged under one flag state is
/// never served to the other.
fn bridge_project_with_options(
    project_path: &Path,
    no_cache: bool,
    json: bool,
    options: &BridgeOptions,
) -> Result<BridgeResult, String> {
    if !is_initialized(project_path) {
        error_msg(&format!(
            "CodeGraph not initialized in {}",
            project_path.display()
        ));
        info("Run \"codegraph init\" first");
        process::exit(1);
    }
    let conn =
        DatabaseConnection::open(get_database_path(project_path)).map_err(|e| e.to_string())?;
    let queries = QueryBuilder::new(conn.get_db().map_err(|e| e.to_string())?);
    let cached =
        build_analysis_graph_cached_with_options(&queries, project_path, !no_cache, options)
            .map_err(|e| e.to_string())?;
    if cached.from_cache && !json {
        println!("{}", dim("(cached graph)"));
    }
    Ok(cached.result)
}

/// Resolve a user-supplied symbol to its analysis-graph node via the index
/// search, using the same exact-match conventions as `callers`/`callees`/
/// `impact` (exact name or `.`/`::`-qualified suffix wins; otherwise the top
/// search hit that the bridge mapped).
fn resolve_analysis_symbol(
    cg: &CodeGraph,
    id_map: &HashMap<String, ANodeId>,
    symbol: &str,
) -> Result<Option<ANodeId>, String> {
    let matches = cg
        .search_nodes(
            symbol,
            Some(&SearchOptions {
                limit: Some(50),
                ..Default::default()
            }),
        )
        .map_err(|e| e.to_string())?;
    for m in &matches {
        if is_exact_symbol_match(&m.node.name, symbol) || matches.len() == 1 {
            if let Some(aid) = id_map.get(&m.node.id) {
                return Ok(Some(aid.clone()));
            }
        }
    }
    // Fallback: top search hit with an analysis mapping (skipped node kinds
    // like variables/imports have no analysis node).
    for m in &matches {
        if let Some(aid) = id_map.get(&m.node.id) {
            return Ok(Some(aid.clone()));
        }
    }
    Ok(None)
}

/// Resolve a symbol with the host index open/closed around it.
fn resolve_symbol_via_index(
    project_path: &Path,
    id_map: &HashMap<String, ANodeId>,
    symbol: &str,
) -> Result<Option<ANodeId>, String> {
    let cg = CodeGraph::open(project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
    let resolved = resolve_analysis_symbol(&cg, id_map, symbol);
    cg.close();
    resolved
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(())
}

/// Print an `analyze` report wrapped in the versioned JSON envelope —
/// `{"schemaVersion": N, "kind": "<kind>", "data": …}` (see
/// [`analyze::ReportEnvelope`]). Every `analyze … --json` goes through here.
fn print_report_json<T: serde::Serialize>(kind: &'static str, data: &T) -> Result<(), String> {
    print_json(&analyze::ReportEnvelope::new(kind, data))
}

fn print_symbol_line(kind: &str, name: &str, file: &str, line: u32) {
    let loc = if line != 0 {
        format!(":{line}")
    } else {
        String::new()
    };
    println!("{}{}", cyan(&format!("{kind:<12}")), white(name));
    println!("{}", dim(&format!("  {file}{loc}")));
}

/// codegraph analyze <subcommand>
fn cmd_analyze(command: AnalyzeCommands) {
    match command {
        AnalyzeCommands::Query {
            query,
            path,
            max_nodes,
            why,
            explain,
            lcov,
            no_cache,
            json,
        } => cmd_analyze_query(
            &query,
            path.as_deref(),
            &max_nodes,
            why,
            explain,
            lcov.as_deref(),
            no_cache,
            json,
        ),
        AnalyzeCommands::Complexity {
            path,
            top,
            no_cache,
            json,
        } => cmd_analyze_complexity(path.as_deref(), &top, no_cache, json),
        AnalyzeCommands::Communities {
            path,
            sample,
            no_cache,
            json,
        } => cmd_analyze_communities(path.as_deref(), &sample, no_cache, json),
        AnalyzeCommands::Dominators {
            symbol,
            path,
            top,
            no_cache,
            json,
        } => cmd_analyze_dominators(&symbol, path.as_deref(), &top, no_cache, json),
        AnalyzeCommands::Slice {
            symbol,
            direction,
            path,
            depth,
            value_level,
            source_annotated,
            no_cache,
            json,
        } => cmd_analyze_slice(
            &symbol,
            &direction,
            path.as_deref(),
            &depth,
            value_level,
            source_annotated,
            no_cache,
            json,
        ),
        AnalyzeCommands::Cycles {
            path,
            no_cache,
            json,
        } => cmd_analyze_cycles(path.as_deref(), no_cache, json),
        AnalyzeCommands::Impact {
            symbol,
            signature,
            path,
            no_cache,
            json,
        } => cmd_analyze_impact(
            &symbol,
            signature.as_deref(),
            path.as_deref(),
            no_cache,
            json,
        ),
        AnalyzeCommands::Taint {
            source,
            sink,
            suggest,
            value_level,
            source_annotated,
            path,
            max_nodes,
            top,
            no_cache,
            json,
        } => cmd_analyze_taint(
            source.as_deref(),
            sink.as_deref(),
            suggest,
            value_level,
            source_annotated,
            path.as_deref(),
            &max_nodes,
            &top,
            no_cache,
            json,
        ),
        AnalyzeCommands::CoChange {
            symbol,
            min_support,
            max_commits,
            top,
            path,
            no_cache,
            json,
        } => cmd_analyze_co_change(
            symbol.as_deref(),
            &min_support,
            &max_commits,
            &top,
            path.as_deref(),
            no_cache,
            json,
        ),
        AnalyzeCommands::Coverage {
            lcov,
            untested,
            top,
            path,
            no_cache,
            json,
        } => cmd_analyze_coverage(&lcov, untested, &top, path.as_deref(), no_cache, json),
        AnalyzeCommands::Validate {
            symbol,
            params_before,
            params_after,
            path,
            no_cache,
            json,
        } => cmd_analyze_validate(
            &symbol,
            &params_before,
            &params_after,
            path.as_deref(),
            no_cache,
            json,
        ),
        AnalyzeCommands::Traits {
            type_name,
            path,
            no_cache,
            json,
        } => cmd_analyze_traits(type_name.as_deref(), path.as_deref(), no_cache, json),
        AnalyzeCommands::Centrality {
            top,
            path,
            no_cache,
            json,
        } => cmd_analyze_centrality(&top, path.as_deref(), no_cache, json),
        AnalyzeCommands::Critical {
            top,
            path,
            no_cache,
            json,
        } => cmd_analyze_critical(&top, path.as_deref(), no_cache, json),
        AnalyzeCommands::Export {
            format,
            symbol,
            depth,
            path,
            no_cache,
            json,
        } => cmd_analyze_export(
            &format,
            symbol.as_deref(),
            &depth,
            path.as_deref(),
            no_cache,
            json,
        ),
        AnalyzeCommands::Types {
            symbol,
            path,
            no_cache,
            json,
        } => cmd_analyze_types(&symbol, path.as_deref(), no_cache, json),
        AnalyzeCommands::Generics {
            symbol,
            path,
            no_cache,
            json,
        } => cmd_analyze_generics(symbol.as_deref(), path.as_deref(), no_cache, json),
        AnalyzeCommands::Boundaries {
            path,
            no_cache,
            json,
        } => cmd_analyze_boundaries(path.as_deref(), no_cache, json),
        AnalyzeCommands::Capabilities { json } => cmd_analyze_capabilities(json),
        AnalyzeCommands::Schema { kind, json } => cmd_analyze_schema(&kind, json),
        AnalyzeCommands::Stats {
            estimate_reachability,
            top,
            path,
            no_cache,
            json,
        } => cmd_analyze_stats(estimate_reachability, &top, path.as_deref(), no_cache, json),
        AnalyzeCommands::Cfg {
            symbol,
            path,
            no_cache,
            json,
        } => cmd_analyze_cfg(&symbol, path.as_deref(), no_cache, json),
        AnalyzeCommands::Dataflow {
            symbol,
            path,
            no_cache,
            json,
        } => cmd_analyze_dataflow(&symbol, path.as_deref(), no_cache, json),
        AnalyzeCommands::Diff {
            base,
            depth,
            top,
            path,
            no_cache,
            json,
        } => cmd_analyze_diff(&base, &depth, &top, path.as_deref(), no_cache, json),
    }
}

/// codegraph analyze query "<dsl>" [--why] [--explain] [--lcov <path>]
///
/// Runs the analysis engine's pipe-based query DSL over the bridged graph.
/// `--explain` parses + optimises only (never touches the index, so it works
/// without an initialized project); `--why` adds per-row provenance;
/// `--lcov` annotates coverage onto the in-memory graph first so the
/// `untested` operator returns real rows instead of treating every function
/// as untested.
#[allow(clippy::too_many_arguments)]
fn cmd_analyze_query(
    query: &str,
    path_arg: Option<&str>,
    max_nodes_arg: &str,
    why: bool,
    explain: bool,
    lcov: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let body = || -> Result<(), String> {
        if explain {
            let report = analyze::explain_report(query)?;
            if json {
                return print_report_json("queryPlan", &report);
            }
            println!(
                "{}",
                bold(&format!(
                    "\nOptimised plan ({} query, not executed):\n",
                    report.kind
                ))
            );
            for (i, step) in report.steps.iter().enumerate() {
                println!("  {} {}", dim(&format!("{}.", i + 1)), white(step));
            }
            println!();
            info(&format!(
                "BFS schedule hint: {}{}",
                report.strategy,
                if report.parallel { " (parallel)" } else { "" }
            ));
            return Ok(());
        }

        let project_path = resolve_project_path(path_arg);
        let mut bridged = bridge_project(&project_path, no_cache, json)?;
        if let Some(lcov_path) = lcov {
            analyze::annotate_coverage(&mut bridged.graph, Path::new(lcov_path), &project_path)?;
        }
        let max_nodes = parse_int_js(max_nodes_arg).unwrap_or(50).max(1) as usize;
        let report = analyze::query_report_with_sources(
            &bridged.graph,
            query,
            max_nodes,
            why,
            Some(&project_path),
        )?;

        if json {
            return print_report_json("query", &report);
        }

        if report.nodes.is_empty() && report.metadata.is_empty() {
            info("Query matched no nodes");
            return Ok(());
        }

        if !report.nodes.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "\nQuery results ({} node{}{}):\n",
                    report.node_count,
                    if report.node_count == 1 { "" } else { "s" },
                    if report.truncated {
                        format!(" of {}", report.total_before_truncation)
                    } else {
                        String::new()
                    }
                ))
            );
            let kind_w = report
                .nodes
                .iter()
                .map(|n| n.kind.len())
                .chain(["KIND".len()])
                .max()
                .unwrap_or(4);
            let name_w = report
                .nodes
                .iter()
                .map(|n| n.name.len())
                .chain(["NAME".len()])
                .max()
                .unwrap_or(4);
            println!(
                "  {}  {}  {}",
                dim(&format!("{:<kind_w$}", "KIND")),
                dim(&format!("{:<name_w$}", "NAME")),
                dim("LOCATION")
            );
            for row in &report.nodes {
                let loc = if row.line != 0 {
                    format!("{}:{}", row.file, row.line)
                } else {
                    row.file.clone()
                };
                println!(
                    "  {}  {}  {}",
                    cyan(&format!("{:<kind_w$}", row.kind)),
                    white(&format!("{:<name_w$}", row.name)),
                    dim(&loc)
                );
            }
            println!();
        }

        if !report.edges.is_empty() {
            const EDGE_CAP: usize = 25;
            println!("{}", bold("Edges:"));
            for edge in report.edges.iter().take(EDGE_CAP) {
                println!(
                    "  {} {} {} {}",
                    white(&edge.from),
                    dim("->"),
                    white(&edge.to),
                    dim(&format!("({})", edge.kind))
                );
            }
            if report.edges.len() > EDGE_CAP {
                println!(
                    "{}",
                    dim(&format!(
                        "  ... {} more (use --json for all)",
                        report.edges.len() - EDGE_CAP
                    ))
                );
            }
            println!();
        }

        if !report.metadata.is_empty() {
            for line in &report.metadata {
                println!("{}", dim(line));
            }
            println!();
        }

        if let Some(pre) = &report.preconditions {
            if !pre.guards.is_empty() {
                println!("{}", bold("Guarding conditions (source-level):"));
                for guard in &pre.guards {
                    println!(
                        "  {} {} {} {}",
                        white(&guard.caller.name),
                        dim("->"),
                        white(&guard.callee),
                        dim(&format!("({}:{})", guard.file, guard.line))
                    );
                    println!("    {}", cyan(&guard.conditions.join(" -> ")));
                }
                println!();
            }
            info(&pre.note);
        }

        if why {
            match &report.why {
                Some(entries) if !entries.is_empty() => {
                    println!("{}", bold("Why (provenance):"));
                    for entry in entries {
                        for step in &entry.steps {
                            let origin = if step.predecessors.is_empty() {
                                "seed".to_string()
                            } else {
                                format!("from {}", step.predecessors.join(", "))
                            };
                            println!(
                                "  {} {}",
                                white(&entry.symbol.name),
                                dim(&format!("<- {} ({origin}, stage {})", step.op, step.stage))
                            );
                        }
                    }
                    println!();
                }
                Some(_) => {}
                None => info("why-provenance is not available for aggregation queries"),
            }
        }

        if report.truncated {
            info(&format!(
                "Result truncated to {} nodes {} raise --max-nodes for more",
                report.node_count,
                get_glyphs().dash
            ));
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze query failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze complexity [--top N]
fn cmd_analyze_complexity(path_arg: Option<&str>, top_arg: &str, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let top = parse_int_js(top_arg).unwrap_or(20).max(1) as usize;
        let report = analyze::complexity_report(&bridged.graph, &project_path, top);

        if json {
            return print_report_json("complexity", &report);
        }

        if report.functions.is_empty() {
            info("No functions with complexity metrics found (run with --json for skip reasons)");
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nMost complex functions (top {} of {} analyzed):\n",
                report.functions.len(),
                format_number(report.functions_analyzed as u64)
            ))
        );
        for f in &report.functions {
            let mi = f
                .maintainability_index
                .map(|v| format!(", MI {}", js_to_fixed(v, 0)))
                .unwrap_or_default();
            print_symbol_line(
                &f.symbol.kind,
                &f.symbol.name,
                &f.symbol.file,
                f.symbol.line,
            );
            println!(
                "{}",
                dim(&format!(
                    "  cyclomatic {}, cognitive {}, nesting {}{mi}",
                    f.cyclomatic, f.cognitive, f.max_nesting
                ))
            );
            println!();
        }

        let skipped_total: usize = report.skipped.values().sum();
        if skipped_total > 0 {
            info(&format!(
                "{} of {} functions skipped (unsupported language or unreadable source) {} use --json for the breakdown",
                format_number(skipped_total as u64),
                format_number(
                    (report.functions_total
                        + report.skipped.get("placeholder").copied().unwrap_or(0))
                        as u64
                ),
                get_glyphs().dash
            ));
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze complexity failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze communities
fn cmd_analyze_communities(path_arg: Option<&str>, sample_arg: &str, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let sample = parse_int_js(sample_arg).unwrap_or(8).max(1) as usize;
        let report = analyze::communities_report(&bridged.graph, sample);

        if json {
            return print_report_json("communities", &report);
        }

        if report.communities.is_empty() {
            info("No multi-member call-graph communities found");
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nCall-graph communities ({} multi-member, modularity {}):\n",
                report.multi_member_count,
                js_to_fixed(report.modularity, 2)
            ))
        );
        for community in &report.communities {
            println!(
                "{} {}",
                cyan(&format!("Community {}", community.id)),
                dim(&format!("({} symbols)", community.size))
            );
            if !community.top_files.is_empty() {
                println!(
                    "{}",
                    dim(&format!("  files: {}", community.top_files.join(", ")))
                );
            }
            let names: Vec<&str> = community.members.iter().map(|m| m.name.as_str()).collect();
            let more = if community.truncated {
                format!(" (+{} more)", community.size - community.members.len())
            } else {
                String::new()
            };
            println!("  {}{}", names.join(", "), dim(&more));
            println!();
        }
        info(&format!(
            "{} symbols without call relationships remain singleton communities",
            format_number(report.singleton_count as u64)
        ));

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze communities failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze dominators <symbol>
fn cmd_analyze_dominators(
    symbol: &str,
    path_arg: Option<&str>,
    top_arg: &str,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(entry) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let top = parse_int_js(top_arg).unwrap_or(50).max(1) as usize;
        let Some(report) = analyze::dominators_report(&bridged.graph, &entry, top) else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("dominators", &report);
        }

        if report.nodes.is_empty() {
            info(&format!("No nodes reachable from \"{symbol}\""));
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nDominators from \"{symbol}\" ({} reachable nodes analyzed):\n",
                report.analyzed
            ))
        );
        for entry in &report.nodes {
            print_symbol_line(
                &entry.symbol.kind,
                &entry.symbol.name,
                &entry.symbol.file,
                entry.symbol.line,
            );
            if let Some(idom) = &entry.immediate_dominator {
                println!(
                    "{}",
                    dim(&format!(
                        "  immediate dominator: {} (chain depth {})",
                        idom.name, entry.dominator_depth
                    ))
                );
            }
            println!();
        }
        if report.truncated {
            info(&format!(
                "Output capped {} raise with --top to analyze more nodes",
                get_glyphs().dash
            ));
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze dominators failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze slice <symbol> [--direction fwd|bwd]
#[allow(clippy::too_many_arguments)]
fn cmd_analyze_slice(
    symbol: &str,
    direction_arg: &str,
    path_arg: Option<&str>,
    depth_arg: &str,
    value_level: bool,
    source_annotated: bool,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let direction = match direction_arg {
        "fwd" | "forward" => SliceDirection::Forward,
        "bwd" | "backward" => SliceDirection::Backward,
        other => {
            error_msg(&format!(
                "--direction must be \"fwd\" or \"bwd\" (got \"{other}\")."
            ));
            process::exit(1);
        }
    };
    if source_annotated && value_level {
        error_msg(
            "--source and --value-level are mutually exclusive: the --source report already \
             rides the engine's value-level oracle when the index carries byte offsets.",
        );
        process::exit(1);
    }

    if source_annotated {
        let body = || -> Result<(), String> {
            let bridged = bridge_project(&project_path, no_cache, json)?;
            let report = analyze::source_slice_report(
                &bridged.graph,
                &project_path,
                symbol,
                direction,
                SOURCE_REPORT_MAX_ENTRIES,
            );
            if json {
                return print_report_json("sliceSource", &report);
            }
            println!();
            println!("{}", report.report.trim_end());
            println!();
            println!("{}", report.data_dependencies.trim_end());
            println!();
            warn(&report.note);
            Ok(())
        };
        if let Err(msg) = body() {
            error_msg(&format!("analyze slice failed: {msg}"));
            process::exit(1);
        }
        return;
    }

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(seed) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let depth = parse_int_js(depth_arg).unwrap_or(10).clamp(1, 100) as usize;
        let report = if value_level {
            analyze_ir::value_slice_report(&bridged.graph, &project_path, &seed, direction, depth)
        } else {
            analyze::slice_report(&bridged.graph, &seed, direction, depth)
        };
        let Some(report) = report else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("slice", &report);
        }

        if report.nodes.is_empty() {
            if value_level {
                // The value-level note explains *why* the slice is empty
                // (no value flow, fallback, or coverage gaps) — print it
                // instead of the generic no-call-edges line.
                info(&format!("Slice from \"{symbol}\" is empty"));
                warn(&report.note);
            } else {
                info(&format!(
                    "Slice from \"{symbol}\" is empty {} no call edges in that direction",
                    get_glyphs().dash
                ));
            }
            return Ok(());
        }

        let heading = match direction {
            SliceDirection::Forward => "Forward slice",
            SliceDirection::Backward => "Backward slice",
        };
        let granularity = if report.granularity == "value-level" {
            "value-level hops"
        } else {
            "call hops"
        };
        println!(
            "{}",
            bold(&format!(
                "\n{heading} of \"{symbol}\" ({} symbols within {} {granularity}):\n",
                report.size, report.max_depth
            ))
        );
        for node in &report.nodes {
            print_symbol_line(&node.kind, &node.name, &node.file, node.line);
            println!();
        }
        warn(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze slice failed: {msg}"));
        process::exit(1);
    }
}

/// Human label for a cycle kind emitted by `analyze::cycles_report`.
fn cycle_kind_label(kind: &str) -> &'static str {
    match kind {
        "mutualRecursion" => "mutual recursion",
        "selfRecursion" => "direct recursion",
        "moduleCycle" => "module cycle",
        _ => "mixed cycle",
    }
}

/// codegraph analyze cycles
fn cmd_analyze_cycles(path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let report = analyze::cycles_report(&bridged.graph);

        if json {
            return print_report_json("cycles", &report);
        }

        if report.cycles.is_empty() {
            success("No dependency cycles or recursion clusters found");
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nDependency cycles and recursion clusters ({}):\n",
                report.cycle_count
            ))
        );
        for cycle in &report.cycles {
            println!(
                "{} {}",
                cyan(cycle_kind_label(&cycle.kind)),
                dim(&format!(
                    "({} member{})",
                    cycle.size,
                    if cycle.size == 1 { "" } else { "s" }
                ))
            );
            for member in &cycle.members {
                let loc = if member.line != 0 {
                    format!(":{}", member.line)
                } else {
                    String::new()
                };
                println!(
                    "  {} {}",
                    white(&member.name),
                    dim(&format!("{}{loc}", member.file))
                );
            }
            println!();
        }
        for suggestion in &report.break_suggestions {
            info(&format!(
                "Break suggestion: remove the {} -> {} edge",
                suggestion.from.name, suggestion.to.name
            ));
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze cycles failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze impact <symbol> [--signature <sig>]
fn cmd_analyze_impact(
    symbol: &str,
    signature: Option<&str>,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(target) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(report) = analyze::impact_report(&bridged.graph, &target, signature) else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("impact", &report);
        }

        if report.tasks.is_empty() {
            info(&format!(
                "No call sites found for \"{symbol}\" {} a signature edit cascades nowhere",
                get_glyphs().dash
            ));
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nSignature-edit cascade for \"{symbol}\" {} {} call site{} in {} file{}:\n",
                get_glyphs().dash,
                report.call_site_count,
                if report.call_site_count == 1 { "" } else { "s" },
                report.task_count,
                if report.task_count == 1 { "" } else { "s" }
            ))
        );
        println!(
            "{}",
            dim(&format!("New signature: {}", report.new_signature))
        );
        println!();
        for task in &report.tasks {
            println!("{}", cyan(&task.file));
            for site in &task.call_sites {
                let loc = if site.line != 0 {
                    format!(":{}", site.line)
                } else {
                    String::new()
                };
                println!("  {}{}", white(&site.caller), dim(&loc));
            }
            println!();
        }
        info(
            "Cascade lists the direct call sites a signature edit must update; for the transitive blast radius use \"codegraph impact\".",
        );

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze impact failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze taint [source-symbol] [sink-symbol] [--suggest]
///
/// With both symbols: call-graph paths from source to sink (the existing
/// behavior). With `--suggest` — or when both symbols are omitted — ranks
/// candidate sources/sinks by identifier naming instead.
#[allow(clippy::too_many_arguments)]
fn cmd_analyze_taint(
    source: Option<&str>,
    sink: Option<&str>,
    suggest: bool,
    value_level: bool,
    source_annotated: bool,
    path_arg: Option<&str>,
    max_nodes_arg: &str,
    top_arg: &str,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    if source_annotated && value_level {
        error_msg(
            "--source and --value-level are mutually exclusive: the --source report already \
             rides the engine's value-level oracle when the index carries byte offsets.",
        );
        process::exit(1);
    }

    if suggest || (source.is_none() && sink.is_none()) {
        if value_level {
            error_msg(
                "--value-level applies to source\u{2192}sink tracing; give both \
                 <source-symbol> and <sink-symbol> (it has no effect on --suggest).",
            );
            process::exit(1);
        }
        if source_annotated {
            error_msg(
                "--source applies to source\u{2192}sink tracing; give both <source-symbol> \
                 and <sink-symbol> (it has no effect on --suggest).",
            );
            process::exit(1);
        }
        let body = || -> Result<(), String> {
            let bridged = bridge_project(&project_path, no_cache, json)?;
            let top = parse_int_js(top_arg).unwrap_or(20).max(1) as usize;
            let report = analyze::taint_suggest_report(&bridged.graph, top);

            if json {
                return print_report_json("taintSuggest", &report);
            }

            if report.source_count == 0 && report.sink_count == 0 {
                info(&report.note);
                return Ok(());
            }

            println!(
                "{}",
                bold(&format!(
                    "\nSuggested taint candidates ({} source{}, {} sink{} of {} functions):\n",
                    report.source_count,
                    if report.source_count == 1 { "" } else { "s" },
                    report.sink_count,
                    if report.sink_count == 1 { "" } else { "s" },
                    format_number(report.functions_classified as u64)
                ))
            );
            if !report.sources.is_empty() {
                println!("{}", cyan("Sources (named like untrusted input):"));
                for candidate in &report.sources {
                    println!(
                        "  {} {}",
                        white(&candidate.symbol.name),
                        dim(&format!(
                            "{} (score {})",
                            candidate.symbol.file,
                            js_to_fixed(candidate.score, 2)
                        ))
                    );
                }
                println!();
            }
            if !report.sinks.is_empty() {
                println!("{}", cyan("Sinks (named like dangerous operations):"));
                for candidate in &report.sinks {
                    println!(
                        "  {} {}",
                        white(&candidate.symbol.name),
                        dim(&format!(
                            "{} (score {})",
                            candidate.symbol.file,
                            js_to_fixed(candidate.score, 2)
                        ))
                    );
                }
                println!();
            }
            if !report.pairs.is_empty() {
                println!("{}", cyan("Top pairs to confirm:"));
                for pair in &report.pairs {
                    println!(
                        "  {} {} {} {}",
                        white(&pair.source.name),
                        dim("->"),
                        white(&pair.sink.name),
                        dim(&format!("(priority {})", js_to_fixed(pair.priority, 2)))
                    );
                }
                println!();
            }
            info(&report.note);
            Ok(())
        };
        if let Err(msg) = body() {
            error_msg(&format!("analyze taint failed: {msg}"));
            process::exit(1);
        }
        return;
    }

    let (Some(source), Some(sink)) = (source, sink) else {
        error_msg(
            "analyze taint needs both <source-symbol> and <sink-symbol> (or --suggest / no \
             symbols for name-based suggestion).",
        );
        process::exit(1);
    };

    if source_annotated {
        let body = || -> Result<(), String> {
            let bridged = bridge_project(&project_path, no_cache, json)?;
            let report = analyze::source_taint_report(
                &bridged.graph,
                &project_path,
                source,
                sink,
                SOURCE_TAINT_MAX_PATHS,
            );
            if json {
                return print_report_json("taintSource", &report);
            }
            println!();
            println!("{}", report.report.trim_end());
            println!();
            warn(&report.note);
            Ok(())
        };
        if let Err(msg) = body() {
            error_msg(&format!("analyze taint failed: {msg}"));
            process::exit(1);
        }
        return;
    }

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let source_id = resolve_analysis_symbol(&cg, &bridged.id_map, source)?;
        let sink_id = resolve_analysis_symbol(&cg, &bridged.id_map, sink)?;
        cg.close();
        let Some(source_id) = source_id else {
            info(&format!(
                "Symbol \"{source}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(sink_id) = sink_id else {
            info(&format!(
                "Symbol \"{sink}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        let max_nodes = parse_int_js(max_nodes_arg).unwrap_or(6).clamp(0, 32) as usize;
        let report = if value_level {
            analyze_ir::value_taint_report(
                &bridged.graph,
                &project_path,
                &source_id,
                &sink_id,
                max_nodes,
                25,
            )
        } else {
            analyze::taint_report(&bridged.graph, &source_id, &sink_id, max_nodes, 25)
        };
        let Some(report) = report else {
            info("Source or sink not found in the analysis graph");
            return Ok(());
        };

        if json {
            return print_report_json("taint", &report);
        }

        if report.paths.is_empty() {
            if value_level {
                // The value-level note explains the absence (no value flow,
                // call-graph fallback, or coverage gaps) — print it instead
                // of the generic intermediate-node-cap line.
                if report.granularity == "value-level" {
                    info(&format!(
                        "No value-level flow from \"{source}\" to \"{sink}\""
                    ));
                } else {
                    info(&format!("No paths from \"{source}\" to \"{sink}\""));
                }
                warn(&report.note);
            } else {
                info(&format!(
                    "No paths from \"{source}\" to \"{sink}\" within {} intermediate nodes",
                    report.max_intermediate_nodes
                ));
            }
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nPaths from \"{source}\" to \"{sink}\" ({} found{}):\n",
                report.path_count,
                if report.truncated {
                    format!(", showing {}", report.paths.len())
                } else {
                    String::new()
                }
            ))
        );
        for path in &report.paths {
            let names: Vec<&str> = path.nodes.iter().map(|n| n.name.as_str()).collect();
            println!("  {}", white(&names.join(" -> ")));
            println!(
                "{}",
                dim(&format!("    via {}", path.edge_kinds.join(", ")))
            );
            println!();
        }
        warn(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze taint failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze co-change [symbol] [--min-support N] [--max-commits N]
fn cmd_analyze_co_change(
    symbol: Option<&str>,
    min_support_arg: &str,
    max_commits_arg: &str,
    top_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let seed = match symbol {
            Some(symbol) => {
                let Some(seed) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)?
                else {
                    info(&format!(
                        "Symbol \"{symbol}\" not found in the analysis graph"
                    ));
                    return Ok(());
                };
                Some(seed)
            }
            None => None,
        };
        let min_support = parse_int_js(min_support_arg).unwrap_or(2).max(1) as u32;
        let max_commits = parse_int_js(max_commits_arg).unwrap_or(500).max(1) as usize;
        let top = parse_int_js(top_arg).unwrap_or(25).max(1) as usize;
        let report = analyze::co_change_report(
            &bridged.graph,
            &project_path,
            seed.as_ref(),
            min_support,
            max_commits,
            top,
        );

        if json {
            return print_report_json("coChange", &report);
        }

        if report.commits_analyzed == 0 {
            warn(&report.note);
            return Ok(());
        }
        if report.pairs.is_empty() {
            info(&format!(
                "No cross-file co-change pairs at min support {} across {} commits{}",
                report.min_support,
                format_number(report.commits_analyzed as u64),
                symbol
                    .map(|s| format!(" touching \"{s}\""))
                    .unwrap_or_default()
            ));
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nCo-change pairs ({} of {} cross-file, {} commits, min support {}):\n",
                report.pairs.len(),
                format_number(report.cross_file_pair_count as u64),
                format_number(report.commits_analyzed as u64),
                report.min_support
            ))
        );
        for pair in &report.pairs {
            println!(
                "  {} {} {}",
                white(&pair.a.name),
                dim("<->"),
                white(&pair.b.name)
            );
            println!(
                "{}",
                dim(&format!(
                    "    together {}x, confidence {} ({} <-> {})",
                    pair.times_changed_together,
                    js_to_fixed(pair.confidence, 2),
                    pair.a.file,
                    pair.b.file
                ))
            );
        }
        println!();
        if report.same_file_pair_count > 0 {
            info(&format!(
                "{} same-file pairs folded (same-file symbols co-change by construction)",
                format_number(report.same_file_pair_count as u64)
            ));
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze co-change failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze coverage --lcov <path> [--untested]
fn cmd_analyze_coverage(
    lcov: &str,
    untested_only: bool,
    top_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let mut bridged = bridge_project(&project_path, no_cache, json)?;
        let top = parse_int_js(top_arg).unwrap_or(50).max(1) as usize;
        let report = analyze::coverage_report(
            &mut bridged.graph,
            Path::new(lcov),
            &project_path,
            untested_only,
            top,
        )?;

        if json {
            return print_report_json("coverage", &report);
        }

        println!(
            "{}",
            bold(&format!(
                "\nCoverage: {} tested / {} untested of {} functions ({} LCOV files):\n",
                format_number(report.functions_tested as u64),
                format_number(report.functions_untested as u64),
                format_number(report.functions_total as u64),
                report.lcov_files
            ))
        );
        for function in &report.functions {
            print_symbol_line(
                &function.symbol.kind,
                &function.symbol.name,
                &function.symbol.file,
                function.symbol.line,
            );
            println!(
                "{}",
                if function.tested {
                    dim(&format!("  tested ({} hits)", function.coverage_count))
                } else {
                    yellow("  untested")
                }
            );
            println!();
        }
        if report.truncated {
            info(&format!(
                "Listing capped at {} {} raise with --top for more",
                report.functions.len(),
                get_glyphs().dash
            ));
        }
        if report.parse_warnings > 0 {
            warn(&format!(
                "{} malformed LCOV lines skipped",
                report.parse_warnings
            ));
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze coverage failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze validate <symbol> --params-before N --params-after M
fn cmd_analyze_validate(
    symbol: &str,
    params_before_arg: &str,
    params_after_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let parse_params = |label: &str, value: &str| -> usize {
        match parse_int_js(value) {
            Some(n) if n >= 0 => n as usize,
            _ => {
                error_msg(&format!(
                    "--{label} must be a non-negative integer (got \"{value}\")."
                ));
                process::exit(1);
            }
        }
    };
    let params_before = parse_params("params-before", params_before_arg);
    let params_after = parse_params("params-after", params_after_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(target) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(report) =
            analyze::validate_report(&bridged.graph, &target, params_before, params_after)
        else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("validate", &report);
        }

        println!(
            "{}",
            bold(&format!(
                "\nSignature change for \"{symbol}\": {} -> {} parameter{}\n",
                report.params_before,
                report.params_after,
                if report.params_after == 1 { "" } else { "s" }
            ))
        );
        if report.is_safe {
            success(&format!(
                "Safe: no incompatible callers ({} compatible)",
                report.compatible.len()
            ));
        } else {
            warn(&format!(
                "Unsafe: {} caller{} updating",
                report.incompatible.len(),
                if report.incompatible.len() == 1 {
                    " needs"
                } else {
                    "s need"
                }
            ));
            println!();
            for caller in &report.incompatible {
                print_symbol_line(
                    &caller.symbol.kind,
                    &caller.symbol.name,
                    &caller.symbol.file,
                    caller.symbol.line,
                );
                println!("{}", dim(&format!("  {}", caller.reason)));
                println!();
            }
        }
        if !report.call_sites.is_empty() {
            println!("{}", cyan("Affected call sites:"));
            for site in &report.call_sites {
                let loc = if site.line != 0 {
                    format!(":{}", site.line)
                } else {
                    String::new()
                };
                println!(
                    "  {} {}",
                    white(&site.caller),
                    dim(&format!("{}{loc}", site.file))
                );
            }
            println!();
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze validate failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze traits [type]
fn cmd_analyze_traits(type_name: Option<&str>, path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let report = analyze::traits_report(&bridged.graph, type_name);

        if json {
            return print_report_json("traits", &report);
        }

        if report.hierarchies.is_empty() && report.clusters.is_empty() {
            match type_name {
                Some(filter) => info(&format!(
                    "No trait hierarchy or type cluster matches \"{filter}\""
                )),
                None => info(&report.note),
            }
            return Ok(());
        }

        if !report.hierarchies.is_empty() {
            println!(
                "{}",
                bold(&format!("\nTrait hierarchies ({}):\n", report.trait_count))
            );
            for hierarchy in &report.hierarchies {
                println!(
                    "{} {}",
                    cyan(&hierarchy.trait_ref.name),
                    dim(&format!(
                        "({} implementor{}, {})",
                        hierarchy.implementor_count,
                        if hierarchy.implementor_count == 1 {
                            ""
                        } else {
                            "s"
                        },
                        hierarchy.trait_ref.file
                    ))
                );
                for implementor in &hierarchy.implementors {
                    println!("  {} {}", white(&implementor.name), dim(&implementor.file));
                }
                println!();
            }
        }

        if !report.dispatch_calls.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "Trait-dispatch calls ({}):\n",
                    report.dispatch_call_count
                ))
            );
            for dispatch in &report.dispatch_calls {
                println!(
                    "  {} {} {} {}",
                    white(&dispatch.caller.name),
                    dim("->"),
                    white(&dispatch.callee.name),
                    dim(&format!("(via {})", dispatch.trait_ref.name))
                );
            }
            println!();
        }

        if !report.clusters.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "Functions clustered by primary type ({}):\n",
                    report.cluster_count
                ))
            );
            for cluster in &report.clusters {
                let names: Vec<&str> = cluster.functions.iter().map(|f| f.name.as_str()).collect();
                let more = if cluster.truncated {
                    format!(
                        " (+{} more)",
                        cluster.function_count - cluster.functions.len()
                    )
                } else {
                    String::new()
                };
                println!(
                    "{} {}",
                    cyan(&cluster.primary_type.name),
                    dim(&format!("({} functions)", cluster.function_count))
                );
                println!("  {}{}", names.join(", "), dim(&more));
                println!();
            }
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze traits failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze centrality [--top N]
fn cmd_analyze_centrality(top_arg: &str, path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let top = parse_int_js(top_arg).unwrap_or(20).max(1) as usize;
        let report = analyze::centrality_report(&bridged.graph, top);

        if json {
            return print_report_json("centrality", &report);
        }

        if report.nodes.is_empty() {
            info("No symbols to rank (empty graph)");
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nMost central symbols (PageRank over {} nodes, damping {}):\n",
                format_number(report.analyzed as u64),
                js_to_fixed(report.damping_factor, 2)
            ))
        );
        for ranked in &report.nodes {
            print_symbol_line(
                &ranked.symbol.kind,
                &ranked.symbol.name,
                &ranked.symbol.file,
                ranked.symbol.line,
            );
            println!(
                "{}",
                dim(&format!("  score {}", js_to_fixed(ranked.score, 4)))
            );
            println!();
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze centrality failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze critical
fn cmd_analyze_critical(top_arg: &str, path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let top = parse_int_js(top_arg).unwrap_or(25).max(1) as usize;
        let report = analyze::critical_report(&bridged.graph, top);

        if json {
            return print_report_json("critical", &report);
        }

        if report.nodes.is_empty() && report.bridges.is_empty() {
            success("No articulation nodes or bridge edges — no single point of failure");
            return Ok(());
        }

        if !report.nodes.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "\nArticulation nodes ({}) — removal disconnects the graph:\n",
                    report.articulation_count
                ))
            );
            for node in &report.nodes {
                print_symbol_line(&node.kind, &node.name, &node.file, node.line);
                println!();
            }
        }
        if !report.bridges.is_empty() {
            println!(
                "{}",
                bold(&format!("Bridge edges ({}):\n", report.bridge_count))
            );
            for bridge in &report.bridges {
                println!(
                    "  {} {} {}",
                    white(&bridge.from.name),
                    dim("->"),
                    white(&bridge.to.name)
                );
            }
            println!();
        }
        if report.truncated {
            info(&format!(
                "Output capped {} raise with --top for more",
                get_glyphs().dash
            ));
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze critical failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze export --format dot [--symbol <s> --depth N]
///
/// Human output is the raw DOT document (pipe straight to `dot -Tsvg`);
/// `--json` wraps it in the envelope.
fn cmd_analyze_export(
    format: &str,
    symbol: Option<&str>,
    depth_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    if format != "dot" {
        error_msg(&format!(
            "--format must be \"dot\" (got \"{format}\"); other formats are not supported yet."
        ));
        process::exit(1);
    }
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        // The DOT goes to stdout verbatim — suppress the human-mode cache
        // notice so the output stays pipeable.
        let bridged = bridge_project(&project_path, no_cache, true)?;
        let seed = match symbol {
            Some(symbol) => {
                let Some(seed) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)?
                else {
                    info(&format!(
                        "Symbol \"{symbol}\" not found in the analysis graph"
                    ));
                    return Ok(());
                };
                Some(seed)
            }
            None => None,
        };
        let depth = parse_int_js(depth_arg).unwrap_or(2).clamp(1, 64) as usize;
        let Some(report) = analyze::export_report(&bridged.graph, seed.as_ref(), depth) else {
            info(&format!(
                "Symbol \"{}\" not found in the analysis graph",
                symbol.unwrap_or_default()
            ));
            return Ok(());
        };

        if json {
            return print_report_json("export", &report);
        }

        print!("{}", report.dot);
        let _ = io::stdout().flush();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze export failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze types <symbol>
fn cmd_analyze_types(symbol: &str, path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let mut bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(target) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(report) = analyze::types_report(&mut bridged.graph, &target)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("types", &report);
        }

        println!(
            "{}",
            bold(&format!("\nPossible concrete types for \"{symbol}\":\n"))
        );
        if report.input_types.is_empty() && report.return_types.is_empty() {
            info(&report.note);
            return Ok(());
        }
        if !report.input_types.is_empty() {
            println!(
                "{} {}",
                cyan("inputs: "),
                white(&report.input_types.join(", "))
            );
        }
        if !report.return_types.is_empty() {
            println!(
                "{} {}",
                cyan("returns:"),
                white(&report.return_types.join(", "))
            );
        }
        println!();
        info(&format!(
            "{} functions annotated by the propagation pass",
            format_number(report.functions_annotated as u64)
        ));
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze types failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze generics [symbol]
fn cmd_analyze_generics(symbol: Option<&str>, path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let report = analyze::generics_report(&bridged.graph, symbol);

        if json {
            return print_report_json("generics", &report);
        }

        if !report.instantiations.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "\nGeneric instantiations ({}):\n",
                    report.instantiation_count
                ))
            );
            for instantiation in &report.instantiations {
                println!(
                    "  {} {} {} {}",
                    white(&instantiation.generic.name),
                    dim("<-"),
                    white(&instantiation.callsite.name),
                    dim(&format!("[{}]", instantiation.type_args.join(", ")))
                );
            }
            println!();
        }
        if !report.likely_generic_definitions.is_empty() {
            println!(
                "{}",
                bold(&format!(
                    "\nLikely generic definitions ({}, signature heuristic):\n",
                    report.likely_generic_count
                ))
            );
            for definition in &report.likely_generic_definitions {
                print_symbol_line(
                    &definition.symbol.kind,
                    &definition.symbol.name,
                    &definition.symbol.file,
                    definition.symbol.line,
                );
                println!(
                    "{}",
                    dim(&format!(
                        "  type params: {}",
                        definition.type_params.join(", ")
                    ))
                );
                println!();
            }
        }
        if report.instantiations.is_empty() && report.likely_generic_definitions.is_empty() {
            match symbol {
                Some(filter) => info(&format!("No generic definition matches \"{filter}\"")),
                None => info("No generic definitions detected"),
            }
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze generics failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze boundaries
fn cmd_analyze_boundaries(path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let mut bridged = bridge_project(&project_path, no_cache, json)?;
        let report = analyze::boundaries_report(&mut bridged.graph);

        if json {
            return print_report_json("boundaries", &report);
        }

        if report.boundary_count == 0 {
            warn(&report.note);
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nCross-language boundaries ({}):\n",
                report.boundary_count
            ))
        );
        if !report.http_routes.is_empty() {
            println!("{}", cyan("HTTP routes:"));
            for route in &report.http_routes {
                println!(
                    "  {} {} {}",
                    white(&format!("{} {}", route.method, route.path)),
                    dim("->"),
                    white(&route.provider.name)
                );
            }
            println!();
        }
        if !report.ffi_exports.is_empty() {
            println!("{}", cyan("FFI exports (C ABI):"));
            for export in &report.ffi_exports {
                println!(
                    "  {} {}",
                    white(&export.symbol_name),
                    dim(&export.provider.file)
                );
            }
            println!();
        }
        if !report.wasm_boundaries.is_empty() {
            println!("{}", cyan("WASM boundaries:"));
            for boundary in &report.wasm_boundaries {
                let module = boundary
                    .module
                    .as_ref()
                    .map(|m| format!("{m}."))
                    .unwrap_or_default();
                println!(
                    "  {} {}{} {}",
                    dim(&boundary.direction),
                    dim(&module),
                    white(&boundary.name),
                    dim(&boundary.provider.file)
                );
            }
            println!();
        }
        info(&format!(
            "Cross-language stitching: {} clients seen, {} call edges emitted",
            report.cross_language_calls.clients_seen, report.cross_language_calls.edges_emitted
        ));
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze boundaries failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze capabilities
///
/// Pure environment read — works without an initialized project.
fn cmd_analyze_capabilities(json: bool) {
    let report = analyze::capabilities_report();

    if json {
        if let Err(msg) = print_report_json("capabilities", &report) {
            error_msg(&format!("analyze capabilities failed: {msg}"));
            process::exit(1);
        }
        return;
    }

    println!("{}", bold("\nAnalysis-engine capabilities:\n"));
    for capability in &report.capabilities {
        let state = if capability.enabled {
            green("on ")
        } else {
            red("off")
        };
        println!(
            "  {} {} {}",
            state,
            white(&format!("{:<18}", capability.name)),
            dim(&capability.env_var)
        );
        if let Some(value) = &capability.env_value {
            println!("{}", dim(&format!("      env override: \"{value}\"")));
        }
        if !capability.disables.is_empty() {
            println!(
                "{}",
                dim(&format!(
                    "      disabling also disables: {}",
                    capability.disables.join(", ")
                ))
            );
        }
    }
    println!();
    info(&report.note);
}

/// codegraph analyze schema <kind>
///
/// Prints the engine's JSON Schema document verbatim (it is already JSON, so
/// `--json` prints the same bytes). Works without an initialized project.
fn cmd_analyze_schema(kind: &str, _json: bool) {
    match analyze::schema_text(kind) {
        Ok(schema) => println!("{schema}"),
        Err(msg) => {
            error_msg(&format!("analyze schema failed: {msg}"));
            process::exit(1);
        }
    }
}

/// codegraph analyze stats [--estimate-reachability]
fn cmd_analyze_stats(
    estimate_reachability: bool,
    top_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let top = parse_int_js(top_arg).unwrap_or(10).max(1) as usize;
        let report = analyze::stats_report(&bridged.graph, estimate_reachability, top);

        if json {
            return print_report_json("stats", &report);
        }

        println!("{}", bold("\nBridged analysis graph:\n"));
        let nodes_by_kind: Vec<String> = report
            .nodes_by_kind
            .iter()
            .map(|(kind, count)| format!("{kind} {}", format_number(*count as u64)))
            .collect();
        let edges_by_kind: Vec<String> = report
            .edges_by_kind
            .iter()
            .map(|(kind, count)| format!("{kind} {}", format_number(*count as u64)))
            .collect();
        println!(
            "  {} {}",
            white(&format!(
                "{} nodes",
                format_number(report.node_count as u64)
            )),
            dim(&format!("({})", nodes_by_kind.join(", ")))
        );
        println!(
            "  {} {}",
            white(&format!(
                "{} edges",
                format_number(report.edge_count as u64)
            )),
            dim(&format!("({})", edges_by_kind.join(", ")))
        );
        println!(
            "  {} {}",
            white(&format!(
                "{} files",
                format_number(report.file_count as u64)
            )),
            dim(&format!(
                "{} unresolved-call placeholders",
                format_number(report.placeholder_count as u64)
            ))
        );
        println!();

        if let Some(reachability) = &report.reachability {
            println!(
                "{}",
                bold(&format!(
                    "Widest-reaching symbols ({}):\n",
                    reachability.method
                ))
            );
            for entry in &reachability.top {
                println!(
                    "  {} {}",
                    white(&entry.symbol.name),
                    dim(&format!(
                        "reaches {}, reached by {} ({})",
                        js_to_fixed(entry.descendants, 0),
                        js_to_fixed(entry.ancestors, 0),
                        entry.symbol.file
                    ))
                );
            }
            println!();
            info(&reachability.note);
        }

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze stats failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze cfg <symbol>
///
/// Control-flow graph of one function, built by re-parsing its on-disk
/// source with the host grammars (the `analyze complexity` anchor pattern).
/// Languages without engine CFG rules get the report's honest capability
/// note instead of an empty graph.
fn cmd_analyze_cfg(symbol: &str, path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(target) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(report) = analyze_ir::cfg_report(&bridged.graph, &project_path, &target) else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("cfg", &report);
        }

        if !report.analyzed {
            // Honest capability note — never an empty block list.
            warn(&report.note);
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!(
                "\nControl-flow graph of \"{}\" ({} block{}, {} edge{}):\n",
                report.symbol.name,
                report.block_count,
                if report.block_count == 1 { "" } else { "s" },
                report.edge_count,
                if report.edge_count == 1 { "" } else { "s" },
            ))
        );
        print_symbol_line(
            &report.symbol.kind,
            &report.symbol.name,
            &report.symbol.file,
            report.symbol.line,
        );
        println!();
        println!("{}", cyan("Blocks:"));
        for block in &report.blocks {
            let lines = if block.start_line == 0 && block.end_line == 0 {
                String::new()
            } else if block.start_line == block.end_line {
                format!("  line {}", block.start_line)
            } else {
                format!("  lines {}-{}", block.start_line, block.end_line)
            };
            println!(
                "  {} {}{}",
                white(&format!("[{}] {}", block.id, block.label)),
                dim(&format!("({})", block.kind)),
                dim(&lines)
            );
        }
        println!();
        println!("{}", cyan("Edges:"));
        for edge in &report.edges {
            println!(
                "  {} {} {} {}",
                white(&edge.from.to_string()),
                dim("->"),
                white(&edge.to.to_string()),
                dim(&format!("({})", edge.kind))
            );
        }
        println!();
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze cfg failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph analyze dataflow <symbol>
///
/// Per-function dataflow facts (params, returns, assignments, argument
/// flows, mutations), same source re-parse anchoring as `analyze cfg`.
fn cmd_analyze_dataflow(symbol: &str, path_arg: Option<&str>, no_cache: bool, json: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        let bridged = bridge_project(&project_path, no_cache, json)?;
        let Some(target) = resolve_symbol_via_index(&project_path, &bridged.id_map, symbol)? else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };
        let Some(report) = analyze_ir::dataflow_report(&bridged.graph, &project_path, &target)
        else {
            info(&format!(
                "Symbol \"{symbol}\" not found in the analysis graph"
            ));
            return Ok(());
        };

        if json {
            return print_report_json("dataflow", &report);
        }

        if !report.analyzed {
            // Honest capability note — never empty fact sections.
            warn(&report.note);
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!("\nDataflow of \"{}\":\n", report.symbol.name))
        );
        print_symbol_line(
            &report.symbol.kind,
            &report.symbol.name,
            &report.symbol.file,
            report.symbol.line,
        );
        println!();

        if !report.params.is_empty() {
            println!("{}", cyan("Params:"));
            for p in &report.params {
                let ty = p
                    .type_annotation
                    .as_deref()
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default();
                let default = if p.has_default { " (has default)" } else { "" };
                println!(
                    "  {}{}",
                    white(&format!("{}{ty}", p.name)),
                    dim(&format!("  position {}{default}", p.position))
                );
            }
            println!();
        }
        if !report.assignments.is_empty() {
            println!("{}", cyan("Assignments:"));
            for a in &report.assignments {
                println!(
                    "  {} {}",
                    white(&a.target),
                    dim(&format!("<- {} (line {})", a.source_kind, a.line))
                );
            }
            println!();
        }
        if !report.returns.is_empty() {
            println!("{}", cyan("Returns:"));
            for r in &report.returns {
                println!(
                    "  {} {}",
                    white(&r.expression),
                    dim(&format!("(line {})", r.line))
                );
            }
            println!();
        }
        if !report.arg_flows.is_empty() {
            println!("{}", cyan("Argument flows:"));
            for f in &report.arg_flows {
                let from = f
                    .source_param
                    .as_deref()
                    .map(|p| format!("param {p} -> "))
                    .unwrap_or_default();
                println!(
                    "  {} {}",
                    white(&format!("{from}{} arg {}", f.callee, f.arg_position)),
                    dim(&format!("(line {})", f.line))
                );
            }
            println!();
        }
        if !report.mutations.is_empty() {
            println!("{}", cyan("Mutations:"));
            for m in &report.mutations {
                println!(
                    "  {} {}",
                    white(&format!("{}.{}", m.target, m.method)),
                    dim(&format!("(line {})", m.line))
                );
            }
            println!();
        }
        if report.params.is_empty()
            && report.assignments.is_empty()
            && report.returns.is_empty()
            && report.arg_flows.is_empty()
            && report.mutations.is_empty()
        {
            info(&format!(
                "No dataflow facts in \"{}\" {} the body has no params, assignments, returns, \
                 argument flows, or mutations",
                report.symbol.name,
                get_glyphs().dash
            ));
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze dataflow failed: {msg}"));
        process::exit(1);
    }
}

/// The honest no-base note `analyze diff` prints (exit 0): a diff needs a
/// snapshot of the pre-edit state, and any analyze command caches one.
const NO_BASE_NOTE: &str = "no base snapshot — run any analyze command on the base state first";

/// codegraph analyze diff [--base <snapshot|auto>] [--depth N] [--top N]
///
/// Working-tree vs base. Bridges the current index FIRST (which refreshes
/// the snapshot cache — rotation preserves the pre-edit generation as
/// `.prev`), then resolves the base: `auto` picks the last cached snapshot
/// built from a different index fingerprint (stale current generation, else
/// `.prev`); an explicit path loads a snapshot file or cache directory.
/// After diffing, the working tree's per-function complexity is written as
/// a sidecar next to the current snapshot so the NEXT diff has before
/// metrics.
fn cmd_analyze_diff(
    base_arg: &str,
    depth_arg: &str,
    top_arg: &str,
    path_arg: Option<&str>,
    no_cache: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        // Same init/open contract as `bridge_project`, but the fingerprint
        // is needed for base resolution, so the cache wrapper is driven
        // directly here.
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            info("Run \"codegraph init\" first");
            process::exit(1);
        }
        let conn = DatabaseConnection::open(get_database_path(&project_path))
            .map_err(|e| e.to_string())?;
        let queries = QueryBuilder::new(conn.get_db().map_err(|e| e.to_string())?);
        let fingerprint = compute_index_fingerprint(&queries).map_err(|e| e.to_string())?;
        let cached = build_analysis_graph_cached(&queries, &project_path, !no_cache)
            .map_err(|e| e.to_string())?;
        if cached.from_cache && !json {
            println!("{}", dim("(cached graph)"));
        }
        let bridged = cached.result;

        let base = if base_arg == "auto" {
            load_auto_base_snapshot(&project_path, fingerprint)
        } else {
            Some(
                load_explicit_base_snapshot(Path::new(base_arg))
                    .map_err(|e| format!("cannot load base snapshot \"{base_arg}\": {e}"))?,
            )
        };
        let Some(base) = base else {
            // Honest no-base case (exit 0): nothing older than the working
            // tree is cached. This run primed the cache, so after the next
            // edit + re-index a plain `analyze diff` will work.
            if json {
                return print_report_json(
                    "diff",
                    &serde_json::json!({ "baseAvailable": false, "note": NO_BASE_NOTE }),
                );
            }
            info(NO_BASE_NOTE);
            return Ok(());
        };

        let depth = parse_int_js(depth_arg).unwrap_or(3).max(1) as usize;
        let top = parse_int_js(top_arg).unwrap_or(50).max(1) as usize;
        let current_complexity = analyze::measure_complexity_map(&bridged.graph, &project_path);
        let report = analyze::diff_report(&base, &bridged.graph, &current_complexity, depth, top);
        // Best-effort: annotate the current generation so the next diff has
        // before-metrics. A failure only degrades that future report.
        let _ = store_complexity_sidecar(&project_path, fingerprint, &current_complexity);

        if json {
            return print_report_json("diff", &report);
        }

        let base_label = match &report.base.index_fingerprint {
            Some(fp) => format!("{} {fp}", report.base.source),
            None => report.base.source.clone(),
        };
        if report.nodes_added_count == 0
            && report.nodes_removed_count == 0
            && report.nodes_changed_count == 0
            && report.edges_added_count == 0
            && report.edges_removed_count == 0
        {
            success(&format!(
                "No differences vs the base snapshot ({base_label})"
            ));
            return Ok(());
        }

        println!(
            "{}",
            bold(&format!("\nDiff vs base snapshot ({base_label}):\n"))
        );

        println!(
            "{}",
            bold(&format!(
                "Nodes: {} added, {} removed, {} changed",
                report.nodes_added_count, report.nodes_removed_count, report.nodes_changed_count
            ))
        );
        let loc = |file: &str, line: u32| {
            if line != 0 {
                format!("{file}:{line}")
            } else {
                file.to_string()
            }
        };
        for n in &report.nodes_added {
            println!(
                "  {} {} {} {}",
                green("+"),
                cyan(&n.kind),
                white(&n.name),
                dim(&loc(&n.file, n.line))
            );
        }
        for n in &report.nodes_removed {
            println!(
                "  {} {} {} {}",
                red("-"),
                cyan(&n.kind),
                white(&n.name),
                dim(&loc(&n.file, n.line))
            );
        }
        for n in &report.nodes_changed {
            println!(
                "  {} {} {} {} {}",
                yellow("~"),
                cyan(&n.symbol.kind),
                white(&n.symbol.name),
                dim(&loc(&n.symbol.file, n.symbol.line)),
                dim(&format!("({})", n.reasons.join(", ")))
            );
        }
        println!();

        if report.edges_added_count > 0 || report.edges_removed_count > 0 {
            println!(
                "{}",
                bold(&format!(
                    "Edges: {} added, {} removed",
                    report.edges_added_count, report.edges_removed_count
                ))
            );
            for e in &report.edges_added {
                println!(
                    "  {} {} {} {} {}",
                    green("+"),
                    white(&e.from),
                    dim("->"),
                    white(&e.to),
                    dim(&format!("({})", e.kind))
                );
            }
            for e in &report.edges_removed {
                println!(
                    "  {} {} {} {} {}",
                    red("-"),
                    white(&e.from),
                    dim("->"),
                    white(&e.to),
                    dim(&format!("({})", e.kind))
                );
            }
            println!();
        }

        if !report.changed_functions.is_empty() {
            println!("{}", bold("Changed functions (complexity):"));
            let fmt_metric = |before: Option<u32>, after: Option<u32>, delta: Option<i64>| {
                let b = before.map_or("?".to_string(), |v| v.to_string());
                let a = after.map_or("?".to_string(), |v| v.to_string());
                match delta {
                    Some(d) => format!("{b} -> {a} ({d:+})"),
                    None => format!("{b} -> {a}"),
                }
            };
            for f in &report.changed_functions {
                println!(
                    "  {} {} {} {}",
                    white(&f.symbol.name),
                    dim(&format!(
                        "cyclomatic {}",
                        fmt_metric(f.cyclomatic_before, f.cyclomatic_after, f.cyclomatic_delta)
                    )),
                    dim(&format!(
                        "cognitive {}",
                        fmt_metric(f.cognitive_before, f.cognitive_after, f.cognitive_delta)
                    )),
                    dim(&format!("lines {} -> {}", f.lines_before, f.lines_after))
                );
            }
            println!();
        }

        if report.new_cycle_count > 0 {
            println!(
                "{}",
                bold(&format!(
                    "Newly-introduced cycles ({}):",
                    report.new_cycle_count
                ))
            );
            for cycle in &report.new_cycles {
                let members: Vec<&str> = cycle.members.iter().map(|m| m.name.as_str()).collect();
                println!(
                    "  {} {}",
                    cyan(cycle_kind_label(&cycle.kind)),
                    white(&members.join(", "))
                );
            }
            println!();
        }
        if report.resolved_cycle_count > 0 {
            info(&format!(
                "{} cycle{} from the base no longer exist{}",
                report.resolved_cycle_count,
                if report.resolved_cycle_count == 1 {
                    ""
                } else {
                    "s"
                },
                if report.resolved_cycle_count == 1 {
                    "s"
                } else {
                    ""
                }
            ));
        }

        println!(
            "{}",
            bold(&format!(
                "Impact of the delta (depth {}): {} symbol{}",
                report.impact.depth,
                report.impact.impacted_count,
                if report.impact.impacted_count == 1 {
                    ""
                } else {
                    "s"
                }
            ))
        );
        for n in &report.impact.nodes {
            println!(
                "  {} {} {}",
                cyan(&n.kind),
                white(&n.name),
                dim(&loc(&n.file, n.line))
            );
        }
        println!();

        if report.truncated || report.impact.truncated {
            info(&format!(
                "Listings capped at {top} entries {} raise --top for more (counts are exact)",
                get_glyphs().dash
            ));
        }
        info(&report.note);

        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze diff failed: {msg}"));
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
