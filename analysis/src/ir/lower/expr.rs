//! Expressions: every lowered expression yields a [`Value`] (operand +
//! place) and is recorded as an [`crate::ir::ExprValue`].

use tree_sitter::Node;

use super::{Lowerer, Value, compact};
use crate::ir::model::{BinOpKind, IrOp, Operand, Place, Span, Var};

impl Lowerer<'_, '_> {
    /// Lower one expression. The recursion head of expression lowering.
    pub(super) fn expr(&mut self, node: Node<'_>) -> Value {
        crate::ensure_sufficient_stack(|| {
            let outer = self.func.set_span(Span::of(node));
            let value = self.expr_inner(node);
            self.func.set_span(outer);
            self.record(node, &value);
            value
        })
    }

    fn expr_inner(&mut self, node: Node<'_>) -> Value {
        let kind = node.kind();
        let rules = self.rules;
        if self.is_identifier(node) {
            let mut name = self.text(node);
            if let Some(stands_for) = self.macros.get(name) {
                if stands_for.starts_with('"')
                    || stands_for.starts_with(|c: char| c.is_ascii_digit())
                {
                    return Value::constant(stands_for.clone());
                }
                name = stands_for;
            }
            let var = Var::new(name);
            return Value {
                operand: Operand::Var(var.clone()),
                place: Some(Place::var(var)),
            };
        }
        if rules.literals.contains(&kind) {
            return Value::constant(self.text(node));
        }
        if rules.interpolating_strings.contains(&kind) {
            return self.interpolation(node);
        }
        if let Some(scope) = self.inline_scope(kind) {
            return self.inline_value(node, scope);
        }
        if rules.ignored.contains(&kind) || self.cfg.is_nested_scope(kind) {
            return Value::constant(format!("<{kind}>"));
        }
        if let Some(expression) = rules.expression {
            if let Some(shape) = expression.macro_call(kind) {
                return self.macro_call(node, *shape);
            }
            if let Some(shape) = expression.binding(kind) {
                // `if let p = v`: binds, and tests whether it matched.
                self.binding(node, *shape);
                return Value::constant("<let>");
            }
            if kind == expression.try_kind {
                return self.try_value(node);
            }
            if kind == expression.structs.kind {
                return self.struct_value(node, expression.structs);
            }
            if kind == expression.tuples.0 {
                return self.tuple_value(node);
            }
        }
        if rules.call(kind).is_some() {
            return self.call(node);
        }
        if let Some(shape) = rules.member(kind) {
            let base = self.slot(node, shape.object).map(|n| self.expr(n));
            let field = self
                .slot(node, shape.property)
                .map(|n| self.text(n).to_string())
                .unwrap_or_default();
            return self.field_read(base, field);
        }
        if let Some(shape) = rules.subscript(kind) {
            let base = self.slot(node, shape.object).map(|n| self.expr(n));
            let index = self.slot(node, shape.index);
            let field = self.element_field(index);
            if let Some(index) = index {
                drop(self.expr(index));
            }
            return self.field_read(base, field);
        }
        if let Some(shape) = rules.ternary(kind) {
            let condition = self
                .slot(node, shape.condition)
                .map_or(Operand::Const("<cond>".into()), |n| self.expr(n).operand);
            let consequence = self.slot(node, shape.consequence);
            let alternative = self.slot(node, shape.alternative);
            return self.choice(condition, consequence, alternative);
        }
        if rules.assignments.contains(&kind) {
            return self.assignment(node);
        }
        if rules.updates.contains(&kind) {
            let target = node
                .child_by_field_name("argument")
                .or_else(|| self.named_children(node).into_iter().next());
            let Some(target) = target else {
                return Value::constant("");
            };
            let current = self.expr(target);
            let dst = self.fresh_temp();
            self.func.push(IrOp::BinOp {
                dst: dst.clone(),
                lhs: current.operand,
                op: BinOpKind::Add,
                rhs: Operand::Const("1".into()),
            });
            self.write(target, Operand::Var(dst.clone()));
            return Value::of(Operand::Var(dst));
        }
        if rules.binaries.contains(&kind) {
            return self.binary(node);
        }
        if rules.unaries.contains(&kind) {
            return self.unary(node);
        }
        if rules.passthrough.contains(&kind) {
            let children = self.named_children(node);
            let inner = node.child_by_field_name("value");
            // Evaluate every part (`a, b`), yield the value one.
            let mut last = None;
            for child in children {
                if child.kind().ends_with("type") || child.kind() == "type_descriptor" {
                    continue;
                }
                let value = self.expr(child);
                if inner.is_none_or(|inner| inner == child) {
                    last = Some(value);
                }
            }
            return last.unwrap_or_else(|| Value::constant(""));
        }
        if rules.collections.contains(&kind) {
            let mut args = Vec::new();
            for child in self.named_children(node) {
                args.push(self.argument(child).operand);
            }
            return self.synthetic("<collection>", args);
        }
        if self.is_statement_like(node) {
            if rules.expression.is_some() {
                return self.construct_value(node);
            }
            self.stmt(node);
            return Value::constant("");
        }
        // Anything else: its parts, combined.
        let children = self.named_children(node);
        if children.is_empty() {
            return Value::constant(self.text(node));
        }
        let mut args = Vec::new();
        for child in children {
            args.push(self.expr(child).operand);
        }
        self.synthetic(&format!("<{kind}>"), args)
    }

    /// `dst = <name>(args…)`: a value built from `args`.
    pub(super) fn synthetic(&mut self, name: &str, args: Vec<Operand>) -> Value {
        let dst = self.fresh_temp();
        let places = vec![None; args.len()];
        self.func.push(IrOp::Call {
            dst: Some(dst.clone()),
            callee: name.to_string(),
            receiver: None,
            args,
        });
        self.record_call(None, places);
        Value::of(Operand::Var(dst))
    }

    fn field_read(&mut self, base: Option<Value>, field: String) -> Value {
        let Some(base) = base else {
            return Value::constant(field);
        };
        let place = base.place.as_ref().map(|place| place.field(&field).0);
        let dst = self.fresh_temp();
        self.func.push(IrOp::FieldRead {
            dst: dst.clone(),
            base: base.operand,
            field,
        });
        Value {
            operand: Operand::Var(dst),
            place,
        }
    }

    /// `condition ? consequence : alternative` into a fresh temporary.
    fn choice(
        &mut self,
        condition: Operand,
        consequence: Option<Node<'_>>,
        alternative: Option<Node<'_>>,
    ) -> Value {
        let result = self.fresh_temp();
        let then = self.fresh_label();
        let end = self.fresh_label();
        self.func.push(IrOp::Branch {
            cond: condition,
            target: then,
        });
        let otherwise = alternative.map_or(Operand::Const(String::new()), |n| self.expr(n).operand);
        self.func.push(IrOp::Assign {
            dst: result.clone(),
            src: otherwise,
        });
        self.func.push(IrOp::Jump { target: end });
        self.func.push(IrOp::Label(then));
        let taken = consequence.map_or(Operand::Const(String::new()), |n| self.expr(n).operand);
        self.func.push(IrOp::Assign {
            dst: result.clone(),
            src: taken,
        });
        self.func.push(IrOp::Label(end));
        Value::of(Operand::Var(result))
    }

    fn assignment(&mut self, node: Node<'_>) -> Value {
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");
        let operator = node
            .child_by_field_name("operator")
            .map(|n| self.text(n).trim().to_string())
            .unwrap_or_default();
        let mut value = right.map_or(Value::constant(""), |right| self.expr(right));
        let Some(left) = left else {
            return value;
        };
        // `x op= v` is `x = x op v`.
        if let Some(op) = operator.strip_suffix('=').filter(|op| !op.is_empty()) {
            let current = self.expr(left);
            let dst = self.fresh_temp();
            self.func.push(IrOp::BinOp {
                dst: dst.clone(),
                lhs: current.operand,
                op: BinOpKind::from_source(op).unwrap_or(BinOpKind::Add),
                rhs: value.operand,
            });
            value = Value::of(Operand::Var(dst));
        }
        self.write(left, value.operand.clone());
        Value::of(value.operand)
    }

    fn binary(&mut self, node: Node<'_>) -> Value {
        let (left, right) = match (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) {
            (Some(left), Some(right)) => (left, right),
            // Python `comparison_operator`: `a < b`, operators apart.
            _ => {
                let children = self.named_children(node);
                match (children.first(), children.get(1)) {
                    (Some(left), Some(right)) => (*left, *right),
                    _ => return self.synthetic("<binary>", Vec::new()),
                }
            }
        };
        let operator = node
            .child_by_field_name("operator")
            .or_else(|| node.child_by_field_name("operators"))
            .or_else(|| {
                // The first anonymous child between the operands.
                let mut cursor = node.walk();
                node.children(&mut cursor)
                    .find(|child| !child.is_named() && child.start_byte() >= left.end_byte())
            })
            .map(|n| self.text(n).trim().to_string())
            .unwrap_or_default();
        let lhs = self.expr(left);
        let rhs = self.expr(right);
        let dst = self.fresh_temp();
        // An operator the IR does not model still combines its operands.
        let op = BinOpKind::from_source(&operator).unwrap_or(BinOpKind::Add);
        // In a language with pointers, `p + n` points into `p`'s storage
        // (`fgets(data + len, …)` writes `data`).
        let place = if !self.rules.address_ops.is_empty()
            && matches!(op, BinOpKind::Add | BinOpKind::Sub)
        {
            lhs.place.clone()
        } else {
            None
        };
        self.func.push(IrOp::BinOp {
            dst: dst.clone(),
            lhs: lhs.operand,
            op,
            rhs: rhs.operand,
        });
        Value {
            operand: Operand::Var(dst),
            place,
        }
    }

    fn unary(&mut self, node: Node<'_>) -> Value {
        let operand = node
            .child_by_field_name("operand")
            .or_else(|| node.child_by_field_name("argument"))
            .or_else(|| self.named_children(node).into_iter().last());
        let Some(operand) = operand else {
            return Value::constant(self.text(node));
        };
        let operator = node
            .child_by_field_name("operator")
            .map(|n| self.text(n).trim().to_string())
            .or_else(|| node.child(0).map(|n| self.text(n).trim().to_string()))
            .unwrap_or_default();
        let value = self.expr(operand);
        let (op, rhs, lhs) = match operator.as_str() {
            "-" => (BinOpKind::Sub, value.operand, Operand::Const("0".into())),
            "!" | "not" => (BinOpKind::Eq, Operand::Const("false".into()), value.operand),
            // `*p`, `&x`, `+x`, `~x`: the operand's data (and, through a
            // pointer, its storage).
            _ => return value,
        };
        let dst = self.fresh_temp();
        self.func.push(IrOp::BinOp {
            dst: dst.clone(),
            lhs,
            op,
            rhs,
        });
        Value::of(Operand::Var(dst))
    }

    /// A string that may interpolate values: constant unless it does.
    fn interpolation(&mut self, node: Node<'_>) -> Value {
        let parts: Vec<Node<'_>> = self
            .named_children(node)
            .into_iter()
            .filter(|child| !self.rules.string_fragments.contains(&child.kind()))
            .collect();
        if parts.is_empty() {
            return Value::constant(self.text(node));
        }
        let mut args = Vec::new();
        for part in parts {
            args.push(self.argument(part).operand);
        }
        self.synthetic("<concat>", args)
    }

    /// An argument (or an element or interpolated part): unwrapped from
    /// `name=value`, `...xs`, `${…}`.
    fn argument(&mut self, node: Node<'_>) -> Value {
        if !self.rules.argument_wrappers.contains(&node.kind()) {
            return self.expr(node);
        }
        let inner = node
            .child_by_field_name("value")
            .or_else(|| node.child_by_field_name("expression"))
            .or_else(|| self.named_children(node).into_iter().last());
        match inner {
            Some(inner) => {
                let value = self.expr(inner);
                self.record(node, &value);
                value
            }
            None => Value::constant(self.text(node)),
        }
    }

    fn call(&mut self, node: Node<'_>) -> Value {
        let Some(shape) = self.rules.call(node.kind()).copied() else {
            return Value::constant("");
        };
        // `Ok(x)` / `Err(x)`: the variant's field holds `x`.
        if let Some(expression) = self.rules.expression {
            let function = shape.function.and_then(|f| node.child_by_field_name(f));
            let arguments = shape.arguments.and_then(|f| node.child_by_field_name(f));
            if let (Some(function), Some(arguments)) = (function, arguments) {
                let name = self.text(function);
                let args = self.named_children(arguments);
                if let (Some(variant), [arg]) = (
                    expression.variant_fields.iter().find(|v| **v == name),
                    args.as_slice(),
                ) {
                    return self.variant_value(variant, *arg);
                }
            }
        }
        let arguments = shape
            .arguments
            .and_then(|field| node.child_by_field_name(field))
            .or_else(|| {
                self.named_children(node)
                    .into_iter()
                    .find(|child| self.rules.argument_lists.contains(&child.kind()))
            });
        let callee_end = arguments.map_or(node.end_byte(), |a| a.start_byte());
        let mut receiver: Option<Value> = None;
        let callee = if let Some(field) = shape.function {
            let mut function = node.child_by_field_name(field);
            // `x.parse::<T>()` calls `x.parse`.
            if let Some(expression) = self.rules.expression {
                for _ in 0..4 {
                    let inner = function.and_then(|f| {
                        expression
                            .callee_wrapper(f.kind())
                            .and_then(|field| f.child_by_field_name(field))
                    });
                    match inner {
                        Some(inner) => function = Some(inner),
                        None => break,
                    }
                }
            }
            if let Some(member) = function.and_then(|f| self.rules.member(f.kind()).copied()) {
                let function = function.expect("member access");
                receiver = self.slot(function, member.object).map(|n| self.expr(n));
            }
            function.map_or_else(String::new, |f| compact(self.text(f)))
        } else if shape.constructor {
            let name = shape
                .name
                .and_then(|field| node.child_by_field_name(field))
                .or_else(|| {
                    self.named_children(node)
                        .into_iter()
                        .find(|child| Some(*child) != arguments)
                });
            format!("new {}", name.map_or("", |n| self.text(n)).trim())
        } else {
            receiver = shape
                .object
                .and_then(|field| node.child_by_field_name(field))
                .map(|object| self.expr(object));
            let start = node.start_byte();
            compact(self.source.get(start..callee_end).unwrap_or_default())
        };

        let mut args = Vec::new();
        let mut places = Vec::new();
        // A closure argument's parameters take the receiver's data.
        self.call_receivers
            .push(receiver.as_ref().map(|r| r.operand.clone()));
        match arguments {
            Some(list) => {
                // Every argument keeps its position (a lambda is a constant).
                let mut cursor = list.walk();
                let items: Vec<Node<'_>> = list
                    .named_children(&mut cursor)
                    .filter(|arg| !arg.is_extra() && !arg.kind().contains("comment"))
                    .collect();
                for arg in items {
                    let value = self.argument(arg);
                    places.push(value.place);
                    args.push(value.operand);
                }
            }
            // `print $x`, `include $f`: the operands are the arguments.
            None if shape.function.is_none() && shape.object.is_none() && !shape.constructor => {
                for arg in self.named_children(node) {
                    let value = self.expr(arg);
                    places.push(value.place);
                    args.push(value.operand);
                }
            }
            None => {}
        }
        self.call_receivers.pop();
        let dst = self.fresh_temp();
        let receiver_place = receiver.as_ref().and_then(|r| r.place.clone());
        self.func.push(IrOp::Call {
            dst: Some(dst.clone()),
            callee: callee.trim_end_matches(['?', '.']).to_string(),
            receiver: receiver.map(|r| r.operand),
            args,
        });
        self.record_call(receiver_place, places);
        Value::of(Operand::Var(dst))
    }
}
