//! Tool dispatch and cross-cutting execution wrappers.

use std::sync::LazyLock;

use serde_json::{Map, Value};

use super::super::schema::ToolResult;
use super::ToolHandler;

impl ToolHandler {
    pub fn execute(&self, tool_name: &str, args: &Value) -> ToolResult {
        static EMPTY: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);
        let args = args.as_object().unwrap_or(&EMPTY);

        // Run the engine's post-open reconcile gate once.
        if let Some(gate) = self.catch_up_gate.borrow_mut().take() {
            gate();
        }

        // EXCEEDS TS: cooperative cancellation between pipeline stages — the
        // catch-up sync above is the long first-call stage; if the client
        // cancelled while it ran, stop here. The session suppresses the
        // response (this placeholder is never sent).
        if self.call_context.is_cancelled() {
            return self.error_result("Request cancelled by client");
        }

        // Honor the optional tool allowlist (CODEGRAPH_MCP_TOOLS).
        if !self.is_tool_allowed(tool_name) {
            return self.error_result(&format!(
                "Tool {tool_name} is disabled via CODEGRAPH_MCP_TOOLS"
            ));
        }

        // Cross-cutting input validation.
        if let Err(r) = self.validate_optional_path(args.get("projectPath"), "projectPath") {
            return r;
        }
        if args.contains_key("path") {
            if let Err(r) = self.validate_optional_path(args.get("path"), "path") {
                return r;
            }
        }
        if args.contains_key("pattern") {
            if let Err(r) = self.validate_optional_path(args.get("pattern"), "pattern") {
                return r;
            }
        }
        if args.contains_key("file") {
            if let Err(r) = self.validate_optional_path(args.get("file"), "file") {
                return r;
            }
        }

        let project_path: Option<String> = args
            .get("projectPath")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Make this call's cancel flag observable to in-flight graph traversals
        // (find_path/BFS/DFS/impact/type-hierarchy poll it). Dropped at the end
        // of dispatch, so a flag never leaks into the next call.
        let _cancel_guard =
            crate::graph::cancel::CancelGuard::install(self.call_context.cancel_flag());

        let result = match tool_name {
            "codegraph_search" => self.handle_search(args),
            "codegraph_callers" => self.handle_callers(args),
            "codegraph_callees" => self.handle_callees(args),
            "codegraph_impact" => self.handle_impact(args),
            "codegraph_explore" => self.handle_explore(args),
            "codegraph_node" => self.handle_node(args),
            "codegraph_status" => {
                // status embeds the pending-files list as a first-class section,
                // so skip the auto-banner wrappers.
                return match self.handle_status(args) {
                    Ok(r) => r,
                    Err(e) => self.error_result(&format!("Tool execution failed: {e}")),
                };
            }
            "codegraph_files" => self.handle_files(args),
            "codegraph_vuln" => self.handle_vuln(args),
            "codegraph_verify_roles" => self.handle_verify_roles(args),
            "codegraph_arch" => self.handle_arch(args),
            "codegraph_xref" => self.handle_xref(args),
            "codegraph_paths" => self.handle_paths(args),
            _ => return self.error_result(&format!("Unknown tool: {tool_name}")),
        };
        let result = match result {
            Ok(r) => r,
            Err(e) => return self.error_result(&format!("Tool execution failed: {e}")),
        };
        let with_worktree = self.with_worktree_notice(result, project_path.as_deref());
        self.with_staleness_notice(with_worktree, project_path.as_deref())
    }
}
