//! Expression-oriented languages ([`crate::ir_rules::ExpressionRules`],
//! Rust): blocks, `if` and `match` have values, patterns bind names from
//! the value they match, and macros take token trees.
//!
//! - A block's value is its last expression when that is not a statement
//!   (`{ a; b }` is `b`); a function's body value is what it returns.
//! - `if` and `match` in expression position assign each branch's value
//!   to one temporary; a match arm first binds its pattern's names from
//!   the scrutinee (field-insensitively: `Some((a, b))` gives `a` and `b`
//!   the scrutinee's data).
//! - `let p = v` and `if let p = v` bind the same way; `let … else`
//!   branches to its (diverging) block.
//! - A macro call `m!(…)` is a call of `m!` whose arguments are the token
//!   tree's comma-separated groups: a lone name reads that variable, a
//!   format string reads the names its `{name}` placeholders use, any
//!   other group reads every name in it (not the fields or methods after
//!   a `.`, nor path segments).

use tree_sitter::Node;

use super::{Lowerer, Value};
use crate::cfg_rules::Construct;
use crate::ir::model::{IrOp, Operand, Span, Var};
use crate::ir_rules::{BindingShape, MacroShape};

/// Most nodes of one macro's token tree read (a `json!`/`html!` body can
/// be the size of a file): past it, the rest reads nothing.
const MAX_MACRO_TOKENS: usize = 4096;

impl Lowerer<'_, '_> {
    /// Lower a block's statements; its value is its last expression, when
    /// that is not a statement.
    pub(super) fn block_value(&mut self, node: Node<'_>) -> Option<Value> {
        let expression = self.rules.expression?;
        let children = self.named_children(node);
        let tail = children
            .last()
            .copied()
            .filter(|last| !expression.statements.contains(&last.kind()));
        let outer = self.func.set_span(Span::of(node));
        for child in &children {
            if Some(*child) != tail {
                self.stmt(*child);
            }
        }
        let value = tail.map(|tail| self.expr(tail));
        self.func.set_span(outer);
        value
    }

    /// The value of a control-flow construct in expression position.
    pub(super) fn construct_value(&mut self, node: Node<'_>) -> Value {
        match self.cfg.classify(node.kind()) {
            Construct::Block => self
                .block_value(node)
                .unwrap_or_else(|| Value::constant("()")),
            Construct::If => self.if_value(node),
            Construct::Switch => self.match_value(node),
            _ => {
                self.stmt(node);
                Value::constant("()")
            }
        }
    }

    /// `if c { a } else { b }`: one temporary holding the taken branch's
    /// value.
    fn if_value(&mut self, node: Node<'_>) -> Value {
        let condition = match node.child_by_field_name("condition") {
            Some(condition) => self.expr(condition).operand,
            None => Operand::Const("<cond>".into()),
        };
        let result = self.fresh_temp();
        let then = self.fresh_label();
        let end = self.fresh_label();
        self.func.push(IrOp::Branch {
            cond: condition,
            target: then,
        });
        let otherwise = node
            .child_by_field_name("alternative")
            .map_or(Operand::Const("()".into()), |alt| self.expr(alt).operand);
        self.func.push(IrOp::Assign {
            dst: result.clone(),
            src: otherwise,
        });
        self.func.push(IrOp::Jump { target: end });
        self.func.push(IrOp::Label(then));
        let taken = node
            .child_by_field_name("consequence")
            .map_or(Operand::Const("()".into()), |block| {
                self.expr(block).operand
            });
        self.func.push(IrOp::Assign {
            dst: result.clone(),
            src: taken,
        });
        self.func.push(IrOp::Label(end));
        Value::of(Operand::Var(result))
    }

    /// `match v { p => a, … }`: each arm binds its pattern from `v`, tests
    /// its guard, and yields its value into one temporary. Arms are tried
    /// in order; the last is taken when none before it is (a match is
    /// exhaustive).
    pub(super) fn match_value(&mut self, node: Node<'_>) -> Value {
        let result = self.fresh_temp();
        let Some(expression) = self.rules.expression else {
            self.stmt(node);
            return Value::constant("()");
        };
        let arm_shape = expression.arm;
        let scrutinee = node
            .child_by_field_name("value")
            .map_or(Operand::Const("()".into()), |v| self.expr(v).operand);
        let container = node.child_by_field_name("body").unwrap_or(node);
        let arms: Vec<Node<'_>> = self
            .named_children(container)
            .into_iter()
            .filter(|child| child.kind() == arm_shape.kind)
            .collect();
        let labels: Vec<_> = arms.iter().map(|_| self.fresh_label()).collect();
        let end = self.fresh_label();
        if let Some((last, rest)) = labels.split_last() {
            for &label in rest {
                self.func.push(IrOp::Branch {
                    cond: Operand::Const("<case>".into()),
                    target: label,
                });
            }
            self.func.push(IrOp::Jump { target: *last });
        }
        for (arm, label) in arms.iter().zip(labels) {
            let outer = self.func.set_span(Span::of(*arm));
            self.func.push(IrOp::Label(label));
            if let Some(pattern) = arm.child_by_field_name(arm_shape.pattern) {
                self.bind(pattern, &scrutinee);
                if let Some(guard) = pattern.child_by_field_name(arm_shape.guard) {
                    drop(self.expr(guard));
                }
            }
            let value = arm
                .child_by_field_name(arm_shape.value)
                .map_or(Operand::Const("()".into()), |v| self.expr(v).operand);
            self.func.push(IrOp::Assign {
                dst: result.clone(),
                src: value,
            });
            self.func.push(IrOp::Jump { target: end });
            self.func.set_span(outer);
        }
        self.func.push(IrOp::Label(end));
        Value::of(Operand::Var(result))
    }

    /// `e?`: `e`'s value, after a branch that returns when `e` is an error.
    /// The branch tests `e` itself (`ok = e; if ok == false { return }`),
    /// so a guard whose check is `e` — `validate(&x)?`,
    /// `Uuid::parse_str(id).map_err(…)?` — holds on the way on.
    pub(super) fn try_value(&mut self, node: Node<'_>) -> Value {
        let Some(inner) = self.named_children(node).into_iter().next() else {
            return Value::constant("()");
        };
        let value = self.expr(inner);
        let ok = self.fresh_temp();
        self.func.push(IrOp::Assign {
            dst: ok.clone(),
            src: value.operand.clone(),
        });
        let failed = self.fresh_temp();
        self.func.push(IrOp::BinOp {
            dst: failed.clone(),
            lhs: Operand::Var(ok),
            op: crate::ir::model::BinOpKind::Eq,
            rhs: Operand::Const("false".into()),
        });
        let early = self.fresh_label();
        let on = self.fresh_label();
        self.func.push(IrOp::Branch {
            cond: Operand::Var(failed),
            target: early,
        });
        self.func.push(IrOp::Jump { target: on });
        self.func.push(IrOp::Label(early));
        self.func.push(IrOp::Return { value: None });
        self.func.push(IrOp::Label(on));
        value
    }

    /// Every name `pattern` binds takes `value`'s data. A variant's path
    /// and a guard bind nothing. Iterative: depth is bounded by the input.
    pub(super) fn bind(&mut self, pattern: Node<'_>, value: &Operand) {
        let skip_fields = self
            .rules
            .expression
            .map_or(&[][..], |expression| expression.pattern_skip_fields);
        let mut stack = vec![pattern];
        while let Some(node) = stack.pop() {
            if self.is_identifier(node) {
                self.declare(node);
                self.func.push(IrOp::Assign {
                    dst: Var::new(self.text(node)),
                    src: value.clone(),
                });
                continue;
            }
            if self.rules.literals.contains(&node.kind()) {
                continue;
            }
            let skipped: Vec<Node<'_>> = skip_fields
                .iter()
                .filter_map(|field| node.child_by_field_name(field))
                .collect();
            let children = self.named_children(node);
            // Reversed, so names bind in source order.
            stack.extend(
                children
                    .into_iter()
                    .rev()
                    .filter(|child| !skipped.contains(child)),
            );
        }
    }

    /// `let p = v;` (a statement) or `let p = v` in a condition: `p`'s
    /// names take `v`. A `let … else` branches to its block when the
    /// pattern does not match.
    pub(super) fn binding(&mut self, node: Node<'_>, shape: BindingShape) {
        let pattern = node.child_by_field_name(shape.pattern);
        let value = node.child_by_field_name(shape.value);
        let Some(pattern) = pattern else {
            if let Some(value) = value {
                drop(self.expr(value));
            }
            return;
        };
        let Some(value) = value else {
            // `let x;`: declared, no value.
            let mut stack = vec![pattern];
            while let Some(node) = stack.pop() {
                if self.is_identifier(node) {
                    self.declare(node);
                } else {
                    stack.extend(self.named_children(node));
                }
            }
            return;
        };
        let value = self.expr(value).operand;
        let otherwise = shape
            .otherwise
            .and_then(|field| node.child_by_field_name(field));
        match otherwise {
            Some(block) => {
                let other = self.fresh_label();
                let end = self.fresh_label();
                self.func.push(IrOp::Branch {
                    cond: Operand::Const("<no-match>".into()),
                    target: other,
                });
                self.bind(pattern, &value);
                self.func.push(IrOp::Jump { target: end });
                self.func.push(IrOp::Label(other));
                self.stmt(block);
                self.func.push(IrOp::Label(end));
            }
            None => self.bind(pattern, &value),
        }
    }

    /// `m!(…)`: a call of `m!` with the token tree's groups as arguments.
    pub(super) fn macro_call(&mut self, node: Node<'_>, shape: MacroShape) -> Value {
        let name = node
            .child_by_field_name(shape.name)
            .map_or("", |n| self.text(n))
            .trim()
            .to_string();
        let tokens = {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|child| child.kind() == shape.tokens)
        };
        let mut args = Vec::new();
        let mut places = Vec::new();
        if let Some(tokens) = tokens {
            for group in self.token_groups(tokens) {
                let value = self.token_group(&group, shape);
                places.push(value.place);
                args.push(value.operand);
            }
        }
        let dst = self.fresh_temp();
        self.func.push(IrOp::Call {
            dst: Some(dst.clone()),
            callee: format!("{name}!"),
            receiver: None,
            args,
        });
        self.record_call(None, places);
        Value::of(Operand::Var(dst))
    }

    /// A token tree's top-level tokens, split at commas (its delimiters
    /// left out).
    fn token_groups<'t>(&self, tree: Node<'t>) -> Vec<Vec<Node<'t>>> {
        let mut groups: Vec<Vec<Node<'t>>> = Vec::new();
        let mut current: Vec<Node<'t>> = Vec::new();
        let count = tree.child_count();
        let mut cursor = tree.walk();
        for (index, child) in tree.children(&mut cursor).enumerate() {
            if child.is_extra() || child.kind().contains("comment") {
                continue;
            }
            let delimiter = !child.is_named() && (index == 0 || index + 1 == count);
            if delimiter {
                continue;
            }
            if !child.is_named() && self.text(child) == "," {
                if !current.is_empty() {
                    groups.push(std::mem::take(&mut current));
                }
                continue;
            }
            current.push(child);
        }
        if !current.is_empty() {
            groups.push(current);
        }
        groups
    }

    /// One argument group of a macro call.
    fn token_group(&mut self, group: &[Node<'_>], shape: MacroShape) -> Value {
        if let [single] = group {
            if self.is_identifier(*single) {
                return self.expr(*single);
            }
            if shape.format_strings.contains(&single.kind()) {
                let names = format_placeholders(self.text(*single));
                if names.is_empty() {
                    return Value::constant(self.text(*single));
                }
                let mut args = vec![Operand::Const(self.text(*single).to_string())];
                args.extend(names.into_iter().map(|name| Operand::Var(Var::new(name))));
                return self.synthetic("<format>", args);
            }
            if single.is_named() && single.kind() != shape.tokens && single.child_count() == 0 {
                return Value::constant(self.text(*single));
            }
        }
        // Every name read in the group, in order, once.
        let mut names: Vec<String> = Vec::new();
        let mut seen = 0usize;
        let mut stack: Vec<Node<'_>> = group.iter().rev().copied().collect();
        let mut previous = String::new();
        while let Some(token) = stack.pop() {
            seen += 1;
            if seen > MAX_MACRO_TOKENS {
                break;
            }
            if token.kind() == shape.tokens {
                let mut cursor = token.walk();
                let inner: Vec<Node<'_>> = token.children(&mut cursor).collect();
                stack.extend(inner.into_iter().rev());
                previous.clear();
                continue;
            }
            let text = self.text(token);
            if self.is_identifier(token) {
                let next = token.next_sibling().map(|n| self.text(n));
                let field_or_path = previous == "." || previous == "::";
                let head_of_path = matches!(next, Some("::") | Some("!"));
                if !field_or_path && !head_of_path && !names.iter().any(|n| n == text) {
                    names.push(text.to_string());
                }
            } else if shape.format_strings.contains(&token.kind()) {
                for name in format_placeholders(text) {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            previous = text.to_string();
        }
        let args = names
            .into_iter()
            .map(|name| Operand::Var(Var::new(name)))
            .collect();
        self.synthetic("<tokens>", args)
    }
}

/// The names a format string's placeholders read: `"{x} {y:?} {0} {{z}}"`
/// → `x`, `y`.
pub(crate) fn format_placeholders(literal: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let bytes = literal.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'{') {
            index += 2;
            continue;
        }
        let start = index + 1;
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        let name = &literal[start..end];
        let closes = matches!(bytes.get(end), Some(b'}') | Some(b':'));
        if closes
            && !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && !names.iter().any(|n| n == name)
        {
            names.push(name.to_string());
        }
        index = end.max(index + 1);
    }
    names
}
