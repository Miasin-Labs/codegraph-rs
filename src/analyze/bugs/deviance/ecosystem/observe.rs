//! Per-call facts about library API calls, read from syntax: how the
//! result is used, which object the call works on and what else is called
//! on it before and after, and whether an async function keeps the result
//! alive across an `.await`.
//!
//! The same walk serves both sides: the builder observes every crate of
//! the cargo cache with it, and `analyze bugs` observes the project, so a
//! project's site is judged by exactly the facts its belief was mined from.
//!
//! Rust only. One pass per file, in source order; per-site work is a
//! lookup, never a rescan of the function (a name's later uses are recorded
//! as the walk meets them).

use std::collections::{BTreeSet, HashMap, HashSet};

use tree_sitter::Node;

use super::super::results::{self, Use};
use super::super::rules::{self, Rules};
use super::super::syntax;
use crate::analyze::bugs::Project;
use crate::deps::beliefs::model::{SiteObs, UseClass};
use crate::ensure_sufficient_stack;
use crate::types::Language;

/// A call the index (or a read-only pass) resolved into another crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalSite {
    pub file: String,
    /// 1-based line and 0-based column of the call expression.
    pub line: u32,
    pub col: u32,
    /// `crate@compat::Qualified::name`.
    pub api: String,
    /// The called item's own name (the call's last path segment).
    pub name: String,
}

/// One observed call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ObservedSite {
    pub obs: SiteObs,
    pub col: u32,
    /// The enclosing function (qualified name).
    pub function: Option<String>,
}

/// What the walk saw.
#[derive(Debug, Default)]
pub(crate) struct Observed {
    /// API keys, indexed by [`SiteObs::api`].
    pub apis: Vec<String>,
    pub sites: Vec<ObservedSite>,
    /// Every name called outside test code, resolved or not.
    pub called_names: HashSet<String>,
}

/// Methods that hand their receiver's value on unchanged (`m.lock().unwrap()`
/// is still the guard).
const GUARD_PRESERVING: &[&str] = &["unwrap", "expect"];
/// Most statements after a binding looked through for `.await`/`drop`.
const MAX_STATEMENTS_SCANNED: usize = 400;

/// Observe `sites` (external calls of `project`) in the project's Rust
/// files.
pub(crate) fn observe(project: &mut Project, sites: &[ExternalSite]) -> Observed {
    let Some(rules) = rules::for_language(Language::Rust) else {
        return Observed::default();
    };
    let mut by_file: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, site) in sites.iter().enumerate() {
        by_file.entry(site.file.as_str()).or_default().push(index);
    }
    let mut observed = Observed::default();
    let mut api_ids: HashMap<String, u32> = HashMap::new();
    let mut object_ids: HashMap<(String, ObjectKey), u32> = HashMap::new();
    let mut files: Vec<String> = project
        .files()
        .iter()
        .filter(|file| file.ends_with(".rs"))
        .cloned()
        .collect();
    files.sort();
    for file in files {
        if project.parsed(&file).is_none() {
            continue;
        }
        let project = &*project;
        let Some(parsed) = project.parsed_cached(&file) else {
            continue;
        };
        let mut in_file: Vec<&ExternalSite> = by_file
            .get(file.as_str())
            .map(|list| list.iter().map(|&i| &sites[i]).collect())
            .unwrap_or_default();
        in_file.sort_by_key(|site| (site.line, site.col));
        let mut walk = FileWalk {
            rules,
            source: &parsed.source,
            sites: &in_file,
            functions: project.functions_in(&file),
            fn_stack: Vec::new(),
            in_test: 0,
            found: Vec::new(),
            lets: HashMap::new(),
            escapes_after: HashMap::new(),
            called_names: &mut observed.called_names,
        };
        let mut ancestors = Vec::new();
        walk.visit(parsed.tree.root_node(), &mut ancestors);
        let FileWalk {
            found,
            lets,
            escapes_after,
            ..
        } = walk;
        let functions = project.functions_in(&file);
        finish_file(
            &file,
            found,
            &lets,
            &escapes_after,
            functions,
            &mut api_ids,
            &mut object_ids,
            &mut observed,
        );
    }
    observed
}

/// The object a call works on, within one function.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ObjectKey {
    /// A local (or parameter, or captured) name: `v` of `v.set_len(n)`.
    Name(String),
    /// `self.field`: state that outlives the function.
    Field(String),
    /// The value a call chain builds and nothing binds, by the chain's
    /// first call.
    Chain(usize),
}

/// A site found by the walk, before the per-function grouping.
struct Found<'s> {
    site: &'s ExternalSite,
    /// Evaluation order in the file: start, then end byte.
    order: (u32, u32, usize),
    function: Option<usize>,
    use_class: UseClass,
    object: Option<ObjectKey>,
    /// A chain object that is used as a value (returned, passed, stored).
    chain_escapes: bool,
    /// A method call on a receiver (not an associated fn or a free fn).
    receiver: bool,
    await_held: Option<bool>,
}

struct FileWalk<'a, 's> {
    rules: &'static Rules,
    source: &'a str,
    sites: &'a [&'s ExternalSite],
    functions: &'a [crate::analyze::bugs::FnSpan],
    fn_stack: Vec<usize>,
    in_test: usize,
    found: Vec<Found<'s>>,
    /// Per function: each `let`-bound name and where it was bound.
    lets: HashMap<(usize, String), Vec<(u32, u32)>>,
    /// Per function and bound name: positions where the value leaves
    /// (returned, passed, stored, captured by a macro).
    escapes_after: HashMap<(usize, String), Vec<(u32, u32)>>,
    called_names: &'a mut HashSet<String>,
}

impl<'a, 's> FileWalk<'a, 's> {
    fn visit<'t>(&mut self, node: Node<'t>, ancestors: &mut Vec<Node<'t>>) {
        ensure_sufficient_stack(|| {
            let kind = node.kind();
            let entered = (kind == "function_item")
                .then(|| self.function_at(node))
                .flatten();
            if let Some(function) = entered {
                self.fn_stack.push(function);
            }
            if self.in_test == 0 {
                match kind {
                    "call_expression" => self.on_call(node, ancestors),
                    "let_declaration" => self.on_let(node),
                    "identifier" => self.on_identifier(node, ancestors),
                    _ => {}
                }
            }
            ancestors.push(node);
            let mut cursor = node.walk();
            let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
            let mut test_item = false;
            for child in children {
                let child_kind = child.kind();
                if self.rules.attributes.contains(&child_kind) {
                    let attribute = syntax::text(child, self.source);
                    test_item |= self
                        .rules
                        .test_markers
                        .iter()
                        .any(|marker| attribute.contains(marker));
                } else if test_item && !self.rules.comments.contains(&child_kind) {
                    test_item = false;
                    self.in_test += 1;
                    self.visit(child, ancestors);
                    self.in_test -= 1;
                    continue;
                }
                self.visit(child, ancestors);
            }
            ancestors.pop();
            if entered.is_some() {
                self.fn_stack.pop();
            }
        });
    }

    fn function_at(&self, node: Node<'_>) -> Option<usize> {
        let line = syntax::start(node).0;
        let at = self
            .functions
            .partition_point(|span| span.start_line < line);
        (at < self.functions.len() && self.functions[at].start_line == line).then_some(at)
    }

    fn in_test_function(&self) -> bool {
        self.fn_stack
            .last()
            .is_some_and(|&function| self.functions[function].is_test)
    }

    fn on_let(&mut self, node: Node<'_>) {
        let Some(function) = self.fn_stack.last().copied() else {
            return;
        };
        if let Some(name) = bound_name(node, self.source) {
            self.lets
                .entry((function, name.to_string()))
                .or_default()
                .push(syntax::start(node));
        }
    }

    /// A use of a bound name that lets its value leave the function's view:
    /// anything but a method receiver or a field read (`v.len()`, `v.0`),
    /// the binding itself, or `drop(v)`.
    fn on_identifier<'t>(&mut self, node: Node<'t>, ancestors: &[Node<'t>]) {
        let Some(function) = self.fn_stack.last().copied() else {
            return;
        };
        let name = syntax::text(node, self.source);
        let key = (function, name.to_string());
        if !self.lets.contains_key(&key) {
            return;
        }
        let Some(&parent) = ancestors.last() else {
            return;
        };
        let stays = match parent.kind() {
            "field_expression" => parent.child_by_field_name("value") == Some(node),
            "let_declaration" => parent.child_by_field_name("pattern") == Some(node),
            "arguments" => ancestors
                .len()
                .checked_sub(2)
                .map(|i| ancestors[i])
                .is_some_and(|call| is_drop_call(call, self.source)),
            _ => false,
        };
        if !stays {
            self.escapes_after
                .entry(key)
                .or_default()
                .push(syntax::start(node));
        }
    }

    fn on_call<'t>(&mut self, call: Node<'t>, ancestors: &[Node<'t>]) {
        if self.in_test_function() {
            return;
        }
        let Some(name) = syntax::call_name(self.rules, call, self.source) else {
            return;
        };
        self.called_names.insert(name.to_string());
        let at = syntax::start(call);
        let lo = self
            .sites
            .partition_point(|site| (site.line, site.col) < at);
        let matching: Vec<&'s ExternalSite> = self.sites[lo..]
            .iter()
            .take_while(|site| (site.line, site.col) == at)
            .filter(|site| site.name == name)
            .copied()
            .collect();
        if matching.is_empty() {
            return;
        }
        let use_class = match results::classify(self.rules, self.source, call, ancestors) {
            Use::Used => UseClass::Used,
            Use::Discarded => UseClass::Discarded,
            Use::Handled(_) => UseClass::Handled,
            Use::Checked => UseClass::Checked,
        };
        let (object, chain_escapes) = self.object_of(call, ancestors);
        let await_held = self.await_held(call, ancestors);
        let receiver = syntax::receiver(self.rules, call).is_some();
        let function = self.fn_stack.last().copied();
        for site in matching {
            self.found.push(Found {
                site,
                order: (at.0, at.1, call.end_byte()),
                function,
                use_class,
                object: object.clone(),
                chain_escapes,
                receiver,
                await_held,
            });
        }
    }

    /// The object `call` works on: its receiver's root place, or — for a
    /// call that starts a chain (`Vec::with_capacity(n)`, `Client::builder()`)
    /// or whose receiver is one — the value the chain builds, named by the
    /// `let` that binds it when one does.
    fn object_of<'t>(&self, call: Node<'t>, ancestors: &[Node<'t>]) -> (Option<ObjectKey>, bool) {
        let receiver = syntax::receiver(self.rules, call);
        let root = match receiver {
            Some(receiver) => chain_root(receiver),
            None => call,
        };
        match root.kind() {
            "identifier" => (
                Some(ObjectKey::Name(syntax::text(root, self.source).to_string())),
                false,
            ),
            "self" => {
                // `self.f.m()`: the field; `self.m()`: not an object here.
                let field = receiver.and_then(|receiver| self_field(receiver, self.source));
                (field.map(ObjectKey::Field), false)
            }
            "call_expression" => self.chain_object(root, call, ancestors),
            _ => (None, false),
        }
    }

    /// The object built by the chain starting at the call `root`: the
    /// name a `let` binds its end to, else the chain itself (escaping when
    /// its value is used rather than dropped as a statement).
    fn chain_object<'t>(
        &self,
        root: Node<'t>,
        call: Node<'t>,
        ancestors: &[Node<'t>],
    ) -> (Option<ObjectKey>, bool) {
        // Climb from `call` to the chain's end; `call` lies on the chain
        // from `root`.
        let mut top = call;
        let mut depth = ancestors.len();
        while depth > 0 {
            let parent = ancestors[depth - 1];
            let continues = match parent.kind() {
                "field_expression" => parent.child_by_field_name("value") == Some(top),
                "call_expression" => parent.child_by_field_name("function") == Some(top),
                "try_expression" | "await_expression" | "parenthesized_expression" => true,
                _ => false,
            };
            if !continues {
                break;
            }
            top = parent;
            depth -= 1;
        }
        let parent = depth.checked_sub(1).map(|i| ancestors[i]);
        match parent {
            Some(parent)
                if parent.kind() == "let_declaration"
                    && parent.child_by_field_name("value") == Some(top) =>
            {
                match bound_name(parent, self.source) {
                    Some(name) => (Some(ObjectKey::Name(name.to_string())), false),
                    None => (Some(ObjectKey::Chain(root.start_byte())), true),
                }
            }
            Some(parent) if parent.kind() == "expression_statement" => {
                (Some(ObjectKey::Chain(root.start_byte())), false)
            }
            _ => (Some(ObjectKey::Chain(root.start_byte())), true),
        }
    }

    /// In async code, whether the value `call` returns — bound by a `let`,
    /// through `.unwrap()`/`.expect()`/`?`/`.await` — is still alive at the
    /// first `.await` later in its block (`None` without such an await).
    fn await_held<'t>(&self, call: Node<'t>, ancestors: &[Node<'t>]) -> Option<bool> {
        let mut current = call;
        let mut depth = ancestors.len();
        let (binding, block) = loop {
            let parent = *ancestors.get(depth.checked_sub(1)?)?;
            match parent.kind() {
                "try_expression" | "await_expression" | "parenthesized_expression" => {
                    current = parent;
                    depth -= 1;
                }
                "field_expression" if parent.child_by_field_name("value") == Some(current) => {
                    let method = parent
                        .child_by_field_name("field")
                        .map(|field| syntax::text(field, self.source))?;
                    let grand = *ancestors.get(depth.checked_sub(2)?)?;
                    if !GUARD_PRESERVING.contains(&method)
                        || grand.kind() != "call_expression"
                        || grand.child_by_field_name("function") != Some(parent)
                    {
                        return None;
                    }
                    current = grand;
                    depth -= 2;
                }
                "let_declaration" if parent.child_by_field_name("value") == Some(current) => {
                    let name = bound_name(parent, self.source)?;
                    let block = *ancestors.get(depth.checked_sub(2)?)?;
                    break ((parent, name), block);
                }
                _ => return None,
            }
        };
        if !self.is_async(&ancestors[..depth]) {
            return None;
        }
        let (statement, name) = binding;
        // Its own block: dropped (`drop(g)`) or awaited first.
        let mut seen = 0;
        for next in statements_after(block, statement) {
            seen += 1;
            if seen > MAX_STATEMENTS_SCANNED {
                return None;
            }
            let (drops, awaits) = scan_statement(next, name, self.source);
            if drops {
                return Some(false);
            }
            if awaits {
                return Some(true);
            }
        }
        // The block ends (and drops it) before an `.await` further on in the
        // same future: dropped first.
        let mut inner = block;
        for &outer in ancestors[..depth.saturating_sub(2)].iter().rev() {
            match outer.kind() {
                "function_item" | "closure_expression" | "async_block" => return None,
                "block" => {
                    for next in statements_after(outer, inner).take(MAX_STATEMENTS_SCANNED) {
                        if scan_statement(next, name, self.source).1 {
                            return Some(false);
                        }
                    }
                }
                _ => {}
            }
            inner = outer;
        }
        None
    }

    /// The innermost function-like ancestor is async (an `async fn` or an
    /// `async` block).
    fn is_async(&self, ancestors: &[Node<'_>]) -> bool {
        for node in ancestors.iter().rev() {
            match node.kind() {
                "async_block" => return true,
                "closure_expression" => {
                    return syntax::text(*node, self.source)
                        .trim_start()
                        .starts_with("async");
                }
                "function_item" => {
                    let mut cursor = node.walk();
                    return node.named_children(&mut cursor).any(|child| {
                        child.kind() == "function_modifiers"
                            && syntax::text(child, self.source).contains("async")
                    });
                }
                _ => {}
            }
        }
        false
    }
}

/// The receiver chain's root: `v` of `v.a().b`, the first call of
/// `Foo::new().a()`, `self` of `self.x.y`.
fn chain_root(mut node: Node<'_>) -> Node<'_> {
    loop {
        let next = match node.kind() {
            "field_expression" => node.child_by_field_name("value"),
            "call_expression" => {
                let function = node.child_by_field_name("function");
                match function.map(|f| f.kind()) {
                    Some("field_expression") => function,
                    _ => return node,
                }
            }
            "try_expression"
            | "await_expression"
            | "parenthesized_expression"
            | "reference_expression"
            | "unary_expression"
            | "index_expression" => node.named_child(0),
            _ => return node,
        };
        match next {
            Some(next) => node = next,
            None => return node,
        }
    }
}

/// `self.f` of a receiver rooted at `self` through a field (`self.f.g` →
/// `self.f`); `None` for `self` itself or a method result.
fn self_field(receiver: Node<'_>, source: &str) -> Option<String> {
    let mut node = receiver;
    let mut first_field = None;
    loop {
        match node.kind() {
            "field_expression" => {
                first_field = node
                    .child_by_field_name("field")
                    .map(|field| syntax::text(field, source).to_string());
                node = node.child_by_field_name("value")?;
            }
            "self" => return first_field.map(|field| format!("self.{field}")),
            _ => return None,
        }
    }
}

/// The name a `let` binds (`let v`, `let mut v`, `let v: T`), not a pattern.
fn bound_name<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    let pattern = node.child_by_field_name("pattern")?;
    (pattern.kind() == "identifier").then(|| syntax::text(pattern, source))
}

/// `drop(x)`, `mem::drop(x)`, `std::mem::drop(x)`.
fn is_drop_call(node: Node<'_>, source: &str) -> bool {
    node.kind() == "call_expression"
        && node
            .child_by_field_name("function")
            .is_some_and(|function| {
                matches!(
                    syntax::text(function, source),
                    "drop" | "mem::drop" | "std::mem::drop" | "core::mem::drop"
                )
            })
}

/// The named children of `block` after its child `child` (an ancestor of
/// the walk's position): the statements that run after it.
fn statements_after<'t>(block: Node<'t>, child: Node<'t>) -> impl Iterator<Item = Node<'t>> {
    let mut cursor = block.walk();
    let children: Vec<Node<'t>> = block.named_children(&mut cursor).collect();
    let at = children.iter().position(|node| {
        node.byte_range() == child.byte_range() || node.end_byte() >= child.end_byte()
    });
    let from = at.map_or(children.len(), |at| at + 1);
    children.into_iter().skip(from)
}

/// Whether a statement drops `name` (`drop(name)`), and whether it awaits —
/// not counting closures, async blocks or items nested in it.
fn scan_statement(statement: Node<'_>, name: &str, source: &str) -> (bool, bool) {
    let mut drops = false;
    let mut awaits = false;
    let mut stack = vec![statement];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "closure_expression" | "async_block" | "function_item" => continue,
            "await_expression" => awaits = true,
            "call_expression" if is_drop_call(node, source) => {
                let argument = node
                    .child_by_field_name("arguments")
                    .and_then(|args| args.named_child(0));
                if argument.is_some_and(|arg| syntax::text(arg, source) == name) {
                    drops = true;
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    (drops, awaits)
}

/// Group one file's sites by function and object: what comes before and
/// after each on its object, and whether the object escapes.
#[allow(clippy::too_many_arguments)]
fn finish_file(
    file: &str,
    found: Vec<Found<'_>>,
    lets: &HashMap<(usize, String), Vec<(u32, u32)>>,
    escapes_after: &HashMap<(usize, String), Vec<(u32, u32)>>,
    functions: &[crate::analyze::bugs::FnSpan],
    api_ids: &mut HashMap<String, u32>,
    object_ids: &mut HashMap<(String, ObjectKey), u32>,
    observed: &mut Observed,
) {
    let ids: Vec<u32> = found
        .iter()
        .map(|found| intern(api_ids, &found.site.api, observed))
        .collect();
    // Per (function, object): the sites on it, by position.
    let mut groups: HashMap<(Option<usize>, ObjectKey), Vec<usize>> = HashMap::new();
    for (index, found) in found.iter().enumerate() {
        if let Some(object) = &found.object {
            groups
                .entry((found.function, object.clone()))
                .or_default()
                .push(index);
        }
    }
    let mut before: Vec<Vec<u32>> = vec![Vec::new(); found.len()];
    let mut after: Vec<Vec<u32>> = vec![Vec::new(); found.len()];
    let mut methods_before = vec![0u32; found.len()];
    let mut methods_after = vec![0u32; found.len()];
    let mut constructed = vec![false; found.len()];
    for members in groups.values_mut() {
        // In evaluation order: a chain's calls share a start, and the inner
        // one (the receiver) ends first.
        members.sort_by_key(|&index| found[index].order);
        let mut seen: BTreeSet<u32> = BTreeSet::new();
        let (mut methods, mut made) = (0u32, false);
        for &index in members.iter() {
            before[index] = seen
                .iter()
                .copied()
                .filter(|&api| api != ids[index])
                .collect();
            seen.insert(ids[index]);
            methods_before[index] = methods;
            made |= !found[index].receiver;
            constructed[index] = made;
            methods += u32::from(found[index].receiver);
        }
        seen.clear();
        methods = 0;
        for &index in members.iter().rev() {
            after[index] = seen
                .iter()
                .copied()
                .filter(|&api| api != ids[index])
                .collect();
            seen.insert(ids[index]);
            methods_after[index] = methods;
            methods += u32::from(found[index].receiver);
        }
    }
    for (index, found) in found.into_iter().enumerate() {
        let at = (found.site.line, found.site.col);
        let escapes = match (&found.object, found.function) {
            (Some(ObjectKey::Name(name)), Some(function)) => {
                let key = (function, name.clone());
                // Both lists are in source order (the walk's).
                let local = lets
                    .get(&key)
                    .and_then(|bound| bound.first())
                    .is_some_and(|&pos| pos < at);
                let leaves = escapes_after
                    .get(&key)
                    .and_then(|uses| uses.last())
                    .is_some_and(|&pos| pos > at);
                !local || leaves
            }
            (Some(ObjectKey::Chain(_)), _) => found.chain_escapes,
            (Some(_), _) | (None, _) => true,
        };
        let object = found.object.map(|key| {
            let scope = format!("{file}#{}", found.function.map_or(-1, |f| f as i64));
            let next = object_ids.len() as u32;
            *object_ids.entry((scope, key)).or_insert(next)
        });
        observed.sites.push(ObservedSite {
            obs: SiteObs {
                api: ids[index],
                file: file.to_string(),
                line: at.0,
                use_class: found.use_class,
                object,
                escapes,
                before: std::mem::take(&mut before[index]),
                after: std::mem::take(&mut after[index]),
                await_held: found.await_held,
                receiver: found.receiver,
                constructed: constructed[index],
                methods_before: methods_before[index],
                methods_after: methods_after[index],
            },
            col: at.1,
            function: found
                .function
                .map(|function| functions[function].qualified_name.clone()),
        });
    }
}

/// The id of `api` in `observed.apis`, added when new.
fn intern(ids: &mut HashMap<String, u32>, api: &str, observed: &mut Observed) -> u32 {
    if let Some(&id) = ids.get(api) {
        return id;
    }
    let id = observed.apis.len() as u32;
    observed.apis.push(api.to_string());
    ids.insert(api.to_string(), id);
    id
}
