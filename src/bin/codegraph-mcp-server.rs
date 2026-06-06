//! Standalone MCP stdio server binary.
//!
//! Mirrors the `codegraph serve --mcp` CLI path (`src/bin/codegraph.ts`,
//! `serve` command) so the MCP server can be driven over stdio before the
//! full clap CLI lands `serve --mcp`. The integration suite
//! (`tests/mcp_server_test.rs`) spawns this via
//! `env!("CARGO_BIN_EXE_codegraph-mcp-server")`.
//!
//! Accepted args (a superset-tolerant parse of the TS `serve` command — the
//! `serve` / `--mcp` tokens are accepted and ignored so the daemon re-spawn
//! invocation `<exe> serve --mcp --path <root>` works against this binary
//! too):
//!   --path/-p <path>   project path (TS `resolveProjectPath`: path.resolve)
//!   --no-watch         disable the file watcher → CODEGRAPH_NO_WATCH=1
//!
//! NOTE for the CLI wave: once `src/bin/codegraph.rs` implements
//! `serve --mcp` (constructing `codegraph::mcp::MCPServer` exactly like
//! below), this helper binary can be deleted and the tests pointed at
//! `CARGO_BIN_EXE_codegraph`.

use codegraph::mcp::MCPServer;

fn main() {
    let mut project_path: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "serve" | "--mcp" => {}
            // Commander sets watch=false when --no-watch is passed. Route it
            // through the same env-var chokepoint the watcher and MCP server
            // already honor (TS src/bin/codegraph.ts serve action).
            "--no-watch" => std::env::set_var("CODEGRAPH_NO_WATCH", "1"),
            "--path" | "-p" => project_path = args.next(),
            _ => {}
        }
    }

    // TS `resolveProjectPath(options.path)` — path.resolve against cwd.
    let resolved = project_path.map(|p| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
        codegraph::utils::lexical_resolve(&cwd, &p)
            .to_string_lossy()
            .to_string()
    });

    let server = MCPServer::new(resolved);
    if let Err(err) = server.start() {
        eprintln!("Failed to start server: {err}");
        std::process::exit(1);
    }
}
