//! Interprocedural propagation along `Calls` edges, run to a fixpoint.

use std::collections::{BTreeSet, HashMap};

use super::solver::FunctionSolver;
use super::worklist::DirtySet;
use super::{AbstractLocation, FACT_BUDGET, PointsToTable, operand_var};
use crate::edges::EdgeKind;
use crate::graph::CodeGraph;
use crate::ir::{IrFunction, IrOp, Operand, Var};
use crate::nodes::NodeId;

/// Run interprocedural points-to analysis across the call graph.
///
/// Every function is solved on its own, its root locations owned by its
/// [`NodeId`]. A worklist over functions then pushes, along `Calls` edges,
/// caller argument pts-sets into callee parameters and callee return
/// pts-sets into the caller's call-site destinations; a function whose
/// seeds grow is re-solved and pushes again. The loop ends when nothing
/// grows — the fixpoint, with no round cap. A table reports
/// `converged == false` only if its own solve hit the fact budget.
pub fn analyze_interprocedural(
    graph: &CodeGraph,
    ir_map: &HashMap<NodeId, IrFunction>,
) -> HashMap<NodeId, PointsToTable> {
    let mut functions: Vec<(&NodeId, &IrFunction)> = ir_map.iter().collect();
    // Sorted so the processing order (and any budget cut-off) is deterministic.
    functions.sort_unstable_by_key(|(id, _)| *id);
    let ids: Vec<&NodeId> = functions.iter().map(|(id, _)| *id).collect();
    let calls = CallEdges::build(graph, &ids);
    let mut solvers: Vec<FunctionSolver<'_>> = functions
        .iter()
        .map(|(id, ir)| FunctionSolver::new((*id).clone(), ir, FACT_BUDGET))
        .collect();

    let mut dirty = DirtySet::all(solvers.len());
    while let Some(func) = dirty.pop() {
        solvers[func].solve();
        for &callee in &calls.callees[func] {
            if push_arguments(&mut solvers, func, callee) {
                dirty.push(callee);
            }
        }
        for &caller in &calls.callers[func] {
            if push_returns(&mut solvers, func, caller) {
                dirty.push(caller);
            }
        }
    }

    ids.into_iter()
        .cloned()
        .zip(solvers.into_iter().map(FunctionSolver::into_table))
        .collect()
}

/// `Calls` edges between analysed functions, as indices into the sorted
/// function list.
struct CallEdges {
    callees: Vec<Vec<usize>>,
    callers: Vec<Vec<usize>>,
}

impl CallEdges {
    fn build(graph: &CodeGraph, ids: &[&NodeId]) -> Self {
        let position: HashMap<&NodeId, usize> =
            ids.iter().enumerate().map(|(pos, id)| (*id, pos)).collect();
        let mut callees = vec![Vec::new(); ids.len()];
        let mut callers = vec![Vec::new(); ids.len()];
        for (caller, id) in ids.iter().enumerate() {
            for (target, edge) in graph.get_edges_from(id) {
                if !matches!(edge.kind, EdgeKind::Calls) {
                    continue;
                }
                let Some(&callee) = position.get(target) else {
                    continue;
                };
                callees[caller].push(callee);
                callers[callee].push(caller);
            }
        }
        for list in callees.iter_mut().chain(callers.iter_mut()) {
            list.sort_unstable();
            list.dedup();
        }
        Self { callees, callers }
    }
}

/// Seed `callee`'s parameters with `caller`'s argument pts-sets. Returns
/// `true` if any parameter's set grew.
fn push_arguments(solvers: &mut [FunctionSolver<'_>], caller: usize, callee: usize) -> bool {
    let seeds = argument_seeds(
        solvers[caller].ir(),
        solvers[caller].table(),
        solvers[callee].ir(),
    );
    let target = &mut solvers[callee];
    let mut grew = false;
    for (param, locs) in &seeds {
        grew |= target.seed(param, locs);
    }
    grew
}

/// Seed `caller`'s destinations of calls to `callee` with what `callee`
/// returns. Returns `true` if any destination's set grew.
fn push_returns(solvers: &mut [FunctionSolver<'_>], callee: usize, caller: usize) -> bool {
    let callee_ir = solvers[callee].ir();
    let returned = returned_pts(callee_ir, solvers[callee].table());
    if returned.is_empty() {
        return false;
    }
    let caller_ir = solvers[caller].ir();
    let target = &mut solvers[caller];
    let mut grew = false;
    for dst in call_destinations(caller_ir, &callee_ir.name) {
        grew |= target.seed(dst, &returned);
    }
    grew
}

/// Map caller argument pts-sets to callee parameter variables.
fn argument_seeds(
    caller_ir: &IrFunction,
    caller_pts: &PointsToTable,
    callee_ir: &IrFunction,
) -> Vec<(Var, BTreeSet<AbstractLocation>)> {
    let mut seeds = Vec::new();
    for op in &caller_ir.body {
        let IrOp::Call { callee, args, .. } = op else {
            continue;
        };
        if callee != &callee_ir.name {
            continue;
        }
        for (param, arg) in callee_ir.params.iter().zip(args) {
            let arg_set = operand_pts_set(caller_pts, arg);
            if !arg_set.is_empty() {
                seeds.push((param.clone(), arg_set));
            }
        }
    }
    seeds
}

/// The union of the pts-sets of every value `callee_ir` returns.
fn returned_pts(callee_ir: &IrFunction, callee_pts: &PointsToTable) -> BTreeSet<AbstractLocation> {
    let mut returned = BTreeSet::new();
    for op in &callee_ir.body {
        if let IrOp::Return { value: Some(val) } = op {
            returned.extend(operand_pts_set(callee_pts, val));
        }
    }
    returned
}

/// Destination variables of `caller_ir`'s calls to `callee_name`.
fn call_destinations<'ir>(
    caller_ir: &'ir IrFunction,
    callee_name: &'ir str,
) -> impl Iterator<Item = &'ir Var> {
    caller_ir.body.iter().filter_map(move |op| match op {
        IrOp::Call {
            dst: Some(dst),
            callee,
            ..
        } if callee == callee_name => Some(dst),
        _ => None,
    })
}

/// `pts(operand)` as an owned set; constants point nowhere.
fn operand_pts_set(pts: &PointsToTable, operand: &Operand) -> BTreeSet<AbstractLocation> {
    operand_var(operand)
        .and_then(|var| pts.vars.get(var.as_ref()).cloned())
        .unwrap_or_default()
}
