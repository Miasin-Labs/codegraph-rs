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
use codegraph::analyze::SliceDirection;
use codegraph::context_analysis::{self, AnalysisContextOptions};
use codegraph::db::{DatabaseConnection, QueryBuilder, get_database_path};
use codegraph::directory::{get_codegraph_dir, is_initialized};
use codegraph::extraction::is_generated_file;
use codegraph::history::{HistoryDb, default_history_path, default_jfc_logs_dir, parse_logs_dir};
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
    MCPServer,
    NodeKind,
    OpenOptions,
    SearchOptions,
    Severity,
    TaskInput,
    analyze as analysis_reports,
    analyze_ir,
};
use codegraph_analysis::nodes::NodeId as ANodeId;

mod analyze;
mod cli;
mod context;
mod files;
mod graph;
mod history;
mod index;
mod install;
mod output;
mod path;
mod serve;

use analyze::{bridge_project_with_options, cmd_analyze, print_json};
use cli::{AnalyzeCommands, Cli, Commands, HistoryCommands};
use context::cmd_context;
use files::cmd_files;
use graph::{CallDirection, cmd_affected, cmd_call_graph, cmd_impact, is_exact_symbol_match};
use history::cmd_history;
use index::{
    cmd_index,
    cmd_init,
    cmd_query,
    cmd_resolve_bench,
    cmd_status,
    cmd_sync,
    cmd_uninit,
    cmd_unlock,
};
use install::{cmd_install, cmd_uninstall};
use output::*;
use path::{resolve_absolute, resolve_project_path};
use serve::cmd_serve;
pub(crate) fn main() {
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
        Commands::Init { path, verbose } => cmd_init(path.as_deref(), verbose),
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
        Commands::ResolveBench { path, limit } => cmd_resolve_bench(path.as_deref(), limit),
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
        Commands::History { command } => cmd_history(command),
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
