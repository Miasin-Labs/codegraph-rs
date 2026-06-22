//! Synthesis support for graph MCP tools.

use serde_json::Map;

use super::super::context::ToolHandler;
use super::super::format::{SynthNote, truthy_meta_string};
use crate::types::{Edge, Provenance};

impl ToolHandler {
    pub(in crate::mcp::tools) fn synth_edge_note(&self, edge: Option<&Edge>) -> Option<SynthNote> {
        let edge = edge?;
        if edge.provenance != Some(Provenance::Heuristic) {
            return None;
        }
        let empty = Map::new();
        let m = edge.metadata.as_ref().unwrap_or(&empty);
        let registered_at = m
            .get("registeredAt")
            .and_then(|v| v.as_str())
            .map(String::from);
        let at = registered_at
            .as_ref()
            .map(|r| format!(" @{r}"))
            .unwrap_or_default();
        let synthesized_by = m
            .get("synthesizedBy")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match synthesized_by {
            "callback" => {
                let via = truthy_meta_string(m.get("via"))
                    .map(|v| format!("`{v}`"))
                    .unwrap_or_else(|| "a registrar".to_string());
                let field = truthy_meta_string(m.get("field"))
                    .map(|f| format!(" on .{f}"))
                    .unwrap_or_default();
                Some(SynthNote {
                    label: format!("callback — registered via {via}{field} (dynamic dispatch)"),
                    compact: format!("dynamic: callback via {via}{at}"),
                    registered_at,
                })
            }
            "event-emitter" => {
                let ev = truthy_meta_string(m.get("event"))
                    .map(|e| format!("`{e}`"))
                    .unwrap_or_else(|| "an event".to_string());
                Some(SynthNote {
                    label: format!("event {ev} — emit → handler (dynamic dispatch)"),
                    compact: format!("dynamic: event {ev}{at}"),
                    registered_at,
                })
            }
            "react-render" => Some(SynthNote {
                label: "React re-render — `setState` re-runs render() (dynamic dispatch)".to_string(),
                compact: format!("dynamic: React re-render via setState{at}"),
                registered_at,
            }),
            "jsx-render" => {
                let child = truthy_meta_string(m.get("via"))
                    .map(|v| format!("<{v}>"))
                    .unwrap_or_else(|| "a child component".to_string());
                Some(SynthNote {
                    label: format!("renders {child} (JSX child — dynamic dispatch)"),
                    compact: format!("dynamic: renders {child}"),
                    registered_at,
                })
            }
            "vue-handler" => {
                let ev = truthy_meta_string(m.get("event"))
                    .map(|e| format!("@{e}"))
                    .unwrap_or_else(|| "a template event".to_string());
                Some(SynthNote {
                    label: format!("Vue template handler — bound to {ev} (dynamic dispatch)"),
                    compact: format!("dynamic: Vue {ev} handler"),
                    registered_at,
                })
            }
            "interface-impl" => Some(SynthNote {
                label: "interface/abstract dispatch — runs the implementation override (dynamic dispatch)"
                    .to_string(),
                compact: format!("dynamic: interface → impl{at}"),
                registered_at,
            }),
            "closure-collection" => {
                let field = truthy_meta_string(m.get("field"))
                    .map(|f| format!("`{f}`"))
                    .unwrap_or_else(|| "a collection".to_string());
                Some(SynthNote {
                    label: format!("closure collection — runs handlers appended to {field} (dynamic dispatch)"),
                    compact: format!("dynamic: runs {field} handlers{at}"),
                    registered_at,
                })
            }
            _ => None,
        }
    }
}
