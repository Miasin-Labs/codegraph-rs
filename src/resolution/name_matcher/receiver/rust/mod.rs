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
//!   (`Foo::new(..)`, `Foo::open(p)?`, `load_graph()`), followed by links
//!   whose types are known (see below);
//! - `let Some(x) = …` / `if let Ok(x) = …` over such an initializer;
//! - another local it copies or borrows (`let y = &x;`).
//!
//! A nearer binding that does not spell the type (`for x in …`, `|x|`,
//! `Some(x) =>`, `let x = a.iter();`) shadows every earlier one, so it ends
//! the search with no answer rather than reading past it.
//!
//! A chained receiver (`self.cache.borrow_mut().clear()`), which reaches
//! resolution as its recorded text ([`infer_rust_chain_type`]), is typed
//! the same way: its head is `self`, a local, or a call, and each link after
//! it is followed on the type reached so far ([`links`]) — a field's
//! declared type, a project method's declared return type, or what a std
//! wrapper or container hands out ([`adaptors`]). The first link whose type
//! is not known ends the chain with no answer: the rest is never guessed.
//!
//! Type names are resolved the way the file's `use` declarations and the
//! project's type aliases say ([`lookup`]): `use tree_sitter::Node` makes
//! `Node` an external type even when the project defines its own `Node`.

mod adaptors;
mod bindings;
mod calls;
mod closures;
mod crates;
mod expr;
mod fields;
mod items;
mod line_index;
mod links;
mod locals;
mod lookup;
mod types;
mod variants;

use bindings::{Binding, binding_in_line};
use expr::{Head, Tail, parse_initializer, scrutinee_end, statement_end, tuple_expression};
pub(in crate::resolution::name_matcher) use fields::{declared_type, self_field_receiver_type};
use locals::caller_fn;
pub(in crate::resolution::name_matcher) use locals::is_local_at_call;
use lookup::resolve_named;
pub(in crate::resolution::name_matcher) use lookup::{
    RustType,
    external_path,
    file_is_module,
    fn_local_uses,
    resolve_type,
};
pub(in crate::resolution::name_matcher) use types::signature_return;
use types::{named_type, signature_params, unwrapped};

use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{Language, Node, NodeKind};

/// How many locals deep `let y = x;` chains are followed.
const MAX_DEPTH: u8 = 4;
/// How many lines a `let` statement may span.
const MAX_STATEMENT_LINES: usize = 40;
const MAX_LINE_BYTES: usize = 10_000;

/// The type of the Rust local `receiver` at `reference`'s call site.
pub(in crate::resolution::name_matcher) fn infer_rust_receiver_type(
    receiver: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<RustType> {
    with_inference(reference, context, |inference, site| {
        let value = inference.local_value(receiver, site.line, site.column, 0)?;
        inference.resolve_link(value)
    })
}

/// The type of the receiver a method call at `reference` dropped, from the
/// text extraction recorded for it (`self.cache.borrow_mut()`,
/// `Rule::new(..)`): the head's type, then each link's, as far as every
/// link is known. `None` when some link's type is not.
pub(in crate::resolution::name_matcher) fn infer_rust_chain_type(
    receiver: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<RustType> {
    with_inference(reference, context, |inference, site| {
        let value = inference.expression_value(receiver, site, 0)?;
        inference.resolve_link(value)
    })
}

/// Run `infer` with the inference state for `reference`'s call site, and
/// the site itself (only the text before the call counts).
fn with_inference<T>(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    infer: impl FnOnce(&Inference<'_>, Site) -> Option<T>,
) -> Option<T> {
    let source = context.read_file_arc(&reference.file_path)?;
    let lines = line_index::lines(&source);
    let call_line = (reference.line as usize)
        .saturating_sub(1)
        .min(lines.len().checked_sub(1)?);
    let scope = caller_fn(reference, context);
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
    let site = Site {
        line: Some(call_line),
        column: Some(reference.column as usize),
    };
    infer(&inference, site)
}

/// Where an expression is written: the locals it names are the ones bound
/// at or above `line`, before byte `column` of it when set.
#[derive(Debug, Clone, Copy)]
struct Site {
    line: Option<usize>,
    column: Option<usize>,
}

impl Site {
    /// An initializer on line `index`, whose locals were bound above it.
    fn before(index: usize) -> Site {
        Site {
            line: index.checked_sub(1),
            column: None,
        }
    }
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
#[derive(Debug, Clone)]
enum Value {
    /// A type as written in `file`, where `Self` is `self_ty`.
    Written {
        text: String,
        self_ty: Option<RustType>,
        file: Origin,
    },
    /// A type already resolved.
    Resolved(RustType),
}

/// Where a written type is written: a file of the project, or — reached
/// through a dependency's return type — a file of a crate outside it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    Project(String),
    /// `file` of `krate`'s graph (see [`crate::resolution::ForeignTypes`]).
    Foreign {
        krate: String,
        file: String,
    },
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
            file: Origin::Project(self.reference.file_path.clone()),
        }
    }

    fn resolve(&self, value: Value) -> Option<RustType> {
        match value {
            Value::Resolved(ty) => Some(ty),
            Value::Written {
                text,
                self_ty,
                file,
            } => self.resolve_written(named_type(&text)?, self_ty.as_ref(), &file),
        }
    }

    /// What a written type means where it is written: in a project file, as
    /// the file's `use` declarations say; in a file of a crate outside the
    /// project, as that crate's graph says (a type it defines or imports),
    /// else by name only (`Vec`, a generic `T`) — no method is looked up
    /// on it there.
    fn resolve_written(
        &self,
        named: types::Named<'_>,
        self_ty: Option<&RustType>,
        file: &Origin,
    ) -> Option<RustType> {
        match file {
            Origin::Project(file) => {
                resolve_named(named, self_ty, file, self.reference, self.context)
            }
            Origin::Foreign { krate, file } => match named {
                types::Named::SelfType => self_ty.cloned(),
                types::Named::Structural(name) => Some(RustType::external(name)),
                types::Named::Path(path) => {
                    let placed = self
                        .context
                        .foreign_types()
                        .and_then(|foreign| foreign.resolve_type(krate, file, path));
                    match placed {
                        Some((krate, path)) => RustType::in_crate(&krate, path),
                        None => Some(RustType::external(path.rsplit("::").next()?)),
                    }
                }
            },
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
                Some(Binding::Tuple {
                    annotation,
                    init,
                    position,
                }) => {
                    if on_call_line && statement_end(&line[init..]).is_none() {
                        continue;
                    }
                    // `let (a, b) = (x, y);` binds `b` to `y`.
                    let element = annotation
                        .is_none()
                        .then(|| self.statement(index, init))
                        .flatten()
                        .and_then(|text| {
                            tuple_expression(&text)?
                                .get(position)
                                .map(|element| element.to_string())
                        });
                    match element {
                        Some(element) => {
                            self.expression_value(&element, Site::before(index), depth)
                        }
                        None => self
                            .let_value(annotation, Some(init), index, depth)
                            .and_then(|value| self.tuple_element(value, position)),
                    }
                }
                Some(Binding::Param {
                    open,
                    param,
                    position,
                }) => self.closure_param_value(index, open, param, position, depth),
                Some(Binding::Variant { variant, position }) => {
                    self.variant_field(variant, position)
                }
                Some(Binding::Item { init, position }) => {
                    let iterable = &line[init..];
                    let end = scrutinee_end(iterable);
                    // `for x in x.children() {`: the call is in the iterable.
                    if on_call_line && end == iterable.len() {
                        continue;
                    }
                    let item = self
                        .expression_value(&iterable[..end], Site::before(index), depth)
                        .and_then(|value| self.iterated_value(value));
                    match position {
                        Some(position) => item.and_then(|item| self.tuple_element(item, position)),
                        None => item,
                    }
                }
                Some(Binding::Unwrapped { init }) => {
                    let scrutinee = &line[init..];
                    let end = scrutinee_end(scrutinee);
                    // `if let Some(x) = x.next()`: the call is in the scrutinee.
                    if on_call_line && end == scrutinee.len() {
                        continue;
                    }
                    self.expression_value(&scrutinee[..end], Site::before(index), depth)
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
        self.expression_value(&text, Site::before(index), depth)
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
            self.expression_value(&text, Site::before(assigned), depth)
        })
    }

    /// The written type of the enclosing fn's parameter `name`.
    fn param(&self, name: &str) -> Option<&str> {
        signature_params(self.signature?)
            .into_iter()
            .find(|(pattern, _)| *pattern == name)
            .map(|(_, written)| written)
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

    /// The type of the expression `text`, written at `site`: its head's
    /// type followed through each link (see [`links`]).
    fn expression_value(&self, text: &str, site: Site, depth: u8) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        let initializer = parse_initializer(text)?;
        // A spelled link (`.collect::<T>()`, `as T`) fixes the type,
        // whatever the links before it were.
        let spelled = initializer
            .tails
            .iter()
            .rposition(|tail| matches!(tail, Tail::Spelled(_)));
        let (mut value, tails) = match spelled.map(|at| (&initializer.tails[at], at)) {
            Some((Tail::Spelled(text), at)) => {
                (self.here(text.clone()), &initializer.tails[at + 1..])
            }
            _ => (
                self.head_value(initializer.head, site, depth)?,
                &initializer.tails[..],
            ),
        };
        for tail in tails {
            value = self.link_value(value, tail)?;
        }
        Some(value)
    }

    fn head_value(&self, head: Head<'_>, site: Site, depth: u8) -> Option<Value> {
        match head {
            Head::Named(name) => Some(self.here(name)),
            Head::Call { path, args } => self.call_value(&path, args, site, depth),
            Head::Local(local) => self.local_value(local, site.line, site.column, depth + 1),
            Head::SelfValue => self.owner.clone().map(Value::Resolved),
            Head::Paren(inner) => self.expression_value(inner, site, depth + 1),
        }
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
