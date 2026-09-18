//! Binding call ops to the functions `Calls` edges say they call.
//!
//! A `Calls` edge says *that* a caller calls a callee, not *which* call op
//! does. A call op names its callee only as source text (`f`, `Foo::new`,
//! `self.items.push`), so each op is matched against the caller's edge
//! targets by the text's last segment, and its qualifier weighs the
//! same-named ones ([`super::evidence`]). An op binds to the single target
//! the qualifier confirms, else to the single one it does not rule out.
//! When two or more remain the op binds to none: seeding every same-named
//! function would invent flows, and a missing flow is the lesser error.

use std::collections::HashMap;

use super::call_text::CalleeText;
use super::evidence::{Evidence, evidence, is_instance_call};
use super::target::Target;
use crate::edges::EdgeKind;
use crate::graph::CodeGraph;
use crate::ir::{IrFunction, IrOp, Operand, Var};
use crate::nodes::NodeId;

/// One call op bound to the function it calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Site {
    /// Index of the `IrOp::Call` in the caller's body.
    pub(super) op: usize,
    /// Position of the callee in the analysed function list.
    pub(super) callee: usize,
    /// `true` for `obj.m(args)` on a value: the op's `receiver` operand is
    /// the callee's receiver. `false` for bare, path and class-qualified
    /// calls, which pass any receiver as their first argument.
    pub(super) instance: bool,
}

/// Every bound call site, indexed both ways over positions in the
/// analysed function list.
#[derive(Debug, Default)]
pub(super) struct CallBindings {
    /// `out[caller]`: the sites in `caller`'s body.
    pub(super) out: Vec<Vec<Site>>,
    /// `into[callee]`: `(caller, op)` of every site bound to `callee`.
    pub(super) into: Vec<Vec<(usize, usize)>>,
}

impl CallBindings {
    /// Bind the call ops of every function in `functions` (the analysed
    /// list, in position order) to the targets of its `Calls` edges.
    pub(super) fn build(graph: &CodeGraph, functions: &[(&NodeId, &IrFunction)]) -> Self {
        let position: HashMap<&NodeId, usize> = functions
            .iter()
            .enumerate()
            .map(|(pos, (id, _))| (*id, pos))
            .collect();
        let targets = Target::all(graph, functions);
        let mut bindings = Self {
            out: vec![Vec::new(); functions.len()],
            into: vec![Vec::new(); functions.len()],
        };
        for (caller, (id, ir)) in functions.iter().enumerate() {
            let mut callees: Vec<usize> = graph
                .get_edges_from(id)
                .into_iter()
                .filter(|(_, edge)| matches!(edge.kind, EdgeKind::Calls))
                .filter_map(|(target, _)| position.get(target).copied())
                .collect();
            callees.sort_unstable();
            callees.dedup();
            if callees.is_empty() {
                continue;
            }
            let caller_owner = targets[caller].owner;
            for (op, instr) in ir.body.iter().enumerate() {
                let IrOp::Call { callee, .. } = instr else {
                    continue;
                };
                let Some(text) = CalleeText::parse(callee) else {
                    continue;
                };
                let Some(callee) = pick(&text, &callees, &targets, caller_owner) else {
                    continue;
                };
                bindings.out[caller].push(Site {
                    op,
                    callee,
                    instance: is_instance_call(text.qualifier, &targets[callee]),
                });
                bindings.into[callee].push((caller, op));
            }
        }
        bindings
    }
}

/// The one target among `callees` that `text` calls, if exactly one fits.
fn pick(
    text: &CalleeText<'_>,
    callees: &[usize],
    targets: &[Target<'_>],
    caller_owner: Option<&str>,
) -> Option<usize> {
    let mut best: Option<(Evidence, usize)> = None;
    let mut tied = false;
    for &callee in callees {
        let target = &targets[callee];
        if target.name != text.name {
            continue;
        }
        let ev = evidence(text, target, caller_owner);
        if ev == Evidence::Against {
            continue;
        }
        match best {
            Some((best_ev, _)) if ev < best_ev => {}
            Some((best_ev, _)) if ev == best_ev => tied = true,
            _ => {
                best = Some((ev, callee));
                tied = false;
            }
        }
    }
    best.filter(|_| !tied).map(|(_, callee)| callee)
}

/// `(callee variable, caller operand)` for each value a bound call passes:
/// the receiver first, then `params[i] ← args[i]`.
///
/// An instance call (`obj.m(x)`) passes its receiver operand to the
/// callee's receiver. A bare, path or class-qualified call to a function
/// with a receiver passes it as the first argument when there is exactly
/// one argument more than parameters (`Foo::m(obj, x)`,
/// `Base.__init__(self, x)`); otherwise no argument is the receiver
/// (`Foo.create(x)` on a TS static-like call). A receiver operand of a
/// call to a function without one (`pkg.F(x)`, `module.f(x)`) is dropped.
pub(super) fn argument_pairs<'a>(
    site: &Site,
    receiver: Option<&'a Operand>,
    args: &'a [Operand],
    callee: &'a IrFunction,
) -> impl Iterator<Item = (&'a Var, &'a Operand)> {
    let mut args = args;
    let mut passed_receiver = None;
    if let Some(self_var) = &callee.receiver {
        if site.instance {
            passed_receiver = receiver.map(|recv| (self_var, recv));
        } else if args.len() == callee.params.len() + 1 {
            passed_receiver = Some((self_var, &args[0]));
            args = &args[1..];
        }
    }
    passed_receiver
        .into_iter()
        .chain(callee.params.iter().zip(args))
}
