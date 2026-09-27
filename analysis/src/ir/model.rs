//! Language-agnostic intermediate representation for intraprocedural analysis.
//!
//! Each LanguageAdapter can lower its AST into this IR. Analyses like CFG
//! construction, dataflow, and complexity then operate on the IR once.
//!
//! The IR is a flat, 3-address-code style instruction list with explicit
//! labels and branches. It deliberately does NOT model SSA, types, or
//! ownership — those can be layered on top later. The goal is to give
//! analyses a single shape to walk rather than reimplementing per-grammar
//! traversals (see `cfg_rules.rs` / `dataflow_rules.rs` for the current
//! per-language duplication this is intended to eventually replace).

use std::collections::HashMap;

use tree_sitter::Node;

use super::{call, signature};

// ─── Core types ──────────────────────────────────────────────────────────────

/// A named variable in the source (a `let` binding, a parameter, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Var(pub String);

impl Var {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An operand to an IR instruction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Operand {
    /// A named source variable.
    Var(Var),
    /// A literal constant (rendered as source text, e.g. `"42"`, `"\"hi\""`).
    Const(String),
    /// A compiler-generated temporary, numbered uniquely within a function.
    Temp(usize),
}

impl Operand {
    pub fn var(name: impl Into<String>) -> Self {
        Operand::Var(Var::new(name))
    }

    pub fn constant(text: impl Into<String>) -> Self {
        Operand::Const(text.into())
    }
}

/// Binary operator kinds supported by the IR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOpKind {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Gt,
    And,
    Or,
    Rem,
    Le,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOpKind {
    /// Map a textual operator (as it appears in source) to a [`BinOpKind`].
    pub fn from_source(op: &str) -> Option<Self> {
        Some(match op {
            "+" | "." => BinOpKind::Add,
            "-" => BinOpKind::Sub,
            "*" => BinOpKind::Mul,
            "/" => BinOpKind::Div,
            "%" => BinOpKind::Rem,
            "==" | "===" | "is" => BinOpKind::Eq,
            "!=" | "!==" | "<>" | "is not" => BinOpKind::Ne,
            "<" => BinOpKind::Lt,
            ">" => BinOpKind::Gt,
            "<=" => BinOpKind::Le,
            ">=" => BinOpKind::Ge,
            "&&" | "and" => BinOpKind::And,
            "||" | "or" => BinOpKind::Or,
            "&" => BinOpKind::BitAnd,
            "|" => BinOpKind::BitOr,
            "^" => BinOpKind::BitXor,
            "<<" => BinOpKind::Shl,
            ">>" | ">>>" => BinOpKind::Shr,
            _ => return None,
        })
    }

    /// Whether the operator yields a comparison or logical result (a
    /// boolean, never a copy of an operand's data).
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOpKind::Eq
                | BinOpKind::Ne
                | BinOpKind::Lt
                | BinOpKind::Gt
                | BinOpKind::Le
                | BinOpKind::Ge
                | BinOpKind::And
                | BinOpKind::Or
        )
    }
}

/// A branch/jump target. Labels are scoped to a single [`IrFunction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(pub u32);

// ─── Instructions ────────────────────────────────────────────────────────────

/// A single 3-address-code style IR operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrOp {
    /// `dst = src`
    Assign { dst: Var, src: Operand },
    /// `dst = lhs op rhs`
    BinOp {
        dst: Var,
        lhs: Operand,
        op: BinOpKind,
        rhs: Operand,
    },
    /// `dst = callee(args...)` (dst is None for statement-position calls).
    ///
    /// `callee` is the callee expression's source text (`f`, `Foo::new`,
    /// `self.items.push`, `pkg.Func`). A call written in method-call syntax
    /// (`recv.m(args)`) carries the value of `recv` in `receiver`; it is
    /// never repeated in `args`, which hold only the parenthesised arguments.
    Call {
        dst: Option<Var>,
        callee: String,
        receiver: Option<Operand>,
        args: Vec<Operand>,
    },
    /// `dst = base.field`
    FieldRead {
        dst: Var,
        base: Operand,
        field: String,
    },
    /// `base.field = src`
    FieldWrite {
        base: Operand,
        field: String,
        src: Operand,
    },
    /// `if cond goto target`
    Branch { cond: Operand, target: Label },
    /// `goto target`
    Jump { target: Label },
    /// `return value?`
    Return { value: Option<Operand> },
    /// `target:` — defines a branch destination.
    Label(Label),
    /// No-op / phi placeholder.
    Nop,
}

/// Where an op or a value comes from: the syntax node's start as a 1-based
/// line and 0-based column (the position the index records a call edge
/// at), and its byte range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub line: u32,
    pub col: u32,
    pub start_byte: usize,
    pub end_byte: usize,
}

impl Span {
    pub fn of(node: Node<'_>) -> Self {
        let start = node.start_position();
        Self {
            line: start.row as u32 + 1,
            col: start.column as u32,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
        }
    }

    /// Whether `other` lies within this span.
    pub fn contains(&self, other: &Span) -> bool {
        self.start_byte <= other.start_byte && other.end_byte <= self.end_byte
    }
}

/// A storage location an expression designates: a variable and a field
/// path below it (`req.param.x` → `req` + `[param, x]`; an element
/// `a[i]` is `a` + `["[]"]`, or `["[k]"]` for a constant key `k`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Place {
    pub base: Var,
    pub fields: Vec<String>,
}

impl Place {
    pub fn var(var: Var) -> Self {
        Self {
            base: var,
            fields: Vec::new(),
        }
    }

    /// Whether `self` is `other` or a path below it.
    pub fn starts_with(&self, other: &Place) -> bool {
        self.base == other.base && self.fields.starts_with(&other.fields)
    }
}

impl std::fmt::Display for Place {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.base.as_str())?;
        for field in &self.fields {
            if field.starts_with('[') {
                f.write_str(field)?;
            } else {
                write!(f, ".{field}")?;
            }
        }
        Ok(())
    }
}

/// The value an expression evaluated to, recorded by lowerers that track
/// expressions (the rules-driven one: [`crate::ir::lower_with_rules`]), so
/// an analysis can find the IR value of a syntax node by its span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprValue {
    pub span: Span,
    /// The node kind, to tell apart nodes sharing a span (`(x)` and `x`).
    pub kind: &'static str,
    pub operand: Operand,
    /// The storage the expression designates, when it designates one
    /// (`x`, `a.b`, `a[i]`, `&x`, `*p`, `p + n`): what a write through it —
    /// an out-parameter — defines.
    pub place: Option<Place>,
    /// Index into `body` of the first op after the expression was
    /// evaluated: the value holds at that point.
    pub at: usize,
}

/// What a call's receiver and arguments designate, for writes through them
/// (C `fgets(buf, …)`, `strcpy(dst, src)`, `list.add(x)`): recorded by
/// the rules-driven lowering, one entry per `Call` op, in op order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPlaces {
    /// Index of the `Call` op in `body`.
    pub op: usize,
    pub receiver: Option<Place>,
    /// Parallel to the op's `args`.
    pub args: Vec<Option<Place>>,
}

/// Fields a [`Place`] keeps below its variable; deeper accesses stand for
/// their prefix (`a.b.c.d` is `a.b.c`), which analyses treat as a weak
/// (non-killing) write.
pub const MAX_PLACE_DEPTH: usize = 3;

/// A lowered function: parameters + a flat instruction list, plus an index
/// from [`Label`] to its position in `body` for O(1) jump resolution.
///
/// `receiver` is the implicit object parameter of a method — Rust `self`,
/// Python's first parameter of an instance/class method, TypeScript `this`,
/// a Go receiver — kept out of `params` so `params[i]` is the parameter the
/// `i`-th parenthesised argument binds to.
///
/// `spans[i]` is where `body[i]` comes from (lowerers set the current span
/// with [`IrFunction::set_span`]; ops pushed without one carry
/// `Span::default()`), so the `match`es on [`IrOp`] never see locations.
#[derive(Debug, Clone, Default)]
pub struct IrFunction {
    pub name: String,
    pub receiver: Option<Var>,
    pub params: Vec<Var>,
    pub body: Vec<IrOp>,
    pub labels: HashMap<Label, usize>,
    pub spans: Vec<Span>,
    /// Expression values, when the lowerer records them.
    pub values: Vec<ExprValue>,
    /// Where each parameter is declared (parallel to `params`, when known).
    pub param_spans: Vec<Span>,
    /// Receiver/argument places of each call, when the lowerer records them.
    pub call_places: Vec<CallPlaces>,
    current: Span,
}

impl IrFunction {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Append an op to the body at the current span. If it is a
    /// [`IrOp::Label`], also record its position in the `labels` index.
    pub fn push(&mut self, op: IrOp) {
        if let IrOp::Label(l) = op {
            self.labels.insert(l, self.body.len());
        }
        self.body.push(op);
        self.spans.push(self.current);
    }

    /// Make `span` the span of the ops pushed next; returns the previous
    /// one, for the caller to restore.
    pub fn set_span(&mut self, span: Span) -> Span {
        std::mem::replace(&mut self.current, span)
    }

    /// Where `body[index]` comes from.
    pub fn span(&self, index: usize) -> Span {
        self.spans.get(index).copied().unwrap_or_default()
    }

    /// The recorded places of the `Call` op at `op`.
    pub fn call_places(&self, op: usize) -> Option<&CallPlaces> {
        self.call_places
            .binary_search_by_key(&op, |places| places.op)
            .ok()
            .map(|index| &self.call_places[index])
    }
}

impl Place {
    /// `self.field`, or `self` itself when it is already
    /// [`MAX_PLACE_DEPTH`] deep (the second value says whether the result
    /// is exact).
    pub fn field(&self, field: &str) -> (Place, bool) {
        if self.fields.len() >= MAX_PLACE_DEPTH {
            return (self.clone(), false);
        }
        let mut place = self.clone();
        place.fields.push(field.to_string());
        (place, true)
    }

    /// Whether the two places can share storage: one is a prefix of the
    /// other, where an unknown element `[]` matches any element.
    pub fn overlaps(&self, other: &Place) -> bool {
        self.base == other.base
            && self
                .fields
                .iter()
                .zip(&other.fields)
                .all(|(a, b)| field_matches(a, b))
    }

    /// Whether a write to `self` overwrites all of `other` (`other` is
    /// `self` or below it, with no unknown element on the way).
    pub fn covers(&self, other: &Place) -> bool {
        self.base == other.base
            && self.fields.len() <= other.fields.len()
            && self
                .fields
                .iter()
                .zip(&other.fields)
                .all(|(a, b)| a == b && a != "[]")
    }
}

fn field_matches(a: &str, b: &str) -> bool {
    a == b || (a == "[]" && b.starts_with('[')) || (b == "[]" && a.starts_with('['))
}

/// Run `lower` with `node` as the span of the ops it pushes, restoring the
/// enclosing span after: each op carries the innermost node being lowered.
pub(crate) fn at_node<R>(
    func: &mut IrFunction,
    node: Node<'_>,
    lower: impl FnOnce(&mut IrFunction) -> R,
) -> R {
    let outer = func.set_span(Span::of(node));
    let result = lower(func);
    func.set_span(outer);
    result
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Lower a tree-sitter AST subtree (typically a function definition) into the
/// language-agnostic [`IrFunction`] representation.
///
/// Returns `None` if the node isn't a function-shaped thing the adapter can
/// handle — callers can fall back to per-grammar logic in the meantime.
pub trait IrLowering {
    fn lower_function(&self, node: Node, source: &str) -> Option<IrFunction>;
}

// ─── Rust lowering (proof of concept) ────────────────────────────────────────

/// Lowering driver for Rust source. Stateless; safe to construct on demand.
pub struct RustIrLowering;

impl RustIrLowering {
    pub fn new() -> Self {
        Self
    }

    fn fresh_label(next: &mut u32) -> Label {
        let l = Label(*next);
        *next += 1;
        l
    }

    fn fresh_temp(next: &mut usize) -> usize {
        let t = *next;
        *next += 1;
        t
    }

    fn text<'a>(node: Node<'a>, source: &'a str) -> &'a str {
        node.utf8_text(source.as_bytes()).unwrap_or("")
    }

    /// Walk a Rust function body block (`block` node) and emit IR.
    fn lower_block(
        block: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) {
        let mut cursor = block.walk();
        for child in block.named_children(&mut cursor) {
            Self::lower_stmt(child, source, func, next_label, next_temp);
        }
    }

    fn lower_stmt(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) {
        // Recursion guard — statement nesting is bounded only by source size.
        crate::ensure_sufficient_stack(|| {
            at_node(func, node, |func| {
                Self::lower_stmt_inner(node, source, func, next_label, next_temp)
            })
        });
    }

    fn lower_stmt_inner(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) {
        match node.kind() {
            "let_declaration" => {
                // `let <pattern> = <value>;`
                let pattern = node.child_by_field_name("pattern");
                let value = node.child_by_field_name("value");
                let Some(pattern) = pattern else { return };
                let name = Self::text(pattern, source).to_string();
                let dst = Var::new(name);

                let src = match value {
                    Some(v) => Self::lower_expr(v, source, func, next_label, next_temp),
                    None => Operand::Const(String::new()),
                };
                func.push(IrOp::Assign { dst, src });
            }
            "expression_statement" => {
                // The semicolon-terminated statement may wrap a flow-control
                // construct like `return x;` — those need to be dispatched
                // through `lower_stmt`, not `lower_expr`. Re-enter for the
                // single named child.
                if let Some(inner) = node.named_child(0) {
                    Self::lower_stmt(inner, source, func, next_label, next_temp);
                }
            }
            "if_expression" => {
                drop(Self::lower_expr(node, source, func, next_label, next_temp));
            }
            "while_expression" => {
                Self::lower_while(node, source, func, next_label, next_temp);
            }
            "loop_expression" => {
                Self::lower_loop(node, source, func, next_label, next_temp);
            }
            "for_expression" => {
                Self::lower_for(node, source, func, next_label, next_temp);
            }
            "return_expression" => {
                let value = node
                    .named_child(0)
                    .map(|child| Self::lower_expr(child, source, func, next_label, next_temp));
                func.push(IrOp::Return { value });
            }
            "block" => {
                Self::lower_block(node, source, func, next_label, next_temp);
            }
            _ => {
                // Fall through: treat as an expression, discard the result.
                drop(Self::lower_expr(node, source, func, next_label, next_temp));
            }
        }
    }

    fn lower_expr(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) -> Operand {
        // Recursion guard — expression nesting is bounded only by source size.
        crate::ensure_sufficient_stack(|| {
            at_node(func, node, |func| {
                Self::lower_expr_inner(node, source, func, next_label, next_temp)
            })
        })
    }

    fn lower_expr_inner(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) -> Operand {
        match node.kind() {
            "identifier" | "self" => Operand::var(Self::text(node, source)),
            "integer_literal" | "float_literal" | "string_literal" | "char_literal"
            | "boolean_literal" => Operand::constant(Self::text(node, source)),
            "call_expression" => Self::lower_call(node, source, func, next_label, next_temp),
            "binary_expression" => Self::lower_binop(node, source, func, next_label, next_temp),
            "field_expression" => Self::lower_field_read(node, source, func, next_label, next_temp),
            "assignment_expression" => {
                Self::lower_assignment(node, source, func, next_label, next_temp)
            }
            "if_expression" => Self::lower_if(node, source, func, next_label, next_temp),
            "block" => {
                Self::lower_block(node, source, func, next_label, next_temp);
                Operand::Const(String::from("()"))
            }
            // `&x`, `&mut x`, `(x)` and `x?` denote (or unwrap) the value of
            // `x`; points-to treats them as `x`, so `f(&x)` passes `x`.
            "reference_expression" | "parenthesized_expression" | "try_expression" => {
                let inner = node
                    .child_by_field_name("value")
                    .or_else(|| node.named_child(0));
                match inner {
                    Some(inner) => Self::lower_expr(inner, source, func, next_label, next_temp),
                    None => Operand::Const(Self::text(node, source).to_string()),
                }
            }
            _ => {
                // Unknown expression shape: surface the raw text as a constant
                // so downstream analyses still see *something*.
                Operand::Const(Self::text(node, source).to_string())
            }
        }
    }

    fn lower_call(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) -> Operand {
        let function = node.child_by_field_name("function");
        let callee = function
            .map(|n| Self::text(n, source).to_string())
            .unwrap_or_default();
        let receiver = function
            .and_then(call::method_receiver)
            .map(|recv| Self::lower_expr(recv, source, func, next_label, next_temp));

        let mut args = Vec::new();
        if let Some(arg_list) = node.child_by_field_name("arguments") {
            let mut cursor = arg_list.walk();
            for arg in arg_list.named_children(&mut cursor) {
                if !call::is_comment(arg) {
                    args.push(Self::lower_expr(arg, source, func, next_label, next_temp));
                }
            }
        }

        let dst_var = Var::new(format!("__t{}", Self::fresh_temp(next_temp)));
        func.push(IrOp::Call {
            dst: Some(dst_var.clone()),
            callee,
            receiver,
            args,
        });
        Operand::Var(dst_var)
    }

    fn lower_binop(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) -> Operand {
        let lhs = node
            .child_by_field_name("left")
            .map(|n| Self::lower_expr(n, source, func, next_label, next_temp))
            .unwrap_or(Operand::Const(String::new()));
        let rhs = node
            .child_by_field_name("right")
            .map(|n| Self::lower_expr(n, source, func, next_label, next_temp))
            .unwrap_or(Operand::Const(String::new()));
        let op_text = node
            .child_by_field_name("operator")
            .map(|n| Self::text(n, source).to_string())
            .unwrap_or_default();
        let op = BinOpKind::from_source(&op_text).unwrap_or(BinOpKind::Add);

        let dst = Var::new(format!("__t{}", Self::fresh_temp(next_temp)));
        func.push(IrOp::BinOp {
            dst: dst.clone(),
            lhs,
            op,
            rhs,
        });
        Operand::Var(dst)
    }

    fn lower_field_read(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) -> Operand {
        let base = node
            .child_by_field_name("value")
            .map(|n| Self::lower_expr(n, source, func, next_label, next_temp))
            .unwrap_or(Operand::Const(String::new()));
        let field = node
            .child_by_field_name("field")
            .map(|n| Self::text(n, source).to_string())
            .unwrap_or_default();

        let dst = Var::new(format!("__t{}", Self::fresh_temp(next_temp)));
        func.push(IrOp::FieldRead {
            dst: dst.clone(),
            base,
            field,
        });
        Operand::Var(dst)
    }

    fn lower_assignment(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) -> Operand {
        let left = node.child_by_field_name("left");
        let src = match node.child_by_field_name("right") {
            Some(r) => Self::lower_expr(r, source, func, next_label, next_temp),
            None => Operand::Const(String::new()),
        };

        let Some(left) = left else { return src };

        if left.kind() == "field_expression" {
            let base = left
                .child_by_field_name("value")
                .map(|n| Self::lower_expr(n, source, func, next_label, next_temp))
                .unwrap_or(Operand::Const(String::new()));
            let field = left
                .child_by_field_name("field")
                .map(|n| Self::text(n, source).to_string())
                .unwrap_or_default();
            func.push(IrOp::FieldWrite {
                base,
                field,
                src: src.clone(),
            });
            return src;
        }

        let dst = Var::new(Self::text(left, source));
        func.push(IrOp::Assign {
            dst,
            src: src.clone(),
        });
        src
    }

    fn lower_if(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) -> Operand {
        let cond = node
            .child_by_field_name("condition")
            .map(|n| Self::lower_expr(n, source, func, next_label, next_temp))
            .unwrap_or(Operand::Const(String::from("true")));

        let then_label = Self::fresh_label(next_label);
        let end_label = Self::fresh_label(next_label);

        func.push(IrOp::Branch {
            cond,
            target: then_label,
        });

        // Else / fall-through branch first.
        if let Some(else_clause) = node.child_by_field_name("alternative") {
            let mut cursor = else_clause.walk();
            for child in else_clause.named_children(&mut cursor) {
                Self::lower_stmt(child, source, func, next_label, next_temp);
            }
        }
        func.push(IrOp::Jump { target: end_label });

        // Then branch.
        func.push(IrOp::Label(then_label));
        if let Some(then_block) = node.child_by_field_name("consequence") {
            Self::lower_block(then_block, source, func, next_label, next_temp);
        }

        func.push(IrOp::Label(end_label));
        Operand::Const(String::from("()"))
    }

    /// `while <cond> { <body> }` → header / branch / body / jump-back / end.
    fn lower_while(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) {
        let header = Self::fresh_label(next_label);
        let body = Self::fresh_label(next_label);
        let end = Self::fresh_label(next_label);
        func.push(IrOp::Label(header));
        let cond = node
            .child_by_field_name("condition")
            .map(|n| Self::lower_expr(n, source, func, next_label, next_temp))
            .unwrap_or(Operand::Const(String::from("true")));
        func.push(IrOp::Branch { cond, target: body });
        func.push(IrOp::Jump { target: end });
        func.push(IrOp::Label(body));
        if let Some(b) = node.child_by_field_name("body") {
            Self::lower_block(b, source, func, next_label, next_temp);
        }
        func.push(IrOp::Jump { target: header });
        func.push(IrOp::Label(end));
    }

    /// Rust `loop { ... }` — unconditional back-edge.
    fn lower_loop(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) {
        let header = Self::fresh_label(next_label);
        let end = Self::fresh_label(next_label);
        func.push(IrOp::Label(header));
        if let Some(b) = node.child_by_field_name("body") {
            Self::lower_block(b, source, func, next_label, next_temp);
        }
        func.push(IrOp::Jump { target: header });
        func.push(IrOp::Label(end));
    }

    /// `for <pat> in <expr> { ... }` — iterator-call modelled opaquely.
    fn lower_for(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        next_label: &mut u32,
        next_temp: &mut usize,
    ) {
        if let Some(iter_expr) = node.child_by_field_name("value") {
            drop(Self::lower_expr(
                iter_expr, source, func, next_label, next_temp,
            ));
        }
        let pat_name = node
            .child_by_field_name("pattern")
            .map(|n| Self::text(n, source).trim().to_string())
            .unwrap_or_else(|| format!("__iter{}", Self::fresh_temp(next_temp)));
        let header = Self::fresh_label(next_label);
        let body = Self::fresh_label(next_label);
        let end = Self::fresh_label(next_label);
        func.push(IrOp::Label(header));
        func.push(IrOp::Call {
            dst: Some(Var::new(pat_name)),
            callee: "<iter::next>".into(),
            receiver: None,
            args: Vec::new(),
        });
        func.push(IrOp::Branch {
            cond: Operand::Const("true".into()),
            target: body,
        });
        func.push(IrOp::Jump { target: end });
        func.push(IrOp::Label(body));
        if let Some(b) = node.child_by_field_name("body") {
            Self::lower_block(b, source, func, next_label, next_temp);
        }
        func.push(IrOp::Jump { target: header });
        func.push(IrOp::Label(end));
    }
}

impl Default for RustIrLowering {
    fn default() -> Self {
        Self::new()
    }
}

impl IrLowering for RustIrLowering {
    fn lower_function(&self, node: Node, source: &str) -> Option<IrFunction> {
        if node.kind() != "function_item" {
            return None;
        }

        let name = node
            .child_by_field_name("name")
            .map(|n| Self::text(n, source).to_string())
            .unwrap_or_else(|| "<anon>".to_string());

        let mut func = IrFunction::new(name);
        let sig = signature::rust(node, source);
        func.receiver = sig.receiver;
        func.params = sig.params;

        let mut next_label: u32 = 0;
        let mut next_temp: usize = 0;

        if let Some(body) = node.child_by_field_name("body") {
            Self::lower_block(body, source, &mut func, &mut next_label, &mut next_temp);
        }

        Some(func)
    }
}

// ─── Python lowering ─────────────────────────────────────────────────────────

pub struct PythonIrLowering;

impl PythonIrLowering {
    pub fn new() -> Self {
        Self
    }

    fn fresh_label(next: &mut u32) -> Label {
        let l = Label(*next);
        *next += 1;
        l
    }
    fn fresh_temp(next: &mut usize) -> usize {
        let t = *next;
        *next += 1;
        t
    }
    fn text<'a>(node: Node<'a>, source: &'a str) -> &'a str {
        node.utf8_text(source.as_bytes()).unwrap_or("")
    }

    fn lower_block(block: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        let mut cursor = block.walk();
        for child in block.named_children(&mut cursor) {
            Self::lower_stmt(child, source, func, nl, nt);
        }
    }

    fn lower_stmt(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        // Recursion guard — statement nesting is bounded only by source size.
        crate::ensure_sufficient_stack(|| {
            at_node(func, node, |func| {
                Self::lower_stmt_inner(node, source, func, nl, nt)
            })
        });
    }

    fn lower_stmt_inner(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) {
        match node.kind() {
            "expression_statement" => {
                if let Some(inner) = node.named_child(0) {
                    Self::lower_stmt(inner, source, func, nl, nt);
                }
            }
            "assignment" => {
                let left = node.child_by_field_name("left");
                let right = node.child_by_field_name("right");
                if let (Some(l), Some(r)) = (left, right) {
                    let src = Self::lower_expr(r, source, func, nl, nt);
                    if l.kind() == "attribute" {
                        let base = l
                            .child_by_field_name("object")
                            .map(|n| Self::lower_expr(n, source, func, nl, nt))
                            .unwrap_or(Operand::Const(String::new()));
                        let field = l
                            .child_by_field_name("attribute")
                            .map(|n| Self::text(n, source).to_string())
                            .unwrap_or_default();
                        func.push(IrOp::FieldWrite { base, field, src });
                    } else {
                        func.push(IrOp::Assign {
                            dst: Var::new(Self::text(l, source)),
                            src,
                        });
                    }
                }
            }
            "return_statement" => {
                let value = node
                    .named_child(0)
                    .map(|c| Self::lower_expr(c, source, func, nl, nt));
                func.push(IrOp::Return { value });
            }
            "if_statement" => Self::lower_if(node, source, func, nl, nt),
            "while_statement" => Self::lower_while(node, source, func, nl, nt),
            "for_statement" => Self::lower_for(node, source, func, nl, nt),
            "block" => Self::lower_block(node, source, func, nl, nt),
            _ => {
                Self::lower_expr(node, source, func, nl, nt);
            }
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    fn lower_expr(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) -> Operand {
        // Recursion guard — expression nesting is bounded only by source size.
        crate::ensure_sufficient_stack(|| {
            at_node(func, node, |func| {
                Self::lower_expr_inner(node, source, func, nl, nt)
            })
        })
    }

    fn lower_expr_inner(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) -> Operand {
        match node.kind() {
            "identifier" => Operand::var(Self::text(node, source)),
            "integer" | "float" | "string" | "true" | "false" | "none" => {
                Operand::constant(Self::text(node, source))
            }
            "call" => {
                let function = node.child_by_field_name("function");
                let callee = function
                    .map(|n| Self::text(n, source).to_string())
                    .unwrap_or_default();
                let receiver = function
                    .and_then(call::method_receiver)
                    .map(|recv| Self::lower_expr(recv, source, func, nl, nt));
                let mut args = Vec::new();
                if let Some(al) = node.child_by_field_name("arguments") {
                    let mut c = al.walk();
                    for a in al.named_children(&mut c) {
                        if !call::is_comment(a) {
                            args.push(Self::lower_expr(a, source, func, nl, nt));
                        }
                    }
                }
                let dst = Var::new(format!("__t{}", Self::fresh_temp(nt)));
                func.push(IrOp::Call {
                    dst: Some(dst.clone()),
                    callee,
                    receiver,
                    args,
                });
                Operand::Var(dst)
            }
            "binary_operator" => {
                let lhs = node
                    .child_by_field_name("left")
                    .map(|n| Self::lower_expr(n, source, func, nl, nt))
                    .unwrap_or(Operand::Const(String::new()));
                let rhs = node
                    .child_by_field_name("right")
                    .map(|n| Self::lower_expr(n, source, func, nl, nt))
                    .unwrap_or(Operand::Const(String::new()));
                let op = node
                    .child_by_field_name("operator")
                    .and_then(|n| BinOpKind::from_source(Self::text(n, source)))
                    .unwrap_or(BinOpKind::Add);
                let dst = Var::new(format!("__t{}", Self::fresh_temp(nt)));
                func.push(IrOp::BinOp {
                    dst: dst.clone(),
                    lhs,
                    op,
                    rhs,
                });
                Operand::Var(dst)
            }
            "attribute" => {
                let base = node
                    .child_by_field_name("object")
                    .map(|n| Self::lower_expr(n, source, func, nl, nt))
                    .unwrap_or(Operand::Const(String::new()));
                let field = node
                    .child_by_field_name("attribute")
                    .map(|n| Self::text(n, source).to_string())
                    .unwrap_or_default();
                let dst = Var::new(format!("__t{}", Self::fresh_temp(nt)));
                func.push(IrOp::FieldRead {
                    dst: dst.clone(),
                    base,
                    field,
                });
                Operand::Var(dst)
            }
            _ => Operand::Const(Self::text(node, source).to_string()),
        }
    }

    fn lower_if(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        let cond = node
            .child_by_field_name("condition")
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const("True".into()));
        let then_l = Self::fresh_label(nl);
        let end_l = Self::fresh_label(nl);
        func.push(IrOp::Branch {
            cond,
            target: then_l,
        });
        if let Some(el) = node.child_by_field_name("alternative") {
            let mut c = el.walk();
            for ch in el.named_children(&mut c) {
                Self::lower_stmt(ch, source, func, nl, nt);
            }
        }
        func.push(IrOp::Jump { target: end_l });
        func.push(IrOp::Label(then_l));
        if let Some(body) = node.child_by_field_name("consequence") {
            Self::lower_block(body, source, func, nl, nt);
        }
        func.push(IrOp::Label(end_l));
    }

    fn lower_while(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        let header = Self::fresh_label(nl);
        let body_l = Self::fresh_label(nl);
        let end = Self::fresh_label(nl);
        func.push(IrOp::Label(header));
        let cond = node
            .child_by_field_name("condition")
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const("True".into()));
        func.push(IrOp::Branch {
            cond,
            target: body_l,
        });
        func.push(IrOp::Jump { target: end });
        func.push(IrOp::Label(body_l));
        if let Some(b) = node.child_by_field_name("body") {
            Self::lower_block(b, source, func, nl, nt);
        }
        func.push(IrOp::Jump { target: header });
        func.push(IrOp::Label(end));
    }

    fn lower_for(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        if let Some(iter_expr) = node.child_by_field_name("right") {
            Self::lower_expr(iter_expr, source, func, nl, nt);
        }
        let pat = node
            .child_by_field_name("left")
            .map(|n| Self::text(n, source).trim().to_string())
            .unwrap_or_else(|| format!("__iter{}", Self::fresh_temp(nt)));
        let header = Self::fresh_label(nl);
        let body_l = Self::fresh_label(nl);
        let end = Self::fresh_label(nl);
        func.push(IrOp::Label(header));
        func.push(IrOp::Call {
            dst: Some(Var::new(pat)),
            callee: "<iter::next>".into(),
            receiver: None,
            args: Vec::new(),
        });
        func.push(IrOp::Branch {
            cond: Operand::Const("true".into()),
            target: body_l,
        });
        func.push(IrOp::Jump { target: end });
        func.push(IrOp::Label(body_l));
        if let Some(b) = node.child_by_field_name("body") {
            Self::lower_block(b, source, func, nl, nt);
        }
        func.push(IrOp::Jump { target: header });
        func.push(IrOp::Label(end));
    }
}

impl Default for PythonIrLowering {
    fn default() -> Self {
        Self::new()
    }
}

impl IrLowering for PythonIrLowering {
    fn lower_function(&self, node: Node, source: &str) -> Option<IrFunction> {
        if node.kind() != "function_definition" {
            return None;
        }
        let name = node
            .child_by_field_name("name")
            .map(|n| Self::text(n, source).to_string())
            .unwrap_or_else(|| "<anon>".into());
        let mut func = IrFunction::new(name);
        let sig = signature::python(node, source);
        func.receiver = sig.receiver;
        func.params = sig.params;
        let (mut nl, mut nt) = (0u32, 0usize);
        if let Some(body) = node.child_by_field_name("body") {
            Self::lower_block(body, source, &mut func, &mut nl, &mut nt);
        }
        Some(func)
    }
}

// ─── TypeScript / JavaScript lowering ────────────────────────────────────────

pub struct TypeScriptIrLowering;

impl TypeScriptIrLowering {
    pub fn new() -> Self {
        Self
    }
    fn fresh_label(next: &mut u32) -> Label {
        let l = Label(*next);
        *next += 1;
        l
    }
    fn fresh_temp(next: &mut usize) -> usize {
        let t = *next;
        *next += 1;
        t
    }
    fn text<'a>(node: Node<'a>, source: &'a str) -> &'a str {
        node.utf8_text(source.as_bytes()).unwrap_or("")
    }

    fn lower_block(block: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        let mut cursor = block.walk();
        for child in block.named_children(&mut cursor) {
            Self::lower_stmt(child, source, func, nl, nt);
        }
    }

    fn lower_stmt(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        // Recursion guard — statement nesting is bounded only by source size.
        crate::ensure_sufficient_stack(|| {
            at_node(func, node, |func| {
                Self::lower_stmt_inner(node, source, func, nl, nt)
            })
        });
    }

    fn lower_stmt_inner(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) {
        match node.kind() {
            "lexical_declaration" | "variable_declaration" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "variable_declarator" {
                        if let Some(n) = child.child_by_field_name("name") {
                            let dst = Var::new(Self::text(n, source));
                            let src = child
                                .child_by_field_name("value")
                                .map(|v| Self::lower_expr(v, source, func, nl, nt))
                                .unwrap_or(Operand::Const("undefined".into()));
                            func.push(IrOp::Assign { dst, src });
                        }
                    }
                }
            }
            "expression_statement" => {
                if let Some(inner) = node.named_child(0) {
                    Self::lower_expr(inner, source, func, nl, nt);
                }
            }
            "return_statement" => {
                let value = node
                    .named_child(0)
                    .map(|c| Self::lower_expr(c, source, func, nl, nt));
                func.push(IrOp::Return { value });
            }
            "if_statement" => Self::lower_if(node, source, func, nl, nt),
            "while_statement" => Self::lower_while(node, source, func, nl, nt),
            "for_statement" => Self::lower_for_c(node, source, func, nl, nt),
            "for_in_statement" | "for_of_statement" => {
                Self::lower_for_in(node, source, func, nl, nt)
            }
            "statement_block" => Self::lower_block(node, source, func, nl, nt),
            _ => {
                Self::lower_expr(node, source, func, nl, nt);
            }
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    fn lower_expr(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) -> Operand {
        // Recursion guard — expression nesting is bounded only by source size.
        crate::ensure_sufficient_stack(|| {
            at_node(func, node, |func| {
                Self::lower_expr_inner(node, source, func, nl, nt)
            })
        })
    }

    fn lower_expr_inner(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) -> Operand {
        match node.kind() {
            "identifier" | "shorthand_property_identifier" | "this" => {
                Operand::var(Self::text(node, source))
            }
            "number" | "string" | "template_string" | "true" | "false" | "null" | "undefined" => {
                Operand::constant(Self::text(node, source))
            }
            "call_expression" => {
                let function = node.child_by_field_name("function");
                let callee = function
                    .map(|n| Self::text(n, source).to_string())
                    .unwrap_or_default();
                let receiver = function
                    .and_then(call::method_receiver)
                    .map(|recv| Self::lower_expr(recv, source, func, nl, nt));
                let mut args = Vec::new();
                if let Some(al) = node.child_by_field_name("arguments") {
                    let mut c = al.walk();
                    for a in al.named_children(&mut c) {
                        if !call::is_comment(a) {
                            args.push(Self::lower_expr(a, source, func, nl, nt));
                        }
                    }
                }
                let dst = Var::new(format!("__t{}", Self::fresh_temp(nt)));
                func.push(IrOp::Call {
                    dst: Some(dst.clone()),
                    callee,
                    receiver,
                    args,
                });
                Operand::Var(dst)
            }
            "binary_expression" => {
                let lhs = node
                    .child_by_field_name("left")
                    .map(|n| Self::lower_expr(n, source, func, nl, nt))
                    .unwrap_or(Operand::Const(String::new()));
                let rhs = node
                    .child_by_field_name("right")
                    .map(|n| Self::lower_expr(n, source, func, nl, nt))
                    .unwrap_or(Operand::Const(String::new()));
                let op = node
                    .child_by_field_name("operator")
                    .and_then(|n| BinOpKind::from_source(Self::text(n, source)))
                    .unwrap_or(BinOpKind::Add);
                let dst = Var::new(format!("__t{}", Self::fresh_temp(nt)));
                func.push(IrOp::BinOp {
                    dst: dst.clone(),
                    lhs,
                    op,
                    rhs,
                });
                Operand::Var(dst)
            }
            "assignment_expression" => {
                let left = node.child_by_field_name("left");
                let src = node
                    .child_by_field_name("right")
                    .map(|r| Self::lower_expr(r, source, func, nl, nt))
                    .unwrap_or(Operand::Const(String::new()));
                if let Some(l) = left {
                    if l.kind() == "member_expression" {
                        let base = l
                            .child_by_field_name("object")
                            .map(|n| Self::lower_expr(n, source, func, nl, nt))
                            .unwrap_or(Operand::Const(String::new()));
                        let field = l
                            .child_by_field_name("property")
                            .map(|n| Self::text(n, source).to_string())
                            .unwrap_or_default();
                        func.push(IrOp::FieldWrite {
                            base,
                            field,
                            src: src.clone(),
                        });
                    } else {
                        func.push(IrOp::Assign {
                            dst: Var::new(Self::text(l, source)),
                            src: src.clone(),
                        });
                    }
                }
                src
            }
            "member_expression" => {
                let base = node
                    .child_by_field_name("object")
                    .map(|n| Self::lower_expr(n, source, func, nl, nt))
                    .unwrap_or(Operand::Const(String::new()));
                let field = node
                    .child_by_field_name("property")
                    .map(|n| Self::text(n, source).to_string())
                    .unwrap_or_default();
                let dst = Var::new(format!("__t{}", Self::fresh_temp(nt)));
                func.push(IrOp::FieldRead {
                    dst: dst.clone(),
                    base,
                    field,
                });
                Operand::Var(dst)
            }
            _ => Operand::Const(Self::text(node, source).to_string()),
        }
    }

    fn lower_if(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        let cond = node
            .child_by_field_name("condition")
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const("true".into()));
        let then_l = Self::fresh_label(nl);
        let end_l = Self::fresh_label(nl);
        func.push(IrOp::Branch {
            cond,
            target: then_l,
        });
        if let Some(el) = node.child_by_field_name("alternative") {
            Self::lower_stmt(el, source, func, nl, nt);
        }
        func.push(IrOp::Jump { target: end_l });
        func.push(IrOp::Label(then_l));
        if let Some(b) = node.child_by_field_name("consequence") {
            Self::lower_stmt(b, source, func, nl, nt);
        }
        func.push(IrOp::Label(end_l));
    }

    fn lower_while(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        let header = Self::fresh_label(nl);
        let body_l = Self::fresh_label(nl);
        let end = Self::fresh_label(nl);
        func.push(IrOp::Label(header));
        let cond = node
            .child_by_field_name("condition")
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const("true".into()));
        func.push(IrOp::Branch {
            cond,
            target: body_l,
        });
        func.push(IrOp::Jump { target: end });
        func.push(IrOp::Label(body_l));
        if let Some(b) = node.child_by_field_name("body") {
            Self::lower_stmt(b, source, func, nl, nt);
        }
        func.push(IrOp::Jump { target: header });
        func.push(IrOp::Label(end));
    }

    fn lower_for_c(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        if let Some(init) = node.child_by_field_name("initializer") {
            Self::lower_stmt(init, source, func, nl, nt);
        }
        let header = Self::fresh_label(nl);
        let body_l = Self::fresh_label(nl);
        let end = Self::fresh_label(nl);
        func.push(IrOp::Label(header));
        let cond = node
            .child_by_field_name("condition")
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const("true".into()));
        func.push(IrOp::Branch {
            cond,
            target: body_l,
        });
        func.push(IrOp::Jump { target: end });
        func.push(IrOp::Label(body_l));
        if let Some(b) = node.child_by_field_name("body") {
            Self::lower_stmt(b, source, func, nl, nt);
        }
        if let Some(upd) = node.child_by_field_name("increment") {
            Self::lower_expr(upd, source, func, nl, nt);
        }
        func.push(IrOp::Jump { target: header });
        func.push(IrOp::Label(end));
    }

    fn lower_for_in(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        if let Some(iter_expr) = node.child_by_field_name("right") {
            Self::lower_expr(iter_expr, source, func, nl, nt);
        }
        let pat = node
            .child_by_field_name("left")
            .map(|n| Self::text(n, source).trim().to_string())
            .unwrap_or_else(|| format!("__iter{}", Self::fresh_temp(nt)));
        let header = Self::fresh_label(nl);
        let body_l = Self::fresh_label(nl);
        let end = Self::fresh_label(nl);
        func.push(IrOp::Label(header));
        func.push(IrOp::Call {
            dst: Some(Var::new(pat)),
            callee: "<iter::next>".into(),
            receiver: None,
            args: Vec::new(),
        });
        func.push(IrOp::Branch {
            cond: Operand::Const("true".into()),
            target: body_l,
        });
        func.push(IrOp::Jump { target: end });
        func.push(IrOp::Label(body_l));
        if let Some(b) = node.child_by_field_name("body") {
            Self::lower_stmt(b, source, func, nl, nt);
        }
        func.push(IrOp::Jump { target: header });
        func.push(IrOp::Label(end));
    }
}

impl Default for TypeScriptIrLowering {
    fn default() -> Self {
        Self::new()
    }
}

impl IrLowering for TypeScriptIrLowering {
    fn lower_function(&self, node: Node, source: &str) -> Option<IrFunction> {
        if !matches!(
            node.kind(),
            "function_declaration"
                | "function"
                | "arrow_function"
                | "method_definition"
                | "generator_function_declaration"
        ) {
            return None;
        }
        let name = node
            .child_by_field_name("name")
            .map(|n| Self::text(n, source).to_string())
            .unwrap_or_else(|| "<anon>".into());
        let mut func = IrFunction::new(name);
        let sig = signature::typescript(node, source);
        func.receiver = sig.receiver;
        func.params = sig.params;
        let (mut nl, mut nt) = (0u32, 0usize);
        if let Some(body) = node.child_by_field_name("body") {
            if body.kind() == "statement_block" {
                Self::lower_block(body, source, &mut func, &mut nl, &mut nt);
            } else {
                let val = Self::lower_expr(body, source, &mut func, &mut nl, &mut nt);
                func.push(IrOp::Return { value: Some(val) });
            }
        }
        Some(func)
    }
}

// ─── Go lowering ─────────────────────────────────────────────────────────────

pub struct GoIrLowering;

impl GoIrLowering {
    pub fn new() -> Self {
        Self
    }
    fn fresh_label(next: &mut u32) -> Label {
        let l = Label(*next);
        *next += 1;
        l
    }
    fn fresh_temp(next: &mut usize) -> usize {
        let t = *next;
        *next += 1;
        t
    }
    fn text<'a>(node: Node<'a>, source: &'a str) -> &'a str {
        node.utf8_text(source.as_bytes()).unwrap_or("")
    }

    fn lower_block(block: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        // Go blocks contain a `statement_list` wrapper; descend into it.
        let stmts = block
            .named_children(&mut block.walk())
            .find(|c| c.kind() == "statement_list")
            .unwrap_or(block);
        let mut cursor = stmts.walk();
        for child in stmts.named_children(&mut cursor) {
            Self::lower_stmt(child, source, func, nl, nt);
        }
    }

    fn lower_stmt(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        // Recursion guard — statement nesting is bounded only by source size.
        crate::ensure_sufficient_stack(|| {
            at_node(func, node, |func| {
                Self::lower_stmt_inner(node, source, func, nl, nt)
            })
        });
    }

    fn lower_stmt_inner(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) {
        match node.kind() {
            "short_var_declaration" => Self::lower_short_var(node, source, func, nl, nt),
            "assignment_statement" => Self::lower_assign(node, source, func, nl, nt),
            "return_statement" => Self::lower_return(node, source, func, nl, nt),
            "if_statement" => Self::lower_if(node, source, func, nl, nt),
            "for_statement" => Self::lower_for(node, source, func, nl, nt),
            "inc_statement" | "dec_statement" => {
                // `i++` / `i--` — treat as assign i = i +/- 1
                if let Some(operand) = node.named_child(0) {
                    let name = Self::text(operand, source).to_string();
                    let op = if node.kind() == "inc_statement" {
                        BinOpKind::Add
                    } else {
                        BinOpKind::Sub
                    };
                    let dst = Var::new(format!("__t{}", Self::fresh_temp(nt)));
                    func.push(IrOp::BinOp {
                        dst: dst.clone(),
                        lhs: Operand::var(&name),
                        op,
                        rhs: Operand::constant("1"),
                    });
                    func.push(IrOp::Assign {
                        dst: Var::new(name),
                        src: Operand::Var(dst),
                    });
                }
            }
            "expression_statement" => {
                if let Some(inner) = node.named_child(0) {
                    drop(Self::lower_expr(inner, source, func, nl, nt));
                }
            }
            "block" => Self::lower_block(node, source, func, nl, nt),
            _ => {
                drop(Self::lower_expr(node, source, func, nl, nt));
            }
        }
    }

    fn lower_short_var(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) {
        // `left` and `right` are `expression_list` wrappers.
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");
        let dst_name = left
            .and_then(|n| n.named_child(0))
            .map(|n| Self::text(n, source).to_string())
            .unwrap_or_default();
        let src = right
            .and_then(|n| n.named_child(0))
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const(String::new()));
        func.push(IrOp::Assign {
            dst: Var::new(dst_name),
            src,
        });
    }

    fn lower_assign(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        // `left` and `right` are `expression_list` wrappers.
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");
        let src = right
            .and_then(|n| n.named_child(0))
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const(String::new()));
        let Some(l) = left.and_then(|n| n.named_child(0)) else {
            return;
        };
        if l.kind() == "selector_expression" {
            let base = l
                .child_by_field_name("operand")
                .map(|n| Self::lower_expr(n, source, func, nl, nt))
                .unwrap_or(Operand::Const(String::new()));
            let field = l
                .child_by_field_name("field")
                .map(|n| Self::text(n, source).to_string())
                .unwrap_or_default();
            func.push(IrOp::FieldWrite { base, field, src });
        } else {
            func.push(IrOp::Assign {
                dst: Var::new(Self::text(l, source)),
                src,
            });
        }
    }

    fn lower_return(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        // Go return: `return expr_list?` — the expression_list holds the value(s).
        let value = node
            .named_child(0)
            .and_then(|list| {
                if list.kind() == "expression_list" {
                    list.named_child(0)
                } else {
                    Some(list)
                }
            })
            .map(|c| Self::lower_expr(c, source, func, nl, nt));
        func.push(IrOp::Return { value });
    }

    fn lower_if(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        let cond = node
            .child_by_field_name("condition")
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const("true".into()));
        let then_l = Self::fresh_label(nl);
        let end_l = Self::fresh_label(nl);
        func.push(IrOp::Branch {
            cond,
            target: then_l,
        });
        if let Some(el) = node.child_by_field_name("alternative") {
            Self::lower_block(el, source, func, nl, nt);
        }
        func.push(IrOp::Jump { target: end_l });
        func.push(IrOp::Label(then_l));
        if let Some(b) = node.child_by_field_name("consequence") {
            Self::lower_block(b, source, func, nl, nt);
        }
        func.push(IrOp::Label(end_l));
    }

    fn lower_for(node: Node, source: &str, func: &mut IrFunction, nl: &mut u32, nt: &mut usize) {
        // Go for-statement wraps init/cond/update in a `for_clause` child.
        let clause = node
            .named_children(&mut node.walk())
            .find(|c| c.kind() == "for_clause");
        if let Some(init) = clause.and_then(|c| c.child_by_field_name("initializer")) {
            Self::lower_stmt(init, source, func, nl, nt);
        }
        let header = Self::fresh_label(nl);
        let body_l = Self::fresh_label(nl);
        let end = Self::fresh_label(nl);
        func.push(IrOp::Label(header));
        let cond = clause
            .and_then(|c| c.child_by_field_name("condition"))
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const("true".into()));
        func.push(IrOp::Branch {
            cond,
            target: body_l,
        });
        func.push(IrOp::Jump { target: end });
        func.push(IrOp::Label(body_l));
        if let Some(b) = node.child_by_field_name("body") {
            Self::lower_block(b, source, func, nl, nt);
        }
        if let Some(upd) = clause.and_then(|c| c.child_by_field_name("update")) {
            Self::lower_stmt(upd, source, func, nl, nt);
        }
        func.push(IrOp::Jump { target: header });
        func.push(IrOp::Label(end));
    }

    fn lower_expr(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) -> Operand {
        // Recursion guard — expression nesting is bounded only by source size.
        crate::ensure_sufficient_stack(|| {
            at_node(func, node, |func| {
                Self::lower_expr_inner(node, source, func, nl, nt)
            })
        })
    }

    fn lower_expr_inner(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) -> Operand {
        match node.kind() {
            "identifier" => Operand::var(Self::text(node, source)),
            "int_literal"
            | "float_literal"
            | "rune_literal"
            | "raw_string_literal"
            | "interpreted_string_literal"
            | "true"
            | "false" => Operand::constant(Self::text(node, source)),
            "call_expression" => Self::lower_call(node, source, func, nl, nt),
            "binary_expression" => Self::lower_binop(node, source, func, nl, nt),
            "selector_expression" => Self::lower_selector(node, source, func, nl, nt),
            "parenthesized_expression" => node
                .named_child(0)
                .map(|n| Self::lower_expr(n, source, func, nl, nt))
                .unwrap_or(Operand::Const(String::new())),
            _ => Operand::Const(Self::text(node, source).to_string()),
        }
    }

    fn lower_call(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) -> Operand {
        let function = node.child_by_field_name("function");
        let callee = function
            .map(|n| Self::text(n, source).to_string())
            .unwrap_or_default();
        let receiver = function
            .and_then(call::method_receiver)
            .map(|recv| Self::lower_expr(recv, source, func, nl, nt));
        let mut args = Vec::new();
        if let Some(al) = node.child_by_field_name("arguments") {
            let mut c = al.walk();
            for a in al.named_children(&mut c) {
                if !call::is_comment(a) {
                    args.push(Self::lower_expr(a, source, func, nl, nt));
                }
            }
        }
        let dst = Var::new(format!("__t{}", Self::fresh_temp(nt)));
        func.push(IrOp::Call {
            dst: Some(dst.clone()),
            callee,
            receiver,
            args,
        });
        Operand::Var(dst)
    }

    fn lower_binop(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) -> Operand {
        let lhs = node
            .child_by_field_name("left")
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const(String::new()));
        let rhs = node
            .child_by_field_name("right")
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const(String::new()));
        let op = node
            .child_by_field_name("operator")
            .and_then(|n| BinOpKind::from_source(Self::text(n, source)))
            .unwrap_or(BinOpKind::Add);
        let dst = Var::new(format!("__t{}", Self::fresh_temp(nt)));
        func.push(IrOp::BinOp {
            dst: dst.clone(),
            lhs,
            op,
            rhs,
        });
        Operand::Var(dst)
    }

    fn lower_selector(
        node: Node,
        source: &str,
        func: &mut IrFunction,
        nl: &mut u32,
        nt: &mut usize,
    ) -> Operand {
        let base = node
            .child_by_field_name("operand")
            .map(|n| Self::lower_expr(n, source, func, nl, nt))
            .unwrap_or(Operand::Const(String::new()));
        let field = node
            .child_by_field_name("field")
            .map(|n| Self::text(n, source).to_string())
            .unwrap_or_default();
        let dst = Var::new(format!("__t{}", Self::fresh_temp(nt)));
        func.push(IrOp::FieldRead {
            dst: dst.clone(),
            base,
            field,
        });
        Operand::Var(dst)
    }
}

impl Default for GoIrLowering {
    fn default() -> Self {
        Self::new()
    }
}

impl IrLowering for GoIrLowering {
    fn lower_function(&self, node: Node, source: &str) -> Option<IrFunction> {
        if !matches!(node.kind(), "function_declaration" | "method_declaration") {
            return None;
        }
        let name = node
            .child_by_field_name("name")
            .map(|n| Self::text(n, source).to_string())
            .unwrap_or_else(|| "<anon>".into());
        let mut func = IrFunction::new(name);
        let sig = signature::go(node, source);
        func.receiver = sig.receiver;
        func.params = sig.params;
        let (mut nl, mut nt) = (0u32, 0usize);
        if let Some(body) = node.child_by_field_name("body") {
            Self::lower_block(body, source, &mut func, &mut nl, &mut nt);
        }
        Some(func)
    }
}

// ─── Dispatcher ──────────────────────────────────────────────────────────────

/// Lower a tree-sitter function node using the appropriate language driver.
pub fn lower_for_language(lang_id: &str, node: Node, source: &str) -> Option<IrFunction> {
    match lang_id {
        "rust" => RustIrLowering::new().lower_function(node, source),
        "python" => PythonIrLowering::new().lower_function(node, source),
        "typescript" | "javascript" | "arkts" => {
            TypeScriptIrLowering::new().lower_function(node, source)
        }
        "go" => GoIrLowering::new().lower_function(node, source),
        // Java, C, C++, PHP: the rules-driven lowering (`ir_rules.rs` +
        // `cfg_rules.rs`).
        _ => super::lower_with_rules(lang_id, node, source),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use tree_sitter::Parser;

    use super::*;

    fn parse_rust(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        p.parse(src, None).expect("parse failed")
    }

    fn find_function<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == "function_item" {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(f) = find_function(child) {
                return Some(f);
            }
        }
        None
    }

    #[test]
    fn lowers_simple_rust_function() {
        let src = r#"
            fn add_one(x: i32) -> i32 {
                let y = x + 1;
                return y;
            }
        "#;
        let tree = parse_rust(src);
        let func_node = find_function(tree.root_node()).expect("found fn");

        let ir = RustIrLowering::new()
            .lower_function(func_node, src)
            .expect("lowered");

        assert_eq!(ir.name, "add_one");
        assert_eq!(ir.params.len(), 1, "expected 1 param, got {:?}", ir.params);

        // We expect at least: a BinOp for `x + 1`, an Assign into `y`, and a Return.
        let has_binop = ir.body.iter().any(|op| {
            matches!(
                op,
                IrOp::BinOp {
                    op: BinOpKind::Add,
                    ..
                }
            )
        });
        let has_assign_y = ir.body.iter().any(|op| match op {
            IrOp::Assign { dst, .. } => dst.as_str() == "y",
            _ => false,
        });
        let has_return = ir
            .body
            .iter()
            .any(|op| matches!(op, IrOp::Return { value: Some(_) }));

        assert!(has_binop, "missing BinOp::Add in {:#?}", ir.body);
        assert!(has_assign_y, "missing Assign to y in {:#?}", ir.body);
        assert!(has_return, "missing Return in {:#?}", ir.body);
    }

    #[test]
    fn binop_from_source_roundtrip() {
        assert_eq!(BinOpKind::from_source("+"), Some(BinOpKind::Add));
        assert_eq!(BinOpKind::from_source("=="), Some(BinOpKind::Eq));
        assert_eq!(BinOpKind::from_source("&&"), Some(BinOpKind::And));
        assert_eq!(BinOpKind::from_source("???"), None);
    }

    #[test]
    fn label_index_is_populated() {
        let mut f = IrFunction::new("t");
        f.push(IrOp::Label(Label(0)));
        f.push(IrOp::Nop);
        f.push(IrOp::Label(Label(1)));
        assert_eq!(f.labels.get(&Label(0)), Some(&0));
        assert_eq!(f.labels.get(&Label(1)), Some(&2));
    }

    #[test]
    fn rust_while_loop_emits_back_edge() {
        let src = r#"
            fn count() {
                let mut i = 0;
                while i < 10 {
                    i = i + 1;
                }
            }
        "#;
        let tree = parse_rust(src);
        let func_node = find_function(tree.root_node()).expect("found fn");
        let ir = RustIrLowering::new()
            .lower_function(func_node, src)
            .expect("lowered");

        let label_positions: Vec<Label> = ir
            .body
            .iter()
            .filter_map(|op| {
                if let IrOp::Label(l) = op {
                    Some(*l)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            label_positions.len() >= 3,
            "while loop should emit 3+ labels: {:?}",
            label_positions
        );

        let has_back_edge = ir
            .body
            .iter()
            .any(|op| matches!(op, IrOp::Jump { target } if *target == label_positions[0]));
        assert!(has_back_edge, "expected a Jump back to the header label");
    }

    fn parse_python(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        p.parse(src, None).expect("parse failed")
    }

    fn find_python_function<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == "function_definition" {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(f) = find_python_function(child) {
                return Some(f);
            }
        }
        None
    }

    #[test]
    fn lowers_python_function() {
        let src = "\
def greet(name):
    msg = name + '!'
    return msg
";
        let tree = parse_python(src);
        let f = find_python_function(tree.root_node()).expect("py fn");
        let ir = PythonIrLowering::new()
            .lower_function(f, src)
            .expect("lowered");

        assert_eq!(ir.name, "greet");
        assert_eq!(ir.params.len(), 1);
        let has_return = ir
            .body
            .iter()
            .any(|op| matches!(op, IrOp::Return { value: Some(_) }));
        assert!(has_return, "missing Return in {:#?}", ir.body);
    }

    #[test]
    fn python_field_write_through_attribute_assignment() {
        let src = "\
def set_name(obj, n):
    obj.name = n
";
        let tree = parse_python(src);
        let f = find_python_function(tree.root_node()).expect("py fn");
        let ir = PythonIrLowering::new()
            .lower_function(f, src)
            .expect("lowered");
        let has_fw = ir.body.iter().any(|op| match op {
            IrOp::FieldWrite { field, .. } => field == "name",
            _ => false,
        });
        assert!(
            has_fw,
            "missing FieldWrite for obj.name = n: {:#?}",
            ir.body
        );
    }

    fn parse_ts(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        p.parse(src, None).expect("parse failed")
    }

    fn find_ts_function<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
        if matches!(node.kind(), "function_declaration" | "method_definition") {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(f) = find_ts_function(child) {
                return Some(f);
            }
        }
        None
    }

    #[test]
    fn lowers_typescript_function() {
        let src = r#"
            function add(a: number, b: number): number {
                const c = a + b;
                return c;
            }
        "#;
        let tree = parse_ts(src);
        let f = find_ts_function(tree.root_node()).expect("ts fn");
        let ir = TypeScriptIrLowering::new()
            .lower_function(f, src)
            .expect("lowered");

        assert_eq!(ir.name, "add");
        assert_eq!(ir.params.len(), 2);
        let has_assign_c = ir.body.iter().any(|op| match op {
            IrOp::Assign { dst, .. } => dst.as_str() == "c",
            _ => false,
        });
        assert!(has_assign_c, "missing Assign to c in {:#?}", ir.body);
    }

    #[test]
    fn dispatcher_routes_by_language() {
        let src = r#"
            fn foo() -> i32 { return 42; }
        "#;
        let tree = parse_rust(src);
        let f = find_function(tree.root_node()).expect("rs fn");
        let ir = lower_for_language("rust", f, src).expect("rust route");
        assert_eq!(ir.name, "foo");

        assert!(lower_for_language("nope", f, src).is_none());
    }

    // ─── Go tests ────────────────────────────────────────────────────────────

    fn parse_go(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_go::LANGUAGE.into()).unwrap();
        p.parse(src, None).expect("parse failed")
    }

    fn find_go_function<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
        if matches!(node.kind(), "function_declaration" | "method_declaration") {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(f) = find_go_function(child) {
                return Some(f);
            }
        }
        None
    }

    #[test]
    fn lowers_go_function() {
        let src = r#"
package main

func add(a int, b int) int {
    c := a + b
    return c
}
"#;
        let tree = parse_go(src);
        let f = find_go_function(tree.root_node()).expect("go fn");
        let ir = GoIrLowering::new().lower_function(f, src).expect("lowered");

        assert_eq!(ir.name, "add");
        assert_eq!(ir.params.len(), 2, "expected 2 params, got {:?}", ir.params);

        let has_binop = ir.body.iter().any(|op| {
            matches!(
                op,
                IrOp::BinOp {
                    op: BinOpKind::Add,
                    ..
                }
            )
        });
        let has_assign_c = ir.body.iter().any(|op| match op {
            IrOp::Assign { dst, .. } => dst.as_str() == "c",
            _ => false,
        });
        let has_return = ir
            .body
            .iter()
            .any(|op| matches!(op, IrOp::Return { value: Some(_) }));

        assert!(has_binop, "missing BinOp::Add in {:#?}", ir.body);
        assert!(has_assign_c, "missing Assign to c in {:#?}", ir.body);
        assert!(has_return, "missing Return in {:#?}", ir.body);
    }

    #[test]
    fn go_for_loop_emits_back_edge() {
        let src = r#"
package main

func count() {
    for i := 0; i < 10; i++ {
    }
}
"#;
        let tree = parse_go(src);
        let f = find_go_function(tree.root_node()).expect("go fn");
        let ir = GoIrLowering::new().lower_function(f, src).expect("lowered");

        let label_positions: Vec<Label> = ir
            .body
            .iter()
            .filter_map(|op| {
                if let IrOp::Label(l) = op {
                    Some(*l)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            label_positions.len() >= 3,
            "for loop should emit 3+ labels: {:?}",
            label_positions
        );

        let has_back_edge = ir
            .body
            .iter()
            .any(|op| matches!(op, IrOp::Jump { target } if *target == label_positions[0]));
        assert!(has_back_edge, "expected a Jump back to the header label");
    }

    #[test]
    fn go_dispatcher_routes() {
        let src = r#"
package main

func hello() { return }
"#;
        let tree = parse_go(src);
        let f = find_go_function(tree.root_node()).expect("go fn");
        let ir = lower_for_language("go", f, src).expect("go route");
        assert_eq!(ir.name, "hello");
    }
}
