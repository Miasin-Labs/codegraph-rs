//! Storage a function shares with other functions — a class's fields, a
//! module's globals — under one canonical name, so every function spells
//! it the same way.
//!
//! Java reads one static field as `data` inside its class, `Cls.data` in
//! another class and `this.data` in an instance method; C reads a global by
//! its bare name everywhere. [`canonicalize`] rewrites each such access into
//! a variable named [`shared_var`] of the field's key (`@Cls.data`):
//! `x.f` reads and writes become plain `Assign`s of that variable, one op
//! for one op, so op indices, spans and recorded values keep lining up.
//! Which names are shared is the caller's to say ([`SharedNames`], from the
//! index); the function's own parameters, receiver, declared locals and
//! temporaries never are.

use std::collections::{HashMap, HashSet};

use super::model::{IrFunction, IrOp, Operand, Place, Var};

/// Prefix of a shared variable's name.
const PREFIX: &str = "@";

/// What the caller knows about names a function does not declare.
pub trait SharedNames {
    /// The key of a bare name the function neither declares nor takes: a
    /// field of its class, a global (`None`: something else — a class, a
    /// function, a constant the analysis need not share).
    fn bare(&self, name: &str) -> Option<String>;
    /// The key of `owner.field` where `owner` is a name the function
    /// neither declares nor takes: a class (`Cls.data`) or the receiver
    /// (`this.data`).
    fn member(&self, owner: &str, field: &str) -> Option<String>;
}

/// The variable standing for shared storage `key`.
pub fn shared_var(key: &str) -> Var {
    Var::new(format!("{PREFIX}{key}"))
}

/// The key of a shared variable, or `None` for any other variable.
pub fn shared_key(var: &Var) -> Option<&str> {
    var.as_str().strip_prefix(PREFIX)
}

/// Rewrite every access of `func` to shared storage into its shared
/// variable. Returns whether anything was rewritten.
pub fn canonicalize(func: &mut IrFunction, names: &dyn SharedNames) -> bool {
    let local: HashSet<Var> = func
        .params
        .iter()
        .chain(&func.receiver)
        .chain(&func.locals)
        .cloned()
        .collect();
    let mut renamer = Renamer {
        names,
        local,
        bare: HashMap::new(),
        member: HashMap::new(),
        changed: false,
    };
    let body = std::mem::take(&mut func.body);
    func.body = body.into_iter().map(|op| renamer.op(op)).collect();
    for value in &mut func.values {
        value.operand = renamer.operand(value.operand.clone());
        value.place = value.place.take().map(|place| renamer.place(place));
    }
    for places in &mut func.call_places {
        places.receiver = places.receiver.take().map(|place| renamer.place(place));
        for arg in &mut places.args {
            *arg = arg.take().map(|place| renamer.place(place));
        }
    }
    renamer.changed
}

struct Renamer<'n> {
    names: &'n dyn SharedNames,
    local: HashSet<Var>,
    bare: HashMap<Var, Option<Var>>,
    member: HashMap<(Var, String), Option<Var>>,
    changed: bool,
}

impl Renamer<'_> {
    fn is_candidate(&self, var: &Var) -> bool {
        !self.local.contains(var) && !var.as_str().starts_with("__t") && shared_key(var).is_none()
    }

    /// The shared variable a bare name stands for.
    fn bare(&mut self, var: &Var) -> Option<Var> {
        if !self.is_candidate(var) {
            return None;
        }
        let names = self.names;
        self.bare
            .entry(var.clone())
            .or_insert_with(|| names.bare(var.as_str()).map(|key| shared_var(&key)))
            .clone()
    }

    /// The shared variable `owner.field` stands for.
    fn member(&mut self, owner: &Var, field: &str) -> Option<Var> {
        if !self.is_candidate(owner) {
            return None;
        }
        let names = self.names;
        self.member
            .entry((owner.clone(), field.to_string()))
            .or_insert_with(|| {
                names
                    .member(owner.as_str(), field)
                    .map(|key| shared_var(&key))
            })
            .clone()
    }

    fn var(&mut self, var: Var) -> Var {
        match self.bare(&var) {
            Some(shared) => {
                self.changed = true;
                shared
            }
            None => var,
        }
    }

    fn operand(&mut self, operand: Operand) -> Operand {
        match operand {
            Operand::Var(var) => Operand::Var(self.var(var)),
            other => other,
        }
    }

    fn place(&mut self, place: Place) -> Place {
        if let Some(field) = place.fields.first() {
            if let Some(shared) = self.member(&place.base, field) {
                self.changed = true;
                return Place {
                    base: shared,
                    fields: place.fields[1..].to_vec(),
                };
            }
        }
        Place {
            base: self.var(place.base),
            fields: place.fields,
        }
    }

    fn op(&mut self, op: IrOp) -> IrOp {
        match op {
            IrOp::FieldRead { dst, base, field } => {
                if let Operand::Var(owner) = &base {
                    if let Some(shared) = self.member(owner, &field) {
                        self.changed = true;
                        return IrOp::Assign {
                            dst: self.var(dst),
                            src: Operand::Var(shared),
                        };
                    }
                }
                IrOp::FieldRead {
                    dst: self.var(dst),
                    base: self.operand(base),
                    field,
                }
            }
            IrOp::FieldWrite { base, field, src } => {
                if let Operand::Var(owner) = &base {
                    if let Some(shared) = self.member(owner, &field) {
                        self.changed = true;
                        return IrOp::Assign {
                            dst: shared,
                            src: self.operand(src),
                        };
                    }
                }
                IrOp::FieldWrite {
                    base: self.operand(base),
                    field,
                    src: self.operand(src),
                }
            }
            IrOp::Assign { dst, src } => IrOp::Assign {
                dst: self.var(dst),
                src: self.operand(src),
            },
            IrOp::BinOp { dst, lhs, op, rhs } => IrOp::BinOp {
                dst: self.var(dst),
                lhs: self.operand(lhs),
                op,
                rhs: self.operand(rhs),
            },
            IrOp::Call {
                dst,
                callee,
                receiver,
                args,
            } => IrOp::Call {
                dst: dst.map(|dst| self.var(dst)),
                callee,
                receiver: receiver.map(|r| self.operand(r)),
                args: args.into_iter().map(|a| self.operand(a)).collect(),
            },
            IrOp::Branch { cond, target } => IrOp::Branch {
                cond: self.operand(cond),
                target,
            },
            IrOp::Return { value } => IrOp::Return {
                value: value.map(|v| self.operand(v)),
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::lower_with_rules;

    struct Fields;

    impl SharedNames for Fields {
        fn bare(&self, name: &str) -> Option<String> {
            (name == "data").then(|| "A.data".to_string())
        }
        fn member(&self, owner: &str, field: &str) -> Option<String> {
            (matches!(owner, "A" | "this") && field == "data").then(|| "A.data".to_string())
        }
    }

    fn lower(code: &str) -> IrFunction {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .expect("java grammar");
        let tree = parser.parse(code, None).expect("parse");
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "method_declaration" {
                return lower_with_rules("java", node, code).expect("lowers");
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        panic!("no method");
    }

    #[test]
    fn every_spelling_of_a_field_becomes_one_variable() {
        let mut func = lower(
            "class B { void f(String p) { String local = p; data = local; A.data = p; \
             this.data = p; String x = A.data; } }",
        );
        assert!(canonicalize(&mut func, &Fields));
        let writes: Vec<&Var> = func
            .body
            .iter()
            .filter_map(|op| match op {
                IrOp::Assign { dst, .. } if shared_key(dst).is_some() => Some(dst),
                _ => None,
            })
            .collect();
        assert_eq!(writes.len(), 3, "{:?}", func.body);
        assert!(writes.iter().all(|var| shared_key(var) == Some("A.data")));
        let reads = func.body.iter().any(|op| {
            matches!(op, IrOp::Assign { src: Operand::Var(v), .. } if shared_key(v) == Some("A.data"))
        });
        assert!(reads, "{:?}", func.body);
    }

    #[test]
    fn declared_names_and_parameters_are_never_shared() {
        let mut func = lower("class B { void f(String data) { String x = data; } }");
        assert!(!canonicalize(&mut func, &Fields));
        let mut func = lower("class B { void f() { String data = \"a\"; String x = data; } }");
        assert!(!canonicalize(&mut func, &Fields));
    }
}
