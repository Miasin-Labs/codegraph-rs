use super::*;

// =============================================================================
// analyze query --explain
// =============================================================================

/// Result of [`explain_report`] — the optimised query plan, never executed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainReport {
    pub query: String,
    /// `pipe` (operator chain), `expression` (set algebra / path pattern /
    /// selector), or `aggregation` (count/sum/group_by…, which bypass the
    /// plan optimiser).
    pub kind: String,
    /// Optimised steps in execution order: one pipe operator per entry, or
    /// a single rendered expression. Optimiser rewrites are already applied
    /// (depth fusion, filter pushdown, intersect operand reordering).
    pub steps: Vec<String>,
    /// The optimiser's BFS schedule hint (`push`, `pull`, or `auto`).
    pub strategy: String,
    pub parallel: bool,
}

fn strategy_label(strategy: ScheduleStrategy) -> &'static str {
    match strategy {
        ScheduleStrategy::Push => "push",
        ScheduleStrategy::Pull => "pull",
        ScheduleStrategy::Auto => "auto",
    }
}

/// Parse and optimise `query` exactly the way [`query_report`] would before
/// executing it, and return the resulting plan. Pure function of the query
/// string — touches neither a graph nor the index.
pub fn explain_report(query: &str) -> Result<ExplainReport, String> {
    // Mirror `run_query_expr`'s dispatch: the aggregation grammar first
    // (it returns `Plain` for non-aggregation input), then the extended
    // expression grammar as the error-reporting fallback.
    let expr = match parse_aggregate(query) {
        Ok(AggExpr::Plain(plain)) => plain,
        Ok(aggregation) => {
            return Ok(ExplainReport {
                query: query.to_string(),
                kind: "aggregation".to_string(),
                steps: vec![format!("{aggregation:?}")],
                strategy: strategy_label(ScheduleStrategy::Auto).to_string(),
                parallel: false,
            });
        }
        Err(_) => parse_expr(query).map_err(|e| e.to_string())?,
    };

    let plan = optimise_expr(expr);
    let optimised = plan.expr().expect("optimise_expr yields Plan::Expr");
    Ok(match optimised {
        Expr::Pipe(ops) => {
            let schedule = pick_schedule_for_pipe(ops);
            ExplainReport {
                query: query.to_string(),
                kind: "pipe".to_string(),
                steps: ops.iter().map(|op| format!("{op:?}")).collect(),
                strategy: strategy_label(schedule.strategy).to_string(),
                parallel: schedule.parallel,
            }
        }
        other => {
            let schedule = plan.schedule();
            ExplainReport {
                query: query.to_string(),
                kind: "expression".to_string(),
                steps: vec![format!("{other:?}")],
                strategy: strategy_label(schedule.strategy).to_string(),
                parallel: schedule.parallel,
            }
        }
    })
}
