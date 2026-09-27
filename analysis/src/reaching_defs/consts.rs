//! Constant branch conditions: `if (true)`, `if (5 == 5)`, `while (1)`,
//! arithmetic over locals assigned exactly once from constants
//! (`int num = 86; if ((7 * 42) - num > 200)`), and a switch on a character
//! of a constant string (`"ABC".charAt(1)` against `case 'B'`). Anything
//! else — a field, a parameter, another call, a variable assigned twice —
//! is unknown.

use std::collections::HashMap;

use crate::ir::{BinOpKind, IrFunction, IrOp, Operand, Var};

/// Op index of each `Branch` whose condition folds → whether it is taken.
pub fn branch_outcomes(func: &IrFunction) -> HashMap<usize, bool> {
    let folder = Folder::new(func);
    func.body
        .iter()
        .enumerate()
        .filter_map(|(index, op)| match op {
            IrOp::Branch { cond, .. } => match folder.operand(cond, 0)? {
                Value::Int(value) => Some((index, value != 0)),
                Value::Str(_) => None,
            },
            _ => None,
        })
        .collect()
}

/// Nesting a folded expression may have (temporaries chain one op each).
const MAX_DEPTH: usize = 32;

/// Methods that pick a character of a string by index; the fold of
/// `s.m(i)` on a constant `s` and `i` is that character's code.
const CHAR_AT: &[&str] = &["charAt", "codePointAt", "charCodeAt"];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Int(i64),
    Str(String),
}

struct Folder<'f> {
    func: &'f IrFunction,
    /// Variables with exactly one definition: the op defining them.
    single: HashMap<&'f Var, usize>,
}

impl<'f> Folder<'f> {
    fn new(func: &'f IrFunction) -> Self {
        let mut count: HashMap<&Var, (usize, usize)> = HashMap::new();
        for (index, op) in func.body.iter().enumerate() {
            let dst = match op {
                IrOp::Assign { dst, .. }
                | IrOp::BinOp { dst, .. }
                | IrOp::FieldRead { dst, .. }
                | IrOp::Call { dst: Some(dst), .. } => dst,
                _ => continue,
            };
            let entry = count.entry(dst).or_insert((0, index));
            entry.0 += 1;
        }
        // Parameters and the receiver are defined at entry as well.
        for var in func.params.iter().chain(&func.receiver) {
            count.entry(var).or_insert((0, 0)).0 += 2;
        }
        let single = count
            .into_iter()
            .filter(|(_, (defs, _))| *defs == 1)
            .map(|(var, (_, index))| (var, index))
            .collect();
        Self { func, single }
    }

    fn operand(&self, operand: &Operand, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        match operand {
            Operand::Const(text) => literal(text),
            Operand::Var(var) => {
                let &index = self.single.get(var)?;
                match &self.func.body[index] {
                    IrOp::Assign { src, .. } => self.operand(src, depth + 1),
                    IrOp::BinOp { lhs, op, rhs, .. } => {
                        let lhs = self.operand(lhs, depth + 1)?;
                        let rhs = self.operand(rhs, depth + 1)?;
                        apply(*op, lhs, rhs)
                    }
                    IrOp::Call {
                        callee,
                        receiver: Some(receiver),
                        args,
                        ..
                    } if args.len() == 1 && CHAR_AT.iter().any(|m| callee.ends_with(m)) => {
                        let Value::Str(text) = self.operand(receiver, depth + 1)? else {
                            return None;
                        };
                        let Value::Int(index) = self.operand(&args[0], depth + 1)? else {
                            return None;
                        };
                        let index = usize::try_from(index).ok()?;
                        text.chars().nth(index).map(|c| Value::Int(c as i64))
                    }
                    _ => None,
                }
            }
            Operand::Temp(_) => None,
        }
    }
}

/// A literal as written: an integer (`42`, `0x10`, `7L`), a boolean, a
/// character (`'C'`) or a plain double-quoted string.
fn literal(text: &str) -> Option<Value> {
    let text = text.trim();
    match text {
        "true" | "True" | "TRUE" => return Some(Value::Int(1)),
        "false" | "False" | "FALSE" => return Some(Value::Int(0)),
        _ => {}
    }
    if let Some(inner) = text.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
        let mut chars = inner.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) if c != '\\' => Some(Value::Int(c as i64)),
            _ => None,
        };
    }
    if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        return (!inner.contains(['\\', '"'])).then(|| Value::Str(inner.to_string()));
    }
    let digits = text.trim_end_matches(['l', 'L', 'u', 'U']);
    if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        return i64::from_str_radix(hex, 16).ok().map(Value::Int);
    }
    digits.parse::<i64>().ok().map(Value::Int)
}

fn apply(op: BinOpKind, lhs: Value, rhs: Value) -> Option<Value> {
    let (lhs, rhs) = match (lhs, rhs) {
        (Value::Int(lhs), Value::Int(rhs)) => (lhs, rhs),
        (Value::Str(lhs), Value::Str(rhs)) => {
            return match op {
                BinOpKind::Eq => Some(Value::Int(i64::from(lhs == rhs))),
                BinOpKind::Ne => Some(Value::Int(i64::from(lhs != rhs))),
                _ => None,
            };
        }
        _ => return None,
    };
    let bool_of = |b: bool| i64::from(b);
    Some(Value::Int(match op {
        BinOpKind::Add => lhs.checked_add(rhs)?,
        BinOpKind::Sub => lhs.checked_sub(rhs)?,
        BinOpKind::Mul => lhs.checked_mul(rhs)?,
        BinOpKind::Div => lhs.checked_div(rhs)?,
        BinOpKind::Rem => lhs.checked_rem(rhs)?,
        BinOpKind::Eq => bool_of(lhs == rhs),
        BinOpKind::Ne => bool_of(lhs != rhs),
        BinOpKind::Lt => bool_of(lhs < rhs),
        BinOpKind::Gt => bool_of(lhs > rhs),
        BinOpKind::Le => bool_of(lhs <= rhs),
        BinOpKind::Ge => bool_of(lhs >= rhs),
        BinOpKind::And => bool_of(lhs != 0 && rhs != 0),
        BinOpKind::Or => bool_of(lhs != 0 || rhs != 0),
        BinOpKind::BitAnd => lhs & rhs,
        BinOpKind::BitOr => lhs | rhs,
        BinOpKind::BitXor => lhs ^ rhs,
        BinOpKind::Shl => lhs.checked_shl(u32::try_from(rhs).ok()?)?,
        BinOpKind::Shr => lhs.checked_shr(u32::try_from(rhs).ok()?)?,
    }))
}
