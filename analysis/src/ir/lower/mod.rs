//! The rules-driven lowering: one walker for every language that has both
//! an [`IrRules`] table (expressions, declarations, calls, accesses) and a
//! [`CfgRules`] table (control flow). It records, besides the ops and their
//! spans, the value of every expression it lowers ([`ExprValue`]) and the
//! places each call's receiver and arguments designate ([`CallPlaces`]), so
//! an analysis can map syntax (a rule's captures) onto the IR.
//!
//! Unknown syntax is never dropped silently: an expression kind the tables
//! do not name becomes a synthetic call `<kind>(children…)`, whose result
//! carries whatever its children carry.

mod expr;
mod stmt;

use std::collections::HashMap;

use tree_sitter::Node;

use super::model::{CallPlaces, ExprValue, IrFunction, IrOp, Label, Operand, Place, Span, Var};
use crate::cfg_rules::CfgRules;
use crate::ir_rules::{IrRules, Slot};

/// Lower `node` (a function-like node with a body, or a file's root: its
/// top-level code) of language `lang` with the rules tables, or `None`
/// when a table is missing or it has no body.
pub fn lower_with_rules(lang: &str, node: Node<'_>, source: &str) -> Option<IrFunction> {
    lower_with_macros(lang, node, source, &HashMap::new())
}

/// [`lower_with_rules`], reading a name in `macros` (from
/// [`macro_aliases`] of the function's file) as what the macro stands for.
pub fn lower_with_macros(
    lang: &str,
    node: Node<'_>,
    source: &str,
    macros: &HashMap<String, String>,
) -> Option<IrFunction> {
    let rules = IrRules::for_language(lang)?;
    let cfg = CfgRules::for_language(lang)?;
    // A script's top-level code (PHP, Python, JS) is the root's statements.
    let top_level = node.parent().is_none();
    let body = if top_level {
        node
    } else {
        node.child_by_field_name(cfg.body_field)?
    };
    let mut lowerer = Lowerer {
        rules,
        cfg,
        source,
        func: IrFunction::new(function_name(node, source)),
        next_label: 0,
        next_temp: 0,
        breaks: Vec::new(),
        continues: Vec::new(),
        macros,
    };
    lowerer.params(node);
    let exit = lowerer.fresh_label();
    lowerer.func.set_span(Span::of(node));
    if top_level {
        for child in lowerer.named_children(body) {
            lowerer.stmt(child);
        }
    } else if cfg.classify(body.kind()) == crate::cfg_rules::Construct::Block {
        lowerer.stmt(body);
    } else {
        // An expression body (`x => x + 1`).
        let value = lowerer.expr(body);
        lowerer.func.push(IrOp::Return {
            value: Some(value.operand),
        });
    }
    lowerer.func.set_span(Span::of(node));
    lowerer.func.push(IrOp::Label(exit));
    Some(lowerer.func)
}

/// The object-like macros of a file (walked once, from its `root`) whose
/// body is one name or literal, by name. A macro defined twice with
/// different bodies (`#ifdef` branches) stands for nothing.
pub fn macro_aliases(lang: &str, root: Node<'_>, source: &str) -> HashMap<String, String> {
    let Some(rules) = IrRules::for_language(lang) else {
        return HashMap::new();
    };
    if rules.macro_definitions.is_empty() {
        return HashMap::new();
    }
    let mut aliases: HashMap<String, Option<String>> = HashMap::new();
    // Iterative walk: depth is bounded by the input.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if rules.macro_definitions.contains(&node.kind()) {
            let name = node.child_by_field_name("name").map(|n| text(n, source));
            let value = node
                .child_by_field_name("value")
                .map(|n| text(n, source).trim());
            if let (Some(name), Some(value)) = (name, value) {
                let simple = !value.is_empty()
                    && (value.chars().all(|c| c.is_alphanumeric() || c == '_')
                        || (value.starts_with('"')
                            && value.ends_with('"')
                            && value.len() >= 2
                            && !value[1..value.len() - 1].contains('"')));
                let entry = aliases
                    .entry(name.to_string())
                    .or_insert_with(|| simple.then(|| value.to_string()));
                if entry.as_deref() != Some(value) {
                    *entry = None;
                }
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    aliases
        .into_iter()
        .filter_map(|(name, value)| Some((name, value?)))
        .collect()
}

/// The name a function node declares: its `name` field, or the identifier
/// inside its declarator (C/C++).
fn function_name(node: Node<'_>, source: &str) -> String {
    let mut current = node;
    for _ in 0..8 {
        if let Some(name) = current.child_by_field_name("name") {
            return text(name, source).to_string();
        }
        match current.child_by_field_name("declarator") {
            Some(declarator) => current = declarator,
            None => break,
        }
        if current.child_count() == 0 {
            return text(current, source).to_string();
        }
    }
    "<anon>".to_string()
}

/// Whether a parameter declares a reference (C++ `int &data`, `char *&p`):
/// a `reference_declarator` between the parameter and its name.
fn is_reference(param: Node<'_>) -> bool {
    let mut current = param;
    for _ in 0..8 {
        if current.kind() == "reference_declarator" {
            return true;
        }
        match current.child_by_field_name("declarator") {
            Some(next) => current = next,
            None => {
                let mut cursor = current.walk();
                return current
                    .named_children(&mut cursor)
                    .any(|child| child.kind() == "reference_declarator");
            }
        }
    }
    false
}

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or_default()
}

/// Whitespace removed, at most the last `MAX_CALLEE` bytes (the method name
/// is at the end).
fn compact(text: &str) -> String {
    const MAX_CALLEE: usize = 160;
    let flat: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if flat.len() <= MAX_CALLEE {
        return flat;
    }
    let mut start = flat.len() - MAX_CALLEE;
    while !flat.is_char_boundary(start) {
        start += 1;
    }
    flat[start..].to_string()
}

/// A lowered expression: its operand, and the storage it designates.
#[derive(Debug, Clone)]
pub(super) struct Value {
    operand: Operand,
    place: Option<Place>,
}

impl Value {
    fn constant(text: impl Into<String>) -> Self {
        Self {
            operand: Operand::Const(text.into()),
            place: None,
        }
    }

    fn of(operand: Operand) -> Self {
        Self {
            operand,
            place: None,
        }
    }
}

struct Lowerer<'r, 's> {
    rules: &'r IrRules,
    cfg: &'r CfgRules,
    source: &'s str,
    func: IrFunction,
    next_label: u32,
    next_temp: usize,
    /// Where `break` goes: the innermost loop or switch end.
    breaks: Vec<Label>,
    /// Where `continue` goes: the innermost loop's next iteration.
    continues: Vec<Label>,
    /// Object-like macros read as what they stand for.
    macros: &'r HashMap<String, String>,
}

impl<'s> Lowerer<'_, 's> {
    fn fresh_label(&mut self) -> Label {
        let label = Label(self.next_label);
        self.next_label += 1;
        label
    }

    fn fresh_temp(&mut self) -> Var {
        let var = Var::new(format!("__t{}", self.next_temp));
        self.next_temp += 1;
        var
    }

    fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.source)
    }

    fn slot<'t>(&self, node: Node<'t>, slot: Slot) -> Option<Node<'t>> {
        match slot {
            Slot::Field(name) => node.child_by_field_name(name),
            Slot::Child(index) => self.named_children(node).into_iter().nth(index),
        }
    }

    /// Named children that carry code (no comments or ignored kinds).
    fn named_children<'t>(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .filter(|child| !child.is_extra() && !self.rules.ignored.contains(&child.kind()))
            .collect()
    }

    fn is_identifier(&self, node: Node<'_>) -> bool {
        self.rules.identifiers.contains(&node.kind())
    }

    /// The identifier a declarator or parameter declares: the node itself,
    /// else down the first of `fields` present (through `*p`, `a[10]`,
    /// `x = default`), else its first identifier child.
    fn declared_name<'t>(&self, node: Node<'t>, fields: &[&str]) -> Option<Node<'t>> {
        let mut current = node;
        for _ in 0..8 {
            if self.is_identifier(current) {
                return Some(current);
            }
            match fields
                .iter()
                .find_map(|field| current.child_by_field_name(field))
            {
                Some(next) => current = next,
                None => {
                    let children = self.named_children(current);
                    if let Some(name) = children.iter().find(|child| self.is_identifier(**child)) {
                        return Some(*name);
                    }
                    // `(*f)` in `void (*f)(char *)`: a declarator without
                    // a field around the one that names it.
                    current = *children
                        .iter()
                        .find(|child| child.kind().ends_with("_declarator"))?;
                }
            }
        }
        None
    }

    fn params(&mut self, node: Node<'_>) {
        // C/C++ keep the list on the function's declarator.
        let mut owner = node;
        for _ in 0..8 {
            if self
                .rules
                .parameter_lists
                .iter()
                .any(|field| owner.child_by_field_name(field).is_some())
            {
                break;
            }
            match owner.child_by_field_name("declarator") {
                Some(declarator) => owner = declarator,
                None => break,
            }
        }
        for field in self.rules.parameter_lists {
            let Some(list) = owner.child_by_field_name(field) else {
                continue;
            };
            let params: Vec<Node<'_>> = if self.is_identifier(list) {
                // A lone arrow-function parameter (`x => …`).
                vec![list]
            } else {
                self.named_children(list)
            };
            for param in params {
                if self.rules.receiver_parameters.contains(&param.kind()) {
                    continue;
                }
                if let Some(name) = self.declared_name(param, self.rules.parameter_name) {
                    if is_reference(param) {
                        self.func.reference_params.push(self.func.params.len());
                    }
                    self.func.params.push(Var::new(self.text(name)));
                    self.func.param_spans.push(Span::of(name));
                }
            }
        }
    }

    /// Record the identifier `name` as a name the function declares.
    fn declare(&mut self, name: Node<'_>) {
        let var = Var::new(self.text(name));
        if !self.func.locals.contains(&var) {
            self.func.locals.push(var);
        }
    }

    /// Declare every identifier in a binding (`x`, `(a, b)`, a `catch`
    /// clause's `Exception e`): the identifiers of its named descendants,
    /// types left out. Iterative: depth is bounded by the input.
    fn declare_all(&mut self, binding: Node<'_>) {
        let mut stack = vec![binding];
        while let Some(node) = stack.pop() {
            if self.is_identifier(node) {
                self.declare(node);
                continue;
            }
            if node.kind().ends_with("type") || node.kind().contains("type_") {
                continue;
            }
            stack.extend(self.named_children(node));
        }
    }

    /// Record `value` as what `node` evaluated to.
    fn record(&mut self, node: Node<'_>, value: &Value) {
        self.func.values.push(ExprValue {
            span: Span::of(node),
            kind: node.kind(),
            operand: value.operand.clone(),
            place: value.place.clone(),
            at: self.func.body.len(),
        });
    }

    fn record_call(&mut self, receiver: Option<Place>, args: Vec<Option<Place>>) {
        let op = self.func.body.len() - 1;
        self.func
            .call_places
            .push(CallPlaces { op, receiver, args });
    }

    /// `dst = value`, into whatever `target` designates.
    fn write(&mut self, target: Node<'_>, value: Operand) {
        crate::ensure_sufficient_stack(|| self.write_inner(target, value));
    }

    fn write_inner(&mut self, target: Node<'_>, value: Operand) {
        let kind = target.kind();
        if self.is_identifier(target) {
            let dst = Var::new(self.text(target));
            self.func.push(IrOp::Assign { dst, src: value });
            return;
        }
        if let Some(shape) = self.rules.member(kind) {
            let base = self.slot(target, shape.object).map(|n| self.expr(n));
            let field = self
                .slot(target, shape.property)
                .map(|n| self.text(n).to_string())
                .unwrap_or_default();
            if let Some(base) = base {
                self.func.push(IrOp::FieldWrite {
                    base: base.operand,
                    field,
                    src: value,
                });
            }
            return;
        }
        if let Some(shape) = self.rules.subscript(kind) {
            let base = self.slot(target, shape.object).map(|n| self.expr(n));
            let field = self.element_field(self.slot(target, shape.index));
            if let Some(base) = base {
                self.func.push(IrOp::FieldWrite {
                    base: base.operand,
                    field,
                    src: value,
                });
            }
            return;
        }
        if self.rules.passthrough.contains(&kind) {
            if let Some(inner) = self.passthrough_inner(target) {
                self.write(inner, value);
            }
            return;
        }
        if self.rules.address_ops.contains(&kind) {
            // `*p = v`: a write through the pointer (an element of `p`).
            let inner = target
                .child_by_field_name("argument")
                .or_else(|| self.named_children(target).into_iter().last());
            if let Some(inner) = inner {
                let base = self.expr(inner);
                self.func.push(IrOp::FieldWrite {
                    base: base.operand,
                    field: "[]".into(),
                    src: value,
                });
            }
            return;
        }
        // A destructuring pattern (`a, b = …`, `[a, b] = …`, `list($a) = …`):
        // every name in it takes the value.
        let children = self.named_children(target);
        if children.is_empty() {
            return;
        }
        for child in children {
            self.write(child, value.clone());
        }
    }

    /// The field an element access `[index]` uses: `[k]` for a literal key,
    /// `[]` otherwise.
    fn element_field(&self, index: Option<Node<'_>>) -> String {
        match index {
            Some(index) if self.rules.literals.contains(&index.kind()) => {
                format!("[{}]", self.text(index).trim_matches(['"', '\'']))
            }
            _ => "[]".to_string(),
        }
    }

    fn passthrough_inner<'t>(&self, node: Node<'t>) -> Option<Node<'t>> {
        node.child_by_field_name("value")
            .or_else(|| self.named_children(node).into_iter().last())
    }
}

#[cfg(test)]
pub(crate) mod tests;
