//! Interprocedural propagation along bound call sites, run to a fixpoint.

use std::collections::{BTreeSet, HashMap};

use super::binding::{CallBindings, Site, argument_pairs};
use super::solver::FunctionSolver;
use super::worklist::DirtySet;
use super::{AbstractLocation, FACT_BUDGET, PointsToTable, operand_var};
use crate::graph::CodeGraph;
use crate::ir::{IrFunction, IrOp, Operand, Var};
use crate::nodes::NodeId;

/// A call op bound to the function it calls: see [`bind_call_sites`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CallBinding {
    /// The calling function.
    pub caller: NodeId,
    /// Index of the [`IrOp::Call`] in the caller's body.
    pub op: usize,
    /// The function the op calls.
    pub callee: NodeId,
}

/// The call ops of `ir_map`'s functions that [`analyze_interprocedural`]
/// passes values through, each with the function it calls, sorted.
///
/// Only a `Calls` edge between two analysed functions can bind a call op,
/// and then only an op whose callee text names the edge's target — by its
/// last segment, with the qualifier deciding between same-named targets;
/// an op that still fits several targets binds to none.
pub fn bind_call_sites(
    graph: &CodeGraph,
    ir_map: &HashMap<NodeId, IrFunction>,
) -> Vec<CallBinding> {
    let functions = sorted_functions(ir_map);
    let bindings = CallBindings::build(graph, &functions);
    let mut out: Vec<CallBinding> = bindings
        .out
        .iter()
        .enumerate()
        .flat_map(|(caller, sites)| sites.iter().map(move |site| (caller, site)))
        .map(|(caller, site)| CallBinding {
            caller: functions[caller].0.clone(),
            op: site.op,
            callee: functions[site.callee].0.clone(),
        })
        .collect();
    out.sort();
    out
}

/// `ir_map`'s entries sorted by id, so positions (and any budget cut-off)
/// are deterministic.
fn sorted_functions(ir_map: &HashMap<NodeId, IrFunction>) -> Vec<(&NodeId, &IrFunction)> {
    let mut functions: Vec<(&NodeId, &IrFunction)> = ir_map.iter().collect();
    functions.sort_unstable_by_key(|(id, _)| *id);
    functions
}

/// Run interprocedural points-to analysis across the call graph.
///
/// Every function is solved on its own, its root locations owned by its
/// [`NodeId`]. Call ops are bound to the targets of the caller's `Calls`
/// edges ([`bind_call_sites`]); a worklist over functions then pushes, at
/// each bound call site, the caller's argument pts-sets into the callee's
/// receiver and parameters and the callee's return pts-set into the op's
/// destination. A function whose seeds grow is re-solved and pushes again.
/// The loop ends when nothing grows — the fixpoint, with no round cap. A
/// table reports `converged == false` only if its own solve hit the fact
/// budget.
pub fn analyze_interprocedural(
    graph: &CodeGraph,
    ir_map: &HashMap<NodeId, IrFunction>,
) -> HashMap<NodeId, PointsToTable> {
    let functions = sorted_functions(ir_map);
    let bindings = CallBindings::build(graph, &functions);
    let mut solvers: Vec<FunctionSolver<'_>> = functions
        .iter()
        .map(|(id, ir)| FunctionSolver::new((*id).clone(), ir, FACT_BUDGET))
        .collect();
    // Total size of the sets last pushed from each site / each function's
    // returns. Sets only grow, so an unchanged total means nothing new.
    let mut pushed_args: Vec<Vec<usize>> = bindings
        .out
        .iter()
        .map(|sites| vec![0; sites.len()])
        .collect();
    let mut pushed_returns = vec![0; solvers.len()];

    let mut dirty = DirtySet::all(solvers.len());
    while let Some(func) = dirty.pop() {
        solvers[func].solve();
        for (site, pushed) in bindings.out[func].iter().zip(&mut pushed_args[func]) {
            if push_arguments(&mut solvers, func, site, pushed) {
                dirty.push(site.callee);
            }
        }
        let callers = &bindings.into[func];
        if callers.is_empty() {
            continue;
        }
        let returned = returned_pts(solvers[func].ir(), solvers[func].table());
        if returned.len() == pushed_returns[func] {
            continue;
        }
        pushed_returns[func] = returned.len();
        for &(caller, op) in callers {
            if push_return(&mut solvers[caller], op, &returned) {
                dirty.push(caller);
            }
        }
    }

    functions
        .into_iter()
        .map(|(id, _)| id.clone())
        .zip(solvers.into_iter().map(FunctionSolver::into_table))
        .collect()
}

/// Seed the callee of `site` with what `caller` passes there, unless the
/// passed sets are no larger than at the last push (`pushed`). Returns
/// `true` if any receiver or parameter set grew.
fn push_arguments(
    solvers: &mut [FunctionSolver<'_>],
    caller: usize,
    site: &Site,
    pushed: &mut usize,
) -> bool {
    let caller_ir = solvers[caller].ir();
    let IrOp::Call { receiver, args, .. } = &caller_ir.body[site.op] else {
        return false;
    };
    let callee_ir = solvers[site.callee].ir();
    let caller_pts = solvers[caller].table();
    let passed = || {
        argument_pairs(site, receiver.as_ref(), args, callee_ir)
            .filter_map(|(param, arg)| Some((param, operand_pts(caller_pts, arg)?)))
    };
    let size: usize = passed().map(|(_, locs)| locs.len()).sum();
    if size == *pushed {
        return false;
    }
    *pushed = size;
    let seeds: Vec<(&Var, BTreeSet<AbstractLocation>)> = passed()
        .map(|(param, locs)| (param, locs.clone()))
        .collect();
    let target = &mut solvers[site.callee];
    let mut grew = false;
    for (param, locs) in &seeds {
        grew |= target.seed(param, locs);
    }
    grew
}

/// Seed the destination of the call at `op` in `caller` with what the
/// callee returns. Returns `true` if it grew.
fn push_return(
    caller: &mut FunctionSolver<'_>,
    op: usize,
    returned: &BTreeSet<AbstractLocation>,
) -> bool {
    let caller_ir = caller.ir();
    match &caller_ir.body[op] {
        IrOp::Call { dst: Some(dst), .. } => caller.seed(dst, returned),
        _ => false,
    }
}

/// The union of the pts-sets of every value `callee_ir` returns.
fn returned_pts(callee_ir: &IrFunction, callee_pts: &PointsToTable) -> BTreeSet<AbstractLocation> {
    let mut returned = BTreeSet::new();
    for op in &callee_ir.body {
        if let IrOp::Return { value: Some(val) } = op {
            if let Some(locs) = operand_pts(callee_pts, val) {
                returned.extend(locs.iter().cloned());
            }
        }
    }
    returned
}

/// `pts(operand)`; `None` for a constant or an untracked variable.
fn operand_pts<'t>(
    pts: &'t PointsToTable,
    operand: &Operand,
) -> Option<&'t BTreeSet<AbstractLocation>> {
    operand_var(operand).and_then(|var| pts.vars.get(var.as_ref()))
}
