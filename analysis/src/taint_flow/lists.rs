//! Position-aware local lists ([`crate::propagation_rules::ListRules`]):
//! within one straight run of ops (a basic block), a list made by
//! `new ArrayList<>()` and then only appended to, removed from and read at
//! constant positions keeps track of which append supplied each element.
//! A read at a known position then carries that append's argument alone —
//! the OWASP Benchmark's `add("safe"); add(param); add("moresafe");
//! remove(0); get(1)` reads `"moresafe"`. Any other use of the list, or the
//! end of the block, forgets it (reads fall back to the whole list).

use std::collections::HashMap;

use crate::ir::{IrFunction, IrOp, Operand, Var};
use crate::propagation_rules::PropagationRules;
use crate::reaching_defs::is_temp;

/// `get` ops whose result is one append's argument: op → (argument, the
/// append op).
pub(super) fn positional_reads(
    func: &IrFunction,
    rules: &PropagationRules,
) -> HashMap<usize, (Operand, usize)> {
    let lists = rules.lists;
    let mut reads = HashMap::new();
    if lists.types.is_empty() {
        return reads;
    }
    // Temporaries holding a new, empty list.
    let mut fresh: Vec<&Var> = Vec::new();
    // Tracked lists: their elements (argument, append op), in order.
    let mut state: HashMap<&Var, Vec<(Operand, usize)>> = HashMap::new();
    for (index, op) in func.body.iter().enumerate() {
        match op {
            IrOp::Label(_) | IrOp::Branch { .. } | IrOp::Jump { .. } | IrOp::Return { .. } => {
                state.clear();
                fresh.clear();
            }
            IrOp::Call {
                dst: Some(dst),
                callee,
                receiver: None,
                args,
            } if args.is_empty() && is_temp(dst) && is_list_type(callee, lists.types) => {
                fresh.push(dst);
            }
            IrOp::Assign {
                dst,
                src: Operand::Var(src),
            } if fresh.contains(&src) => {
                state.insert(dst, Vec::new());
            }
            IrOp::Call {
                receiver: Some(Operand::Var(list)),
                callee,
                args,
                ..
            } if state.contains_key(list) => {
                let name = method(callee);
                let elements = state.get_mut(list).expect("tracked");
                let position = match args.first() {
                    Some(Operand::Const(text)) => text.trim().parse::<usize>().ok(),
                    _ => None,
                };
                let others_mention = args
                    .iter()
                    .any(|a| matches!(a, Operand::Var(v) if v == list));
                if others_mention {
                    state.remove(list);
                } else if lists.appends.contains(&name) && args.len() == 1 {
                    elements.push((args[0].clone(), index));
                } else if lists.removes_at.contains(&name) && args.len() == 1 {
                    match position.filter(|&i| i < elements.len()) {
                        Some(i) => {
                            elements.remove(i);
                        }
                        None => {
                            state.remove(list);
                        }
                    }
                } else if lists.removes_first.contains(&name) && args.is_empty() {
                    if elements.is_empty() {
                        state.remove(list);
                    } else {
                        elements.remove(0);
                    }
                } else if lists.reads_at.contains(&name) && args.len() == 1 {
                    match position.and_then(|i| elements.get(i)) {
                        Some(element) => {
                            reads.insert(index, element.clone());
                        }
                        None => {
                            state.remove(list);
                        }
                    }
                } else if lists.reads_first.contains(&name) && args.is_empty() {
                    match elements.first() {
                        Some(element) => {
                            reads.insert(index, element.clone());
                        }
                        None => {
                            state.remove(list);
                        }
                    }
                } else if !rules.is_clean_result(name) {
                    state.remove(list);
                }
            }
            _ => {
                // Any other use of a tracked list forgets it.
                for var in crate::reaching_defs::op_reads(op)
                    .into_iter()
                    .filter_map(|operand| match operand {
                        Operand::Var(var) => Some(var),
                        _ => None,
                    })
                {
                    state.remove(var);
                }
                if let IrOp::Assign { dst, .. }
                | IrOp::BinOp { dst, .. }
                | IrOp::FieldRead { dst, .. }
                | IrOp::Call { dst: Some(dst), .. } = op
                {
                    state.remove(dst);
                }
            }
        }
    }
    reads
}

/// The method a callee names: its last segment.
fn method(callee: &str) -> &str {
    callee.rsplit(['.', ':']).next().unwrap_or(callee)
}

/// Whether `new java.util.ArrayList<String>` constructs one of `types`.
fn is_list_type(callee: &str, types: &[&str]) -> bool {
    let Some(rest) = callee.strip_prefix("new ") else {
        return false;
    };
    let bare = rest.split('<').next().unwrap_or(rest).trim();
    let last = bare.rsplit('.').next().unwrap_or(bare);
    types.contains(&last)
}
