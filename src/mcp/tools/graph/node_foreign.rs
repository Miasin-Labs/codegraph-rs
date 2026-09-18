//! `codegraph_node` across graphs.
//!
//! * A project symbol's detail lists what it references in other graphs
//!   (`external`: dependency shards, linked projects), each edge followed
//!   into its graph for the target's current place — or why it cannot be.
//! * A symbol that lives in another graph (`serde_json::from_str`, or a
//!   name with `graph`) is read from THAT graph: its signature and a short
//!   source window from the dependency's source directory or the linked
//!   project's checkout (never the network), its absolute `file` (so it can
//!   be read or edited directly — and so the session ledger never confuses
//!   it with a same-named file of this project), and this project's own
//!   call sites into it as `callers`.

use super::super::context::ToolHandler;
use super::super::format::number_source_lines;
use super::super::output::{ExternalRef, NodeDetailOutput, NodeOutput, SymbolRef, SymbolRow};
use super::super::schema::ToolResult;
use super::federated::availability_note;
use super::node::{TRAIL_CAP, already_sent_note, was_sent};
use crate::codegraph::CodeGraph;
use crate::error::Result;
use crate::federation::{ForeignSymbol, GraphSet, read_window};
use crate::mcp::explore_session::ProjectState;
use crate::types::{EdgeKind, Node};

/// The edge kinds a symbol's `external` list shows.
const EXTERNAL_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Instantiates,
    EdgeKind::References,
    EdgeKind::Implements,
    EdgeKind::Extends,
];
/// The short window a foreign definition shows by default.
const WINDOW_LINES: usize = 40;
const WINDOW_CHARS: usize = 3_000;
/// With `includeCode`, as much as a project symbol's body.
const FULL_LINES: usize = 300;
const FULL_CHARS: usize = 12_000;

/// List what `node` references in other graphs on `detail`; returns the
/// text line saying so ("" when nothing).
pub(super) fn attach_external(
    fed: &GraphSet,
    cg: &CodeGraph,
    node: &Node,
    detail: &mut NodeDetailOutput,
) -> Result<String> {
    let edges =
        super::federated::external_edges_of(cg, std::slice::from_ref(&node.id), EXTERNAL_KINDS)?;
    if edges.is_empty() {
        return Ok(String::new());
    }
    detail.external_omitted = edges.len().saturating_sub(TRAIL_CAP);
    let followed = fed.follow_all(&edges[..edges.len().min(TRAIL_CAP)]);
    detail.external = followed
        .iter()
        .map(|item| ExternalRef {
            name: item.edge.target_qualified_name.clone(),
            kind: item.edge.target_kind.as_str(),
            graph: item.label.clone(),
            file: item.file().to_string(),
            line: item.line(),
            unavailable: availability_note(item),
        })
        .collect();
    let listed: Vec<String> = followed
        .iter()
        .map(|item| {
            let location = match item.line() {
                Some(line) => format!("{}:{line}", item.file()),
                None => item.file().to_string(),
            };
            let note = availability_note(item)
                .map(|note| format!(", {note}"))
                .unwrap_or_default();
            format!(
                "{} ({} {location}{note})",
                item.edge.target_qualified_name, item.label
            )
        })
        .collect();
    let more = if detail.external_omitted > 0 {
        format!(", +{} more", detail.external_omitted)
    } else {
        String::new()
    };
    Ok(format!("\nInto other graphs: {}{more}", listed.join(", ")))
}

impl ToolHandler {
    /// Symbol mode for definitions in other graphs.
    pub(super) fn foreign_node_result(
        &self,
        fed: &GraphSet,
        cg: &CodeGraph,
        foreign: &[ForeignSymbol],
        include_code: bool,
        prior: Option<&ProjectState>,
    ) -> Result<ToolResult> {
        let mut sections = Vec::new();
        let mut details = Vec::new();
        for found in foreign {
            let (text, detail) = self.foreign_detail(fed, cg, found, include_code, prior);
            sections.push(text);
            details.push(detail);
        }
        let text = if sections.len() == 1 {
            sections.remove(0)
        } else {
            format!(
                "{} definitions in other graphs\n\n{}",
                sections.len(),
                sections.join("\n\n---\n\n")
            )
        };
        self.node_result(
            &self.truncate_output(&text),
            NodeOutput::new(details.len(), false, details),
            &[],
        )
    }

    fn foreign_detail(
        &self,
        fed: &GraphSet,
        cg: &CodeGraph,
        found: &ForeignSymbol,
        include_code: bool,
        prior: Option<&ProjectState>,
    ) -> (String, NodeDetailOutput) {
        let node = &found.node;
        let path = found.graph.path_of(&node.file_path);
        let absolute = path.to_string_lossy().into_owned();
        let (max_lines, max_chars) = if include_code {
            (FULL_LINES, FULL_CHARS)
        } else {
            (WINDOW_LINES, WINDOW_CHARS)
        };
        let window = read_window(
            &found.graph.root,
            &node.file_path,
            node.start_line.max(1),
            node.end_line.max(node.start_line),
            max_lines,
            max_chars,
        );
        let already_sent = window.as_ref().is_some_and(|window| {
            was_sent(cg, prior, &absolute, window.start as usize, &window.text)
        });

        let mut lines = vec![
            format!(
                "## {} ({}) — {}",
                node.name,
                node.kind.as_str(),
                found.graph.label
            ),
            String::new(),
            format!("**Location:** {absolute}:{}", node.start_line),
        ];
        if let Some(signature) = node.signature.as_deref().filter(|s| !s.is_empty()) {
            lines.push(format!("**Signature:** `{signature}`"));
        }
        let mut detail = NodeDetailOutput::bare(SymbolRow {
            file: Some(absolute.clone()),
            ..SymbolRow::new(node)
        });
        detail.graph = Some(found.graph.label.clone());
        match (&window, already_sent) {
            (Some(_), true) => {
                detail.already_sent = true;
                lines.push(already_sent_note(&absolute));
            }
            (Some(window), false) => {
                lines.push(String::new());
                lines.push(format!("```{}", node.language.as_str()));
                lines.push(number_source_lines(&window.text, window.start as usize));
                lines.push("```".to_string());
                if window.truncated {
                    lines.push(format!(
                        "> Lines {}–{} of the definition; it continues to line {}{}.",
                        window.start,
                        window.end,
                        node.end_line,
                        if include_code {
                            ""
                        } else {
                            " (`includeCode: true` for more)"
                        }
                    ));
                    detail.code_end_line = Some(window.end as usize);
                    detail.code_truncated = true;
                }
                detail.code = Some(window.text.clone());
            }
            (None, _) => lines.push(format!(
                "> The source of {} is not on this machine any more; only its indexed \
                 location is known.",
                found.graph.label
            )),
        }
        if let Some(callers) = fed.callers_in(
            cg.get_project_root(),
            &found.graph.id,
            std::slice::from_ref(node),
            TRAIL_CAP,
        ) {
            let listed: Vec<String> = callers
                .callers
                .iter()
                .map(|caller| {
                    format!(
                        "{} ({}:{})",
                        caller.node.name,
                        caller.node.file_path,
                        caller.line.unwrap_or(caller.node.start_line)
                    )
                })
                .collect();
            lines.push(String::new());
            lines.push(format!(
                "Called from this project: {}{}",
                listed.join(", "),
                if callers.omitted > 0 {
                    format!(", +{} more", callers.omitted)
                } else {
                    String::new()
                }
            ));
            detail.callers = callers
                .callers
                .iter()
                .map(|caller| SymbolRef::from(&caller.node))
                .collect();
            detail.callers_omitted = callers.omitted;
        }
        (lines.join("\n"), detail)
    }
}
