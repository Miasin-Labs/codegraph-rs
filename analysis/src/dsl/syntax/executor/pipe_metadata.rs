use super::QueryEngine;
use super::pipe::PipeState;

impl<'a> QueryEngine<'a> {
    pub(super) fn apply_untested(&self, state: &mut PipeState) {
        state.working_set.retain(|id| {
            self.graph
                .get_node(id)
                .map(|n| {
                    n.metadata
                        .get("coverage_tested")
                        .map(|v| v != "true")
                        .unwrap_or(true)
                })
                .unwrap_or(false)
        });
        state
            .metadata
            .push(format!("untested count={}", state.working_set.len()));
    }

    pub(super) fn apply_possible_types(&self, state: &mut PipeState) {
        state.working_set.retain(|id| {
            self.graph
                .get_node(id)
                .map(|n| {
                    n.metadata.contains_key("possible_input_types")
                        || n.metadata.contains_key("possible_return_types")
                })
                .unwrap_or(false)
        });
        for id in &state.working_set {
            if let Some(n) = self.graph.get_node(id) {
                let mut parts = Vec::new();
                if let Some(inputs) = n.metadata.get("possible_input_types") {
                    parts.push(format!("inputs={inputs}"));
                }
                if let Some(returns) = n.metadata.get("possible_return_types") {
                    parts.push(format!("returns={returns}"));
                }
                if !parts.is_empty() {
                    state.metadata.push(format!(
                        "possible_types {} {}",
                        n.qualified_name,
                        parts.join(" ")
                    ));
                }
            }
        }
    }

    pub(super) fn apply_complexity(&self, state: &mut PipeState) {
        state.working_set.retain(|id| {
            self.graph
                .get_node(id)
                .and_then(|n| n.complexity.as_ref())
                .is_some()
        });
        for id in &state.working_set {
            if let Some(n) = self.graph.get_node(id)
                && let Some(cx) = &n.complexity
            {
                let mut parts = vec![
                    format!("cognitive={}", cx.cognitive),
                    format!("cyclomatic={}", cx.cyclomatic),
                    format!("max_nesting={}", cx.max_nesting),
                ];
                if let Some(ref h) = cx.halstead {
                    parts.push(format!("volume={:.1}", h.volume));
                    parts.push(format!("effort={:.1}", h.effort));
                    parts.push(format!("bugs={:.3}", h.bugs));
                }
                if let Some(ref loc) = cx.loc {
                    parts.push(format!(
                        "loc(total={},source={},comment={})",
                        loc.total, loc.source, loc.comment
                    ));
                }
                if let Some(mi) = cx.maintainability_index {
                    parts.push(format!("MI={:.1}", mi));
                }
                state.metadata.push(format!(
                    "complexity {} {}",
                    n.qualified_name,
                    parts.join(" ")
                ));
            }
        }
    }

    pub(super) fn apply_cfg(&self, state: &mut PipeState) {
        state.working_set.retain(|id| {
            self.graph
                .get_node(id)
                .and_then(|n| n.cfg.as_ref())
                .is_some()
        });
        for id in &state.working_set {
            if let Some(n) = self.graph.get_node(id)
                && let Some(ref cfg) = n.cfg
            {
                state.metadata.push(format!(
                    "cfg {} {}",
                    n.qualified_name,
                    cfg.format_summary().replace('\n', " | ")
                ));
            }
        }
    }

    pub(super) fn apply_dataflow(&self, state: &mut PipeState) {
        state.working_set.retain(|id| {
            self.graph
                .get_node(id)
                .and_then(|n| n.dataflow.as_ref())
                .is_some()
        });
        for id in &state.working_set {
            if let Some(n) = self.graph.get_node(id)
                && let Some(ref df) = n.dataflow
            {
                state.metadata.push(format!(
                    "dataflow {} {}",
                    n.qualified_name,
                    df.format_summary().replace('\n', " | ")
                ));
            }
        }
    }
}
