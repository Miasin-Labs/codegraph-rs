use super::super::{Expr, parse_expr};
use super::aggregate::run_aggregate_unified;
use super::set::combine_set_op;
use super::{QueryConfig, QueryEngine, QueryError, QueryResult};
use crate::graph::CodeGraph;

impl<'a> QueryEngine<'a> {
    /// Execute an [`Expr`] (extended grammar with set algebra, path patterns, and selectors).
    pub fn execute_expr(
        &self,
        expr: &Expr,
        config: &QueryConfig,
    ) -> Result<QueryResult, QueryError> {
        crate::ensure_sufficient_stack(|| self.execute_expr_inner(expr, config))
    }

    pub fn execute_expr_inner(
        &self,
        expr: &Expr,
        config: &QueryConfig,
    ) -> Result<QueryResult, QueryError> {
        match expr {
            Expr::Pipe(ops) => self.execute(ops, config),
            Expr::Entrypoints(kind_filter) => Ok(self.execute_entrypoints(*kind_filter, config)),
            Expr::PathQuery(pq) => self.execute_path_query(pq, config),
            Expr::SetOp { op, left, right } => {
                let left_result = self.execute_expr(left, config)?;
                let right_result = self.execute_expr(right, config)?;
                Ok(combine_set_op(
                    *op,
                    left_result,
                    right_result,
                    config.max_nodes,
                ))
            }
            Expr::DominatorsOf(inner) => {
                let seed = self.execute_expr(inner, config)?;
                Ok(self.execute_dominators_of(&seed.nodes, config))
            }
            Expr::DominatesOf(inner) => {
                let seed = self.execute_expr(inner, config)?;
                Ok(self.execute_dominates_of(&seed.nodes, config))
            }
            Expr::TraitImplsOf(inner) => {
                let seed = self.execute_expr(inner, config)?;
                Ok(self.execute_trait_impls_of(&seed.nodes, config))
            }
            Expr::PipeFrom { base, ops } => {
                let seed = self.execute_expr(base, config)?;
                self.execute_pipe_from(&seed.nodes, ops, config)
            }
            Expr::MultiPath {
                sources,
                to,
                max_depth,
            } => self.execute_multi_path(sources, to, *max_depth, config),
        }
    }
}

/// Convenience: parse + execute an extended-grammar query.
pub fn run_query_expr(
    query: &str,
    graph: &CodeGraph,
    config: &QueryConfig,
) -> Result<QueryResult, QueryError> {
    if let Ok(agg) = crate::dsl::aggregate::parse_aggregate(query) {
        if let crate::dsl::aggregate::AggExpr::Plain(plain) = agg {
            let plan = crate::dsl::plan::optimise_expr(plain);
            let optimised = plan
                .expr()
                .expect("optimise_expr yields Plan::Expr")
                .clone();
            return QueryEngine::new(graph).execute_expr(&optimised, config);
        }
        return run_aggregate_unified(query, graph, config);
    }

    let expr = parse_expr(query)?;
    let plan = crate::dsl::plan::optimise_expr(expr);
    let optimised = plan
        .expr()
        .expect("optimise_expr yields Plan::Expr")
        .clone();
    QueryEngine::new(graph).execute_expr(&optimised, config)
}
