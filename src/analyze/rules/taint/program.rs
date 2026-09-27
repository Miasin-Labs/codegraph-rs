//! The program taint rules solve over: the functions of the files, lowered
//! to IR on demand, shared storage canonicalized ([`codegraph_analysis::ir::
//! shared`]), and every call joined to the functions it may run.
//!
//! A call's targets come from the index's call edges (by line/col and
//! name, [`Semantics::callee_functions`]). Where the index resolved nothing
//! — or only a target without a body, an interface or abstract method —
//! the syntax decides, in this order, and never picks among many:
//!
//! 1. a constructor `new X(…)`: `X`'s constructor (by arity);
//! 2. the receiver's allocation (`new X(…).m()`, `Base b = new X(); b.m()`,
//!    C++ `X()`): `m` of `X` or its nearest supertype defining it;
//! 3. an interface or abstract target: its implementations in the
//!    subtypes, when at most [`MAX_IMPLEMENTATIONS`] (a guess);
//! 4. a bare `m(…)`: a C function pointer assigned only function names,
//!    else `m` of the caller's class (or its supertypes), else the one
//!    function `m` of the file;
//! 5. `x.m()` of an unknown receiver: the one method `m` of the file (a
//!    guess).
//!
//! Only functions holding a rule's marks, and what they call, are lowered.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use codegraph_analysis::ir::{IrFunction, IrOp, Operand, Var, shared};
use codegraph_analysis::taint_flow::FuncId;
use codegraph_analysis::taint_flow::program::{CallTargets, Function};
use tree_sitter::Node;

use super::super::engine::FileInput;
use super::super::lang::{self, LangRules};
use super::super::semantics::Semantics;
use super::facts::{Facts, simple_type};

/// Implementations an interface call may run, at most.
pub(super) const MAX_IMPLEMENTATIONS: usize = 4;

/// A function node of some file.
pub(super) struct Candidate {
    pub file: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: u32,
    pub name: String,
    /// Its class's simple name, when it is a method.
    pub owner: Option<String>,
    /// A script's top-level code (the file's root).
    pub top_level: bool,
}

/// A call's resolution before its targets are program ids.
struct Pending {
    names: Vec<String>,
    in_project: bool,
    guessed: bool,
    /// Candidates.
    targets: Vec<usize>,
}

/// Every function node of `files`, and the program built from the ones
/// taint needs.
pub(super) struct Table<'a> {
    pub files: &'a [&'a FileInput<'a>],
    pub candidates: Vec<Candidate>,
    by_range: HashMap<(usize, usize, usize), usize>,
    by_path: HashMap<&'a str, usize>,
    /// (file, name) → candidates.
    by_name: HashMap<(usize, String), Vec<usize>>,
    /// (owner, name) → candidates.
    by_owner: HashMap<(String, String), Vec<usize>>,
    pub facts: Facts,
    /// Candidate → program id, once included.
    pub ids: HashMap<usize, FuncId>,
    /// Program id → candidate.
    pub candidate_of: Vec<usize>,
    pub functions: Vec<Function>,
    /// Ops lowered so far (the node budget).
    pub ops: usize,
    /// Stopped including functions: the budget ran out.
    pub partial: bool,
}

/// The class-like ancestor's simple name.
fn owner_of(node: Node, source: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if matches!(
            n.kind(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "class_specifier"
                | "struct_specifier"
                | "class_definition"
                | "class"
        ) {
            return n
                .child_by_field_name("name")
                .map(|name| simple_type(source.get(name.byte_range()).unwrap_or_default()));
        }
        current = n.parent();
    }
    None
}

impl<'a> Table<'a> {
    pub fn new(files: &'a [&'a FileInput<'a>]) -> Self {
        let mut table = Table {
            files,
            candidates: Vec::new(),
            by_range: HashMap::new(),
            by_path: HashMap::new(),
            by_name: HashMap::new(),
            by_owner: HashMap::new(),
            facts: Facts::read(files),
            ids: HashMap::new(),
            candidate_of: Vec::new(),
            functions: Vec::new(),
            ops: 0,
            partial: false,
        };
        for (index, file) in files.iter().enumerate() {
            table.by_path.insert(file.path, index);
            let rules = lang::for_language(file.language);
            // Iterative walk: depth is bounded by the input.
            let mut stack = vec![file.tree.root_node()];
            while let Some(node) = stack.pop() {
                if rules.functions.contains(&node.kind()) {
                    let name = lang::function_name(node, file.source);
                    // C++ out-of-line `X::m`: the owner is in the name.
                    let (owner, short) = match name.rsplit_once("::") {
                        Some((owner, short)) => (Some(simple_type(owner)), short.to_string()),
                        None => (owner_of(node, file.source), name.clone()),
                    };
                    table.add(Candidate {
                        file: index,
                        start_byte: node.start_byte(),
                        end_byte: node.end_byte(),
                        start_line: node.start_position().row as u32 + 1,
                        name: short,
                        owner,
                        top_level: false,
                    });
                }
                let mut cursor = node.walk();
                stack.extend(node.named_children(&mut cursor));
            }
        }
        table
    }

    fn add(&mut self, candidate: Candidate) -> usize {
        let index = self.candidates.len();
        self.by_range.insert(
            (candidate.file, candidate.start_byte, candidate.end_byte),
            index,
        );
        if !candidate.top_level {
            self.by_name
                .entry((candidate.file, candidate.name.clone()))
                .or_default()
                .push(index);
            if let Some(owner) = &candidate.owner {
                self.by_owner
                    .entry((owner.clone(), candidate.name.clone()))
                    .or_default()
                    .push(index);
            }
        }
        self.candidates.push(candidate);
        index
    }

    /// The candidate for function node `node` of file `file` (the file's
    /// top-level code for its root).
    pub fn candidate(&mut self, file: usize, node: Node) -> usize {
        let key = (file, node.start_byte(), node.end_byte());
        if let Some(&index) = self.by_range.get(&key) {
            return index;
        }
        self.add(Candidate {
            file,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_line: node.start_position().row as u32 + 1,
            name: "the top-level code".to_string(),
            owner: None,
            top_level: node.parent().is_none(),
        })
    }

    /// The function node of a candidate.
    pub fn node(&self, candidate: usize) -> Option<Node<'a>> {
        let c = &self.candidates[candidate];
        let root = self.files[c.file].tree.root_node();
        if c.top_level {
            return Some(root);
        }
        let mut node = root.descendant_for_byte_range(c.start_byte, c.end_byte)?;
        // The smallest node spanning the range may be a child sharing it;
        // the depth bounds the climb.
        while node.byte_range() != (c.start_byte..c.end_byte) || !self.is_function(c.file, node) {
            node = node.parent()?;
        }
        Some(node)
    }

    fn is_function(&self, file: usize, node: Node) -> bool {
        lang::for_language(self.files[file].language)
            .functions
            .contains(&node.kind())
    }

    /// Include candidates `seeds` and every function they may call
    /// (lowering each once), while fewer than `max_ops` ops are lowered.
    pub fn include(&mut self, seeds: &[usize], semantics: &dyn Semantics, max_ops: usize) {
        let mut queue: Vec<usize> = seeds.to_vec();
        queue.reverse();
        let mut pending: Vec<(FuncId, Vec<(usize, Pending)>)> = Vec::new();
        let mut failed: HashSet<usize> = HashSet::new();
        while let Some(candidate) = queue.pop() {
            if self.ids.contains_key(&candidate) || failed.contains(&candidate) {
                continue;
            }
            if self.ops >= max_ops {
                self.partial = true;
                break;
            }
            let Some(ir) = self.lower(candidate) else {
                failed.insert(candidate);
                continue;
            };
            self.ops += ir.body.len();
            let id = self.functions.len() as FuncId;
            self.ids.insert(candidate, id);
            self.candidate_of.push(candidate);
            let language = lang::for_language(self.files[self.candidates[candidate].file].language)
                .ir
                .unwrap_or_default();
            let calls = self.calls_of(candidate, &ir, semantics);
            for (_, call) in &calls {
                queue.extend(call.targets.iter().copied());
            }
            self.functions.push(Function {
                ir,
                language,
                calls: HashMap::new(),
            });
            pending.push((id, calls));
        }
        // Each call's targets as program ids, now that they exist.
        for (id, calls) in pending {
            let resolved: HashMap<usize, CallTargets> = calls
                .into_iter()
                .map(|(op, call)| {
                    let targets: Vec<FuncId> = call
                        .targets
                        .iter()
                        .filter_map(|c| self.ids.get(c).copied())
                        .collect();
                    let complete = targets.len() == call.targets.len();
                    (
                        op,
                        CallTargets {
                            names: call.names,
                            in_project: call.in_project || !call.targets.is_empty(),
                            // A target left out (budget, no IR) is unknown
                            // code: nothing is assumed of the call.
                            targets: if complete { targets } else { Vec::new() },
                            guessed: call.guessed,
                        },
                    )
                })
                .collect();
            self.functions[id as usize].calls = resolved;
        }
    }

    /// The candidate's IR, shared storage canonicalized.
    fn lower(&self, candidate: usize) -> Option<Arc<IrFunction>> {
        let c = &self.candidates[candidate];
        let file = self.files[c.file];
        let rules = lang::for_language(file.language);
        let node = self.node(candidate)?;
        let ir: Rc<IrFunction> = file.lowered(rules, node)?;
        let mut ir = (*ir).clone();
        let scope = self.facts.scope(c.owner.as_deref());
        shared::canonicalize(&mut ir, &scope);
        Some(Arc::new(ir))
    }

    /// The candidate of file `file` named `name` (at line `line`, or the
    /// nearest one, when several).
    fn find(&self, file: &str, name: &str, line: u32) -> Option<usize> {
        let &index = self.by_path.get(file)?;
        let short = name.rsplit("::").next().unwrap_or(name);
        self.by_name
            .get(&(index, short.to_string()))?
            .iter()
            .copied()
            .min_by_key(|&c| self.candidates[c].start_line.abs_diff(line))
    }

    /// Whether a candidate has a body to lower (not an interface or
    /// abstract method, not a prototype).
    fn has_body(&self, candidate: usize) -> bool {
        self.candidates[candidate].top_level
            || self
                .node(candidate)
                .is_some_and(|node| node.child_by_field_name("body").is_some())
    }

    /// Methods `name` of `class`, or of its nearest supertype defining it.
    /// Classes of the same name in several files (inner classes) are told
    /// apart by preferring the caller's file.
    fn method_in_lineage(&self, class: &str, name: &str, file: usize) -> Vec<usize> {
        for ancestor in self.facts.lineage(class) {
            if let Some(found) = self.by_owner.get(&(ancestor, name.to_string())) {
                let bodies: Vec<usize> = found
                    .iter()
                    .copied()
                    .filter(|&c| self.has_body(c))
                    .collect();
                if bodies.len() > 1 {
                    let local: Vec<usize> = bodies
                        .iter()
                        .copied()
                        .filter(|&c| self.candidates[c].file == file)
                        .collect();
                    if !local.is_empty() {
                        return local;
                    }
                }
                if !bodies.is_empty() {
                    return bodies;
                }
            }
        }
        Vec::new()
    }

    /// Implementations of `name` in the subtypes of `class`.
    fn implementations(&self, class: &str, name: &str) -> Vec<usize> {
        let mut found: Vec<usize> = Vec::new();
        for sub in self.facts.subtypes(class) {
            for &c in self
                .by_owner
                .get(&(sub, name.to_string()))
                .into_iter()
                .flatten()
            {
                if self.has_body(c) && !found.contains(&c) {
                    found.push(c);
                }
            }
        }
        found
    }

    /// The functions of file `file` named `name` that have a body.
    fn same_file(&self, file: usize, name: &str) -> Vec<usize> {
        self.by_name
            .get(&(file, name.to_string()))
            .into_iter()
            .flatten()
            .copied()
            .filter(|&c| self.has_body(c))
            .collect()
    }

    /// Keep the candidates taking `args` arguments when that narrows them.
    fn by_arity(&self, found: Vec<usize>, args: usize) -> Vec<usize> {
        if found.len() < 2 {
            return found;
        }
        let fitting: Vec<usize> = found
            .iter()
            .copied()
            .filter(|&c| self.arity(c) == Some(args))
            .collect();
        if fitting.is_empty() { found } else { fitting }
    }

    /// How many parameters a candidate declares.
    fn arity(&self, candidate: usize) -> Option<usize> {
        let mut current = self.node(candidate)?;
        for _ in 0..6 {
            if let Some(list) = current.child_by_field_name("parameters") {
                let mut cursor = list.walk();
                return Some(
                    list.named_children(&mut cursor)
                        .filter(|p| !p.is_extra() && !p.kind().contains("comment"))
                        .count(),
                );
            }
            current = current.child_by_field_name("declarator")?;
        }
        None
    }

    /// What each call op of a candidate's IR may run.
    fn calls_of(
        &self,
        candidate: usize,
        ir: &IrFunction,
        semantics: &dyn Semantics,
    ) -> Vec<(usize, Pending)> {
        let c = &self.candidates[candidate];
        let file = self.files[c.file];
        let rules: &LangRules = lang::for_language(file.language);
        let allocated = allocated_types(ir, &self.facts);
        let pointers = function_pointers(ir);
        let mut out = Vec::new();
        for (op, ir_op) in ir.body.iter().enumerate() {
            let IrOp::Call {
                callee,
                receiver,
                args,
                ..
            } = ir_op
            else {
                continue;
            };
            if callee.starts_with('<') {
                continue;
            }
            let span = ir.span(op);
            let resolution = semantics.call_at(file, span.line, span.col, callee);
            let name = lang::last_name(callee).to_string();
            let mut pending = Pending {
                names: resolution.names.clone(),
                in_project: resolution.in_project,
                guessed: false,
                targets: Vec::new(),
            };
            // The index, with interfaces opened to their implementations.
            let mut bodyless_owner: Option<String> = None;
            for callee_ref in semantics.callee_functions(file, span.line, span.col, callee) {
                match self.find(&callee_ref.file, &callee_ref.name, callee_ref.line) {
                    Some(found) if self.has_body(found) => {
                        if !pending.targets.contains(&found) {
                            pending.targets.push(found);
                        }
                    }
                    found => {
                        bodyless_owner = found
                            .and_then(|f| self.candidates[f].owner.clone())
                            .or_else(|| {
                                let mut parts = callee_ref.qualified_name.rsplit("::");
                                parts.next();
                                parts.next().map(simple_type)
                            });
                    }
                }
            }
            if pending.targets.is_empty() {
                self.fallback(
                    &mut pending,
                    FallbackCall {
                        file: c.file,
                        owner: c.owner.as_deref(),
                        callee,
                        name: &name,
                        receiver: receiver.as_ref(),
                        args: args.len(),
                        bodyless_owner: bodyless_owner.as_deref(),
                        index_resolved: resolution.in_project,
                    },
                    &allocated,
                    &pointers,
                );
            }
            // A name the file defines is project code even unresolved.
            pending.in_project |= !pending.targets.is_empty() || file.defines(rules, &name);
            if pending.in_project || !pending.names.is_empty() {
                out.push((op, pending));
            }
        }
        out
    }

    /// The syntax's answer for a call the index did not resolve to a body.
    fn fallback(
        &self,
        pending: &mut Pending,
        call: FallbackCall,
        allocated: &HashMap<Var, String>,
        pointers: &HashMap<&str, Vec<String>>,
    ) {
        // `this.m()` runs on the caller's class, like a bare `m()`.
        let receiver_class = call.receiver.and_then(|r| match r {
            Operand::Var(var) if matches!(var.as_str(), "this" | "self" | "$this") => {
                call.owner.map(str::to_string)
            }
            Operand::Var(var) => allocated.get(var).cloned(),
            _ => None,
        });
        let (found, guessed) = if let Some(constructed) = call.callee.strip_prefix("new ") {
            let class = simple_type(constructed);
            (self.method_in_lineage(&class, &class, call.file), false)
        } else if let Some(class) = receiver_class {
            (self.method_in_lineage(&class, call.name, call.file), false)
        } else if let Some(owner) = call.bodyless_owner {
            let found = self.implementations(owner, call.name);
            if found.len() > MAX_IMPLEMENTATIONS {
                return;
            }
            pending.targets = found;
            pending.guessed = true;
            return;
        } else if call.receiver.is_none() && !call.callee.contains(['.', ':']) {
            let pointed: Vec<usize> = pointers
                .get(call.callee)
                .into_iter()
                .flatten()
                .flat_map(|function| self.same_file(call.file, function))
                .collect();
            let found = if !pointed.is_empty() {
                pointed
            } else {
                let in_class = call
                    .owner
                    .map(|owner| self.method_in_lineage(owner, call.name, call.file))
                    .unwrap_or_default();
                if in_class.is_empty() {
                    self.same_file(call.file, call.name)
                } else {
                    in_class
                }
            };
            (found, false)
        } else if let Some(Operand::Var(var)) = call.receiver.filter(|_| !call.index_resolved) {
            // `x.m()` of a variable of unknown type: the file's one `m`,
            // never for a call's result or a class (a library's API).
            if var.as_str().starts_with("__t") || self.facts.is_class(var.as_str()) {
                return;
            }
            (self.same_file(call.file, call.name), true)
        } else {
            return;
        };
        let found = self.by_arity(found, call.args);
        if found.len() == 1 {
            pending.targets = found;
            pending.guessed = guessed;
        }
    }
}

/// A call as the fallback reads it.
struct FallbackCall<'c> {
    file: usize,
    /// The calling function's class.
    owner: Option<&'c str>,
    callee: &'c str,
    name: &'c str,
    receiver: Option<&'c Operand>,
    args: usize,
    /// The class of an index target without a body.
    bodyless_owner: Option<&'c str>,
    /// The index resolved it (to code outside the program).
    index_resolved: bool,
}

/// Variables and temporaries holding a freshly constructed object, and its
/// class: `new X(…)`, or a C++ `X(…)` of a known class. A variable assigned
/// anything else is left out.
fn allocated_types(ir: &IrFunction, facts: &Facts) -> HashMap<Var, String> {
    let mut temps: HashMap<&Var, String> = HashMap::new();
    for op in &ir.body {
        if let IrOp::Call {
            dst: Some(dst),
            callee,
            receiver: None,
            ..
        } = op
        {
            let class = match callee.strip_prefix("new ") {
                Some(rest) => Some(simple_type(rest)),
                None => Some(simple_type(callee)).filter(|name| facts.is_class(name)),
            };
            if let Some(class) = class {
                temps.insert(dst, class);
            }
        }
    }
    let mut vars: HashMap<Var, Option<String>> = HashMap::new();
    for op in &ir.body {
        let (dst, class) = match op {
            IrOp::Assign {
                dst,
                src: Operand::Var(src),
            } => (dst, temps.get(src).cloned()),
            IrOp::Assign { dst, .. } | IrOp::BinOp { dst, .. } | IrOp::FieldRead { dst, .. } => {
                (dst, None)
            }
            IrOp::Call { dst: Some(dst), .. } if !temps.contains_key(dst) => (dst, None),
            _ => continue,
        };
        let entry = vars.entry(dst.clone()).or_insert_with(|| class.clone());
        if *entry != class {
            *entry = None;
        }
    }
    let mut out: HashMap<Var, String> = temps
        .into_iter()
        .map(|(var, class)| (var.clone(), class))
        .collect();
    for (var, class) in vars {
        if let Some(class) = class {
            out.insert(var, class);
        }
    }
    out
}

/// Local function pointers: a variable assigned only function names
/// (`void (*f)(char *) = badSink;`) → those names.
fn function_pointers(ir: &IrFunction) -> HashMap<&str, Vec<String>> {
    let mut out: HashMap<&str, Option<Vec<String>>> = HashMap::new();
    for op in &ir.body {
        let IrOp::Assign { dst, src } = op else {
            continue;
        };
        if !ir.locals.contains(dst) {
            continue;
        }
        let entry = out.entry(dst.as_str()).or_insert_with(|| Some(Vec::new()));
        match (src, entry.as_mut()) {
            (Operand::Var(name), Some(names))
                if !name.as_str().starts_with("__t") && !ir.locals.contains(name) =>
            {
                let name = name.as_str().to_string();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            _ => *entry = None,
        }
    }
    out.into_iter()
        .filter_map(|(var, names)| Some((var, names.filter(|n| !n.is_empty())?)))
        .collect()
}
