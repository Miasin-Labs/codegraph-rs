//! Security and analysis MCP tools.

use serde_json::{Map, Value};

use super::context::ToolHandler;
use super::format::num_or;
use super::schema::ToolResult;
use crate::analysis_bridge::{BridgeOptions, build_analysis_graph_cached_with_options};
use crate::db::{DatabaseConnection, QueryBuilder, get_database_path};
use crate::error::{CodeGraphError, Result};
use crate::utils::clamp;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_vuln(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let root = cg.get_project_root().to_path_buf();
        let min_confidence = clamp(num_or(args, "minConfidence", 0.5), 0.0, 1.0);

        let conn = DatabaseConnection::open(get_database_path(&root))
            .map_err(|e| CodeGraphError::other(format!("vuln: open db: {e}")))?;
        let db = conn
            .get_db()
            .map_err(|e| CodeGraphError::other(format!("vuln: db handle: {e}")))?;
        let queries = QueryBuilder::new(db);
        let cached = build_analysis_graph_cached_with_options(
            &queries,
            &root,
            true,
            &BridgeOptions::default(),
        )
        .map_err(|e| CodeGraphError::other(format!("vuln: bridge analysis graph: {e}")))?;

        let report = crate::analyze::vuln_report(&cached.result.graph, &root, min_confidence);
        Ok(self.text_result(&self.truncate_output(&report.render_human())))
    }

    /// codegraph_verify_roles — the "model proposes, graph proves" boundary.
    ///
    /// The agent layer (which has model access) supplies role proposals naming
    /// suspected sinks and guards; this tool resolves each name against the
    /// bridged analysis graph, runs the proposals through the sound
    /// [`GraphVerifier`], and emits only the missing-guard findings the call
    /// graph actually corroborates — tagged with the `llm` inference origin. A
    /// hallucinated sink (too few callers) or guard (never precedes the sink) is
    /// dropped before any finding is produced, so the model supplies semantic
    /// judgment while the graph supplies ground truth.
    pub(in crate::mcp::tools) fn handle_verify_roles(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        use codegraph_analysis::context::resolver::resolve_symbol;
        use codegraph_analysis::vuln::classify::{
            PredicateRole,
            RoleProposal,
            findings_from_verified_roles,
        };

        let Some(raw) = args.get("roles").and_then(|v| v.as_array()) else {
            return Ok(self.text_result(
                "codegraph_verify_roles: `roles` must be an array of \
                 {symbol, role, confidence?, rationale?} objects \
                 (role ∈ sink|guard|source|sanitizer).",
            ));
        };
        if raw.is_empty() {
            return Ok(self.text_result("codegraph_verify_roles: `roles` was empty."));
        }
        let min_callers = clamp(num_or(args, "minCallers", 4.0), 1.0, 1_000.0).round() as usize;

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let root = cg.get_project_root().to_path_buf();
        let conn = DatabaseConnection::open(get_database_path(&root))
            .map_err(|e| CodeGraphError::other(format!("verify_roles: open db: {e}")))?;
        let db = conn
            .get_db()
            .map_err(|e| CodeGraphError::other(format!("verify_roles: db handle: {e}")))?;
        let queries = QueryBuilder::new(db);
        let cached = build_analysis_graph_cached_with_options(
            &queries,
            &root,
            true,
            &BridgeOptions::default(),
        )
        .map_err(|e| CodeGraphError::other(format!("verify_roles: bridge graph: {e}")))?;
        let graph = &cached.result.graph;

        let mut proposals: Vec<RoleProposal> = Vec::new();
        let mut unresolved: Vec<String> = Vec::new();
        for item in raw {
            let Some(symbol) = item.get("symbol").and_then(|v| v.as_str()) else {
                continue;
            };
            let role = match item.get("role").and_then(|v| v.as_str()) {
                Some(r) => match r.to_ascii_lowercase().as_str() {
                    "sink" => PredicateRole::Sink,
                    "guard" => PredicateRole::Guard,
                    "source" => PredicateRole::Source,
                    "sanitizer" => PredicateRole::Sanitizer,
                    other => {
                        return Ok(self.text_result(&format!(
                            "codegraph_verify_roles: unknown role \"{other}\" for \"{symbol}\" \
                             (expected sink|guard|source|sanitizer)."
                        )));
                    }
                },
                None => continue,
            };
            let confidence = item
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.6)
                .clamp(0.0, 1.0);
            let rationale = item
                .get("rationale")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();

            let matches = resolve_symbol(graph, symbol);
            if matches.is_empty() {
                unresolved.push(symbol.to_owned());
                continue;
            }
            // A common name may resolve to several nodes; propose the role for
            // each so verification can keep whichever the graph corroborates.
            for node in matches.into_iter().take(5) {
                proposals.push(RoleProposal {
                    node,
                    role,
                    confidence,
                    rationale: rationale.clone(),
                });
            }
        }

        if proposals.is_empty() {
            return Ok(self.text_result(&format!(
                "codegraph_verify_roles: none of the proposed symbols resolved in the \
                 graph ({} unresolved: {}).",
                unresolved.len(),
                unresolved.join(", ")
            )));
        }

        let findings = findings_from_verified_roles(graph, &proposals, min_callers);

        let mut out = format!(
            "Role verification — {} proposal(s) → {} verified finding(s)\n  \
             (model proposes, graph proves; origin: llm)\n",
            proposals.len(),
            findings.len(),
        );
        if !unresolved.is_empty() {
            out.push_str(&format!(
                "  unresolved symbols (skipped): {}\n",
                unresolved.join(", ")
            ));
        }
        const CAP: usize = 60;
        for f in findings.iter().take(CAP) {
            let (file, line, symbol) = match graph.get_node(&f.site) {
                Some(n) => (
                    n.file_path.to_string_lossy().into_owned(),
                    n.span.start_line,
                    n.name.clone(),
                ),
                None => (String::new(), 0, format!("node#{}", f.site.0)),
            };
            let class = f
                .class
                .as_ref()
                .map(|c| format!(" [{c}]"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  - {}:{} ({}{}, {:.0}% via llm) {}\n    {}\n",
                file,
                line,
                f.template.id(),
                class,
                f.confidence * 100.0,
                symbol,
                f.message,
            ));
        }
        if findings.len() > CAP {
            out.push_str(&format!("  ... and {} more\n", findings.len() - CAP));
        }
        if findings.is_empty() {
            out.push_str(
                "  No proposal survived graph verification — the call graph contradicted \
                 every proposed sink/guard, so nothing was emitted.\n",
            );
        }
        Ok(self.text_result(&self.truncate_output(&out)))
    }
}
