use super::{
    MCPServer,
    blue,
    bold,
    cyan,
    dim,
    error_msg,
    get_glyphs,
    process,
    resolve_project_path,
};

pub(crate) fn cmd_serve(path_arg: Option<&str>, mcp: bool, no_watch: bool) {
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
