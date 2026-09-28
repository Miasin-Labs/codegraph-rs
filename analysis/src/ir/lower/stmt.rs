//! Statements: control flow by [`CfgRules::classify`], declarations and
//! expression statements by [`IrRules`].
//!
//! Every construct lowers to labels, `Branch` and `Jump` the way the
//! bespoke lowerers do: `Branch` jumps to its target when taken and falls
//! through otherwise. Exceptions are approximated: a `catch` is reachable
//! from the start and the end of its `try` body.

use tree_sitter::Node;

use super::{Lowerer, Value};
use crate::cfg_rules::Construct;
use crate::ir::model::{BinOpKind, IrOp, Label, Operand, Span, Var};

/// Fields holding a conditional's taken branch.
const CONSEQUENCE_FIELDS: &[&str] = &["consequence", "body"];
/// Fields holding a conditional's alternatives.
const ALTERNATIVE_FIELDS: &[&str] = &["alternative"];
/// Fields holding a C-style `for` loop's parts.
const FOR_INIT_FIELDS: &[&str] = &["init", "initializer", "initialize"];
const FOR_UPDATE_FIELDS: &[&str] = &["update", "increment"];

impl Lowerer<'_, '_> {
    /// Lower one statement. The recursion head of statement lowering.
    pub(super) fn stmt(&mut self, node: Node<'_>) {
        crate::ensure_sufficient_stack(|| {
            let outer = self.func.set_span(Span::of(node));
            self.stmt_inner(node);
            self.func.set_span(outer);
        });
    }

    fn stmt_inner(&mut self, node: Node<'_>) {
        let kind = node.kind();
        if node.is_extra() || self.rules.ignored.contains(&kind) {
            return;
        }
        if self.inline_scope(kind).is_some() {
            drop(self.expr(node));
            return;
        }
        if self.cfg.is_nested_scope(kind) {
            return;
        }
        if let Some(expression) = self.rules.expression {
            if let Some(shape) = expression.binding(kind) {
                self.binding(node, *shape);
                return;
            }
            if self.cfg.classify(kind) == Construct::Switch {
                drop(self.match_value(node));
                return;
            }
        }
        match self.cfg.classify(kind) {
            Construct::Block => {
                for child in self.named_children(node) {
                    self.stmt(child);
                }
            }
            Construct::If => self.if_stmt(node),
            Construct::Loop | Construct::InfiniteLoop => self.loop_stmt(node),
            Construct::Switch => self.switch_stmt(node),
            Construct::Try => self.try_stmt(node),
            Construct::Labeled => {
                let label = self.cfg.labels.label_kind();
                for child in self.named_children(node) {
                    if Some(child.kind()) != label {
                        self.stmt(child);
                    }
                }
            }
            Construct::Return => {
                let value = self
                    .named_children(node)
                    .into_iter()
                    .next()
                    .map(|child| self.expr(child).operand);
                // Inside an inlined closure: its value, and on after it.
                if let Some((exit, result)) = self.inline_exits.last().cloned() {
                    if let Some(value) = value {
                        self.func.push(IrOp::Assign {
                            dst: result,
                            src: value,
                        });
                    }
                    self.func.push(IrOp::Jump { target: exit });
                    return;
                }
                self.func.push(IrOp::Return { value });
            }
            Construct::Throw => {
                for child in self.named_children(node) {
                    drop(self.expr(child));
                }
                self.func.push(IrOp::Return { value: None });
            }
            Construct::Break => self.jump_to(self.breaks.last().copied()),
            Construct::Continue => self.jump_to(self.continues.last().copied()),
            Construct::Fallthrough => {}
            Construct::Plain => self.plain(node),
        }
    }

    /// A jump to `target`, or out of the function when there is none.
    fn jump_to(&mut self, target: Option<Label>) {
        match target {
            Some(target) => self.func.push(IrOp::Jump { target }),
            None => self.func.push(IrOp::Return { value: None }),
        }
    }

    fn plain(&mut self, node: Node<'_>) {
        if let Some(shape) = self.rules.declaration(node.kind()) {
            for child in self.named_children(node) {
                if !shape.declarators.contains(&child.kind()) {
                    // `int x;`, `char *p;`, `int a[10];`: declared, no value.
                    if let Some(name) = self.declared_name(child, shape.name) {
                        self.declare(name);
                    }
                    continue;
                }
                let value = child.child_by_field_name(shape.value);
                let name = self.declared_name(child, shape.name);
                if let Some(name) = name {
                    self.declare(name);
                }
                if let (Some(name), Some(value)) = (name, value) {
                    let value = self.expr(value);
                    self.func.push(IrOp::Assign {
                        dst: Var::new(self.text(name)),
                        src: value.operand,
                    });
                }
            }
            return;
        }
        // An expression statement, or a statement the tables do not name
        // (`echo`, `with`, `synchronized`): its parts, in order.
        if node.kind() == "expression_statement" || self.is_statement_like(node) {
            for child in self.named_children(node) {
                drop(self.expr(child));
            }
            return;
        }
        drop(self.expr(node));
    }

    /// A node the expression lowering hands back to statements.
    pub(super) fn is_statement_like(&self, node: Node<'_>) -> bool {
        let kind = node.kind();
        kind.ends_with("_statement")
            || self.rules.declaration(kind).is_some()
            || self.cfg.classify(kind) != Construct::Plain
    }

    fn condition(&mut self, node: Node<'_>) -> Operand {
        match node.child_by_field_name("condition") {
            Some(condition) => self.expr(condition).operand,
            None => Operand::Const("<cond>".into()),
        }
    }

    fn consequence<'t>(node: Node<'t>) -> Option<Node<'t>> {
        CONSEQUENCE_FIELDS
            .iter()
            .find_map(|field| node.child_by_field_name(field))
    }

    fn alternatives<'t>(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut cursor = node.walk();
        for field in ALTERNATIVE_FIELDS {
            let found: Vec<_> = node.children_by_field_name(field, &mut cursor).collect();
            if !found.is_empty() {
                return found;
            }
        }
        let Some(else_kind) = self.cfg.else_node else {
            return Vec::new();
        };
        node.named_children(&mut cursor)
            .filter(|child| child.kind() == else_kind)
            .collect()
    }

    fn if_stmt(&mut self, node: Node<'_>) {
        let condition = self.condition(node);
        let alternatives = self.alternatives(node);
        self.conditional(condition, Self::consequence(node), &alternatives);
    }

    /// `if condition { consequence } <alternatives…>`, where an alternative
    /// is an `elif` (with its own condition) or the final `else`.
    fn conditional(
        &mut self,
        condition: Operand,
        consequence: Option<Node<'_>>,
        alternatives: &[Node<'_>],
    ) {
        let then = self.fresh_label();
        let end = self.fresh_label();
        self.func.push(IrOp::Branch {
            cond: condition,
            target: then,
        });
        if let Some((first, rest)) = alternatives.split_first() {
            if self.cfg.elif_nodes.contains(&first.kind()) {
                let outer = self.func.set_span(Span::of(*first));
                let condition = self.condition(*first);
                self.conditional(condition, Self::consequence(*first), rest);
                self.func.set_span(outer);
            } else {
                self.stmt(*first);
            }
        }
        self.func.push(IrOp::Jump { target: end });
        self.func.push(IrOp::Label(then));
        if let Some(consequence) = consequence {
            self.stmt(consequence);
        }
        self.func.push(IrOp::Label(end));
    }

    fn loop_stmt(&mut self, node: Node<'_>) {
        let kind = node.kind();
        let head = self.fresh_label();
        let body = self.fresh_label();
        let next = self.fresh_label();
        let end = self.fresh_label();
        if self.rules.do_loops.contains(&kind) {
            self.func.push(IrOp::Label(body));
            self.loop_body(node, next, end);
            self.func.push(IrOp::Label(next));
            let condition = self.condition(node);
            self.func.push(IrOp::Branch {
                cond: condition,
                target: body,
            });
            self.func.push(IrOp::Label(end));
            return;
        }

        let foreach = self.rules.foreach(kind).copied();
        let iterable = foreach
            .and_then(|shape| self.slot(node, shape.iterable))
            .map(|iterable| self.expr(iterable));
        for field in FOR_INIT_FIELDS {
            let mut cursor = node.walk();
            let inits: Vec<Node<'_>> = node.children_by_field_name(field, &mut cursor).collect();
            for init in inits {
                self.stmt_or_expr(init);
            }
        }
        self.func.push(IrOp::Label(head));
        let condition = match (foreach, iterable) {
            (Some(shape), Some(iterable)) => {
                // Each iteration binds an element of the iterable.
                let element = self.fresh_temp();
                self.func.push(IrOp::Call {
                    dst: Some(element.clone()),
                    callee: "<next>".into(),
                    receiver: Some(iterable.operand),
                    args: Vec::new(),
                });
                self.record_call(iterable.place, Vec::new());
                if let Some(binding) = self.slot(node, shape.binding) {
                    if self.rules.expression.is_some() {
                        self.bind(binding, &Operand::Var(element));
                    } else {
                        self.declare_all(binding);
                        self.write(binding, Operand::Var(element));
                    }
                }
                Operand::Const("<has-next>".into())
            }
            _ => match node.child_by_field_name("condition") {
                Some(condition) => self.expr(condition).operand,
                // `for (;;)`, `while True` without a field: runs until a
                // `break`.
                None if kind.starts_with("for") => Operand::Const("true".into()),
                None => Operand::Const("<cond>".into()),
            },
        };
        self.func.push(IrOp::Branch {
            cond: condition,
            target: body,
        });
        self.func.push(IrOp::Jump { target: end });
        self.func.push(IrOp::Label(body));
        self.loop_body(node, next, end);
        self.func.push(IrOp::Label(next));
        for field in FOR_UPDATE_FIELDS {
            let mut cursor = node.walk();
            let updates: Vec<Node<'_>> = node.children_by_field_name(field, &mut cursor).collect();
            for update in updates {
                self.stmt_or_expr(update);
            }
        }
        self.func.push(IrOp::Jump { target: head });
        self.func.push(IrOp::Label(end));
    }

    fn loop_body(&mut self, node: Node<'_>, next: Label, end: Label) {
        self.breaks.push(end);
        self.continues.push(next);
        if let Some(body) = node.child_by_field_name("body") {
            self.stmt(body);
        }
        self.breaks.pop();
        self.continues.pop();
    }

    fn stmt_or_expr(&mut self, node: Node<'_>) {
        if self.is_statement_like(node) {
            self.stmt(node);
        } else {
            drop(self.expr(node));
        }
    }

    fn switch_stmt(&mut self, node: Node<'_>) {
        let scrutinee = node
            .child_by_field_name("condition")
            .or_else(|| node.child_by_field_name("value"))
            .or_else(|| node.child_by_field_name("subject"))
            .map(|scrutinee| self.expr(scrutinee).operand);
        let container = node.child_by_field_name("body").unwrap_or(node);
        let arms: Vec<Node<'_>> = self
            .named_children(container)
            .into_iter()
            .filter(|child| self.cfg.case_nodes.contains(&child.kind()))
            .collect();
        let labels: Vec<_> = arms.iter().map(|_| self.fresh_label()).collect();
        let end = self.fresh_label();
        // Each arm is taken when the scrutinee equals one of its values
        // (a pattern or an unknown scrutinee: maybe); no match goes to the
        // default arm, else past the switch.
        let mut default = None;
        for (arm, &label) in arms.iter().zip(&labels) {
            let values = self.case_values(*arm);
            if values.is_empty() {
                if self.cfg.is_default_arm(self.text(*arm)) {
                    default = Some(label);
                } else {
                    self.func.push(IrOp::Branch {
                        cond: Operand::Const("<case>".into()),
                        target: label,
                    });
                }
                continue;
            }
            for value in values {
                let value = self.expr(value).operand;
                let cond = match &scrutinee {
                    Some(scrutinee) => {
                        let test = self.fresh_temp();
                        self.func.push(IrOp::BinOp {
                            dst: test.clone(),
                            lhs: scrutinee.clone(),
                            op: BinOpKind::Eq,
                            rhs: value,
                        });
                        Operand::Var(test)
                    }
                    None => Operand::Const("<case>".into()),
                };
                self.func.push(IrOp::Branch {
                    cond,
                    target: label,
                });
            }
        }
        self.func.push(IrOp::Jump {
            target: default.unwrap_or(end),
        });
        let tracks_breaks = self.cfg.break_exits_switch;
        if tracks_breaks {
            self.breaks.push(end);
        }
        for (arm, label) in arms.iter().zip(labels) {
            self.func.push(IrOp::Label(label));
            let value = arm.child_by_field_name("value");
            for child in self.named_children(*arm) {
                if Some(child) != value && !self.rules.case_labels.contains(&child.kind()) {
                    self.stmt_or_expr(child);
                }
            }
            if !self.cfg.fallthrough_cases.contains(&arm.kind()) {
                self.func.push(IrOp::Jump { target: end });
            }
        }
        if tracks_breaks {
            self.breaks.pop();
        }
        self.func.push(IrOp::Label(end));
    }

    /// The values a switch arm matches (none: a default or a pattern).
    fn case_values<'t>(&self, arm: Node<'t>) -> Vec<Node<'t>> {
        if let Some(value) = arm.child_by_field_name("value") {
            return vec![value];
        }
        self.named_children(arm)
            .into_iter()
            .filter(|child| self.rules.case_labels.contains(&child.kind()))
            .flat_map(|label| self.named_children(label))
            .collect()
    }

    fn try_stmt(&mut self, node: Node<'_>) {
        let children = self.named_children(node);
        let is_catch = |child: &Node<'_>| {
            Some(child.kind()) == self.cfg.catch_node
                || node.child_by_field_name("handler") == Some(*child)
        };
        let is_finally = |child: &Node<'_>| {
            Some(child.kind()) == self.cfg.finally_node
                || node.child_by_field_name("finalizer") == Some(*child)
        };
        let catches: Vec<Node<'_>> = children.iter().copied().filter(is_catch).collect();
        let finally: Vec<Node<'_>> = children.iter().copied().filter(is_finally).collect();
        let body = node.child_by_field_name("body");
        let handlers: Vec<_> = catches.iter().map(|_| self.fresh_label()).collect();
        let after = self.fresh_label();
        let throws = |this: &mut Self| {
            for &handler in &handlers {
                this.func.push(IrOp::Branch {
                    cond: Operand::Const("<throws>".into()),
                    target: handler,
                });
            }
        };
        throws(self);
        if let Some(body) = body {
            self.stmt(body);
        }
        // Python `try … else:` runs when the body did not raise.
        for child in &children {
            if Some(*child) != body && !is_catch(child) && !is_finally(child) {
                self.stmt_or_expr(*child);
            }
        }
        throws(self);
        self.func.push(IrOp::Jump { target: after });
        for (catch, handler) in catches.iter().zip(handlers) {
            self.func.push(IrOp::Label(handler));
            let catch_body = catch.child_by_field_name("body");
            for child in self.named_children(*catch) {
                if Some(child) == catch_body {
                    continue;
                }
                // The caught exception binds the clause's parameter names.
                let exception = Value::of(Operand::Const("<exception>".into()));
                self.declare_all(child);
                self.write(child, exception.operand);
            }
            if let Some(catch_body) = catch_body {
                self.stmt(catch_body);
            }
            self.func.push(IrOp::Jump { target: after });
        }
        self.func.push(IrOp::Label(after));
        for finally in finally {
            for child in self.named_children(finally) {
                self.stmt(child);
            }
        }
    }
}
