//! A taint program over dependency shards: their Rust functions lowered
//! from the (read-only) source trees, calls joined through each shard's
//! own call edges, and — for a query spanning crates — a call a shard
//! leaves unresolved linked by its written path to the one function of
//! that name in another shard.
//!
//! Every function is lowered with the same rules-driven lowering the
//! project's taint rules use, so a dependency's summary means what a
//! project function's does. Work is bounded: functions, ops and a
//! deadline; what does not fit is left out and the program is `partial`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use codegraph_analysis::ir::{IrFunction, IrOp, lower_with_rules};
use codegraph_analysis::taint_flow::program::{CallTargets, Function};
use codegraph_analysis::taint_flow::{FuncId, TaintSpec};
use tree_sitter::Node;

use crate::deps::shard::ShardHandle;
use crate::types::Language;

/// Rust function node kinds a shard records.
const FUNCTION_KINDS: &str = "'function', 'method'";
/// Call arguments one function probes, at most (each is a search).
const MAX_CALL_ARG_PROBES: usize = 128;

/// A file of a crate's tests, benches or examples, at any depth
/// (`test-macros/tests/x.rs`): not the library's code.
pub fn is_test_path(file: &str) -> bool {
    file.split('/')
        .rev()
        .skip(1)
        .any(|dir| matches!(dir, "tests" | "benches" | "examples" | "test" | "testdata"))
        || file.ends_with("/tests.rs")
        || file == "tests.rs"
}

/// A function of a shard, as its index records it.
#[derive(Debug, Clone)]
pub struct FnRow {
    pub id: String,
    pub name: String,
    pub qualified: String,
    pub file: String,
    pub start_line: u32,
    start_col: u32,
    start_byte: Option<usize>,
    end_byte: Option<usize>,
}

/// A call edge out of a function: where it is written and what it runs.
#[derive(Debug, Clone, Copy)]
struct CallRow {
    line: u32,
    col: u32,
    target: usize,
}

/// A shard's Rust functions and the calls among them.
pub struct ShardFunctions {
    pub fns: Vec<FnRow>,
    by_id: HashMap<String, usize>,
    calls: Vec<Vec<CallRow>>,
    by_name: HashMap<String, Vec<usize>>,
    /// Per file: the names its `use` declarations bind, and the crate
    /// each path starts in (`matcher` → `hyper_util`).
    imports: HashMap<String, HashMap<String, String>>,
    /// The crate names code uses for this shard (`hyper_util`; the
    /// toolchain's `std`, `core`, `alloc`).
    pub crates: Vec<String>,
    /// The toolchain's library (never linked into for propagation).
    pub is_toolchain: bool,
}

impl ShardFunctions {
    /// At most `max` functions of `handle` (bulk reads; nothing written).
    pub fn load(handle: &ShardHandle, max: usize) -> rusqlite::Result<Self> {
        let conn = handle.queries().db().conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, name, qualified_name, file_path, start_line, start_byte, end_byte, \
                    start_column \
             FROM nodes WHERE kind IN ({FUNCTION_KINDS}) AND language = 'rust' \
             ORDER BY file_path, start_line LIMIT ?1"
        ))?;
        let fns: Vec<FnRow> = stmt
            .query_map([max as i64], |row| {
                Ok(FnRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    qualified: row.get(2)?,
                    file: row.get(3)?,
                    start_line: row.get::<_, i64>(4)? as u32,
                    start_byte: row.get::<_, Option<i64>>(5)?.map(|b| b as usize),
                    end_byte: row.get::<_, Option<i64>>(6)?.map(|b| b as usize),
                    start_col: row.get::<_, i64>(7)? as u32,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let by_id: HashMap<String, usize> = fns
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id.clone(), index))
            .collect();
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, row) in fns.iter().enumerate() {
            by_name.entry(row.name.clone()).or_default().push(index);
        }
        let mut calls: Vec<Vec<CallRow>> = vec![Vec::new(); fns.len()];
        let mut stmt = conn.prepare(
            "SELECT source, target, line, IFNULL(col, 0) FROM edges \
             WHERE kind = 'calls' AND line IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as u32,
                row.get::<_, i64>(3)? as u32,
            ))
        })?;
        for row in rows {
            let (source, target, line, col) = row?;
            if let (Some(&from), Some(&to)) = (by_id.get(&source), by_id.get(&target)) {
                calls[from].push(CallRow {
                    line,
                    col,
                    target: to,
                });
            }
        }
        let mut imports: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT file_path, signature FROM nodes \
             WHERE kind = 'import' AND language = 'rust' AND signature IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (file, text) = row?;
            let bound = imports.entry(file).or_default();
            for leaf in crate::resolution::name_matcher::parse_use_leaves(&text) {
                let name = match &leaf.binding {
                    crate::resolution::name_matcher::UseBinding::Name(n)
                    | crate::resolution::name_matcher::UseBinding::Module(n) => n.clone(),
                    crate::resolution::name_matcher::UseBinding::Glob => continue,
                };
                if let Some(root) = leaf.path.first() {
                    bound.insert(name, root.clone());
                }
            }
        }
        let meta = handle.meta();
        let is_toolchain = meta.ecosystem == crate::deps::Ecosystem::Rust;
        let crates = if is_toolchain {
            crate::deps::toolchain::STD_CRATES
                .iter()
                .map(|c| (*c).to_string())
                .collect()
        } else {
            vec![meta.name.replace('-', "_")]
        };
        Ok(Self {
            fns,
            by_id,
            calls,
            by_name,
            imports,
            crates,
            is_toolchain,
        })
    }

    /// The crate a path written in `file` starts in: its first segment,
    /// through the file's `use` declarations (`None`: this crate's own
    /// path, or nothing to tell).
    pub fn crate_of_path(&self, file: &str, callee: &str) -> Option<String> {
        let first = callee.split("::").next()?.trim();
        if first.is_empty() || matches!(first, "crate" | "self" | "super" | "Self" | "$crate") {
            return None;
        }
        let root = self
            .imports
            .get(file)
            .and_then(|bound| bound.get(first))
            .map_or(first, String::as_str);
        if matches!(root, "crate" | "self" | "super" | "$crate")
            || self.crates.iter().any(|c| c == root)
        {
            return None;
        }
        Some(root.to_string())
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    /// The one function whose qualified name is `qualified` or ends with
    /// `::qualified` (`Matcher::from_system`).
    pub fn unique_named(&self, qualified: &str) -> Option<usize> {
        let last = qualified.rsplit("::").next().unwrap_or(qualified);
        let suffix = format!("::{qualified}");
        let found: Vec<usize> = self
            .by_name
            .get(last)?
            .iter()
            .copied()
            .filter(|&i| {
                let q = &self.fns[i].qualified;
                q == qualified || q.ends_with(&suffix)
            })
            .collect();
        (found.len() == 1).then(|| found[0])
    }
}

/// How calls a shard leaves unresolved are linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linking {
    /// Not at all: another crate's code is a library call.
    None,
    /// By written path to the one function of that name in another shard
    /// of the program (a query across crates).
    ByName,
}

/// Bounds on one program.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_functions: usize,
    pub max_ops: usize,
    pub deadline: Option<Instant>,
}

/// One shard in a program.
pub struct Unit<'a> {
    pub handle: &'a ShardHandle,
    pub fns: ShardFunctions,
}

/// What a probe of a function marks: the unresolved calls whose
/// arguments are sinks and results environment sources, by op.
#[derive(Debug, Clone, Default)]
pub struct Probes {
    /// `(op, argument, callee as written)`.
    pub call_args: Vec<(usize, usize, String)>,
    /// `(op, callee as written)`.
    pub environment: Vec<(usize, String)>,
}

/// The program: its functions (with calls), where each came from, and
/// the probes of each.
pub struct Program<'a> {
    pub units: Vec<Unit<'a>>,
    pub functions: Vec<Function>,
    /// Program id → (unit, row).
    pub origin: Vec<(usize, usize)>,
    ids: HashMap<(usize, usize), FuncId>,
    pub probes: Vec<Probes>,
    /// The function uses `unsafe` (raw pointers, intrinsics: flows the IR
    /// does not follow), or calls one in the program that does.
    pub lossy: Vec<bool>,
    pub ops: usize,
    /// A bound left functions out.
    pub partial: bool,
}

/// Environment reads, by the callee's written path.
fn is_environment_read(callee: &str) -> bool {
    let tail: Vec<&str> = callee.rsplit("::").take(2).collect();
    matches!(tail.as_slice(), [name, "env"] if matches!(*name, "var" | "var_os" | "vars" | "vars_os"))
}

/// The last name of a callee as written (`a.b::<T>` → `b`).
fn last_name(callee: &str) -> &str {
    let callee = callee.split("::<").next().unwrap_or(callee);
    callee
        .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find(|part| !part.is_empty())
        .unwrap_or(callee)
}

impl<'a> Program<'a> {
    pub fn new(units: Vec<Unit<'a>>) -> Self {
        Self {
            units,
            functions: Vec::new(),
            origin: Vec::new(),
            ids: HashMap::new(),
            probes: Vec::new(),
            lossy: Vec::new(),
            ops: 0,
            partial: false,
        }
    }

    pub fn id_of(&self, unit: usize, row: usize) -> Option<FuncId> {
        self.ids.get(&(unit, row)).copied()
    }

    /// Include `seeds` and, following calls, what they run (by the
    /// shards' edges, and by name across shards with [`Linking::ByName`]),
    /// then join every call to its targets.
    pub fn include(&mut self, seeds: &[(usize, usize)], linking: Linking, limits: &Limits) {
        let mut queue: VecDeque<(usize, usize)> = seeds.iter().copied().collect();
        let mut seen: HashSet<(usize, usize)> = HashSet::new();
        while !queue.is_empty() {
            // One wave: every queued function, a file parsed once.
            let mut wave: BTreeMap<(usize, String), Vec<usize>> = BTreeMap::new();
            while let Some((unit, row)) = queue.pop_front() {
                if seen.insert((unit, row)) {
                    let file = self.units[unit].fns.fns[row].file.clone();
                    wave.entry((unit, file)).or_default().push(row);
                }
            }
            let mut added: Vec<FuncId> = Vec::new();
            for ((unit, file), rows) in wave {
                if self.functions.len() >= limits.max_functions
                    || self.ops >= limits.max_ops
                    || limits.deadline.is_some_and(|d| Instant::now() >= d)
                {
                    self.partial = true;
                    break;
                }
                let source_dir = self.units[unit].handle.source_dir().to_path_buf();
                for (row, ir, uses_unsafe) in
                    lower_rows(&source_dir, &file, &self.units[unit].fns, &rows)
                {
                    let id = self.functions.len() as FuncId;
                    self.lossy.push(uses_unsafe);
                    self.ops += ir.body.len();
                    self.ids.insert((unit, row), id);
                    self.origin.push((unit, row));
                    self.functions.push(Function {
                        ir: Arc::new(ir),
                        language: "rust",
                        calls: HashMap::new(),
                    });
                    self.probes.push(Probes::default());
                    added.push(id);
                }
            }
            for id in added {
                for next in self.callees(id, linking) {
                    if !seen.contains(&next) {
                        queue.push_back(next);
                    }
                }
            }
        }
        for id in 0..self.functions.len() as FuncId {
            self.join_calls(id, linking);
        }
        self.spread_lossy();
    }

    /// A caller of a lossy function is lossy (a worklist over callers).
    fn spread_lossy(&mut self) {
        let mut callers: Vec<Vec<FuncId>> = vec![Vec::new(); self.functions.len()];
        for (id, function) in self.functions.iter().enumerate() {
            for call in function.calls.values() {
                for &target in &call.targets {
                    callers[target as usize].push(id as FuncId);
                }
            }
        }
        let mut work: Vec<FuncId> = (0..self.functions.len() as FuncId)
            .filter(|&id| self.lossy[id as usize])
            .collect();
        while let Some(id) = work.pop() {
            for &caller in &callers[id as usize] {
                if !std::mem::replace(&mut self.lossy[caller as usize], true) {
                    work.push(caller);
                }
            }
        }
    }

    /// What function `id`'s calls may run, as (unit, row).
    fn callees(&self, id: FuncId, linking: Linking) -> Vec<(usize, usize)> {
        let (unit, row) = self.origin[id as usize];
        let mut out: Vec<(usize, usize)> = self.units[unit].fns.calls[row]
            .iter()
            .map(|call| (unit, call.target))
            .collect();
        for op in &self.functions[id as usize].ir.body {
            if let IrOp::Call { callee, .. } = op {
                if let Some(target) = self.by_path_within(unit, row, callee) {
                    out.push((unit, target));
                }
            }
        }
        if linking == Linking::ByName {
            let file = &self.units[unit].fns.fns[row].file;
            for op in &self.functions[id as usize].ir.body {
                if let IrOp::Call { callee, .. } = op {
                    if let Some(found) = self.by_name_elsewhere(unit, file, callee) {
                        out.push(found);
                    }
                }
            }
        }
        out
    }

    /// A path call (`matcher::Matcher::from_system`, written in `file` of
    /// `unit`) to the one function of that name in the crate the path
    /// starts in (through the file's `use`s), when that crate's shard is
    /// in the program.
    fn by_name_elsewhere(&self, unit: usize, file: &str, callee: &str) -> Option<(usize, usize)> {
        if !callee.contains("::") || callee.starts_with('<') || callee.contains('.') {
            return None;
        }
        let krate = self.units[unit].fns.crate_of_path(file, callee)?;
        let other = self
            .units
            .iter()
            .position(|u| u.fns.crates.contains(&krate))?;
        if self.units[other].fns.is_toolchain {
            // std moves data through raw pointers and intrinsics the IR
            // does not follow (`Box::new` writes through
            // `write_via_move`): its calls stay library calls.
            return None;
        }
        let parts: Vec<&str> = callee.split("::").filter(|p| !p.is_empty()).collect();
        // `Type::method` (the last two segments) names it.
        let named = parts[parts.len().saturating_sub(2)..].join("::");
        let row = self.units[other].fns.unique_named(&named)?;
        Some((other, row))
    }

    /// A `Self::m(..)` or `Type::m(..)` call the shard's index left
    /// unresolved, to the one `Type::m` of the same shard (`Self` is the
    /// caller's own type).
    fn by_path_within(&self, unit: usize, row: usize, callee: &str) -> Option<usize> {
        if !callee.contains("::") || callee.starts_with('<') || callee.contains('.') {
            return None;
        }
        let parts: Vec<&str> = callee.split("::").filter(|p| !p.is_empty()).collect();
        let (owner, method) = match parts.as_slice() {
            [.., owner, method] => (*owner, *method),
            _ => return None,
        };
        let owner = if owner == "Self" {
            let caller = &self.units[unit].fns.fns[row].qualified;
            let mut segments = caller.rsplit("::");
            segments.next();
            segments.next()?
        } else {
            owner
        };
        if !owner.starts_with(|c: char| c.is_ascii_uppercase()) {
            return None;
        }
        self.units[unit]
            .fns
            .unique_named(&format!("{owner}::{method}"))
    }

    /// Crates path calls of the program name whose shards it does not
    /// hold (for a caller to open and rebuild with).
    pub fn wanted_crates(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (id, function) in self.functions.iter().enumerate() {
            let (unit, row) = self.origin[id];
            let file = &self.units[unit].fns.fns[row].file;
            for op in &function.ir.body {
                let IrOp::Call { callee, .. } = op else {
                    continue;
                };
                if !callee.contains("::") || callee.contains('.') {
                    continue;
                }
                let Some(krate) = self.units[unit].fns.crate_of_path(file, callee) else {
                    continue;
                };
                let loaded = self.units.iter().any(|u| u.fns.crates.contains(&krate));
                let toolchain = crate::deps::toolchain::STD_CRATES.contains(&krate.as_str());
                if !loaded && !toolchain && !out.contains(&krate) {
                    out.push(krate);
                }
            }
        }
        out
    }

    /// Join each call op of `id` to the program functions it runs; mark
    /// the probes of the calls nothing in the program answers.
    fn join_calls(&mut self, id: FuncId, linking: Linking) {
        let (unit, row) = self.origin[id as usize];
        let ir = Arc::clone(&self.functions[id as usize].ir);
        let edges = &self.units[unit].fns.calls[row];
        let mut calls: HashMap<usize, CallTargets> = HashMap::new();
        let mut probes = Probes::default();
        for (op, ir_op) in ir.body.iter().enumerate() {
            let IrOp::Call { callee, args, .. } = ir_op else {
                continue;
            };
            if callee.starts_with('<') {
                continue;
            }
            let span = ir.span(op);
            let name = last_name(callee);
            let at: Vec<&CallRow> = edges
                .iter()
                .filter(|call| call.line == span.line && call.col == span.col)
                .collect();
            let named: Vec<&CallRow> = at
                .iter()
                .copied()
                .filter(|call| self.units[unit].fns.fns[call.target].name == name)
                .collect();
            let chosen = if named.is_empty() && !callee.contains('.') {
                at
            } else {
                named
            };
            let mut targets: Vec<FuncId> = chosen
                .iter()
                .filter_map(|call| self.id_of(unit, call.target))
                .collect();
            if targets.is_empty() && chosen.is_empty() {
                if let Some(target) = self.by_path_within(unit, row, callee) {
                    targets.extend(self.id_of(unit, target));
                }
            }
            if targets.is_empty() && linking == Linking::ByName {
                let file = &self.units[unit].fns.fns[row].file;
                if let Some((other, row)) = self.by_name_elsewhere(unit, file, callee) {
                    targets.extend(self.id_of(other, row));
                }
            }
            targets.sort_unstable();
            targets.dedup();
            if !targets.is_empty() {
                let names = targets
                    .iter()
                    .map(|&t| {
                        let (u, r) = self.origin[t as usize];
                        self.units[u].fns.fns[r].qualified.clone()
                    })
                    .collect();
                calls.insert(
                    op,
                    CallTargets {
                        names,
                        targets,
                        in_project: true,
                        guessed: false,
                        external: None,
                    },
                );
                continue;
            }
            if !chosen.is_empty() {
                // Resolved in the shard to what has no body here (a trait
                // method's declaration, an intrinsic) or was left out:
                // a library call, which carries its inputs — never
                // nothing, or a summary would drop what flows through it.
                continue;
            }
            if is_environment_read(callee) {
                probes.environment.push((op, callee.clone()));
            } else if callee.contains("::")
                && !callee.ends_with('!')
                && probes.call_args.len() + args.len() <= MAX_CALL_ARG_PROBES
            {
                // A path call into another crate; a macro is no function.
                for arg in 0..args.len() {
                    probes.call_args.push((op, arg, callee.clone()));
                }
            }
        }
        self.functions[id as usize].calls = calls;
        self.probes[id as usize] = probes;
    }

    /// Each function's probes as the solver takes them.
    pub fn specs(&self) -> HashMap<FuncId, TaintSpec> {
        self.probes
            .iter()
            .enumerate()
            .filter(|(_, probes)| !probes.call_args.is_empty() || !probes.environment.is_empty())
            .map(|(id, probes)| {
                (
                    id as FuncId,
                    TaintSpec {
                        call_args: probes
                            .call_args
                            .iter()
                            .map(|(op, arg, _)| (*op, *arg))
                            .collect(),
                        source_calls: probes.environment.iter().map(|(op, _)| *op).collect(),
                        ..TaintSpec::default()
                    },
                )
            })
            .collect()
    }

    /// The function behind a program id.
    pub fn row(&self, id: FuncId) -> &FnRow {
        let (unit, row) = self.origin[id as usize];
        &self.units[unit].fns.fns[row]
    }
}

/// Lower `rows` of `file` (relative to `source_dir`), parsing it once;
/// each with whether its body uses `unsafe`. A file that cannot be read
/// or parsed, or a function whose node is not found, is left out.
fn lower_rows(
    source_dir: &Path,
    file: &str,
    fns: &ShardFunctions,
    rows: &[usize],
) -> Vec<(usize, IrFunction, bool)> {
    let Ok(source) = std::fs::read_to_string(source_dir.join(file)) else {
        return Vec::new();
    };
    let Some(mut parser) = crate::extraction::create_parser(Language::Rust) else {
        return Vec::new();
    };
    let Some(tree) = parser.parse(&source, None) else {
        return Vec::new();
    };
    let root = tree.root_node();
    rows.iter()
        .filter_map(|&row| {
            let node = function_node(root, &fns.fns[row])?;
            let ir = lower_with_rules("rust", node, &source)?;
            let uses_unsafe = source
                .get(node.byte_range())
                .is_some_and(|text| text.contains("unsafe"));
            (ir.body.len() <= codegraph_analysis::taint_flow::MAX_OPS).then_some((
                row,
                ir,
                uses_unsafe,
            ))
        })
        .collect()
}

/// The `function_item` a row records: the one starting where the row
/// does (its byte range, else its line and column), found by descending
/// to that point and climbing — O(depth), never a walk of the file.
fn function_node<'t>(root: Node<'t>, row: &FnRow) -> Option<Node<'t>> {
    let line = row.start_line.saturating_sub(1) as usize;
    let starts_here = |node: &Node<'t>| node.start_position().row == line;
    let climb = |mut node: Node<'t>| -> Option<Node<'t>> {
        loop {
            if node.kind() == "function_item" {
                return starts_here(&node).then_some(node);
            }
            node = node.parent()?;
        }
    };
    if let (Some(start), Some(end)) = (row.start_byte, row.end_byte) {
        if let Some(found) = root.descendant_for_byte_range(start, end).and_then(climb) {
            return Some(found);
        }
    }
    let point = tree_sitter::Point {
        row: line,
        column: row.start_col as usize,
    };
    root.descendant_for_point_range(point, point)
        .and_then(climb)
}

#[cfg(test)]
mod tests {
    use super::{is_environment_read, last_name};

    #[test]
    fn environment_reads_and_names() {
        assert!(is_environment_read("std::env::var"));
        assert!(is_environment_read("env::var_os"));
        assert!(!is_environment_read("var"));
        assert!(!is_environment_read("config::var"));
        assert_eq!(last_name("self.inner.get"), "get");
        assert_eq!(last_name("x.parse::<u32>"), "parse");
        assert_eq!(last_name("Matcher::from_system"), "from_system");
    }
}
