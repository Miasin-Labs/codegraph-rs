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
//! - a single-field tuple-struct pattern with a written type, such as an
//!   axum handler's `State(state): State<AppState>` ([`newtypes`]);
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
mod links;
mod locals;
mod lookup;
mod newtypes;
mod types;
mod variants;

use bindings::{Binding, binding_in_line};
pub(in crate::resolution::name_matcher) use crates::project_crate_dir;
use expr::{Head, Tail, parse_initializer, scrutinee_end, statement_end, tuple_expression};
pub(in crate::resolution::name_matcher) use fields::{declared_type, self_field_receiver_type};
use locals::caller_fn;
pub(in crate::resolution::name_matcher) use locals::is_local_at_call;
use lookup::resolve_named;
pub(in crate::resolution::name_matcher) use lookup::{
    Dispatch,
    RustType,
    external_path,
    file_is_module,
    fn_local_uses,
    generic_param,
    prelude_type,
    resolve_type,
};
pub(in crate::resolution::name_matcher) use types::{is_deref_wrapper, signature_return};
use types::{named_type, signature_params, split_top_level, unwrapped};

use crate::resolution::line_index::{LineSpan, Lines};
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
        inference.resolve_link(through_guard(value, &reference.reference_name))
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
        inference.resolve_link(through_guard(value, &reference.reference_name))
    })
}

/// The receiver a call of `called` (`m`, `recv.m`) runs on when the
/// receiver's value is a lock result (`m.lock()`): the guarded `T`, as a
/// chain link would take it — a `parking_lot`/`tokio` guard hands back the
/// value itself — unless the method is one a `LockResult` answers (std's
/// `m.lock().unwrap()`).
fn through_guard(value: Value, called: &str) -> Value {
    const RESULT_METHODS: &[&str] = &[
        "and_then",
        "as_mut",
        "as_ref",
        "err",
        "expect",
        "expect_err",
        "into_inner",
        "is_err",
        "is_ok",
        "map",
        "map_err",
        "ok",
        "or_else",
        "unwrap",
        "unwrap_err",
        "unwrap_or",
        "unwrap_or_default",
        "unwrap_or_else",
    ];
    let method = called.rsplit(['.', ':']).next().unwrap_or(called);
    match value {
        Value::Written {
            text,
            self_ty,
            file,
        } if !RESULT_METHODS.contains(&method) => match adaptors::guarded(&text) {
            Some(inner) => Value::Written {
                text: inner.to_string(),
                self_ty,
                file,
            },
            None => Value::Written {
                text,
                self_ty,
                file,
            },
        },
        value => value,
    }
}

/// Run `infer` with the inference state for `reference`'s call site, and
/// the site itself (only the text before the call counts).
fn with_inference<T>(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    infer: impl FnOnce(&Inference<'_>, Site) -> Option<T>,
) -> Option<T> {
    let source = context.read_file_arc(&reference.file_path)?;
    let lines = Lines::of(&source);
    let lines = lines.span();
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
        lines,
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
        .scopes_enclosing_line(&reference.file_path, reference.line)
        .into_iter()
        .find(|node| {
            node.language == Language::Rust
                && matches!(node.kind, NodeKind::Function | NodeKind::Method)
        })
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
    lines: LineSpan<'a>,
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
            } => {
                let ty = self.resolve_written(named_type(&text)?, self_ty.as_ref(), &file)?;
                Some(match types::dispatch_of(&text) {
                    Some(dispatch) => ty.dispatched(dispatch, self.context),
                    None => ty,
                })
            }
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
        if let Some(found) = self.pattern_param(name) {
            return found;
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
        let scoped = self.bound_value_scoped(name, last_line, column, depth, true);
        if matches!(scoped, Some(Some(_))) {
            return scoped;
        }
        // The binding in scope does not spell a type (`let x = match y {
        // A(x) => x, … };`): the one inside the closed block that feeds it
        // is the best evidence left.
        match self.bound_value_scoped(name, last_line, column, depth, false) {
            found @ Some(Some(_)) => found,
            _ => scoped,
        }
    }

    fn bound_value_scoped(
        &self,
        name: &str,
        last_line: Option<usize>,
        column: Option<usize>,
        depth: u8,
        scoped: bool,
    ) -> Option<Option<Value>> {
        let lines = last_line
            .filter(|last| *last >= self.first_line)
            .map_or(0..0, |last| self.first_line..last + 1);
        for index in lines.rev() {
            let Some(full) = self.lines.get(index) else {
                continue;
            };
            if full.len() > MAX_LINE_BYTES {
                continue;
            }
            let on_call_line = column.is_some() && Some(index) == last_line;
            let (line, next_lines) = match column {
                Some(column) if on_call_line => (prefix(full, column), self.lines.slice(0..0)),
                _ => (full, self.lines.from(index + 1)),
            };
            let found = binding_in_line(line, next_lines.iter(), name);
            // `let x = { let x = inner(); wrap(x) };`: a binding inside a
            // block that closed before the call is out of scope there.
            if scoped
                && found.is_some()
                && !on_call_line
                && self.closes_before(index, last_line, column)
            {
                continue;
            }
            let value = match found {
                None => continue,
                Some(Binding::Opaque) => None,
                Some(Binding::Typed(written)) => Some(self.here(written.into_owned())),
                Some(Binding::Newtype {
                    constructor,
                    written,
                }) => self.newtype_field(constructor, &written),
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

    /// Whether the block holding a binding on line `index` closes before
    /// the call site (`last_line`, up to byte `column`): more `}` than `{`
    /// from the binding on. Linear in the lines between, which the upward
    /// search for the binding has already read.
    fn closes_before(&self, index: usize, last_line: Option<usize>, column: Option<usize>) -> bool {
        let Some(last) = last_line else {
            return false;
        };
        let mut depth = 0i64;
        // Lexer state carried across lines: a string (`"…\` continues it)
        // or a raw string's `#` count.
        let mut in_string = false;
        let mut raw: Option<usize> = None;
        for at in index..=last {
            let Some(line) = self.lines.get(at) else {
                return false;
            };
            if line.len() > MAX_LINE_BYTES {
                return false;
            }
            let text = if at == last {
                column.map_or(line, |column| prefix(line, column))
            } else {
                line
            };
            // The binding's own line counts from the binding on (`} else {
            // let x = …` does not close the binding's block).
            let text = if at == index {
                text.find("let ")
                    .or_else(|| text.find("for "))
                    .map_or(text, |start| &text[start..])
            } else {
                text
            };
            let bytes = text.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if let Some(hashes) = raw {
                    // Inside `r#"…"#`: only its own closing quote ends it.
                    if bytes[i] == b'"'
                        && bytes[i + 1..].iter().take_while(|&&b| b == b'#').count() >= hashes
                    {
                        raw = None;
                        i += 1 + hashes;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                if in_string {
                    match bytes[i] {
                        b'\\' => i += 1,
                        b'"' => in_string = false,
                        _ => {}
                    }
                    i += 1;
                    continue;
                }
                match bytes[i] {
                    b'/' if bytes.get(i + 1) == Some(&b'/') => break,
                    b'r' if !prev_ident(bytes, i) && raw_open(&bytes[i + 1..]).is_some() => {
                        let hashes = raw_open(&bytes[i + 1..]).unwrap_or_default();
                        raw = Some(hashes);
                        i += 2 + hashes;
                        continue;
                    }
                    b'"' => in_string = true,
                    // A char literal: `'{'`, `'"'`, `'\''`.
                    b'\'' if bytes.get(i + 2) == Some(&b'\'') => i += 2,
                    b'\''
                        if bytes.get(i + 1) == Some(&b'\\') && bytes.get(i + 3) == Some(&b'\'') =>
                    {
                        i += 3;
                    }
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth < 0 {
                            return true;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        false
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
        // `let x = { …; tail };`: the block's value is its tail.
        if let Some(tail) = block_tail(&text) {
            return self.expression_value(tail, Site::before(index), depth + 1);
        }
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
            let line = self.lines.get(assigned)?;
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

    /// The type of `name` when a parameter pattern of the enclosing fn binds
    /// it: `Some(None)` unless the pattern is one [`newtypes`] types
    /// (`State(state): State<AppState>`).
    fn pattern_param(&self, name: &str) -> Option<Option<Value>> {
        let (pattern, written) = signature_params(self.signature?)
            .into_iter()
            .find(|(pattern, _)| bindings::binds(pattern, name))?;
        Some(
            newtypes::newtype_constructor(pattern, name)
                .and_then(|constructor| self.newtype_field(constructor, written)),
        )
    }

    /// The statement text from byte `offset` of line `index` to its `;`.
    fn statement(&self, index: usize, offset: usize) -> Option<String> {
        let mut text = self.lines.get(index)?.get(offset..)?.to_string();
        for next in self.lines.from(index + 1).iter().take(MAX_STATEMENT_LINES) {
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
            // `(StatusCode::OK, body).into_response()`: a tuple runs no
            // project method.
            Head::Paren(inner) if split_top_level(inner, b',').len() > 1 => {
                Some(Value::Resolved(RustType::external(types::TUPLE)))
            }
            Head::Paren(inner) => self.expression_value(inner, site, depth + 1),
        }
    }
}

/// The tail expression of a block initializer `{ a; b; tail }`, when it
/// has one: a tail naming only items (a path call such as
/// `TcpListener::from_std(l).unwrap()`) types the same outside the block.
fn block_tail(text: &str) -> Option<&str> {
    let text = text.trim();
    let inner = text.strip_prefix('{')?.strip_suffix('}')?;
    if types::matching_paren(text) != Some(text.len() - 1) {
        return None;
    }
    let tail = split_top_level(inner, b';').pop()?.trim();
    if tail.is_empty() || tail.starts_with("let ") {
        return None;
    }
    // A local the block binds (`{ let a = …; a.b() }`) is not in scope
    // where the initializer is read from.
    let head_end = tail
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(tail.len());
    let head = &tail[..head_end];
    let binds_head = !head.is_empty()
        && [format!("let {head}"), format!("let mut {head}")]
            .iter()
            .any(|binding| {
                inner.match_indices(binding.as_str()).any(|(at, _)| {
                    !inner[at + binding.len()..]
                        .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                })
            });
    (!binds_head).then_some(tail)
}

/// The `#` count of a raw string opening right after an `r` (`"…`, `#"…`).
fn raw_open(rest: &[u8]) -> Option<usize> {
    let hashes = rest.iter().take_while(|&&b| b == b'#').count();
    (rest.get(hashes) == Some(&b'"')).then_some(hashes)
}

/// Byte `i` continues an identifier (`for` in `bar"`, not a raw string).
fn prev_ident(bytes: &[u8], i: usize) -> bool {
    // `br"…"` is a raw byte string: the `b` does not make `r` an identifier.
    i > 0
        && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')
        && !(bytes[i - 1] == b'b'
            && (i < 2 || !(bytes[i - 2].is_ascii_alphanumeric() || bytes[i - 2] == b'_')))
}

/// `line` up to byte `column`, clamped to a char boundary.
fn prefix(line: &str, column: usize) -> &str {
    let mut end = column.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_initializer_is_typed_by_its_tail() {
        assert_eq!(
            block_tail("{\n let l = bind();\n Listener::from_std(l) }"),
            Some("Listener::from_std(l)")
        );
        // The tail's head is a local the block binds: not in scope outside.
        assert_eq!(block_tail("{ let a = Foo::new(); a }"), None);
        assert_eq!(block_tail("{ let mut a = Foo::new(); a.b() }"), None);
        assert_eq!(block_tail("{ side_effect(); }"), None);
        assert_eq!(block_tail("{ a } + { b }"), None);
        assert_eq!(block_tail("Foo::new()"), None);
    }

    #[test]
    fn reads_raw_string_openings() {
        assert_eq!(raw_open(b"\"x\""), Some(0));
        assert_eq!(raw_open(b"##\"x\"##"), Some(2));
        assert_eq!(raw_open(b"ow"), None);
        assert!(prev_ident(b"bar\"", 2));
        assert!(!prev_ident(b"br\"", 1));
        assert!(!prev_ident(b" r\"", 1));
    }
}
