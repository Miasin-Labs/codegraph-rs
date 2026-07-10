use super::{Parser, Subcommand};

mod analyze;
mod history;

pub(crate) use analyze::AnalyzeCommands;
pub(crate) use history::HistoryCommands;

#[derive(Parser)]
#[command(
    name = "codegraph",
    about = "Code intelligence and knowledge graph for any codebase",
    version
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Initialize CodeGraph in a project directory and build the initial index
    Init {
        #[arg(value_name = "path")]
        path: Option<String>,
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
    /// Explore an area: relevant symbol source and call paths in one response
    Explore {
        #[arg(value_name = "query", required = true, num_args = 1..)]
        query: Vec<String>,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum number of files to include source from
        #[arg(long = "max-files", value_name = "number")]
        max_files: Option<String>,
    },
    /// Show one symbol with source/trail, or read one indexed file
    Node {
        #[arg(value_name = "name")]
        name: Option<String>,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Read this file, or disambiguate a symbol to this file
        #[arg(short = 'f', long, value_name = "file")]
        file: Option<String>,
        /// File mode: 1-based start line
        #[arg(long, value_name = "number")]
        offset: Option<String>,
        /// File mode: maximum number of lines
        #[arg(long, value_name = "number")]
        limit: Option<String>,
        /// File mode: print only the symbol map and dependents
        #[arg(long = "symbols-only")]
        symbols_only: bool,
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
    /// List or stop CodeGraph background daemons
    #[command(visible_alias = "daemons")]
    Daemon {
        /// Project path (used with --stop; defaults to nearest indexed project)
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Stop the daemon for this project
        #[arg(long)]
        stop: bool,
        /// Stop every registered daemon
        #[arg(long, requires = "stop")]
        all: bool,
        /// Output records/results as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Remove a stale lock file that is blocking indexing
    Unlock {
        #[arg(value_name = "path")]
        path: Option<String>,
    },
    /// Benchmark the reference-resolution pass (dry run, no writes)
    #[command(hide = true, name = "resolve-bench")]
    ResolveBench {
        #[arg(value_name = "path")]
        path: Option<String>,
        /// Maximum number of pending unresolved references to resolve
        #[arg(short = 'l', long, default_value = "100000")]
        limit: usize,
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
    /// Tool-call history: ingest agent logs, query usage (hot files/tools, co-access)
    History {
        #[command(subcommand)]
        command: HistoryCommands,
    },
    /// Show or change anonymous usage telemetry (status, on, off)
    Telemetry {
        #[arg(value_name = "action")]
        action: Option<String>,
    },
    /// Update CodeGraph to the latest release (or a specific version)
    Upgrade {
        #[arg(value_name = "version")]
        version: Option<String>,
        /// Check whether an update is available without installing
        #[arg(long)]
        check: bool,
        /// Reinstall even if already on the target version
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Claude UserPromptSubmit hook entry point
    #[command(hide = true, name = "prompt-hook")]
    PromptHook,
    /// Print the installed CodeGraph version
    Version,
}
