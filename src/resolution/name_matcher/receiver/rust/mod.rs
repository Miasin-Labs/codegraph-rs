//! Rust receiver-type inference for `recv.method(..)` calls.
//!
//! The receiver is a local or a parameter of the enclosing fn (else a
//! file-level `static`/`const` or a unit struct). Its type is read from the
//! nearest binding above the call, and only from bindings that pin it down:
//!
//! - an annotation: `let x: Foo = …`, `|x: &Foo|`, a parameter `x: &mut Foo`
//!   (smart pointers such as `Box<Foo>` and `Arc<Foo>` deref to `Foo`);
//! - an initializer that spells the type (`Foo { .. }`, `Foo(..)`,
//!   `Kind::Leaf`, a literal) or calls something whose signature does
//!   (`Foo::new(..)`, `Foo::open(p)?`, `load_graph()`), optionally followed
//!   by `?`, `.unwrap()`, `.clone()`, `.await`, and the like;
//! - `let Some(x) = …` / `if let Ok(x) = …` over such an initializer;
//! - another local it copies or borrows (`let y = &x;`).
//!
//! A nearer binding that does not spell the type (`for x in …`, `|x|`,
//! `Some(x) =>`, `let x = a.iter();`) shadows every earlier one, so it ends
//! the search with no answer rather than reading past it.
//!
//! Type names are resolved the way the file's `use` declarations and the
//! project's type aliases say ([`lookup`]): `use tree_sitter::Node` makes
//! `Node` an external type even when the project defines its own `Node`.

mod bindings;
mod crates;
mod expr;
mod lookup;
mod types;

use bindings::{Binding, binding_in_line};
use expr::{Head, Tail, parse_initializer, scrutinee_end, statement_end};
pub(in crate::resolution::name_matcher) use lookup::{RustType, resolve_type};
use lookup::{assoc_fn_return, free_fn_return, resolve_named};
use types::{is_deref_wrapper, named_type, signature_params, starts_uppercase, unwrapped};

use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{Language, Node, NodeKind};

/// How many locals deep `let y = x;` chains are followed.
const MAX_DEPTH: u8 = 4;
/// How many lines a `let` statement may span.
const MAX_STATEMENT_LINES: usize = 40;
const MAX_LINE_BYTES: usize = 10_000;

/// Std traits called as `Trait::f(..)`: the type is whatever `Self` is.
const STD_TRAITS: &[&str] = &[
    "Clone",
    "Default",
    "From",
    "FromIterator",
    "FromStr",
    "Into",
    "TryFrom",
    "TryInto",
];

/// The type of the Rust local `receiver` at `reference`'s call site.
pub(in crate::resolution::name_matcher) fn infer_rust_receiver_type(
    receiver: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<RustType> {
    let source = context.read_file_arc(&reference.file_path)?;
    let lines: Vec<&str> = source
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let call_line = (reference.line as usize)
        .saturating_sub(1)
        .min(lines.len().checked_sub(1)?);
    let scope = enclosing_fn(reference, context);
    let owner = scope
        .as_ref()
        .and_then(method_owner)
        .map(|owner| resolve_type(owner, &reference.file_path, reference, context));
    let inference = Inference {
        reference,
        context,
        lines: &lines,
        // Outside any fn (a `static` initializer) no local is in scope.
        first_line: scope.as_ref().map_or(call_line + 1, |node| {
            node.start_line.saturating_sub(1) as usize
        }),
        signature: scope.as_ref().and_then(|node| node.signature.as_deref()),
        owner,
    };
    let column = Some(reference.column as usize);
    let value = inference.local_value(receiver, Some(call_line), column, 0)?;
    inference.resolve(value)
}

/// The innermost Rust fn or method whose lines contain the reference.
fn enclosing_fn(reference: &UnresolvedRef, context: &dyn ResolutionContext) -> Option<Node> {
    context
        .get_nodes_in_file(&reference.file_path)
        .into_iter()
        .filter(|node| {
            node.language == Language::Rust
                && matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && node.start_line <= reference.line
                && node.end_line.max(node.start_line) >= reference.line
        })
        .max_by_key(|node| node.start_line)
}

/// The impl type (or trait) a method belongs to: `Graph` for `Graph::load`.
fn method_owner(node: &Node) -> Option<&str> {
    if node.kind != NodeKind::Method {
        return None;
    }
    let (owner, _) = node.qualified_name.rsplit_once("::")?;
    owner.rsplit("::").next()
}

/// A type on its way through an initializer's postfix operations.
enum Value {
    /// A type as written in `file`, where `Self` is `self_ty`.
    Written {
        text: String,
        self_ty: Option<RustType>,
        file: String,
    },
    /// A type already resolved.
    Resolved(RustType),
}

struct Inference<'a> {
    reference: &'a UnresolvedRef,
    context: &'a dyn ResolutionContext,
    lines: &'a [&'a str],
    /// Index of the enclosing fn's first line.
    first_line: usize,
    signature: Option<&'a str>,
    /// What `Self` means in the enclosing fn.
    owner: Option<RustType>,
}

impl Inference<'_> {
    /// A type written in the referencing file.
    fn here(&self, text: impl Into<String>) -> Value {
        Value::Written {
            text: text.into(),
            self_ty: self.owner.clone(),
            file: self.reference.file_path.clone(),
        }
    }

    fn resolve(&self, value: Value) -> Option<RustType> {
        match value {
            Value::Resolved(ty) => Some(ty),
            Value::Written {
                text,
                self_ty,
                file,
            } => resolve_named(
                named_type(&text)?,
                self_ty.as_ref(),
                &file,
                self.reference,
                self.context,
            ),
        }
    }

    /// `Option<T>`/`Result<T, _>` to `T`. A resolved external type stays
    /// external (`Regex::new(..).unwrap()`); a resolved project one is not
    /// known.
    fn unwrap(&self, value: Value) -> Option<Value> {
        match value {
            Value::Written {
                text,
                self_ty,
                file,
            } => Some(Value::Written {
                text: unwrapped(&text)?.to_string(),
                self_ty,
                file,
            }),
            Value::Resolved(ty) if !ty.is_project_type(self.context) => Some(Value::Resolved(ty)),
            Value::Resolved(_) => None,
        }
    }

    /// The type of `name` at or above `last_line` (the call line when
    /// `column` is set: only the text before the call counts): a local or
    /// parameter of the enclosing fn, else a file-level `static`/`const` or
    /// a unit struct.
    fn local_value(
        &self,
        name: &str,
        last_line: Option<usize>,
        column: Option<usize>,
        depth: u8,
    ) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        if let Some(found) = self.bound_value(name, last_line, column, depth) {
            return found;
        }
        if let Some(param) = self.param(name) {
            return Some(self.here(param));
        }
        self.item_value(name)
    }

    /// The type of the nearest binding of `name` in the enclosing fn: `None`
    /// when there is none, `Some(None)` when the nearest one does not spell
    /// a type.
    fn bound_value(
        &self,
        name: &str,
        last_line: Option<usize>,
        column: Option<usize>,
        depth: u8,
    ) -> Option<Option<Value>> {
        let lines = last_line
            .filter(|last| *last >= self.first_line)
            .map_or(0..0, |last| self.first_line..last + 1);
        for index in lines.rev() {
            let full = self.lines[index];
            if full.len() > MAX_LINE_BYTES {
                continue;
            }
            let on_call_line = column.is_some() && Some(index) == last_line;
            let (line, next_lines): (&str, &[&str]) = match column {
                Some(column) if on_call_line => (prefix(full, column), &[]),
                _ => (full, &self.lines[index + 1..]),
            };
            let value = match binding_in_line(line, next_lines, name) {
                None => continue,
                Some(Binding::Opaque) => None,
                Some(Binding::Typed(written)) => Some(self.here(written.into_owned())),
                Some(Binding::Let { annotation, init }) => {
                    // `let x = x.len();`: the new `x` is not in scope in its
                    // own initializer.
                    let finished = init.is_some_and(|init| statement_end(&line[init..]).is_some());
                    if on_call_line && !finished {
                        continue;
                    }
                    match (annotation, init) {
                        // `let x;` … `x = Foo::new();`
                        (None, None) => {
                            last_line.and_then(|last| self.deferred_value(name, index, last, depth))
                        }
                        _ => self.let_value(annotation, init, index, depth),
                    }
                }
                Some(Binding::Unwrapped { init }) => {
                    let scrutinee = &line[init..];
                    let end = scrutinee_end(scrutinee);
                    // `if let Some(x) = x.next()`: the call is in the scrutinee.
                    if on_call_line && end == scrutinee.len() {
                        continue;
                    }
                    self.expression_value(&scrutinee[..end], index, depth)
                        .and_then(|value| self.unwrap(value))
                }
            };
            return Some(value);
        }
        None
    }

    fn let_value(
        &self,
        annotation: Option<&str>,
        init: Option<usize>,
        index: usize,
        depth: u8,
    ) -> Option<Value> {
        if let Some(annotation) = annotation.filter(|written| named_type(written).is_some()) {
            return Some(self.here(annotation));
        }
        let text = self.statement(index, init?)?;
        self.expression_value(&text, index, depth)
    }

    /// The type of `let name;` from its first assignment `name = …;` after
    /// line `index` and at or above `last_line`.
    fn deferred_value(
        &self,
        name: &str,
        index: usize,
        last_line: usize,
        depth: u8,
    ) -> Option<Value> {
        (index + 1..=last_line).find_map(|assigned| {
            let line = self.lines[assigned];
            let rest = line.trim_start().strip_prefix(name)?.trim_start();
            let init = rest
                .strip_prefix('=')
                .filter(|init| !init.starts_with('='))?;
            let text = self.statement(assigned, line.len() - init.len())?;
            self.expression_value(&text, assigned, depth)
        })
    }

    /// The written type of the enclosing fn's parameter `name`.
    fn param(&self, name: &str) -> Option<&str> {
        signature_params(self.signature?)
            .into_iter()
            .find(|(pattern, _)| *pattern == name)
            .map(|(_, written)| written)
    }

    /// A file-level `static`/`const` (`RE.is_match(..)`), or a project unit
    /// struct used as a value (`MyResolver.detect(..)`).
    fn item_value(&self, name: &str) -> Option<Value> {
        if !starts_uppercase(name) {
            return None;
        }
        if !name.bytes().any(|byte| byte.is_ascii_lowercase()) {
            return self.lines.iter().find_map(|line| {
                if line.len() > MAX_LINE_BYTES {
                    return None;
                }
                match binding_in_line(line, &[], name) {
                    Some(Binding::Let {
                        annotation: Some(annotation),
                        ..
                    }) => Some(self.here(annotation)),
                    _ => None,
                }
            });
        }
        let ty = resolve_type(
            name,
            &self.reference.file_path,
            self.reference,
            self.context,
        );
        ty.is_project_type(self.context)
            .then_some(Value::Resolved(ty))
    }

    /// The statement text from byte `offset` of line `index` to its `;`.
    fn statement(&self, index: usize, offset: usize) -> Option<String> {
        let mut text = self.lines[index].get(offset..)?.to_string();
        for next in self.lines.iter().skip(index + 1).take(MAX_STATEMENT_LINES) {
            if statement_end(&text).is_some() {
                break;
            }
            text.push('\n');
            text.push_str(next);
        }
        let end = statement_end(&text)?;
        text.truncate(end);
        Some(text)
    }

    /// The type of the expression `text`, written on line `index`.
    fn expression_value(&self, text: &str, index: usize, depth: u8) -> Option<Value> {
        let initializer = parse_initializer(text)?;
        let mut value = match initializer.head {
            Head::Named(name) => self.here(name),
            Head::Call { path, args } => self.call_value(&path, args, index, depth)?,
            Head::Local(local) => self.local_value(local, index.checked_sub(1), None, depth + 1)?,
        };
        for tail in initializer.tails {
            value = match tail {
                Tail::Same => value,
                Tail::Unwrap => self.unwrap(value)?,
                Tail::Spelled(text) => self.here(text),
            };
        }
        Some(value)
    }

    /// The type a call of `path` returns.
    fn call_value(&self, path: &[&str], args: &str, index: usize, depth: u8) -> Option<Value> {
        let (&callee, qualifier) = path.split_last()?;
        let Some(&owner) = qualifier.last() else {
            return match callee {
                "Some" => Some(Value::Resolved(RustType::external("Option"))),
                "Ok" | "Err" => Some(Value::Resolved(RustType::external("Result"))),
                _ if starts_uppercase(callee) => Some(self.here(callee)),
                _ => self.free_fn_value(callee),
            };
        };
        if starts_uppercase(callee) {
            // A tuple struct or tuple variant: `Kind::Leaf(..)`, `m::Id(..)`.
            let named = if starts_uppercase(owner) {
                qualifier
            } else {
                path
            };
            return Some(self.here(named.join("::")));
        }
        if !starts_uppercase(owner) {
            return self.free_fn_value(&path.join("::"));
        }
        let owner_ty = match owner {
            "Self" => self.owner.clone()?,
            _ => resolve_type(
                &qualifier.join("::"),
                &self.reference.file_path,
                self.reference,
                self.context,
            ),
        };
        if !owner_ty.is_project_type(self.context) {
            if is_deref_wrapper(&owner_ty.name) {
                // `Arc::new(Graph::new())` derefs to what it wraps.
                return matches!(callee, "new" | "from" | "clone" | "pin")
                    .then(|| self.expression_value(args, index, depth + 1))
                    .flatten();
            }
            if STD_TRAITS.contains(&owner_ty.name.as_str()) {
                return None;
            }
            // An external type's associated fn: the type itself (`HashMap::new`).
            return Some(Value::Resolved(owner_ty));
        }
        if let Some((text, file)) = assoc_fn_return(&owner_ty, callee, self.reference, self.context)
        {
            return Some(Value::Written {
                text,
                self_ty: Some(owner_ty),
                file,
            });
        }
        // Trait constructors a derive or blanket impl may supply.
        match callee {
            "default" | "clone" | "from" => Some(Value::Resolved(owner_ty)),
            "try_from" | "from_str" => Some(Value::Written {
                text: "Result<Self>".to_string(),
                self_ty: Some(owner_ty),
                file: self.reference.file_path.clone(),
            }),
            _ => None,
        }
    }

    fn free_fn_value(&self, path: &str) -> Option<Value> {
        let (text, file) = free_fn_return(path, self.reference, self.context)?;
        Some(Value::Written {
            text,
            self_ty: None,
            file,
        })
    }
}

/// `line` up to byte `column`, clamped to a char boundary.
fn prefix(line: &str, column: usize) -> &str {
    let mut end = column.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}
